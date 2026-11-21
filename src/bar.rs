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
use crate::layout::Rect;
use crate::win::{self, wide};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};
use windows::core::PCWSTR;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap, ID2D1DCRenderTarget, ID2D1Factory, ID2D1SolidColorBrush,
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, IDWriteTextLayout, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_METRICS, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory, WICBitmapDitherTypeNone,
    WICBitmapPaletteTypeMedianCut,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
use windows_numerics::Vector2;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject, AC_SRC_ALPHA,
    AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const CLASS: &str = "tessera_bar";
const CLOCK_TIMER: usize = 1;
const WM_MOUSELEAVE: u32 = 0x02A3;
const NOTICE_DURATION: Duration = Duration::from_secs(5);

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
    pub font: String,
    pub font_px: f32,
    pub scale: f32,
    pub show_title: bool,
    pub show_clock: bool,
    pub clock_24h: bool,
}

impl Style {
    pub fn from_config(cfg: &config::Bar, scale: f32) -> Self {
        let color = |s: &str| config::parse_color(s).unwrap_or(0);
        Style {
            background: color(&cfg.background),
            foreground: color(&cfg.foreground),
            accent: color(&cfg.accent),
            muted: color(&cfg.muted),
            opacity: cfg.opacity.clamp(0.0, 1.0),
            radius: cfg.corner_radius.max(0) as f32 * scale,
            font: cfg.font.clone(),
            font_px: cfg.font_size as f32 * scale,
            scale,
            show_title: cfg.show_title,
            show_clock: cfg.show_clock,
            clock_24h: cfg.clock_24h,
        }
    }
}

/// Process-wide graphics factories (created once, on the main thread).
struct Factories {
    d2d: ID2D1Factory,
    dwrite: IDWriteFactory,
    wic: Option<IWICImagingFactory>,
}

/// A 32-bit top-down DIB selected into a memory DC, the target of drawing.
struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
}

impl Surface {
    fn new(w: i32, h: i32) -> Option<Surface> {
        unsafe {
            let screen = GetDC(null_mut());
            let dc = CreateCompatibleDC(screen);
            ReleaseDC(null_mut(), screen);
            let mut info: BITMAPINFO = zeroed();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..zeroed()
            };
            let mut bits: *mut c_void = null_mut();
            let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if bitmap.is_null() {
                DeleteDC(dc);
                return None;
            }
            let old = SelectObject(dc, bitmap as HGDIOBJ);
            Some(Surface { dc, bitmap, old })
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.bitmap as HGDIOBJ);
            DeleteDC(self.dc);
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
}

thread_local! {
    static FACTORIES: RefCell<Option<Factories>> = const { RefCell::new(None) };
    static MODELS: RefCell<HashMap<isize, Model>> = RefCell::new(HashMap::new());
}

/// One-time setup: COM, graphics factories and the window class.
pub fn init() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let d2d: windows::core::Result<ID2D1Factory> = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None);
        let dwrite: windows::core::Result<IDWriteFactory> = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED);
        let wic: Option<IWICImagingFactory> =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok();
        match (d2d, dwrite) {
            (Ok(d2d), Ok(dwrite)) => FACTORIES.with(|f| *f.borrow_mut() = Some(Factories { d2d, dwrite, wic })),
            (Err(e), _) | (_, Err(e)) => error!("bar: could not initialise Direct2D/DirectWrite: {e}"),
        }

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
        let title = wide("tessera bar");
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

    /// Shows a transient message (e.g. a config error) in place of the title.
    pub fn notify(&self, text: &str) {
        with_model(self.hwnd, |model| model.notice = Some((text.to_string(), Instant::now() + NOTICE_DURATION)));
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

/// `COLORREF` plus alpha to a Direct2D colour.
fn rgba(c: u32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: (c & 0xFF) as f32 / 255.0,
        g: ((c >> 8) & 0xFF) as f32 / 255.0,
        b: ((c >> 16) & 0xFF) as f32 / 255.0,
        a,
    }
}

fn rounded(l: f32, t: f32, r: f32, b: f32, radius: f32) -> D2D1_ROUNDED_RECT {
    let radius = radius.min((b - t) / 2.0).min((r - l) / 2.0);
    D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: l, top: t, right: r, bottom: b }, radiusX: radius, radiusY: radius }
}

unsafe fn text_format(f: &Factories, font: &str, weight: i32, px: f32) -> windows::core::Result<IDWriteTextFormat> {
    let name = wide(font);
    let format = f.dwrite.CreateTextFormat(
        PCWSTR(name.as_ptr()),
        None,
        DWRITE_FONT_WEIGHT(weight),
        DWRITE_FONT_STYLE_NORMAL,
        DWRITE_FONT_STRETCH_NORMAL,
        px,
        windows::core::w!("en-us"),
    )?;
    format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
    format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
    let ellipsis = f.dwrite.CreateEllipsisTrimmingSign(&format)?;
    let trimming =
        DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
    format.SetTrimming(&trimming, &ellipsis)?;
    Ok(format)
}

unsafe fn create_gfx(f: &Factories, style: &Style) -> windows::core::Result<Gfx> {
    let props = D2D1_RENDER_TARGET_PROPERTIES {
        r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 96.0,
        dpiY: 96.0,
        usage: D2D1_RENDER_TARGET_USAGE_NONE,
        minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
    };
    let target = f.d2d.CreateDCRenderTarget(&props)?;
    target.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
    target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
    let brush = target.CreateSolidColorBrush(&rgba(0, 1.0), None)?;
    Ok(Gfx {
        target,
        brush,
        regular: text_format(f, &style.font, 450, style.font_px)?,
        strong: text_format(f, &style.font, 650, style.font_px)?,
        icon: None,
    })
}

/// Converts an `HICON` (with its alpha channel) into a Direct2D bitmap.
unsafe fn icon_bitmap(f: &Factories, target: &ID2D1DCRenderTarget, hicon: isize) -> Option<ID2D1Bitmap> {
    let wic = f.wic.as_ref()?;
    let source = wic.CreateBitmapFromHICON(windows::Win32::UI::WindowsAndMessaging::HICON(hicon as _)).ok()?;
    let converter = wic.CreateFormatConverter().ok()?;
    converter
        .Initialize(
            &source,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeMedianCut,
        )
        .ok()?;
    target.CreateBitmapFromWicBitmap(&converter, None).ok()
}

/// A measured, single-line piece of text.
struct Label {
    layout: IDWriteTextLayout,
    width: f32,
}

unsafe fn label(f: &Factories, format: &IDWriteTextFormat, text: &str, max_w: f32, h: f32) -> Option<Label> {
    let utf16: Vec<u16> = text.encode_utf16().collect();
    let layout = f.dwrite.CreateTextLayout(&utf16, format, max_w.max(1.0), h).ok()?;
    let mut metrics = DWRITE_TEXT_METRICS::default();
    layout.GetMetrics(&mut metrics).ok()?;
    Some(Label { layout, width: metrics.widthIncludingTrailingWhitespace.min(max_w).ceil() })
}

/// Draws the bar for `hwnd` and pushes it to the screen.
fn render(hwnd: isize) {
    FACTORIES.with(|f| {
        let factories = f.borrow();
        let Some(factories) = factories.as_ref() else { return };
        with_model(hwnd, |model| unsafe {
            if let Err(e) = draw(factories, model) {
                // Most likely a lost device: rebuild resources next time.
                warn!("bar: draw failed: {e}");
                model.gfx = None;
                return;
            }
            present(hwnd, model);
        });
    });
}

unsafe fn present(hwnd: isize, model: &Model) {
    let Some(surface) = &model.surface else { return };
    let r = model.rect;
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    let screen = GetDC(null_mut());
    UpdateLayeredWindow(
        hwnd as HWND,
        screen,
        &POINT { x: r.x, y: r.y },
        &SIZE { cx: r.w, cy: r.h },
        surface.dc,
        &POINT { x: 0, y: 0 },
        0,
        &blend,
        ULW_ALPHA,
    );
    ReleaseDC(null_mut(), screen);
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

    let bounds = RECT { left: 0, top: 0, right: model.rect.w, bottom: model.rect.h };
    gfx.target.BindDC(windows::Win32::Graphics::Gdi::HDC(surface.dc as _), &bounds as *const RECT as *const _)?;
    gfx.target.BeginDraw();
    gfx.target.Clear(Some(&rgba(0, 0.0)));

    let target = &gfx.target;
    let brush = &gfx.brush;
    let fill = |r: &D2D1_ROUNDED_RECT, color: D2D1_COLOR_F| {
        brush.SetColor(&color);
        target.FillRoundedRectangle(r, brush);
    };
    let island = |l: f32, r: f32| {
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
    let pill_h = h - 2.0 * pad;
    let mut items = Vec::new();
    let mut x = pad;
    for item in &model.state.workspaces {
        let format = if item.active { &gfx.strong } else { &gfx.regular };
        let Some(number) = label(f, format, &(item.index + 1).to_string(), w, h) else { continue };
        let pill_w =
            if item.active { (pill_h * 2.0).max(number.width + pill_h) } else { pill_h.max(number.width + pad) };
        items.push((item.clone(), number, x, pill_w));
        x += pill_w + 2.0 * s;
    }
    let left_w = (x - 2.0 * s + pad).max(h);
    island(0.0, left_w);
    model.hits.clear();
    for (item, number, x, pill_w) in &items {
        let pill = rounded(*x, pad, x + pill_w, pad + pill_h, pill_h / 2.0);
        let color = if item.active {
            let accent = if model.state.focused_monitor { style.accent } else { style.muted };
            fill(&pill, rgba(accent, 1.0));
            rgba(style.background, 1.0)
        } else {
            if model.hover == Some(item.index) {
                fill(&pill, rgba(style.foreground, 0.12));
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
