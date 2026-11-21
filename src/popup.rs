//! Small floating panels opened from the bar: a month calendar (click the
//! date) and an analog + digital clock (click the time).
//!
//! Like the launcher, a popup is a per-pixel-alpha layered window drawn with
//! Direct2D. It takes focus while open so that clicking anywhere else (or
//! pressing Escape) closes it.

use crate::config::FontSource;
use crate::gfx::{label, rgba, rounded, Factories, Surface};
use crate::layout::Rect;
use crate::theme::Palette;
use crate::win::{self, wide};
use std::cell::RefCell;
use std::mem::zeroed;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::time::{Duration, Instant};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{
    ID2D1DCRenderTarget, ID2D1SolidColorBrush, D2D1_CAP_STYLE_ROUND, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE,
    D2D1_STROKE_STYLE_PROPERTIES,
};
use windows::Win32::Graphics::Direct2D::{ID2D1StrokeStyle, D2D1_DASH_STYLE_SOLID, D2D1_LINE_JOIN_ROUND};
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows_numerics::Vector2;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_ESCAPE, VK_LEFT, VK_RIGHT};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const CLASS: &str = "gromak_popup";
const TICK_TIMER: usize = 1;
const ANIM_TIMER: usize = 2;
const OPEN_ANIMATION: Duration = Duration::from_millis(180);

/// Which popup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Calendar,
    Clock,
}

/// Colours and metrics, from the theme and the monitor.
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    pub palette: Palette,
    pub font: FontSource,
    pub font_px: f32,
    pub scale: f32,
}

// ----- calendar maths (pure) --------------------------------------------------

pub fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

pub fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if is_leap(year) => 29,
        _ => 28,
    }
}

/// Day of the week, 0 = Monday .. 6 = Sunday (Sakamoto's method).
pub fn weekday(year: i32, month: u32, day: u32) -> u32 {
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if month < 3 { year - 1 } else { year };
    let sunday_based = (y + y / 4 - y / 100 + y / 400 + T[month as usize - 1] + day as i32).rem_euclid(7);
    ((sunday_based + 6) % 7) as u32
}

/// The 6x7 grid of a month view, Monday first: (year, month, day) per cell,
/// including the trailing/leading days of the neighbouring months.
pub fn month_grid(year: i32, month: u32) -> Vec<(i32, u32, u32)> {
    let (py, pm) = if month == 1 { (year - 1, 12) } else { (year, month - 1) };
    let (ny, nm) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    let lead = weekday(year, month, 1);
    let prev_days = days_in_month(py, pm);
    let mut cells = Vec::with_capacity(42);
    for i in 0..lead {
        cells.push((py, pm, prev_days - lead + 1 + i));
    }
    for d in 1..=days_in_month(year, month) {
        cells.push((year, month, d));
    }
    let mut d = 1;
    while cells.len() < 42 {
        cells.push((ny, nm, d));
        d += 1;
    }
    cells
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const WEEKDAYS: [&str; 7] = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];
const WEEKDAY_NAMES: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

// ----- window ---------------------------------------------------------------

struct Gfx {
    target: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
    round_cap: ID2D1StrokeStyle,
    title: IDWriteTextFormat,
    text: IDWriteTextFormat,
    small: IDWriteTextFormat,
    big: IDWriteTextFormat,
}

struct Popup {
    hwnd: isize,
    kind: Kind,
    style: Option<Style>,
    /// Bottom-right corner the popup hangs from (screen pixels).
    anchor: (i32, i32),
    rect: Rect,
    surface: Option<Surface>,
    surface_size: (i32, i32),
    gfx: Option<Gfx>,
    /// The month shown by the calendar.
    year: i32,
    month: u32,
    /// Clickable areas (window coordinates): previous, next, title.
    hits: [(f32, f32, f32, f32); 3],
    anim_start: Option<Instant>,
}

static OPEN: AtomicBool = AtomicBool::new(false);
static HWND_POPUP: AtomicIsize = AtomicIsize::new(0);

thread_local! {
    static STATE: RefCell<Option<Popup>> = const { RefCell::new(None) };
}

/// Whether a popup is showing.
pub fn is_open() -> bool {
    OPEN.load(Ordering::Relaxed)
}

fn with_state<R>(f: impl FnOnce(&mut Popup) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(f)))
}

/// Releases the popup's drawing state (see [`crate::gfx::shutdown`]).
pub fn shutdown() {
    let state = STATE.with(|s| s.borrow_mut().take());
    drop(state);
}

/// Registers the class and creates the (hidden) popup window.
pub fn init() {
    let class = wide(CLASS);
    let hwnd = unsafe {
        let mut wc: WNDCLASSW = zeroed();
        wc.lpfnWndProc = Some(popup_proc);
        wc.hInstance = GetModuleHandleW(null());
        wc.hCursor = LoadCursorW(null_mut(), IDC_ARROW);
        wc.lpszClassName = class.as_ptr();
        RegisterClassW(&wc);
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class.as_ptr(),
            class.as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            GetModuleHandleW(null()),
            null(),
        )
    };
    if hwnd.is_null() {
        error!("popup: could not create its window");
        return;
    }
    HWND_POPUP.store(hwnd as isize, Ordering::Relaxed);
    let t = win::local_time();
    STATE.with(|s| {
        *s.borrow_mut() = Some(Popup {
            hwnd: hwnd as isize,
            kind: Kind::Calendar,
            style: None,
            anchor: (0, 0),
            rect: Rect::default(),
            surface: None,
            surface_size: (0, 0),
            gfx: None,
            year: t.wYear as i32,
            month: t.wMonth as u32,
            hits: [(0.0, 0.0, 0.0, 0.0); 3],
            anim_start: None,
        })
    });
}

/// Opens `kind` hanging below `anchor` (its top-right corner), or closes it
/// if that popup is already open.
pub fn toggle(kind: Kind, anchor: (i32, i32), style: Style) {
    let reopen_same = is_open() && with_state(|p| p.kind == kind).unwrap_or(false);
    if is_open() {
        close();
    }
    if reopen_same {
        return;
    }
    let Some(hwnd) = with_state(|p| {
        if p.style.as_ref() != Some(&style) {
            p.gfx = None;
        }
        p.style = Some(style);
        p.kind = kind;
        p.anchor = anchor;
        let t = win::local_time();
        p.year = t.wYear as i32;
        p.month = t.wMonth as u32;
        p.anim_start = Some(Instant::now());
        p.hwnd
    }) else {
        return;
    };
    OPEN.store(true, Ordering::Relaxed);
    render();
    unsafe {
        ShowWindow(hwnd as HWND, SW_SHOW);
        SetTimer(hwnd as HWND, ANIM_TIMER, 8, None);
        if kind == Kind::Clock {
            SetTimer(hwnd as HWND, TICK_TIMER, 250, None);
        }
    }
    win::focus(hwnd);
}

fn close() {
    if !OPEN.swap(false, Ordering::Relaxed) {
        return;
    }
    let hwnd = HWND_POPUP.load(Ordering::Relaxed) as HWND;
    unsafe {
        KillTimer(hwnd, TICK_TIMER);
        KillTimer(hwnd, ANIM_TIMER);
        ShowWindow(hwnd, SW_HIDE);
    }
}

impl Popup {
    fn scale(&self) -> f32 {
        self.style.as_ref().map_or(1.0, |s| s.scale)
    }

    fn margin(&self) -> f32 {
        (24.0 * self.scale()).round()
    }

    /// Panel size in pixels.
    fn size(&self) -> (f32, f32) {
        let s = self.scale();
        match self.kind {
            Kind::Calendar => ((7.0 * 38.0 + 32.0) * s, (56.0 + 30.0 + 6.0 * 34.0 + 18.0) * s),
            Kind::Clock => (260.0 * s, 320.0 * s),
        }
    }

    fn change_month(&mut self, delta: i32) {
        let index = self.year * 12 + self.month as i32 - 1 + delta;
        self.year = index.div_euclid(12);
        self.month = (index.rem_euclid(12) + 1) as u32;
    }

    fn present(&self) {
        let Some(surface) = &self.surface else { return };
        let t = self
            .anim_start
            .map_or(1.0, |start| (start.elapsed().as_secs_f32() / OPEN_ANIMATION.as_secs_f32()).clamp(0.0, 1.0));
        let eased = 1.0 - (1.0 - t).powi(5);
        let slide = ((1.0 - eased) * -8.0 * self.scale()).round() as i32;
        let rect = Rect { y: self.rect.y + slide, ..self.rect };
        unsafe { crate::gfx::present(self.hwnd, surface, rect, (eased * 255.0) as u8) };
    }
}

unsafe extern "system" fn popup_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ACTIVATE if (wp & 0xFFFF) as u32 == WA_INACTIVE => {
            close();
            0
        }
        WM_KEYDOWN => {
            match wp as u16 {
                VK_ESCAPE => close(),
                VK_LEFT => {
                    with_state(|p| p.change_month(-1));
                    render();
                }
                VK_RIGHT => {
                    with_state(|p| p.change_month(1));
                    render();
                }
                _ => {}
            }
            0
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp >> 16) & 0xFFFF) as u16 as i16;
            if with_state(|p| p.kind == Kind::Calendar) == Some(true) {
                with_state(|p| p.change_month(if delta > 0 { -1 } else { 1 }));
                render();
            }
            0
        }
        WM_LBUTTONDOWN => {
            let (x, y) = ((lp & 0xFFFF) as i16 as f32, ((lp >> 16) & 0xFFFF) as i16 as f32);
            let changed = with_state(|p| {
                if p.kind != Kind::Calendar {
                    return false;
                }
                let inside = |(l, t, r, b): (f32, f32, f32, f32)| x >= l && x < r && y >= t && y < b;
                if inside(p.hits[0]) {
                    p.change_month(-1);
                } else if inside(p.hits[1]) {
                    p.change_month(1);
                } else if inside(p.hits[2]) {
                    let t = win::local_time();
                    p.year = t.wYear as i32;
                    p.month = t.wMonth as u32;
                } else {
                    return false;
                }
                true
            });
            if changed == Some(true) {
                render();
            }
            0
        }
        WM_TIMER if wp == TICK_TIMER => {
            render();
            0
        }
        WM_TIMER if wp == ANIM_TIMER => {
            let done = with_state(|p| {
                p.present();
                p.anim_start.is_none_or(|s| s.elapsed() >= OPEN_ANIMATION)
            });
            if done != Some(false) {
                with_state(|p| p.anim_start = None);
                KillTimer(hwnd, ANIM_TIMER);
            }
            0
        }
        WM_MOUSEACTIVATE => MA_ACTIVATE as LRESULT,
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

// ----- drawing --------------------------------------------------------------

fn render() {
    crate::gfx::with_factories(|f| {
        with_state(|p| unsafe {
            if let Err(e) = p.draw(f) {
                warn!("popup: draw failed: {e}");
                p.gfx = None;
                return;
            }
            p.present();
        });
    });
}

unsafe fn create_gfx(f: &Factories, style: &Style) -> windows::core::Result<Gfx> {
    let (target, brush) = crate::gfx::render_target(f)?;
    let props = D2D1_STROKE_STYLE_PROPERTIES {
        startCap: D2D1_CAP_STYLE_ROUND,
        endCap: D2D1_CAP_STYLE_ROUND,
        dashCap: D2D1_CAP_STYLE_ROUND,
        lineJoin: D2D1_LINE_JOIN_ROUND,
        miterLimit: 1.0,
        dashStyle: D2D1_DASH_STYLE_SOLID,
        dashOffset: 0.0,
    };
    let round_cap = f.d2d.CreateStrokeStyle(&props, None)?;
    let px = style.font_px;
    Ok(Gfx {
        target,
        brush,
        round_cap,
        title: crate::gfx::font_format(f, &style.font, 650, px * 1.25)?,
        text: crate::gfx::font_format(f, &style.font, 450, px * 1.05)?,
        small: crate::gfx::font_format(f, &style.font, 500, px * 0.9)?,
        big: crate::gfx::font_format(f, &style.font, 650, px * 2.4)?,
    })
}

impl Popup {
    unsafe fn draw(&mut self, f: &Factories) -> windows::core::Result<()> {
        let Some(style) = self.style.clone() else { return Ok(()) };
        let s = style.scale;
        let m = self.margin();
        let (w, h) = self.size();
        // Hang below the anchor, right-aligned to it.
        self.rect = Rect::new(
            self.anchor.0 - (w + m) as i32,
            self.anchor.1 - m as i32 + (6.0 * s) as i32,
            (w + 2.0 * m) as i32,
            (h + 2.0 * m) as i32,
        );
        let size = (self.rect.w, self.rect.h);
        if self.surface.is_none() || self.surface_size != size {
            self.surface = Surface::new(size.0, size.1);
            self.surface_size = size;
        }
        let Some(surface) = &self.surface else { return Ok(()) };
        if self.gfx.is_none() {
            self.gfx = Some(create_gfx(f, &style)?);
        }
        let gfx = self.gfx.as_ref().expect("just created");
        let p = &style.palette;
        crate::gfx::bind(&gfx.target, surface, size.0, size.1)?;
        gfx.target.BeginDraw();
        gfx.target.Clear(Some(&rgba(0, 0.0)));
        let target = &gfx.target;
        let brush = &gfx.brush;
        let radius = (14.0 * s).round();

        // Shadow and panel, like the launcher.
        for i in 0..12 {
            let grow = m * 0.85 * (i + 1) as f32 / 12.0;
            brush.SetColor(&rgba(0, 0.03));
            target.FillRoundedRectangle(
                &rounded(m - grow, m - grow + 5.0 * s, m + w + grow, m + h + grow + 5.0 * s, radius + grow),
                brush,
            );
        }
        let panel = rounded(m + 0.5, m + 0.5, m + w - 0.5, m + h - 0.5, radius);
        brush.SetColor(&rgba(p.background, 1.0));
        target.FillRoundedRectangle(&panel, brush);
        brush.SetColor(&rgba(p.foreground, 0.08));
        target.DrawRoundedRectangle(&panel, brush, 1.0, None);

        let text_at = |format: &IDWriteTextFormat, text: &str, cx: f32, cy: f32, hh: f32, color| {
            if let Some(l) = label(f, format, text, w, hh) {
                brush.SetColor(&color);
                target.DrawTextLayout(
                    Vector2 { X: cx - l.width / 2.0, Y: cy - hh / 2.0 },
                    &l.layout,
                    brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
            }
        };
        let now = win::local_time();

        match self.kind {
            Kind::Calendar => {
                let pad = 16.0 * s;
                let cell = 38.0 * s;
                let row = 34.0 * s;
                let header_h = 56.0 * s;
                // Header: ‹  November 2026  ›
                let cy = m + header_h / 2.0;
                let title = format!("{} {}", MONTHS[self.month as usize - 1], self.year);
                text_at(&gfx.title, &title, m + w / 2.0, cy, header_h, rgba(p.foreground, 1.0));
                let arrow = 28.0 * s;
                let left = (m + pad, cy - arrow / 2.0, m + pad + arrow, cy + arrow / 2.0);
                let right = (m + w - pad - arrow, cy - arrow / 2.0, m + w - pad, cy + arrow / 2.0);
                for (l, t, r, b) in [left, right] {
                    brush.SetColor(&rgba(p.foreground, 0.07));
                    target.FillRoundedRectangle(&rounded(l, t, r, b, arrow / 2.0), brush);
                }
                text_at(&gfx.text, "‹", (left.0 + left.2) / 2.0, cy - s, header_h, rgba(p.foreground, 0.8));
                text_at(&gfx.text, "›", (right.0 + right.2) / 2.0, cy - s, header_h, rgba(p.foreground, 0.8));
                self.hits = [left, right, (m + w * 0.25, m, m + w * 0.75, m + header_h)];

                // Weekday names.
                let grid_left = m + pad;
                let wy = m + header_h + 15.0 * s;
                for (i, name) in WEEKDAYS.iter().enumerate() {
                    let cx = grid_left + cell * (i as f32 + 0.5);
                    let weekend = i >= 5;
                    text_at(&gfx.small, name, cx, wy, 30.0 * s, rgba(p.foreground, if weekend { 0.35 } else { 0.5 }));
                }
                // Days.
                let top = m + header_h + 30.0 * s;
                for (i, (y, mo, d)) in month_grid(self.year, self.month).into_iter().enumerate() {
                    let (col, line) = ((i % 7) as f32, (i / 7) as f32);
                    let cx = grid_left + cell * (col + 0.5);
                    let cy = top + row * (line + 0.5);
                    let today = y == now.wYear as i32 && mo == now.wMonth as u32 && d == now.wDay as u32;
                    let in_month = mo == self.month;
                    let color = if today {
                        let r = row * 0.42;
                        brush.SetColor(&rgba(p.accent, 1.0));
                        target.FillEllipse(
                            &D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: r, radiusY: r },
                            brush,
                        );
                        rgba(p.background, 1.0)
                    } else if in_month {
                        rgba(p.foreground, if col >= 5.0 { 0.6 } else { 0.9 })
                    } else {
                        rgba(p.foreground, 0.22)
                    };
                    let format = if today { &gfx.title } else { &gfx.text };
                    text_at(format, &d.to_string(), cx, cy, row, color);
                }
            }
            Kind::Clock => {
                // Analog face.
                let cx = m + w / 2.0;
                let cy = m + 118.0 * s;
                let r = 88.0 * s;
                brush.SetColor(&rgba(p.foreground, 0.05));
                target.FillEllipse(&D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: r, radiusY: r }, brush);
                brush.SetColor(&rgba(p.foreground, 0.10));
                target.DrawEllipse(
                    &D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: r, radiusY: r },
                    brush,
                    1.0,
                    None,
                );
                let point = |angle: f32, len: f32| {
                    let a = (angle - 90.0).to_radians();
                    Vector2 { X: cx + a.cos() * len, Y: cy + a.sin() * len }
                };
                for i in 0..12 {
                    let major = i % 3 == 0;
                    let (inner, outer) = if major { (r * 0.78, r * 0.9) } else { (r * 0.83, r * 0.9) };
                    brush.SetColor(&rgba(p.foreground, if major { 0.6 } else { 0.3 }));
                    target.DrawLine(
                        point(i as f32 * 30.0, inner),
                        point(i as f32 * 30.0, outer),
                        brush,
                        if major { 2.4 * s } else { 1.4 * s },
                        &gfx.round_cap,
                    );
                }
                let (hh, mm, ss, ms) =
                    (now.wHour as f32, now.wMinute as f32, now.wSecond as f32, now.wMilliseconds as f32);
                let hour_angle = (hh % 12.0 + mm / 60.0) * 30.0;
                let minute_angle = (mm + ss / 60.0) * 6.0;
                let second_angle = (ss + ms / 1000.0).floor() * 6.0;
                brush.SetColor(&rgba(p.foreground, 0.95));
                target.DrawLine(
                    point(hour_angle, -r * 0.08),
                    point(hour_angle, r * 0.5),
                    brush,
                    4.5 * s,
                    &gfx.round_cap,
                );
                target.DrawLine(
                    point(minute_angle, -r * 0.1),
                    point(minute_angle, r * 0.72),
                    brush,
                    3.0 * s,
                    &gfx.round_cap,
                );
                brush.SetColor(&rgba(p.accent, 1.0));
                target.DrawLine(
                    point(second_angle, -r * 0.16),
                    point(second_angle, r * 0.8),
                    brush,
                    1.6 * s,
                    &gfx.round_cap,
                );
                let dot = 4.0 * s;
                target
                    .FillEllipse(&D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: dot, radiusY: dot }, brush);

                // Digital time and date.
                let time = format!("{:02}:{:02}:{:02}", now.wHour, now.wMinute, now.wSecond);
                text_at(&gfx.big, &time, cx, m + 250.0 * s, 48.0 * s, rgba(p.foreground, 1.0));
                let date = format!(
                    "{}, {} {} {}",
                    WEEKDAY_NAMES[now.wDayOfWeek as usize % 7],
                    now.wDay,
                    MONTHS[(now.wMonth as usize + 11) % 12],
                    now.wYear
                );
                text_at(&gfx.text, &date, cx, m + 290.0 * s, 26.0 * s, rgba(p.foreground, 0.55));
            }
        }
        let _ = D2D_RECT_F::default();
        gfx.target.EndDraw(None, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leap_years_and_month_lengths() {
        assert!(is_leap(2024) && is_leap(2000) && !is_leap(1900) && !is_leap(2026));
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2026, 11), 30);
        assert_eq!(days_in_month(2026, 12), 31);
    }

    #[test]
    fn weekdays_are_monday_based() {
        assert_eq!(weekday(2026, 11, 22), 6); // a Sunday
        assert_eq!(weekday(2026, 11, 23), 0); // Monday
        assert_eq!(weekday(2000, 1, 1), 5); // Saturday
        assert_eq!(weekday(2024, 2, 29), 3); // Thursday
    }

    #[test]
    fn month_grid_has_six_full_weeks_starting_monday() {
        let grid = month_grid(2026, 11);
        assert_eq!(grid.len(), 42);
        // 1 November 2026 is a Sunday: six days of October lead.
        assert_eq!(grid[0], (2026, 10, 26));
        assert_eq!(grid[6], (2026, 11, 1));
        assert_eq!(grid[35], (2026, 11, 30));
        assert_eq!(grid[36], (2026, 12, 1));
        // January wraps back to the previous year.
        assert_eq!(month_grid(2027, 1)[0].0, 2026);
    }
}
