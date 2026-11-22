//! Thin wrappers around the Win32 calls gromak needs.
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
const DWMWA_BORDER_COLOR: u32 = 34;
/// Restores the system default border colour.
pub const BORDER_DEFAULT: u32 = 0xFFFF_FFFF;

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
    "gromak_bar",
    "gromak_ctrl",
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

/// Whether gromak itself runs elevated (as administrator).
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
/// player): no caption and covering its whole monitor. Gromak leaves
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
    place_window(h, r, false);
}

/// Like [`set_window_rect`], but waits for the window to move (unless it is
/// hung), so a following show reveals it already in place.
pub fn set_window_rect_now(h: Hwnd, r: Rect) {
    let hung = unsafe { IsHungAppWindow(hw(h)) != 0 };
    place_window(h, r, !hung);
}

fn place_window(h: Hwnd, r: Rect, sync: bool) {
    unsafe {
        if IsZoomed(hw(h)) != 0 {
            show_window(h, SW_SHOWNOACTIVATE);
        }
        let outer = outer_rect(h);
        let (l, t, rr, b) = match frame_bounds(h) {
            Some(f) => (f.left - outer.left, f.top - outer.top, outer.right - f.right, outer.bottom - f.bottom),
            None => (0, 0, 0, 0),
        };
        let mut flags = SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER;
        if !sync {
            flags |= SWP_ASYNCWINDOWPOS;
        }
        let target = to_win_rect(r);
        // Crossing to a monitor with a different DPI makes the window rescale
        // itself after the first move; a second call corrects the size.
        let crosses =
            MonitorFromWindow(hw(h), MONITOR_DEFAULTTONEAREST) != MonitorFromRect(&target, MONITOR_DEFAULTTONEAREST);
        for _ in 0..if crosses { 2 } else { 1 } {
            SetWindowPos(hw(h), null_mut(), r.x - l, r.y - t, r.w + l + rr, r.h + t + b, flags);
        }
    }
}

/// Brings `h` to the top of the (non-topmost) z-order without activating it.
pub fn raise(h: Hwnd) {
    unsafe {
        SetWindowPos(hw(h), HWND_TOP, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS);
    }
}

/// `ShowWindow`, synchronous so that visibility checks made right after are
/// accurate, but asynchronous for hung windows so they cannot block gromak.
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
pub fn focus(h: Hwnd) -> bool {
    let is_foreground = || unsafe {
        let fg = GetForegroundWindow();
        fg == hw(h) || GetAncestor(fg, GA_ROOT) == hw(h)
    };
    unsafe {
        if IsIconic(hw(h)) != 0 {
            ShowWindow(hw(h), SW_RESTORE);
        }
        if SetForegroundWindow(hw(h)) != 0 && is_foreground() {
            return true;
        }
        // Windows refuses to hand the foreground to another app unless we
        // own the last input event. A (masked) Alt tap counts as input and
        // lifts the foreground lock; the unassigned 0xE8 key in between keeps
        // the focused app from opening its menu bar on the Alt release.
        send_alt_tap();
        if SetForegroundWindow(hw(h)) != 0 && is_foreground() {
            return true;
        }
        let fg_thread = GetWindowThreadProcessId(GetForegroundWindow(), null_mut());
        let me = GetCurrentThreadId();
        if fg_thread != 0 && fg_thread != me {
            AttachThreadInput(me, fg_thread, 1);
            BringWindowToTop(hw(h));
            SetForegroundWindow(hw(h));
            AttachThreadInput(me, fg_thread, 0);
        }
        is_foreground()
    }
}

/// Alt down, 0xE8 down/up, Alt up.
fn send_alt_tap() {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::VK_MENU;
    unsafe {
        let mut inputs: [INPUT; 4] = zeroed();
        let keys = [(VK_MENU, 0), (0xE8, 0), (0xE8, KEYEVENTF_KEYUP), (VK_MENU, KEYEVENTF_KEYUP)];
        for (input, (vk, flags)) in inputs.iter_mut().zip(keys) {
            input.r#type = INPUT_KEYBOARD;
            input.Anonymous.ki.wVk = vk;
            input.Anonymous.ki.dwFlags = flags;
        }
        SendInput(inputs.len() as u32, inputs.as_ptr(), size_of::<INPUT>() as i32);
    }
}

/// Raises `h` above other normal windows, synchronously (so it really ends
/// up above a window that was just activated). Skips hung windows.
pub fn raise_now(h: Hwnd) {
    unsafe {
        if IsHungAppWindow(hw(h)) != 0 {
            raise(h);
            return;
        }
        SetWindowPos(hw(h), HWND_TOP, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
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
        DwmSetWindowAttribute(
            hw(h),
            DWMWA_BORDER_COLOR,
            &color as *const u32 as *const c_void,
            size_of::<u32>() as u32,
        );
    }
}

/// The window's small icon (`HICON`), or 0. The handle belongs to the
/// window; do not destroy it. Hung windows are skipped rather than waited on.
pub fn window_icon(h: Hwnd) -> isize {
    const ICON_SMALL: usize = 0;
    const ICON_BIG: usize = 1;
    const ICON_SMALL2: usize = 2;
    for kind in [ICON_SMALL2, ICON_SMALL, ICON_BIG] {
        let mut icon: usize = 0;
        let ok =
            unsafe { SendMessageTimeoutW(hw(h), WM_GETICON, kind, 0, SMTO_ABORTIFHUNG | SMTO_BLOCK, 50, &mut icon) };
        if ok != 0 && icon != 0 {
            return icon as isize;
        }
    }
    for index in [GCLP_HICONSM, GCLP_HICON] {
        let icon = unsafe { GetClassLongPtrW(hw(h), index) };
        if icon != 0 {
            return icon as isize;
        }
    }
    0
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
    top_level_windows().into_iter().filter(|&h| is_taskbar(h)).collect()
}

/// Whether `h` is the primary or a secondary Windows taskbar.
pub fn is_taskbar(h: Hwnd) -> bool {
    matches!(class(h).as_str(), "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
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
        Some(MonitorInfo { handle, rect: to_rect(mi.rcMonitor), work: to_rect(mi.rcWork), scale: dx as f32 / 96.0 })
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

/// The bounds of the monitor at (or nearest to) a point.
pub fn monitor_rect_at(x: i32, y: i32) -> Rect {
    monitor_info(monitor_at(x, y)).map_or_else(Rect::default, |m| m.rect)
}

pub fn local_time() -> SYSTEMTIME {
    let mut t: SYSTEMTIME = unsafe { zeroed() };
    unsafe { GetLocalTime(&mut t) };
    t
}

/// The top-level window under a screen point, or 0.
pub fn window_at(x: i32, y: i32) -> Hwnd {
    unsafe {
        let h = WindowFromPoint(POINT { x, y });
        if h.is_null() {
            return 0;
        }
        GetAncestor(h, GA_ROOT) as Hwnd
    }
}

/// Whether any mouse button is currently held down.
pub fn mouse_button_down() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON};
    [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON].iter().any(|&vk| unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000 != 0)
}

pub fn set_cursor_pos(x: i32, y: i32) {
    unsafe { SetCursorPos(x, y) };
}

/// Runs a command line the way the Run dialog would, without a console
/// window. `start` resolves App Paths and execution aliases (wt.exe, …).
pub fn run_command(command: &str) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("cmd.exe")
        .raw_arg(format!("/C start \"\" {command}"))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
}

/// Starts a program, document or URI without activating it (where the app
/// honours that) and returns its process id (0 when an already-running
/// process handled it, as with most URIs).
pub fn shell_launch(file: &str, params: &str, cwd: Option<&str>) -> Result<u32, String> {
    use windows_sys::Win32::System::Threading::GetProcessId;
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };
    let (verb, file_w, params_w) = (wide("open"), wide(file), wide(params));
    let cwd_w = cwd.map(wide);
    let mut info: SHELLEXECUTEINFOW = unsafe { zeroed() };
    info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file_w.as_ptr();
    info.lpParameters = if params.is_empty() { null() } else { params_w.as_ptr() };
    info.lpDirectory = cwd_w.as_ref().map_or(null(), |c| c.as_ptr());
    info.nShow = SW_SHOWNOACTIVATE;
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        return Err(format!("could not start {file}: {}", std::io::Error::last_os_error()));
    }
    let mut pid = 0;
    if !info.hProcess.is_null() {
        unsafe {
            pid = GetProcessId(info.hProcess);
            CloseHandle(info.hProcess);
        }
    }
    Ok(pid)
}

/// Opens a file, folder or URI (`ms-settings:…`) with its default handler.
pub fn shell_open(target: &str) {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    let (verb, path) = (wide("open"), wide(target));
    let result = unsafe { ShellExecuteW(null_mut(), verb.as_ptr(), path.as_ptr(), null(), null(), SW_SHOWNORMAL) };
    if result as isize <= 32 {
        warn!("could not open {target}: code {}", result as isize);
    }
}

const CF_UNICODETEXT: u32 = 13;

/// The clipboard's text, if it holds any.
pub fn clipboard_text() -> Option<String> {
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalUnlock};
    unsafe {
        if OpenClipboard(null_mut()) == 0 {
            return None;
        }
        let handle = GetClipboardData(CF_UNICODETEXT);
        let text = if handle.is_null() {
            None
        } else {
            let ptr = GlobalLock(handle) as *const u16;
            let text = (!ptr.is_null()).then(|| {
                let len = (0..).take_while(|&i| *ptr.add(i) != 0).count();
                String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
            });
            GlobalUnlock(handle);
            text
        };
        CloseClipboard();
        text
    }
}

/// Puts text on the clipboard. Returns whether it worked.
pub fn set_clipboard_text(text: &str) -> bool {
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    let utf16 = wide(text);
    unsafe {
        if OpenClipboard(null_mut()) == 0 {
            return false;
        }
        EmptyClipboard();
        let bytes = utf16.len() * 2;
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes);
        let mut ok = false;
        if !memory.is_null() {
            let dest = GlobalLock(memory) as *mut u16;
            if !dest.is_null() {
                std::ptr::copy_nonoverlapping(utf16.as_ptr(), dest, utf16.len());
                GlobalUnlock(memory);
                ok = !SetClipboardData(CF_UNICODETEXT, memory).is_null();
            }
            if !ok {
                GlobalFree(memory);
            }
        }
        CloseClipboard();
        ok
    }
}

/// Presses Ctrl+V in the focused window.
pub fn send_paste() {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::VK_CONTROL;
    unsafe {
        let mut inputs: [INPUT; 4] = zeroed();
        let keys = [(VK_CONTROL, 0), (b'V' as u16, 0), (b'V' as u16, KEYEVENTF_KEYUP), (VK_CONTROL, KEYEVENTF_KEYUP)];
        for (input, (vk, flags)) in inputs.iter_mut().zip(keys) {
            input.r#type = INPUT_KEYBOARD;
            input.Anonymous.ki.wVk = vk;
            input.Anonymous.ki.dwFlags = flags;
        }
        SendInput(inputs.len() as u32, inputs.as_ptr(), size_of::<INPUT>() as i32);
    }
}

/// Puts files on the clipboard the way Explorer copies them (`CF_HDROP`),
/// so pasting into chat apps or upload fields sends the file itself.
pub fn set_clipboard_files(paths: &[std::path::PathBuf]) -> bool {
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE, GMEM_ZEROINIT};
    use windows_sys::Win32::UI::Shell::DROPFILES;
    const CF_HDROP: u32 = 15;
    // DROPFILES header, then NUL-separated wide paths, then a final NUL.
    let mut list: Vec<u16> = Vec::new();
    for path in paths {
        list.extend(path.to_string_lossy().encode_utf16());
        list.push(0);
    }
    list.push(0);
    let header = size_of::<DROPFILES>();
    unsafe {
        if OpenClipboard(null_mut()) == 0 {
            return false;
        }
        EmptyClipboard();
        let memory = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, header + list.len() * 2);
        let mut ok = false;
        if !memory.is_null() {
            let base = GlobalLock(memory) as *mut u8;
            if !base.is_null() {
                let drop_files = base as *mut DROPFILES;
                (*drop_files).pFiles = header as u32;
                (*drop_files).fWide = 1;
                std::ptr::copy_nonoverlapping(list.as_ptr(), base.add(header) as *mut u16, list.len());
                GlobalUnlock(memory);
                ok = !SetClipboardData(CF_HDROP, memory).is_null();
            }
            if !ok {
                GlobalFree(memory);
            }
        }
        CloseClipboard();
        ok
    }
}

/// `dwExtraInfo` gromak puts on keys it injects to replay a swallowed key,
/// so its own keyboard hook can recognise and pass them.
pub const INJECTED_MARK: usize = 0x7E55_E7A0;

/// Replays a swallowed key release (`vk`, e.g. the Win key) behind an
/// unassigned key (0xE8), so the shell does not see a lone tap.
pub fn replay_masked_key_up(vk: u16) {
    unsafe {
        let mut inputs: [INPUT; 3] = zeroed();
        let keys = [(0xE8, 0), (0xE8, KEYEVENTF_KEYUP), (vk, KEYEVENTF_KEYUP)];
        for (input, (key, flags)) in inputs.iter_mut().zip(keys) {
            input.r#type = INPUT_KEYBOARD;
            input.Anonymous.ki.wVk = key;
            input.Anonymous.ki.dwFlags = flags;
            input.Anonymous.ki.dwExtraInfo = INJECTED_MARK;
        }
        SendInput(inputs.len() as u32, inputs.as_ptr(), size_of::<INPUT>() as i32);
    }
}

/// Turns DWM's show/hide transitions (the scale-and-fade when a window
/// appears or disappears) off or back on for a window. gromak hides and
/// shows windows on every workspace switch, so the transitions would play
/// constantly.
pub fn set_transitions_disabled(h: Hwnd, disabled: bool) {
    const DWMWA_TRANSITIONS_FORCEDISABLED: u32 = 3;
    let value: BOOL = disabled as BOOL;
    unsafe {
        DwmSetWindowAttribute(
            hw(h),
            DWMWA_TRANSITIONS_FORCEDISABLED,
            &value as *const BOOL as *const c_void,
            size_of::<BOOL>() as u32,
        );
    }
}

/// Asks DWM for square (`true`) or Windows 11's default rounded corners on
/// a window. Works on other processes' windows, like the border colour.
pub fn set_square_corners(h: Hwnd, square: bool) {
    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    // DWMWCP_DONOTROUND, DWMWCP_DEFAULT.
    let value: u32 = if square { 1 } else { 0 };
    unsafe {
        DwmSetWindowAttribute(
            hw(h),
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &value as *const u32 as *const c_void,
            size_of::<u32>() as u32,
        );
    }
}

/// The desktop or taskbar (or no window at all): places focus can rest
/// without belonging to an application.
pub fn is_shell_surface(h: Hwnd) -> bool {
    h == 0 || matches!(class(h).as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
}

/// Gives one of our own windows a blurred ("acrylic") backdrop of whatever
/// is behind it, tinted with `tint` (`COLORREF`) at `tint_alpha` (0-255).
///
/// Uses the undocumented `SetWindowCompositionAttribute`, which (unlike the
/// documented Mica/Acrylic backdrops) keeps blurring while the window is
/// inactive, which a bar always is. Returns false if it is unavailable.
pub fn set_acrylic(h: Hwnd, tint: u32, tint_alpha: u8) -> bool {
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    #[repr(C)]
    struct AccentPolicy {
        state: u32,
        flags: u32,
        gradient: u32,
        animation_id: u32,
    }
    #[repr(C)]
    struct CompositionData {
        attribute: u32,
        data: *mut c_void,
        size: usize,
    }
    type SetWindowCompositionAttribute = unsafe extern "system" fn(HWND, *mut CompositionData) -> BOOL;
    const WCA_ACCENT_POLICY: u32 = 19;
    const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;
    unsafe {
        let user32 = GetModuleHandleW(wide("user32.dll").as_ptr());
        let Some(proc) = GetProcAddress(user32, c"SetWindowCompositionAttribute".as_ptr().cast()) else {
            return false;
        };
        let set: SetWindowCompositionAttribute = std::mem::transmute(proc);
        // GradientColor is 0xAABBGGRR.
        let mut policy = AccentPolicy {
            state: ACCENT_ENABLE_ACRYLICBLURBEHIND,
            flags: 0,
            gradient: (tint & 0x00FF_FFFF) | ((tint_alpha as u32) << 24),
            animation_id: 0,
        };
        let mut data = CompositionData {
            attribute: WCA_ACCENT_POLICY,
            data: &mut policy as *mut AccentPolicy as *mut c_void,
            size: size_of::<AccentPolicy>(),
        };
        set(hw(h), &mut data) != 0
    }
}
