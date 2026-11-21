//! The status bar: a slim, rounded strip per monitor showing workspaces on
//! the left, the focused window's title in the middle and a clock on the
//! right. Clicking a workspace switches to it; scrolling cycles workspaces.
//!
//! The window manager pushes a fresh [`BarState`] whenever something
//! changes. Painting only reads that state, so the bar never needs to touch
//! window-manager internals from inside its window procedure.

use crate::config;
use crate::layout::Rect;
use crate::win::{self, wide};
use std::cell::RefCell;
use std::collections::HashMap;
use std::mem::zeroed;
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::Graphics::GdiPlus::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const CLASS: &str = "tessera_bar";
const CLOCK_TIMER: usize = 1;
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

/// Everything the bar paints, pushed by the window manager.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BarState {
    pub workspaces: Vec<WsItem>,
    pub title: String,
    /// This monitor holds the keyboard focus.
    pub focused_monitor: bool,
}

/// Colours (`COLORREF`) and metrics derived from the config for one monitor.
#[derive(Clone, Debug)]
pub struct Style {
    pub background: u32,
    pub foreground: u32,
    pub accent: u32,
    pub muted: u32,
    pub font: String,
    pub font_px: i32,
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
            font: cfg.font.clone(),
            font_px: (cfg.font_size as f32 * scale).round() as i32,
            show_title: cfg.show_title,
            show_clock: cfg.show_clock,
            clock_24h: cfg.clock_24h,
        }
    }
}

struct Model {
    style: Style,
    font: HFONT,
    state: BarState,
    notice: Option<(String, Instant)>,
    /// Clickable x-ranges of the workspace pills: (left, right, index).
    hits: Vec<(i32, i32, usize)>,
}

thread_local! {
    static MODELS: RefCell<HashMap<isize, Model>> = RefCell::new(HashMap::new());
}

/// One-time setup: registers the window class and starts GDI+.
pub fn init() {
    unsafe {
        let input = GdiplusStartupInput {
            GdiplusVersion: 1,
            DebugEventCallback: 0,
            SuppressBackgroundThread: 0,
            SuppressExternalCodecs: 0,
        };
        let mut token = 0usize;
        GdiplusStartup(&mut token, &input, null_mut());

        let class = wide(CLASS);
        let mut wc: WNDCLASSW = zeroed();
        wc.style = CS_HREDRAW | CS_VREDRAW;
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
        let font_name = wide(&style.font);
        unsafe {
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
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
            );
            if hwnd.is_null() {
                return None;
            }
            let font = CreateFontW(
                -style.font_px,
                0,
                0,
                0,
                500,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                OUT_DEFAULT_PRECIS as u32,
                CLIP_DEFAULT_PRECIS as u32,
                CLEARTYPE_QUALITY as u32,
                (DEFAULT_PITCH | FF_DONTCARE) as u32,
                font_name.as_ptr(),
            );
            MODELS.with(|m| {
                m.borrow_mut().insert(
                    hwnd as isize,
                    Model { style, font, state: BarState::default(), notice: None, hits: Vec::new() },
                )
            });
            win::round_corners(hwnd as isize);
            win::set_border_color(hwnd as isize, win::BORDER_NONE);
            SetTimer(hwnd, CLOCK_TIMER, 1000, None);
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            Some(Bar { hwnd: hwnd as isize })
        }
    }

    /// Replaces what the bar shows; repaints only when something changed.
    pub fn set_state(&self, state: BarState) {
        let changed = MODELS.with(|m| {
            let mut models = m.borrow_mut();
            match models.get_mut(&self.hwnd) {
                Some(model) if model.state != state => {
                    model.state = state;
                    true
                }
                _ => false,
            }
        });
        if changed {
            self.redraw();
        }
    }

    /// Shows a transient message (e.g. a config error) in place of the title.
    pub fn notify(&self, text: &str) {
        MODELS.with(|m| {
            if let Some(model) = m.borrow_mut().get_mut(&self.hwnd) {
                model.notice = Some((text.to_string(), Instant::now() + NOTICE_DURATION));
            }
        });
        self.redraw();
    }

    pub fn set_visible(&self, visible: bool) {
        unsafe {
            ShowWindow(self.hwnd as HWND, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
        }
    }

    fn redraw(&self) {
        unsafe { InvalidateRect(self.hwnd as HWND, null(), 0) };
    }
}

impl Drop for Bar {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.hwnd as HWND);
        }
        if let Some(model) = MODELS.with(|m| m.borrow_mut().remove(&self.hwnd)) {
            unsafe { DeleteObject(model.font as HGDIOBJ) };
        }
    }
}

unsafe extern "system" fn bar_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            0
        }
        WM_ERASEBKGND => 1,
        WM_TIMER => {
            InvalidateRect(hwnd, null(), 0);
            0
        }
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_LBUTTONDOWN => {
            let x = (lp & 0xFFFF) as i16 as i32;
            let hit = MODELS.with(|m| {
                m.borrow()
                    .get(&(hwnd as isize))
                    .and_then(|model| model.hits.iter().find(|(l, r, _)| x >= *l && x < *r).map(|h| h.2))
            });
            if let Some(index) = hit {
                crate::post_to_ctrl(crate::WM_APP_BAR, crate::BAR_WORKSPACE, index as isize);
            }
            0
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp >> 16) & 0xFFFF) as u16 as i16;
            let step = if delta > 0 { -1 } else { 1 };
            crate::post_to_ctrl(crate::WM_APP_BAR, crate::BAR_SCROLL, step);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// `COLORREF` (0x00BBGGRR) to GDI+ ARGB (0xAARRGGBB).
fn argb(c: u32) -> u32 {
    0xFF00_0000 | ((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF)
}

/// Fills an anti-aliased pill (a rectangle with fully rounded ends).
unsafe fn fill_pill(graphics: *mut GpGraphics, x: i32, y: i32, w: i32, h: i32, color: u32) {
    let mut brush: *mut GpSolidFill = null_mut();
    let mut path: *mut GpPath = null_mut();
    GdipCreateSolidFill(argb(color), &mut brush);
    GdipCreatePath(FillModeAlternate, &mut path);
    GdipAddPathArcI(path, x, y, h, h, 90.0, 180.0);
    GdipAddPathArcI(path, x + w - h, y, h, h, 270.0, 180.0);
    GdipClosePathFigure(path);
    GdipFillPath(graphics, brush as *mut GpBrush, path);
    GdipDeletePath(path);
    GdipDeleteBrush(brush as *mut GpBrush);
}

unsafe fn draw_text(hdc: HDC, text: &str, rect: &mut RECT, color: u32, flags: u32) {
    let mut buf: Vec<u16> = text.encode_utf16().collect();
    SetTextColor(hdc, color);
    DrawTextW(hdc, buf.as_mut_ptr(), buf.len() as i32, rect, flags | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX);
}

unsafe fn text_width(hdc: HDC, text: &str) -> i32 {
    let mut buf: Vec<u16> = text.encode_utf16().collect();
    let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    DrawTextW(hdc, buf.as_mut_ptr(), buf.len() as i32, &mut r, DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX);
    r.right - r.left
}

fn clock_text(twenty_four_hour: bool) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let t = win::local_time();
    let day = DAYS[t.wDayOfWeek as usize % 7];
    let month = MONTHS[(t.wMonth as usize + 11) % 12];
    let time = if twenty_four_hour {
        format!("{:02}:{:02}", t.wHour, t.wMinute)
    } else {
        let h = match t.wHour % 12 {
            0 => 12,
            h => h,
        };
        format!("{h}:{:02} {}", t.wMinute, if t.wHour < 12 { "AM" } else { "PM" })
    };
    format!("{day} {} {month}   {time}", t.wDay)
}

unsafe fn paint(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut client: RECT = zeroed();
    GetClientRect(hwnd, &mut client);
    let (w, h) = (client.right, client.bottom);

    // Double-buffer to avoid flicker.
    let mem = CreateCompatibleDC(hdc);
    let bitmap = CreateCompatibleBitmap(hdc, w, h);
    let old_bitmap = SelectObject(mem, bitmap as HGDIOBJ);

    MODELS.with(|m| {
        if let Some(model) = m.borrow_mut().get_mut(&(hwnd as isize)) {
            paint_model(mem, w, h, model);
        }
    });

    BitBlt(hdc, 0, 0, w, h, mem, 0, 0, SRCCOPY);
    SelectObject(mem, old_bitmap);
    DeleteObject(bitmap as HGDIOBJ);
    DeleteDC(mem);
    EndPaint(hwnd, &ps);
}

unsafe fn paint_model(hdc: HDC, w: i32, h: i32, model: &mut Model) {
    let style = &model.style;
    let background = CreateSolidBrush(style.background);
    FillRect(hdc, &RECT { left: 0, top: 0, right: w, bottom: h }, background);
    DeleteObject(background as HGDIOBJ);

    let old_font = SelectObject(hdc, model.font as HGDIOBJ);
    SetBkMode(hdc, TRANSPARENT as _);

    let mut graphics: *mut GpGraphics = null_mut();
    GdipCreateFromHDC(hdc, &mut graphics);
    GdipSetSmoothingMode(graphics, SmoothingModeAntiAlias);

    // Workspaces: a wide accent pill for the active one, plain numbers for
    // occupied ones, dim numbers for empty ones.
    let pad = h / 3;
    let pill_h = h - 2 * (h / 5);
    let pill_y = (h - pill_h) / 2;
    let mut x = pad / 2 + h / 8;
    model.hits.clear();
    for item in &model.state.workspaces {
        let label = (item.index + 1).to_string();
        let pill_w = if item.active { pill_h * 2 } else { pill_h.max(text_width(hdc, &label) + pill_h / 2) };
        let text_color = if item.active {
            let fill = if model.state.focused_monitor { style.accent } else { style.muted };
            fill_pill(graphics, x, pill_y, pill_w, pill_h, fill);
            style.background
        } else if item.occupied {
            style.foreground
        } else {
            style.muted
        };
        let mut r = RECT { left: x, top: 0, right: x + pill_w, bottom: h };
        draw_text(hdc, &label, &mut r, text_color, DT_CENTER);
        model.hits.push((x, x + pill_w, item.index));
        x += pill_w + pill_h / 4;
    }
    let left_end = x;

    // Clock on the right.
    let mut right_start = w;
    if style.show_clock {
        let text = clock_text(style.clock_24h);
        let tw = text_width(hdc, &text);
        right_start = w - pad - tw;
        let mut r = RECT { left: right_start, top: 0, right: w - pad, bottom: h };
        draw_text(hdc, &text, &mut r, style.foreground, DT_RIGHT);
    }

    // Title (or a transient notice) centered on the bar.
    if model.notice.as_ref().is_some_and(|(_, until)| Instant::now() >= *until) {
        model.notice = None;
    }
    let (center_text, color) = match &model.notice {
        Some((text, _)) => (text.as_str(), style.accent),
        None if style.show_title => (model.state.title.as_str(), style.foreground),
        None => ("", style.foreground),
    };
    if !center_text.is_empty() {
        let half = ((w / 2 - left_end).min(right_start - w / 2) - pad).max(0);
        let mut r = RECT { left: w / 2 - half, top: 0, right: w / 2 + half, bottom: h };
        draw_text(hdc, center_text, &mut r, color, DT_CENTER | DT_END_ELLIPSIS);
    }

    GdipDeleteGraphics(graphics);
    SelectObject(hdc, old_font);
}
