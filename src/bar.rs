//! The status bar: widgets ("modules", as in waybar) in three groups per
//! monitor, drawn as floating glass islands or one full-width strip.
//!
//! ```text
//!  ╭──────────────╮      ╭────────────────────╮      ╭────────────────────────────────────────────╮
//!  │ ( 1 )  2  4  │      │ ▣  Window title    │      │ ♫ Song  ◔ 12%  ◑ 48%  🔈 40%  ᯤ ᛒ  Sun 22 · 21:46 │
//!  ╰──────────────╯      ╰────────────────────╯      ╰────────────────────────────────────────────╯
//!     modules_left           modules_center                       modules_right
//! ```
//!
//! Which widgets appear where comes from `modules_left`, `modules_center`
//! and `modules_right` in the config ([`config::Module`]). The space between
//! islands is fully transparent and click-through.
//!
//! The bar is a per-pixel-alpha layered window. Each frame is drawn with
//! Direct2D/DirectWrite into a 32-bit DIB and pushed with
//! `UpdateLayeredWindow`; there is no `WM_PAINT`. The window manager pushes
//! a fresh [`BarState`] whenever something changes, and drawing only reads
//! that state, so the bar never touches window-manager internals. Live data
//! (clock, network, CPU, volume, custom widgets) is checked on a
//! one-second timer and redraws the bar only when it changed.

use crate::config::{self, CustomModule, Module};
use crate::gfx::{icon_bitmap, label, rgba, rounded, Factories, Label, Surface};
use crate::icons::Icon;
use crate::layout::Rect;
use crate::status::{self, Status};
use crate::theme::Palette;
use crate::win::{self, wide};
use crate::{audio, custom, sys};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap, ID2D1DCRenderTarget, ID2D1SolidColorBrush, ID2D1StrokeStyle, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
    D2D1_CAP_STYLE_ROUND, D2D1_DASH_STYLE_SOLID, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_LINE_JOIN_ROUND, D2D1_ROUNDED_RECT,
    D2D1_STROKE_STYLE_PROPERTIES,
};
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows_numerics::Vector2;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const CLASS: &str = "gromak_bar";
const CLOCK_TIMER: usize = 1;
const WM_MOUSELEAVE: u32 = 0x02A3;
const NOTICE_DURATION: Duration = Duration::from_secs(5);
/// Redraw timer while the workspace indicator slides.
const SLIDE_TIMER: usize = 2;
/// How long the workspace indicator takes to slide to a new pill.
const SLIDE_DURATION: Duration = Duration::from_millis(220);
/// Volume change per wheel notch over the volume widget.
const VOLUME_STEP: f32 = 0.02;

/// The sliding highlight behind the active workspace pill: (x, width).
#[derive(Clone, Copy, Debug)]
struct Indicator {
    from: (f32, f32),
    to: (f32, f32),
    start: Instant,
}

impl Indicator {
    fn progress(&self) -> f32 {
        (self.start.elapsed().as_secs_f32() / SLIDE_DURATION.as_secs_f32()).clamp(0.0, 1.0)
    }

    fn done(&self) -> bool {
        self.progress() >= 1.0
    }

    /// Position and width now, with an ease-out-quint curve.
    fn current(&self) -> (f32, f32) {
        let t = 1.0 - (1.0 - self.progress()).powi(5);
        (self.from.0 + (self.to.0 - self.from.0) * t, self.from.1 + (self.to.1 - self.from.1) * t)
    }
}

/// What the bar shows for one workspace.
#[derive(Clone, Debug, PartialEq)]
pub struct WsItem {
    /// 0-based workspace index.
    pub index: usize,
    /// Shown on this monitor.
    pub active: bool,
    /// Has at least one window.
    pub occupied: bool,
}

/// Everything the bar shows, pushed by the window manager.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BarState {
    pub workspaces: Vec<WsItem>,
    pub title: String,
    /// `HICON` of the focused window (0 for none). Owned by that window.
    pub icon: isize,
    /// This monitor holds the keyboard focus.
    pub focused_monitor: bool,
}

/// Colours (`COLORREF`), metrics and options derived from the config,
/// already scaled for one monitor.
#[derive(Clone, Debug)]
pub struct Style {
    pub background: u32,
    pub foreground: u32,
    pub accent: u32,
    pub muted: u32,
    pub opacity: f32,
    pub radius: f32,
    /// One full-width strip instead of floating islands.
    pub full: bool,
    /// Blur what is behind the bar (full style).
    pub blur: bool,
    pub font: crate::config::FontSource,
    /// For numbers: workspace numbers, the time, percentages.
    pub font_mono: crate::config::FontSource,
    pub font_px: f32,
    pub scale: f32,
    pub clock_24h: bool,
    /// Left, centre and right widgets.
    pub modules: [Vec<Module>; 3],
    pub custom: BTreeMap<String, CustomModule>,
}

impl Style {
    /// Layout options from the config, colours from the active palette.
    pub fn new(cfg: &config::Bar, palette: &Palette, scale: f32) -> Self {
        custom::configure(&cfg.custom);
        crate::gfx::set_sharp(cfg.corner_radius <= 0);
        Style {
            background: palette.background,
            foreground: palette.foreground,
            accent: palette.accent,
            muted: palette.muted,
            opacity: cfg.opacity.clamp(0.0, 1.0),
            full: cfg.style == config::BarStyle::Full,
            blur: cfg.blur && cfg.style == config::BarStyle::Full,
            radius: cfg.corner_radius.max(0) as f32 * scale,
            font: cfg.font_source(),
            font_mono: cfg.mono_source(),
            font_px: cfg.font_size as f32 * scale,
            scale,
            clock_24h: cfg.clock_24h,
            modules: cfg.modules(),
            custom: cfg.custom.clone(),
        }
    }

    fn uses(&self, module: &Module) -> bool {
        self.modules.iter().any(|side| side.contains(module))
    }

    /// Network and Bluetooth come from the status thread.
    fn needs_status_thread(&self) -> bool {
        [Module::Network, Module::Bluetooth].iter().any(|m| self.uses(m))
    }
}

/// Something clickable on the bar that highlights under the pointer.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Hit {
    /// A workspace pill (0-based index on this monitor).
    Workspace(usize),
    Date,
    Time,
    Module(Module),
}

/// The live data a frame shows. The bar redraws when it changes.
#[derive(Clone, Debug, Default, PartialEq)]
struct Live {
    time: String,
    date: String,
    status: Option<Status>,
    cpu: u32,
    memory: u32,
    /// Percent and muted.
    volume: Option<(u32, bool)>,
    custom: Vec<(String, String)>,
}

impl Live {
    fn gather(style: &Style) -> Live {
        let mut live = Live::default();
        if style.uses(&Module::Clock) {
            live.time = clock_text(style.clock_24h);
            live.date = date_text();
        }
        if style.uses(&Module::Network) || style.uses(&Module::Bluetooth) {
            live.status = status::current();
        }
        if style.uses(&Module::Cpu) || style.uses(&Module::Memory) {
            let usage = sys::usage();
            live.cpu = (usage.cpu * 100.0).round() as u32;
            live.memory = (usage.memory * 100.0).round() as u32;
        }
        if style.uses(&Module::Volume) {
            live.volume = audio::get().map(|v| ((v.level * 100.0).round() as u32, v.muted));
        }
        for side in &style.modules {
            for module in side {
                if let Module::Custom(name) = module {
                    live.custom.push((name.clone(), custom::output(name)));
                }
            }
        }
        live
    }

    fn custom(&self, name: &str) -> &str {
        self.custom.iter().find(|(n, _)| n == name).map_or("", |(_, text)| text.as_str())
    }
}

/// Device-dependent resources, recreated if Direct2D loses the device.
struct Gfx {
    target: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
    regular: IDWriteTextFormat,
    strong: IDWriteTextFormat,
    mono: IDWriteTextFormat,
    mono_strong: IDWriteTextFormat,
    /// Round caps and joins, for the icons.
    round: ID2D1StrokeStyle,
    /// The icon bitmap and the `HICON` it was made from.
    icon: Option<(isize, ID2D1Bitmap)>,
}

struct Model {
    rect: Rect,
    style: Style,
    state: BarState,
    surface: Option<Surface>,
    gfx: Option<Gfx>,
    notice: Option<(String, Instant)>,
    hover: Option<Hit>,
    tracking_mouse: bool,
    /// Clickable x-ranges.
    hits: Vec<(f32, f32, Hit)>,
    /// The live data last drawn.
    live: Live,
    /// The sliding active-workspace highlight.
    indicator: Option<Indicator>,
    /// An animation is running and needs more frames.
    animating: bool,
}

thread_local! {
    static MODELS: RefCell<HashMap<isize, Model>> = RefCell::new(HashMap::new());
}

/// Releases every bar's drawing state (see [`crate::gfx::shutdown`]).
pub fn shutdown() {
    MODELS.with(|m| m.borrow_mut().clear());
}

/// One-time setup: registers the window class. Call after [`crate::gfx::init`].
pub fn init() {
    unsafe {
        let class = wide(CLASS);
        let mut wc: WNDCLASSW = zeroed();
        wc.lpfnWndProc = Some(bar_proc);
        wc.hInstance = GetModuleHandleW(null());
        wc.hCursor = LoadCursorW(null_mut(), IDC_ARROW);
        wc.lpszClassName = class.as_ptr();
        RegisterClassW(&wc);
    }
}

/// Redraws every bar now (e.g. after the volume changed from a panel).
pub fn refresh_all() {
    let keys: Vec<isize> = MODELS.with(|m| m.borrow().keys().copied().collect());
    for key in keys {
        render(key);
    }
}

/// A bar window on one monitor.
pub struct Bar {
    hwnd: isize,
}

impl Bar {
    pub fn new(rect: Rect, style: Style) -> Option<Bar> {
        let class = wide(CLASS);
        let title = wide("gromak bar");
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class.as_ptr(),
                title.as_ptr(),
                WS_POPUP,
                rect.x,
                rect.y,
                rect.w,
                rect.h,
                null_mut(),
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            )
        };
        if hwnd.is_null() {
            return None;
        }
        if style.blur {
            // The blur sits under our own (translucent) drawing; no extra tint.
            win::set_acrylic(hwnd as isize, style.background, 0);
        }
        if style.needs_status_thread() {
            status::start();
        }
        let model = Model {
            rect,
            style,
            state: BarState::default(),
            surface: Surface::new(rect.w, rect.h),
            gfx: None,
            notice: None,
            hover: None,
            tracking_mouse: false,
            hits: Vec::new(),
            live: Live::default(),
            indicator: None,
            animating: false,
        };
        MODELS.with(|m| m.borrow_mut().insert(hwnd as isize, model));
        render(hwnd as isize);
        unsafe {
            SetTimer(hwnd, CLOCK_TIMER, 1000, None);
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        Some(Bar { hwnd: hwnd as isize })
    }

    /// Replaces what the bar shows; redraws only when something changed.
    pub fn set_state(&self, state: BarState) {
        let changed = with_model(self.hwnd, |model| {
            let changed = model.state != state;
            model.state = state;
            changed
        });
        if changed == Some(true) {
            render(self.hwnd);
        }
    }

    /// The workspace pill (0-based number on this monitor) at a screen point.
    pub fn workspace_at(&self, x: i32, y: i32) -> Option<usize> {
        with_model(self.hwnd, |m| {
            if !m.rect.contains(x, y) {
                return None;
            }
            match hit(&m.hits, (x - m.rect.x) as f32) {
                Some(Hit::Workspace(index)) => Some(index),
                _ => None,
            }
        })
        .flatten()
    }

    /// Shows a transient message (e.g. a config error) in place of the title.
    pub fn notify(&self, text: &str) {
        with_model(self.hwnd, |model| model.notice = Some((text.to_string(), Instant::now() + NOTICE_DURATION)));
        render(self.hwnd);
    }

    /// Applies new colours or options (e.g. after the wallpaper changed).
    pub fn set_style(&self, style: Style) {
        if style.needs_status_thread() {
            status::start();
        }
        with_model(self.hwnd, |model| {
            model.style = style;
            model.gfx = None;
        });
        render(self.hwnd);
    }

    pub fn set_visible(&self, visible: bool) {
        unsafe {
            ShowWindow(self.hwnd as HWND, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
        }
    }
}

impl Drop for Bar {
    fn drop(&mut self) {
        unsafe { DestroyWindow(self.hwnd as HWND) };
        MODELS.with(|m| m.borrow_mut().remove(&self.hwnd));
    }
}

fn with_model<R>(hwnd: isize, f: impl FnOnce(&mut Model) -> R) -> Option<R> {
    MODELS.with(|m| m.borrow_mut().get_mut(&hwnd).map(f))
}

/// What a click on the bar does, worked out while the model is borrowed
/// and carried out after.
enum Click {
    /// Post a `BAR_*` message to the window manager.
    Post(usize, isize),
    Run(String),
    Volume(f32),
    ToggleMute,
}

fn run_command(command: &str) {
    if !command.trim().is_empty() {
        if let Err(e) = win::run_command(command) {
            warn!("bar: running {command}: {e}");
        }
    }
}

unsafe extern "system" fn bar_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let key = hwnd as isize;
    match msg {
        WM_TIMER if wp == SLIDE_TIMER => {
            render(key);
            0
        }
        WM_TIMER => {
            let stale = with_model(key, |m| {
                let expired = m.notice.as_ref().is_some_and(|(_, until)| Instant::now() >= *until);
                expired || Live::gather(&m.style) != m.live
            });
            if stale == Some(true) {
                render(key);
            }
            0
        }
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_MOUSEMOVE => {
            let x = (lp & 0xFFFF) as i16 as f32;
            let changed = with_model(key, |m| {
                if !m.tracking_mouse {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    TrackMouseEvent(&mut tme);
                    m.tracking_mouse = true;
                }
                let hover = hit(&m.hits, x);
                std::mem::replace(&mut m.hover, hover.clone()) != hover
            });
            if changed == Some(true) {
                render(key);
            }
            0
        }
        WM_MOUSELEAVE => {
            let changed = with_model(key, |m| {
                m.tracking_mouse = false;
                m.hover.take().is_some()
            });
            if changed == Some(true) {
                render(key);
            }
            0
        }
        WM_LBUTTONDOWN | WM_RBUTTONUP => {
            let x = (lp & 0xFFFF) as i16 as f32;
            let click = with_model(key, |m| {
                let (l, r, target) = m.hits.iter().find(|(l, r, _)| x >= *l && x < *r).cloned()?;
                // Popups hang below the widget, centred on it (screen pixels).
                let anchor = {
                    let (ax, ay) = (m.rect.x + ((l + r) / 2.0) as i32, m.rect.bottom());
                    ((ay as isize) << 32) | (ax as u32 as isize)
                };
                let module_cmd = |pick: fn(&CustomModule) -> &String, name: &str| {
                    m.style.custom.get(name).map(|c| Click::Run(pick(c).clone()))
                };
                match (msg, target) {
                    (WM_LBUTTONDOWN, Hit::Workspace(i)) => Some(Click::Post(crate::BAR_WORKSPACE, i as isize)),
                    (WM_LBUTTONDOWN, Hit::Date) => Some(Click::Post(crate::BAR_CALENDAR, anchor)),
                    (WM_LBUTTONDOWN, Hit::Time) => Some(Click::Post(crate::BAR_CLOCK, anchor)),
                    (WM_LBUTTONDOWN, Hit::Module(module)) => match module {
                        Module::Network => Some(Click::Post(crate::BAR_NETWORK, anchor)),
                        Module::Bluetooth => Some(Click::Post(crate::BAR_BLUETOOTH, anchor)),
                        Module::Cpu | Module::Memory => Some(Click::Post(crate::BAR_SYSTEM, anchor)),
                        Module::Volume => Some(Click::Post(crate::BAR_VOLUME, anchor)),
                        Module::Custom(name) => module_cmd(|c| &c.on_click, &name),
                        _ => None,
                    },
                    (WM_RBUTTONUP, Hit::Module(Module::Volume)) => Some(Click::ToggleMute),
                    (WM_RBUTTONUP, Hit::Module(Module::Custom(name))) => module_cmd(|c| &c.on_right_click, &name),
                    _ => None,
                }
            })
            .flatten();
            match click {
                Some(Click::Post(kind, value)) => crate::post_to_ctrl(crate::WM_APP_BAR, kind, value),
                Some(Click::Run(command)) => run_command(&command),
                Some(Click::ToggleMute) => {
                    audio::toggle_mute();
                    refresh_all();
                }
                Some(Click::Volume(_)) | None => {}
            }
            0
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp >> 16) & 0xFFFF) as u16 as i16;
            let up = delta > 0;
            // Wheel messages carry screen coordinates.
            let screen_x = (lp & 0xFFFF) as i16 as i32;
            let click = with_model(key, |m| {
                let x = (screen_x - m.rect.x) as f32;
                match hit(&m.hits, x) {
                    Some(Hit::Module(Module::Volume)) => {
                        Some(Click::Volume(if up { VOLUME_STEP } else { -VOLUME_STEP }))
                    }
                    Some(Hit::Module(Module::Custom(name))) => m.style.custom.get(&name).and_then(|c| {
                        let command = if up { &c.on_scroll_up } else { &c.on_scroll_down };
                        (!command.is_empty()).then(|| Click::Run(command.clone()))
                    }),
                    _ => None,
                }
            })
            .flatten();
            match click {
                Some(Click::Volume(delta)) => {
                    audio::change(delta);
                    refresh_all();
                }
                Some(Click::Run(command)) => run_command(&command),
                _ => crate::post_to_ctrl(crate::WM_APP_BAR, crate::BAR_SCROLL, if up { -1 } else { 1 }),
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn hit(hits: &[(f32, f32, Hit)], x: f32) -> Option<Hit> {
    hits.iter().find(|(l, r, _)| x >= *l && x < *r).map(|h| h.2.clone())
}

fn clock_text(twenty_four_hour: bool) -> String {
    let t = win::local_time();
    if twenty_four_hour {
        format!("{:02}:{:02}", t.wHour, t.wMinute)
    } else {
        let h = match t.wHour % 12 {
            0 => 12,
            h => h,
        };
        format!("{h}:{:02} {}", t.wMinute, if t.wHour < 12 { "AM" } else { "PM" })
    }
}

fn date_text() -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let t = win::local_time();
    format!("{} {} {}", DAYS[t.wDayOfWeek as usize % 7], t.wDay, MONTHS[(t.wMonth as usize + 11) % 12])
}

/// Draws the bar for `hwnd` and pushes it to the screen.
fn render(hwnd: isize) {
    crate::gfx::with_factories(|factories| {
        with_model(hwnd, |model| unsafe {
            if let Err(e) = draw(factories, model) {
                // Most likely a lost device: rebuild resources next time.
                warn!("bar: draw failed: {e}");
                model.gfx = None;
                return;
            }
            if let Some(surface) = &model.surface {
                crate::gfx::present(hwnd, surface, model.rect, 255);
            }
            // Keep drawing frames while the indicator slides (~120 fps timer;
            // Windows rounds it to the system timer resolution).
            if model.animating {
                SetTimer(hwnd as HWND, SLIDE_TIMER, 8, None);
            } else {
                KillTimer(hwnd as HWND, SLIDE_TIMER);
            }
        });
    });
}

// ----- layout -----------------------------------------------------------------

/// The small glyph in front of a widget's text.
#[derive(Clone, Copy)]
enum Glyph {
    Network(Status),
    Bluetooth(Status),
    Ring(f32),
    Speaker(f32, bool),
}

/// One measured widget, ready to paint.
enum Piece {
    Workspaces,
    Window {
        title: Label,
        color: D2D1_COLOR_F,
        strong: bool,
    },
    Clock {
        date: Label,
        time: Label,
    },
    /// A glyph and/or text; `caption` is a muted prefix ("CPU").
    Chip {
        module: Module,
        glyph: Option<Glyph>,
        caption: Option<Label>,
        text: Option<Label>,
        color: D2D1_COLOR_F,
    },
}

/// Metrics shared by layout and painting.
struct Metrics {
    s: f32,
    h: f32,
    /// Island padding at its ends.
    inner: f32,
    /// Space between widgets in a group.
    gap: f32,
    /// Workspace pills.
    pad: f32,
    pill_h: f32,
    glyph: f32,
    glyph_gap: f32,
    /// How far a widget's hover highlight reaches past its content.
    hover_pad: f32,
}

impl Metrics {
    fn new(style: &Style, h: f32) -> Metrics {
        let s = style.scale;
        let pad = (5.0 * s).round();
        Metrics {
            s,
            h,
            inner: (20.0 * s).round(),
            gap: (22.0 * s).round(),
            pad,
            pill_h: h - 2.0 * pad,
            glyph: (15.0 * s).round(),
            glyph_gap: (8.0 * s).round(),
            hover_pad: (8.0 * s).round(),
        }
    }
}

/// Width of the workspace pills, and each pill's (item, label, x, width)
/// relative to the group's left edge.
fn workspace_pills(
    f: &Factories,
    gfx: &Gfx,
    state: &BarState,
    m: &Metrics,
    w: f32,
) -> (f32, Vec<(WsItem, Label, f32, f32)>) {
    let mut items = Vec::new();
    let mut x = 0.0;
    for item in &state.workspaces {
        let format = if item.active { &gfx.mono_strong } else { &gfx.mono };
        let Some(number) = (unsafe { label(f, format, &(item.index + 1).to_string(), w, m.h) }) else { continue };
        let pill_w = m.pill_h.max(number.width + m.pad);
        items.push((item.clone(), number, x, pill_w));
        x += pill_w + 3.0 * m.s;
    }
    ((x - 3.0 * m.s).max(0.0), items)
}

impl Piece {
    fn width(&self, m: &Metrics, workspaces_w: f32) -> f32 {
        match self {
            Piece::Workspaces => workspaces_w,
            Piece::Window { title, .. } => title.width,
            Piece::Clock { date, time } => date.width + 2.0 * (9.0 * m.s).round() + (3.0 * m.s).max(2.0) + time.width,
            Piece::Chip { glyph, caption, text, .. } => {
                let mut w = 0.0;
                if glyph.is_some() {
                    w += m.glyph;
                }
                for l in [caption, text].into_iter().flatten() {
                    if w > 0.0 {
                        w += m.glyph_gap;
                    }
                    w += l.width;
                }
                w
            }
        }
    }

    /// Workspace pills reach the island's edge more closely than text.
    fn edge(&self, m: &Metrics) -> f32 {
        match self {
            Piece::Workspaces => m.pad,
            _ => m.inner,
        }
    }
}

/// Measures one widget; `None` hides it (nothing to show).
unsafe fn piece(
    f: &Factories,
    gfx: &Gfx,
    model: &Model,
    module: &Module,
    live: &Live,
    title_max: f32,
) -> Option<Piece> {
    let style = &model.style;
    let (w, h) = (model.rect.w as f32, model.rect.h as f32);
    let fg = style.foreground;
    let chip = |glyph: Option<Glyph>, caption: Option<&str>, text: Option<&str>, strong: bool, max: f32, color| {
        let caption = caption.and_then(|c| label(f, &gfx.regular, c, w, h));
        let text = text.and_then(|t| label(f, if strong { &gfx.mono_strong } else { &gfx.regular }, t, max, h));
        Some(Piece::Chip { module: module.clone(), glyph, caption, text, color })
    };
    match module {
        Module::Workspaces => Some(Piece::Workspaces),
        Module::Window => {
            if let Some((msg, _)) = &model.notice {
                let title = label(f, &gfx.strong, msg, title_max, h)?;
                return Some(Piece::Window { title, color: rgba(style.accent, 1.0), strong: true });
            }
            if model.state.title.is_empty() || title_max <= 0.0 {
                return None;
            }
            let title = label(f, &gfx.regular, &model.state.title, title_max, h)?;
            Some(Piece::Window { title, color: rgba(fg, 0.92), strong: false })
        }
        Module::Clock => Some(Piece::Clock {
            date: label(f, &gfx.regular, &live.date, w, h)?,
            time: label(f, &gfx.mono_strong, &live.time, w, h)?,
        }),
        Module::Network => chip(Some(Glyph::Network(live.status?)), None, None, false, 0.0, rgba(fg, 1.0)),
        Module::Bluetooth => {
            let st = live.status?;
            st.bluetooth?;
            chip(Some(Glyph::Bluetooth(st)), None, None, false, 0.0, rgba(fg, 1.0))
        }
        Module::Cpu => chip(
            Some(Glyph::Ring(live.cpu as f32 / 100.0)),
            Some("CPU"),
            Some(&format!("{}%", live.cpu)),
            true,
            w,
            rgba(fg, 0.95),
        ),
        Module::Memory => chip(
            Some(Glyph::Ring(live.memory as f32 / 100.0)),
            Some("RAM"),
            Some(&format!("{}%", live.memory)),
            true,
            w,
            rgba(fg, 0.95),
        ),
        Module::Volume => {
            let (percent, muted) = live.volume?;
            let text = if muted { "Muted".to_string() } else { format!("{percent}%") };
            let color = rgba(fg, if muted { 0.5 } else { 0.95 });
            chip(Some(Glyph::Speaker(percent as f32 / 100.0, muted)), None, Some(&text), !muted, w, color)
        }
        Module::Custom(name) => {
            let text = live.custom(name);
            if text.is_empty() {
                return None;
            }
            chip(None, None, Some(text), false, (320.0 * style.scale).round(), rgba(fg, 0.92))
        }
    }
}

unsafe fn draw(f: &Factories, model: &mut Model) -> windows::core::Result<()> {
    if model.surface.is_none() {
        return Ok(());
    }
    if model.gfx.is_none() {
        model.gfx = Some(create_gfx(f, &model.style)?);
    }
    if model.notice.as_ref().is_some_and(|(_, until)| Instant::now() >= *until) {
        model.notice = None;
    }
    let style = model.style.clone();
    let live = Live::gather(&style);
    let (w, h) = (model.rect.w as f32, model.rect.h as f32);
    let m = Metrics::new(&style, h);
    let s = m.s;

    // Keep the focused window's icon bitmap current.
    {
        let gfx = model.gfx.as_mut().expect("just created");
        let icon = model.state.icon;
        if gfx.icon.as_ref().map(|(handle, _)| *handle) != Some(icon) {
            gfx.icon = if icon != 0 { icon_bitmap(f, &gfx.target, icon).map(|b| (icon, b)) } else { None };
        }
    }
    let gfx = model.gfx.as_ref().expect("just created");
    let (workspaces_w, pills) = workspace_pills(f, gfx, &model.state, &m, w);
    let icon_w =
        if gfx.icon.is_some() && model.notice.is_none() { (16.0 * s).round() + (9.0 * s).round() } else { 0.0 };

    // ── Layout ───────────────────────────────────────────────────────────
    // The sides first; the centre group (usually the window title) gets
    // the room left between them.
    let mut modules = style.modules.clone();
    if model.notice.is_some() && !modules.iter().any(|side| side.contains(&Module::Window)) {
        modules[1].insert(0, Module::Window);
    }
    let build = |side: &[Module], title_max: f32| -> Vec<Piece> {
        side.iter().filter_map(|module| piece(f, gfx, model, module, &live, title_max)).collect()
    };
    let group_w = |pieces: &[Piece]| -> f32 {
        if pieces.is_empty() {
            return 0.0;
        }
        let content: f32 = pieces
            .iter()
            .map(|p| {
                p.width(&m, workspaces_w) + if matches!(p, Piece::Window { strong: false, .. }) { icon_w } else { 0.0 }
            })
            .sum();
        content + m.gap * (pieces.len() - 1) as f32 + pieces[0].edge(&m) + pieces[pieces.len() - 1].edge(&m)
    };
    let side_title = (w * 0.25).round();
    let left = build(&modules[0], side_title);
    let right = build(&modules[2], side_title);
    let (left_w, right_w) = (group_w(&left), group_w(&right));
    let outer_gap = (10.0 * s).round();
    // Centred on the bar when possible, else in whatever room is left.
    let centred_room = w - 2.0 * left_w.max(right_w) - 2.0 * outer_gap;
    let free_room = w - left_w - right_w - 2.0 * outer_gap;
    let center_without_title =
        group_w(&build(&modules[1].iter().filter(|m| **m != Module::Window).cloned().collect::<Vec<_>>(), 0.0));
    let room = if centred_room > 200.0 * s { centred_room } else { free_room };
    let title_max = (room.min(w * 0.5) - center_without_title - 2.0 * m.inner - icon_w - m.gap).max(0.0);
    let center = build(&modules[1], title_max);
    let center_w = group_w(&center);
    let center_l = ((w - center_w) / 2.0).max(left_w + outer_gap).min(w - right_w - outer_gap - center_w);

    // ── Background ───────────────────────────────────────────────────────
    crate::gfx::bind(&gfx.target, model.surface.as_ref().expect("checked"), model.rect.w, model.rect.h)?;
    gfx.target.BeginDraw();
    gfx.target.Clear(Some(&rgba(0, 0.0)));
    let target = &gfx.target;
    let brush = &gfx.brush;
    let fill = |r: &D2D1_ROUNDED_RECT, color: D2D1_COLOR_F| {
        brush.SetColor(&color);
        target.FillRoundedRectangle(r, brush);
    };
    if style.full {
        // One tinted strip (over the blur, when on) with a hairline along
        // the bottom.
        brush.SetColor(&rgba(style.background, style.opacity));
        target.FillRectangle(&D2D_RECT_F { left: 0.0, top: 0.0, right: w, bottom: h }, brush);
        brush.SetColor(&rgba(style.foreground, 0.08));
        target.FillRectangle(&D2D_RECT_F { left: 0.0, top: h - 1.0, right: w, bottom: h }, brush);
    }
    // Islands: flat rounded shapes with a hairline border.
    let island = |l: f32, r: f32| {
        if style.full || r <= l {
            return;
        }
        let radius = style.radius;
        fill(&rounded(l, 0.0, r, h, radius), rgba(style.background, style.opacity));
        crate::gfx::border(target, brush, (l, 0.0, r, h), radius, rgba(style.foreground, 0.10));
    };
    let text = |l: &Label, x: f32, color: D2D1_COLOR_F| {
        brush.SetColor(&color);
        target.DrawTextLayout(Vector2 { X: x, Y: 0.0 }, &l.layout, brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
    };

    // ── Groups ───────────────────────────────────────────────────────────
    model.hits.clear();
    model.animating = false;
    let mut indicator = model.indicator;
    let mut animating = false;
    let hover = model.hover.clone();
    let mut hits: Vec<(f32, f32, Hit)> = Vec::new();
    for (pieces, start, width) in [(&left, 0.0, left_w), (&center, center_l, center_w), (&right, w - right_w, right_w)]
    {
        if pieces.is_empty() {
            continue;
        }
        island(start, start + width);
        let mut x = start + pieces[0].edge(&m);
        for (i, piece) in pieces.iter().enumerate() {
            if i > 0 {
                x += m.gap;
            }
            let pw = piece.width(&m, workspaces_w);
            let cy = h / 2.0;
            let hovered = |hit: &Hit| hover.as_ref() == Some(hit);
            let highlight = |l: f32, r: f32| {
                fill(&rounded(l, m.pad, r, h - m.pad, (m.pill_h * 0.32).round()), rgba(style.foreground, 0.10));
            };
            match piece {
                Piece::Workspaces => {
                    let radius = (m.pill_h * 0.32).round();
                    if let Some((_, _, tx, tw)) = pills.iter().find(|p| p.0.active) {
                        let target_pos = (x + tx, *tw);
                        let shown = indicator.map_or(target_pos, |ind| ind.current());
                        match indicator {
                            Some(ind) if ind.to == target_pos => {}
                            Some(_) => {
                                indicator = Some(Indicator { from: shown, to: target_pos, start: Instant::now() })
                            }
                            None => {
                                indicator = Some(Indicator { from: target_pos, to: target_pos, start: Instant::now() })
                            }
                        }
                        let ind = indicator.expect("just set");
                        let (ix, iw) = ind.current();
                        animating = !ind.done();
                        let accent = if model.state.focused_monitor { style.accent } else { style.muted };
                        fill(&rounded(ix, m.pad, ix + iw, m.pad + m.pill_h, radius), rgba(accent, 1.0));
                    }
                    for (item, number, px, pill_w) in &pills {
                        let px = x + px;
                        let color = if item.active {
                            rgba(style.background, 1.0)
                        } else {
                            if hovered(&Hit::Workspace(item.index)) {
                                fill(
                                    &rounded(px, m.pad, px + pill_w, m.pad + m.pill_h, radius),
                                    rgba(style.foreground, 0.12),
                                );
                            }
                            if item.occupied {
                                rgba(style.foreground, 0.9)
                            } else {
                                rgba(style.muted, 1.0)
                            }
                        };
                        text(number, px + (pill_w - number.width) / 2.0, color);
                        hits.push((px, px + pill_w, Hit::Workspace(item.index)));
                    }
                }
                Piece::Window { title, color, strong } => {
                    let mut tx = x;
                    if !strong {
                        if let Some((_, bitmap)) = &gfx.icon {
                            let size = (16.0 * s).round();
                            let top = ((h - size) / 2.0).round();
                            let dest = D2D_RECT_F { left: tx, top, right: tx + size, bottom: top + size };
                            target.DrawBitmap(bitmap, Some(&dest), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
                            tx += icon_w;
                            x += icon_w;
                        }
                    }
                    text(title, tx, *color);
                }
                Piece::Clock { date, time } => {
                    let dot = (3.0 * s).max(2.0);
                    let sep = (9.0 * s).round();
                    text(date, x, rgba(style.foreground, 0.6));
                    hits.push((x - m.hover_pad, x + date.width + sep / 2.0, Hit::Date));
                    let mut cx = x + date.width + sep;
                    fill(&rounded(cx, cy - dot / 2.0, cx + dot, cy + dot / 2.0, dot / 2.0), rgba(style.accent, 0.9));
                    cx += dot + sep;
                    text(time, cx, rgba(style.foreground, 1.0));
                    hits.push((cx - sep / 2.0, cx + time.width + m.hover_pad, Hit::Time));
                }
                Piece::Chip { module, glyph, caption, text: label_text, color } => {
                    let hit_kind = Hit::Module(module.clone());
                    let clickable = match module {
                        Module::Custom(name) => style.custom.get(name).is_some_and(|c| {
                            [&c.on_click, &c.on_right_click, &c.on_scroll_up, &c.on_scroll_down]
                                .iter()
                                .any(|cmd| !cmd.is_empty())
                        }),
                        _ => true,
                    };
                    let (hl, hr) = (x - m.hover_pad, x + pw + m.hover_pad);
                    if clickable && hovered(&hit_kind) {
                        highlight(hl, hr);
                    }
                    let mut cx = x;
                    if let Some(glyph) = glyph {
                        let painter = Icon { f, target, brush, round: &gfx.round, x: cx, cy, size: m.glyph };
                        match *glyph {
                            Glyph::Network(st) => painter.network(&st, style.foreground),
                            Glyph::Bluetooth(st) => painter.bluetooth(st.bluetooth, style.foreground, style.accent),
                            Glyph::Ring(v) => {
                                // Calm until busy: the ring turns to the accent colour above 80%.
                                let lit = if v > 0.8 { rgba(style.accent, 1.0) } else { rgba(style.foreground, 0.85) };
                                painter.ring(v, (2.2 * s).max(1.5), lit, rgba(style.foreground, 0.16))
                            }
                            Glyph::Speaker(level, muted) => {
                                painter.speaker(level, muted, *color, rgba(style.foreground, 0.2))
                            }
                        }
                        cx += m.glyph;
                    }
                    if let Some(caption) = caption {
                        cx += m.glyph_gap;
                        text(caption, cx, rgba(style.foreground, 0.45));
                        cx += caption.width;
                    }
                    if let Some(l) = label_text {
                        if cx > x {
                            cx += m.glyph_gap;
                        }
                        text(l, cx, *color);
                    }
                    if clickable {
                        hits.push((hl, hr, hit_kind));
                    }
                }
            }
            x += pw;
        }
    }
    model.hits = hits;
    model.indicator = indicator;
    model.animating = animating;
    model.live = live;
    gfx.target.EndDraw(None, None)
}

unsafe fn create_gfx(f: &Factories, style: &Style) -> windows::core::Result<Gfx> {
    let (target, brush) = crate::gfx::render_target(f)?;
    let round = f.d2d.CreateStrokeStyle(
        &D2D1_STROKE_STYLE_PROPERTIES {
            startCap: D2D1_CAP_STYLE_ROUND,
            endCap: D2D1_CAP_STYLE_ROUND,
            dashCap: D2D1_CAP_STYLE_ROUND,
            lineJoin: D2D1_LINE_JOIN_ROUND,
            miterLimit: 1.0,
            dashStyle: D2D1_DASH_STYLE_SOLID,
            dashOffset: 0.0,
        },
        None,
    )?;
    Ok(Gfx {
        target,
        brush,
        regular: crate::gfx::font_format(f, &style.font, 450, style.font_px)?,
        strong: crate::gfx::font_format(f, &style.font, 650, style.font_px)?,
        mono: crate::gfx::font_format(f, &style.font_mono, 450, style.font_px)?,
        mono_strong: crate::gfx::font_format(f, &style.font_mono, 600, style.font_px)?,
        round,
        icon: None,
    })
}
