//! Thin wrappers around the Win32 calls tessera needs.
//!
//! Windows are passed around as `isize` (`Hwnd`) so they can be stored in
//! hash maps and atomics; they are converted to `HWND` at the call site.

use crate::layout::Rect;
use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{CloseHandle, BOOL, HANDLE, HWND, LPARAM, POINT, RECT, SYSTEMTIME};
use windows_sys::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DwmSetWindowAttribute};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, MonitorFromRect, MonitorFromWindow, HDC, HMONITOR,
    MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows_sys::Win32::System::SystemInformation::GetLocalTime;
use windows_sys::Win32::System::Threading::{
    AttachThreadInput, GetCurrentProcess, GetCurrentThreadId, OpenProcess, OpenProcessToken,
    QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::HiDpi::{
    GetDpiForMonitor, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, MDT_EFFECTIVE_DPI,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{SendInput, INPUT, INPUT_KEYBOARD, KEYEVENTF_KEYUP};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub type Hwnd = isize;

#[inline]
fn hw(h: Hwnd) -> HWND {
    h as HWND
}

// DWM attributes, spelled out so we do not depend on SDK-version-specific names.
const DWMWA_CLOAKED: u32 = 14;
const DWMWA_EXTENDED_FRAME_BOUNDS: u32 = 9;
const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
const DWMWA_BORDER_COLOR: u32 = 34;
/// Restores the system default border colour.
pub const BORDER_DEFAULT: u32 = 0xFFFF_FFFF;
/// Draws no border at all.
pub const BORDER_NONE: u32 = 0xFFFF_FFFE;

/// Window classes that are never tiled: shell surfaces and system popups.
const IGNORED_CLASSES: &[&str] = &[
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Windows.UI.Core.CoreWindow",
    "XamlExplorerHostIslandWindow",
    "TopLevelWindowForOverflowXamlIsland",
    "NotifyIconOverflowWindow",
    "MultitaskingViewFrame",
    "ForegroundStaging",
    "SysShadow",
    "tooltips_class32",
    "#32768",
    "tessera_bar",
    "tessera_ctrl",
];

/// UTF-16, NUL-terminated.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn set_dpi_awareness() {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

pub fn is_window(h: Hwnd) -> bool {
    unsafe { IsWindow(hw(h)) != 0 }
}

pub fn is_visible(h: Hwnd) -> bool {
    unsafe { IsWindowVisible(hw(h)) != 0 }
}

pub fn is_minimized(h: Hwnd) -> bool {
    unsafe { IsIconic(hw(h)) != 0 }
}

pub fn foreground() -> Hwnd {
    unsafe { GetForegroundWindow() as Hwnd }
}

pub fn title(h: Hwnd) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hw(h));
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(hw(h), buf.as_mut_ptr(), buf.len() as i32);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

pub fn class(h: Hwnd) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hw(h), buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

pub fn style(h: Hwnd) -> u32 {
    unsafe { GetWindowLongPtrW(hw(h), GWL_STYLE) as u32 }
}

pub fn ex_style(h: Hwnd) -> u32 {
    unsafe { GetWindowLongPtrW(hw(h), GWL_EXSTYLE) as u32 }
}

pub fn owner(h: Hwnd) -> Hwnd {
    unsafe { GetWindow(hw(h), GW_OWNER) as Hwnd }
}

pub fn pid(h: Hwnd) -> u32 {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hw(h), &mut pid) };
    pid
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

fn open_process(pid: u32) -> Option<OwnedHandle> {
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    (!h.is_null()).then_some(OwnedHandle(h))
}

/// The executable file name (e.g. `firefox.exe`) of a process.
pub fn exe_name(pid: u32) -> String {
    let Some(process) = open_process(pid) else {
        return String::new();
    };
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    if unsafe { QueryFullProcessImageNameW(process.0, 0, buf.as_mut_ptr(), &mut len) } == 0 {
        return String::new();
    }
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    path.rsplit('\\').next().unwrap_or_default().to_string()
}

fn token_elevated(process: HANDLE) -> bool {
    unsafe {
        let mut token: HANDLE = null_mut();
        if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let token = OwnedHandle(token);
        let mut elevation: TOKEN_ELEVATION = zeroed();
        let mut len = 0;
        let ok = GetTokenInformation(
            token.0,
            TokenElevation,
            &mut elevation as *mut _ as *mut c_void,
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

/// Whether tessera itself runs elevated (as administrator).
pub fn self_elevated() -> bool {
    token_elevated(unsafe { GetCurrentProcess() })
}

/// Whether another process runs elevated. Unelevated processes cannot move
/// elevated windows (UIPI), so such windows are left alone.
pub fn process_elevated(pid: u32) -> bool {
    open_process(pid).is_some_and(|p| token_elevated(p.0))
}

pub fn is_cloaked(h: Hwnd) -> bool {
    let mut cloaked: u32 = 0;
    let hr = unsafe {
        DwmGetWindowAttribute(hw(h), DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut c_void, size_of::<u32>() as u32)
    };
    hr == 0 && cloaked != 0
}

fn to_rect(r: RECT) -> Rect {
    Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top)
}

fn to_win_rect(r: Rect) -> RECT {
    RECT { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() }
}

fn outer_rect(h: Hwnd) -> RECT {
    let mut r: RECT = unsafe { zeroed() };
    unsafe { GetWindowRect(hw(h), &mut r) };
    r
}

/// The visible frame (excluding the invisible resize borders on Windows 10+).
fn frame_bounds(h: Hwnd) -> Option<RECT> {
    let mut r: RECT = unsafe { zeroed() };
    let hr = unsafe {
        DwmGetWindowAttribute(
            hw(h),
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut RECT as *mut c_void,
            size_of::<RECT>() as u32,
        )
    };
    (hr == 0).then_some(r)
}

/// The window's visible rectangle on screen.
pub fn window_rect(h: Hwnd) -> Rect {
    to_rect(frame_bounds(h).unwrap_or_else(|| outer_rect(h)))
}

/// Static information used to match window rules.
#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub title: String,
    pub class: String,
    pub exe: String,
    pub pid: u32,
    pub resizable: bool,
    pub has_owner: bool,
}

impl WindowInfo {
    pub fn of(h: Hwnd) -> Self {
        let pid = pid(h);
        WindowInfo {
            title: title(h),
            class: class(h),
            exe: exe_name(pid),
            pid,
            resizable: style(h) & WS_THICKFRAME != 0,
            has_owner: owner(h) != 0,
        }
    }
}

/// Whether `h` looks like a regular application window that can be tiled.
pub fn is_manageable(h: Hwnd) -> bool {
    if h == 0 || !is_window(h) || !is_visible(h) || is_minimized(h) {
        return false;
    }
    unsafe {
        if GetAncestor(hw(h), GA_ROOT) != hw(h) {
            return false;
        }
    }
    let st = style(h);
    let ex = ex_style(h);
    if st & WS_CHILD != 0 || ex & WS_EX_NOACTIVATE != 0 {
        return false;
    }
    if ex & WS_EX_TOOLWINDOW != 0 && ex & WS_EX_APPWINDOW == 0 {
        return false;
    }
    if st & WS_CAPTION != WS_CAPTION && st & WS_THICKFRAME == 0 {
        return false;
    }
    if is_cloaked(h) || title(h).trim().is_empty() {
        return false;
    }
    let class = class(h);
    if IGNORED_CLASSES.contains(&class.as_str()) {
        return false;
    }
    if pid(h) == std::process::id() {
        return false;
    }
    let r = window_rect(h);
    r.w > 0 && r.h > 0
}

/// A window that put itself into fullscreen (F11 in a browser, a video
/// player): no caption and covering its whole monitor. Tessera leaves
/// these alone so the app can manage its own fullscreen state.
pub fn is_app_fullscreen(h: Hwnd) -> bool {
    if style(h) & WS_CAPTION == WS_CAPTION {
        return false;
    }
    let r = window_rect(h);
    monitor_info(monitor_of_window(h)).is_some_and(|m| {
        r.x <= m.rect.x && r.y <= m.rect.y && r.right() >= m.rect.right() && r.bottom() >= m.rect.bottom()
    })
}

/// Moves and resizes `h` so that its *visible* frame matches `r`.
pub fn set_window_rect(h: Hwnd, r: Rect) {
    unsafe {
        if IsZoomed(hw(h)) != 0 {
            ShowWindow(hw(h), SW_SHOWNOACTIVATE);
        }
        let outer = outer_rect(h);
        let (l, t, rr, b) = match frame_bounds(h) {
            Some(f) => (f.left - outer.left, f.top - outer.top, outer.right - f.right, outer.bottom - f.bottom),
            None => (0, 0, 0, 0),
        };
        let flags = SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS;
        let target = to_win_rect(r);
        // Crossing to a monitor with a different DPI makes the window rescale
        // itself after the first move; a second call corrects the size.
        let crosses = MonitorFromWindow(hw(h), MONITOR_DEFAULTTONEAREST) != MonitorFromRect(&target, MONITOR_DEFAULTTONEAREST);
        for _ in 0..if crosses { 2 } else { 1 } {
            SetWindowPos(hw(h), null_mut(), r.x - l, r.y - t, r.w + l + rr, r.h + t + b, flags);
        }
    }
}

/// Brings `h` to the top of the (non-topmost) z-order without activating it.
pub fn raise(h: Hwnd) {
    unsafe {
        SetWindowPos(
            hw(h),
            HWND_TOP,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
        );
    }
}

/// `ShowWindow`, synchronous so that visibility checks made right after are
/// accurate, but asynchronous for hung windows so they cannot block tessera.
fn show_window(h: Hwnd, cmd: SHOW_WINDOW_CMD) {
    unsafe {
        if IsHungAppWindow(hw(h)) != 0 {
            ShowWindowAsync(hw(h), cmd);
        } else {
            ShowWindow(hw(h), cmd);
        }
    }
}

/// Shows a window without activating it.
pub fn show(h: Hwnd) {
    show_window(h, SW_SHOWNA);
}

/// Hides a window.
pub fn hide(h: Hwnd) {
    show_window(h, SW_HIDE);
}

/// Gives keyboard focus to `h`, working around foreground-lock rules.
pub fn focus(h: Hwnd) {
    unsafe {
        if IsIconic(hw(h)) != 0 {
            ShowWindow(hw(h), SW_RESTORE);
        }
        if SetForegroundWindow(hw(h)) != 0 {
            return;
        }
        let fg_thread = GetWindowThreadProcessId(GetForegroundWindow(), null_mut());
        let me = GetCurrentThreadId();
        if fg_thread != 0 && fg_thread != me {
            AttachThreadInput(me, fg_thread, 1);
            BringWindowToTop(hw(h));
            SetForegroundWindow(hw(h));
            AttachThreadInput(me, fg_thread, 0);
        }
    }
}

/// Focuses the desktop, used when switching to an empty workspace so that
/// focus does not jump to a window on another monitor.
pub fn focus_desktop() {
    let progman = wide("Progman");
    unsafe {
        let desktop = FindWindowW(progman.as_ptr(), null());
        if !desktop.is_null() {
            SetForegroundWindow(desktop);
        }
    }
}

/// Politely asks a window to close.
pub fn close(h: Hwnd) {
    unsafe { PostMessageW(hw(h), WM_CLOSE, 0, 0) };
}

/// Sets the Windows 11 DWM border colour (`COLORREF`, or [`BORDER_DEFAULT`]).
pub fn set_border_color(h: Hwnd, color: u32) {
    unsafe {
        DwmSetWindowAttribute(hw(h), DWMWA_BORDER_COLOR, &color as *const u32 as *const c_void, size_of::<u32>() as u32);
    }
}

/// Asks DWM to round the corners of one of our own windows.
pub fn round_corners(h: Hwnd) {
    let round: i32 = 2; // DWMWCP_ROUND
    unsafe {
        DwmSetWindowAttribute(
            hw(h),
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &round as *const i32 as *const c_void,
            size_of::<i32>() as u32,
        );
    }
}

/// Releases the menu-bar activation that Windows performs when Alt or Win is
/// pressed and released on its own: a hotkey swallows the other key, so the
/// focused app would otherwise see a lone Alt and open its menu bar.
/// Sends an unassigned virtual key (0xE8), the same trick AutoHotkey uses.
pub fn mask_menu_key() {
    unsafe {
        let mut inputs: [INPUT; 2] = zeroed();
        for (i, input) in inputs.iter_mut().enumerate() {
            input.r#type = INPUT_KEYBOARD;
            input.Anonymous.ki.wVk = 0xE8;
            input.Anonymous.ki.dwFlags = if i == 1 { KEYEVENTF_KEYUP } else { 0 };
        }
        SendInput(2, inputs.as_ptr(), size_of::<INPUT>() as i32);
    }
}

unsafe extern "system" fn collect_window(h: HWND, lp: LPARAM) -> BOOL {
    let out = &mut *(lp as *mut Vec<Hwnd>);
    out.push(h as Hwnd);
    1
}

/// Every top-level window, in z-order.
pub fn top_level_windows() -> Vec<Hwnd> {
    let mut out: Vec<Hwnd> = Vec::new();
    unsafe { EnumWindows(Some(collect_window), &mut out as *mut _ as LPARAM) };
    out
}

/// The taskbar windows of every monitor.
pub fn taskbars() -> Vec<Hwnd> {
    top_level_windows()
        .into_iter()
        .filter(|&h| matches!(class(h).as_str(), "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"))
        .collect()
}

pub fn set_visible_raw(h: Hwnd, visible: bool) {
    unsafe { ShowWindow(hw(h), if visible { SW_SHOW } else { SW_HIDE }) };
}

#[derive(Debug, Clone, Copy)]
pub struct MonitorInfo {
    pub handle: isize,
    /// The whole monitor.
    pub rect: Rect,
    /// The monitor minus the taskbar.
    pub work: Rect,
    /// DPI scale factor (1.0 = 96 DPI).
    pub scale: f32,
    pub primary: bool,
}

fn monitor_info(handle: isize) -> Option<MonitorInfo> {
    unsafe {
        let mut mi: MONITORINFO = zeroed();
        mi.cbSize = size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(handle as HMONITOR, &mut mi) == 0 {
            return None;
        }
        let (mut dx, mut dy) = (96u32, 96u32);
        if GetDpiForMonitor(handle as HMONITOR, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) != 0 {
            dx = 96;
        }
        Some(MonitorInfo {
            handle,
            rect: to_rect(mi.rcMonitor),
            work: to_rect(mi.rcWork),
            scale: dx as f32 / 96.0,
            primary: mi.dwFlags & MONITORINFOF_PRIMARY != 0,
        })
    }
}

unsafe extern "system" fn collect_monitor(m: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> BOOL {
    let out = &mut *(lp as *mut Vec<isize>);
    out.push(m as isize);
    1
}

/// All monitors, ordered left to right, then top to bottom.
pub fn monitors() -> Vec<MonitorInfo> {
    let mut handles: Vec<isize> = Vec::new();
    unsafe {
        EnumDisplayMonitors(null_mut(), null(), Some(collect_monitor), &mut handles as *mut _ as LPARAM);
    }
    let mut out: Vec<MonitorInfo> = handles.into_iter().filter_map(monitor_info).collect();
    out.sort_by_key(|m| (m.rect.x, m.rect.y));
    out
}

pub fn monitor_of_window(h: Hwnd) -> isize {
    unsafe { MonitorFromWindow(hw(h), MONITOR_DEFAULTTONEAREST) as isize }
}

pub fn cursor_pos() -> (i32, i32) {
    let mut p = POINT { x: 0, y: 0 };
    unsafe { GetCursorPos(&mut p) };
    (p.x, p.y)
}

pub fn monitor_at(x: i32, y: i32) -> isize {
    unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) as isize }
}

pub fn local_time() -> SYSTEMTIME {
    let mut t: SYSTEMTIME = unsafe { zeroed() };
    unsafe { GetLocalTime(&mut t) };
    t
}
