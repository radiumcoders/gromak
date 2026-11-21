//! The status bar: three floating, translucent "islands" per monitor.
//!
//! ```text
//!  ╭──────────────╮            ╭───────────────────────╮       ╭──────────────────╮
//!  │ ( 1 )  2  4  │            │ ▣  Window title       │       │ Sat 21 Nov · 21:46│
//!  ╰──────────────╯            ╰───────────────────────╯       ╰──────────────────╯
//! ```
//!
//! Left: workspaces (a wide accent pill for the active one). Middle: the
//! focused window's icon and title. Right: date and time. The space between
//! islands is fully transparent and click-through.
//!
//! The bar is a per-pixel-alpha layered window. Each frame is drawn with
//! Direct2D/DirectWrite into a 32-bit DIB and pushed with
//! `UpdateLayeredWindow`; there is no `WM_PAINT`. The window manager pushes
//! a fresh [`BarState`] whenever something changes, and drawing only reads
//! that state, so the bar never touches window-manager internals.

use crate::config;
use crate::gfx::{icon_bitmap, label, rgba, rounded, Factories, Label, Surface};
use crate::layout::Rect;
use crate::theme::Palette;
use crate::win::{self, wide};
use std::cell::RefCell;
use std::collections::HashMap;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap, ID2D1DCRenderTarget, ID2D1SolidColorBrush, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ROUNDED_RECT,
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
    pub font_px: f32,
    pub scale: f32,
    pub show_title: bool,
    pub show_clock: bool,
    pub clock_24h: bool,
}

impl Style {
    /// Layout options from the config, colours from the active palette.
    pub fn new(cfg: &config::Bar, palette: &Palette, scale: f32) -> Self {
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
            font_px: cfg.font_size as f32 * scale,
            scale,
            show_title: cfg.show_title,
            show_clock: cfg.show_clock,
            clock_24h: cfg.clock_24h,
        }
    }
}

/// Device-dependent resources, recreated if Direct2D loses the device.
struct Gfx {
    target: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
    regular: IDWriteTextFormat,
    strong: IDWriteTextFormat,
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
    hover: Option<usize>,
    tracking_mouse: bool,
    /// Clickable x-ranges of the workspace pills: (left, right, index).
    hits: Vec<(f32, f32, usize)>,
    /// Last clock text drawn, to skip redundant redraws.
    clock: String,
    /// The sliding active-workspace highlight.
    indicator: Option<Indicator>,
    /// An animation is running and needs more frames.
    animating: bool,
}

thread_local! {
    static MODELS: RefCell<HashMap<isize, Model>> = RefCell::new(HashMap::new());
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
            clock: String::new(),
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
            hit(&m.hits, (x - m.rect.x) as f32)
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
                expired || (m.style.show_clock && clock_text(m.style.clock_24h) != m.clock)
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
                std::mem::replace(&mut m.hover, hover) != hover
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
        WM_LBUTTONDOWN => {
            let x = (lp & 0xFFFF) as i16 as f32;
            if let Some(Some(index)) = with_model(key, |m| hit(&m.hits, x)) {
                crate::post_to_ctrl(crate::WM_APP_BAR, crate::BAR_WORKSPACE, index as isize);
            }
            0
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp >> 16) & 0xFFFF) as u16 as i16;
            crate::post_to_ctrl(crate::WM_APP_BAR, crate::BAR_SCROLL, if delta > 0 { -1 } else { 1 });
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn hit(hits: &[(f32, f32, usize)], x: f32) -> Option<usize> {
    hits.iter().find(|(l, r, _)| x >= *l && x < *r).map(|h| h.2)
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

unsafe fn draw(f: &Factories, model: &mut Model) -> windows::core::Result<()> {
    let Some(surface) = &model.surface else { return Ok(()) };
    if model.gfx.is_none() {
        model.gfx = Some(create_gfx(f, &model.style)?);
    }
    let style = model.style.clone();
    let gfx = model.gfx.as_mut().expect("just created");
    let (w, h) = (model.rect.w as f32, model.rect.h as f32);
    let s = style.scale;

    crate::gfx::bind(&gfx.target, surface, model.rect.w, model.rect.h)?;
    gfx.target.BeginDraw();
    gfx.target.Clear(Some(&rgba(0, 0.0)));

    let target = &gfx.target;
    let brush = &gfx.brush;
    let fill = |r: &D2D1_ROUNDED_RECT, color: D2D1_COLOR_F| {
        brush.SetColor(&color);
        target.FillRoundedRectangle(r, brush);
    };
    // Full style: one tinted strip (over the blur, when on) with a hairline
    // along the bottom; the islands then have no background of their own.
    if style.full {
        brush.SetColor(&rgba(style.background, style.opacity));
        target.FillRectangle(&D2D_RECT_F { left: 0.0, top: 0.0, right: w, bottom: h }, brush);
        brush.SetColor(&rgba(style.foreground, 0.08));
        target.FillRectangle(&D2D_RECT_F { left: 0.0, top: h - 1.0, right: w, bottom: h }, brush);
    }
    let island = |l: f32, r: f32| {
        if style.full {
            return;
        }
        let shape = rounded(l + 0.5, 0.5, r - 0.5, h - 0.5, style.radius);
        fill(&shape, rgba(style.background, style.opacity));
        brush.SetColor(&rgba(style.foreground, 0.10));
        target.DrawRoundedRectangle(&shape, brush, 1.0, None);
    };
    let text = |l: &Label, x: f32, color: D2D1_COLOR_F| {
        brush.SetColor(&color);
        target.DrawTextLayout(Vector2 { X: x, Y: 0.0 }, &l.layout, brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
    };

    let pad = (5.0 * s).round();
    let gap = (10.0 * s).round();

    // ── Workspaces ───────────────────────────────────────────────────────
    // Square pills; the active one is marked by an indicator that slides
    // from the previously active pill.
    let pill_h = h - 2.0 * pad;
    let radius = (pill_h * 0.32).round();
    let mut items = Vec::new();
    let mut x = pad;
    for item in &model.state.workspaces {
        let format = if item.active { &gfx.strong } else { &gfx.regular };
        let Some(number) = label(f, format, &(item.index + 1).to_string(), w, h) else { continue };
        let pill_w = pill_h.max(number.width + pad);
        items.push((item.clone(), number, x, pill_w));
        x += pill_w + 3.0 * s;
    }
    let left_w = (x - 3.0 * s + pad).max(h);
    island(0.0, left_w);
    model.hits.clear();
    model.animating = false;
    if let Some((_, _, tx, tw)) = items.iter().find(|i| i.0.active) {
        let target = (*tx, *tw);
        let shown = model.indicator.map_or(target, |ind| ind.current());
        match model.indicator {
            Some(ind) if ind.to == target => {}
            Some(_) => model.indicator = Some(Indicator { from: shown, to: target, start: Instant::now() }),
            None => model.indicator = Some(Indicator { from: target, to: target, start: Instant::now() }),
        }
        let ind = model.indicator.expect("just set");
        let (ix, iw) = ind.current();
        model.animating = !ind.done();
        let accent = if model.state.focused_monitor { style.accent } else { style.muted };
        fill(&rounded(ix, pad, ix + iw, pad + pill_h, radius), rgba(accent, 1.0));
    }
    for (item, number, x, pill_w) in &items {
        let color = if item.active {
            rgba(style.background, 1.0)
        } else {
            if model.hover == Some(item.index) {
                fill(&rounded(*x, pad, x + pill_w, pad + pill_h, radius), rgba(style.foreground, 0.12));
            }
            if item.occupied {
                rgba(style.foreground, 0.9)
            } else {
                rgba(style.muted, 1.0)
            }
        };
        text(number, x + (pill_w - number.width) / 2.0, color);
        model.hits.push((*x, x + pill_w, item.index));
    }

    // ── Clock ────────────────────────────────────────────────────────────
    let mut right_l = w;
    if style.show_clock {
        let time = clock_text(style.clock_24h);
        let date = label(f, &gfx.regular, &date_text(), w, h);
        let clock = label(f, &gfx.strong, &time, w, h);
        if let (Some(date), Some(clock)) = (date, clock) {
            let inner = (14.0 * s).round();
            let dot = (3.0 * s).max(2.0);
            let sep = (9.0 * s).round();
            let island_w = inner + date.width + sep + dot + sep + clock.width + inner;
            right_l = w - island_w;
            island(right_l, w);
            let mut cx = right_l + inner;
            text(&date, cx, rgba(style.foreground, 0.6));
            cx += date.width + sep;
            fill(&rounded(cx, h / 2.0 - dot / 2.0, cx + dot, h / 2.0 + dot / 2.0, dot / 2.0), rgba(style.accent, 0.9));
            cx += dot + sep;
            text(&clock, cx, rgba(style.foreground, 1.0));
        }
        model.clock = time;
    }

    // ── Title (or a transient notice) ────────────────────────────────────
    if model.notice.as_ref().is_some_and(|(_, until)| Instant::now() >= *until) {
        model.notice = None;
    }
    let (center, color, format, icon) = match &model.notice {
        Some((msg, _)) => (msg.clone(), rgba(style.accent, 1.0), &gfx.strong, 0),
        None if style.show_title => {
            (model.state.title.clone(), rgba(style.foreground, 0.92), &gfx.regular, model.state.icon)
        }
        None => (String::new(), rgba(0, 0.0), &gfx.regular, 0),
    };
    if !center.is_empty() {
        let inner = (14.0 * s).round();
        let icon_size = (16.0 * s).round();
        let icon_gap = (9.0 * s).round();
        if gfx.icon.as_ref().map(|(handle, _)| *handle) != Some(icon) {
            gfx.icon = if icon != 0 { icon_bitmap(f, &gfx.target, icon).map(|b| (icon, b)) } else { None };
        }
        let icon_w = if gfx.icon.is_some() { icon_size + icon_gap } else { 0.0 };
        // Centre on the bar, but never overlap the side islands.
        let space = (right_l - gap) - (left_w + gap);
        let max_text = (space.min(w * 0.5) - 2.0 * inner - icon_w).max(0.0);
        if let Some(title) = label(f, format, &center, max_text, h).filter(|_| max_text > 0.0) {
            let island_w = inner + icon_w + title.width + inner;
            let l = ((w - island_w) / 2.0).max(left_w + gap).min(right_l - gap - island_w);
            island(l, l + island_w);
            let mut tx = l + inner;
            if let Some((_, bitmap)) = &gfx.icon {
                let top = ((h - icon_size) / 2.0).round();
                let dest = D2D_RECT_F { left: tx, top, right: tx + icon_size, bottom: top + icon_size };
                gfx.target.DrawBitmap(bitmap, Some(&dest), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
                tx += icon_w;
            }
            text(&title, tx, color);
        }
    }

    gfx.target.EndDraw(None, None)
}

unsafe fn create_gfx(f: &Factories, style: &Style) -> windows::core::Result<Gfx> {
    let (target, brush) = crate::gfx::render_target(f)?;
    Ok(Gfx {
        target,
        brush,
        regular: crate::gfx::font_format(f, &style.font, 450, style.font_px)?,
        strong: crate::gfx::font_format(f, &style.font, 650, style.font_px)?,
        icon: None,
    })
}
