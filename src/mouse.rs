//! A low-level mouse hook that reports pointer movement, for focus follows
//! mouse.
//!
//! The hook runs on its own thread so it can never slow down the pointer.
//! It does no work beyond noting that the mouse moved: movements are
//! coalesced into at most one pending `WM_APP_MOUSE` message, and the window
//! manager reads the current cursor position when it handles it.

use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION, MSG,
    WH_MOUSE_LL, WM_MOUSEMOVE, WM_QUIT,
};

static ENABLED: AtomicBool = AtomicBool::new(false);
/// A `WM_APP_MOUSE` is queued and not yet handled.
static PENDING: AtomicBool = AtomicBool::new(false);
static THREAD_ID: AtomicU32 = AtomicU32::new(0);

/// Turns movement reporting on or off (follows `focus_follows_mouse`).
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

/// Called by the window manager when it starts handling a movement, so the
/// next movement queues a new message.
pub fn handled() {
    PENDING.store(false, Ordering::Relaxed);
}

/// Installs the hook on a dedicated thread.
pub fn start() {
    let spawned = std::thread::Builder::new().name("mouse-hook".into()).spawn(|| unsafe {
        THREAD_ID.store(GetCurrentThreadId(), Ordering::Relaxed);
        let hook = SetWindowsHookExW(WH_MOUSE_LL, Some(hook_proc), GetModuleHandleW(null()), 0);
        if hook.is_null() {
            error!("could not install the mouse hook; focus follows mouse will not work");
            return;
        }
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {}
        UnhookWindowsHookEx(hook);
    });
    if let Err(e) = spawned {
        error!("could not start the mouse hook thread: {e}");
    }
}

/// Removes the hook and ends its thread.
pub fn stop() {
    let id = THREAD_ID.load(Ordering::Relaxed);
    if id != 0 {
        unsafe { PostThreadMessageW(id, WM_QUIT, 0, 0) };
    }
}

unsafe extern "system" fn hook_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32
        && wp as u32 == WM_MOUSEMOVE
        && ENABLED.load(Ordering::Relaxed)
        && !PENDING.swap(true, Ordering::Relaxed)
    {
        crate::post_to_ctrl(crate::WM_APP_MOUSE, 0, 0);
    }
    CallNextHookEx(null_mut(), code, wp, lp)
}
