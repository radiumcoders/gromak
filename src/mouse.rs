//! A low-level mouse hook: reports pointer movement (for focus follows mouse)
//! and handles mouse-wheel bindings such as `mod+wheeldown`.
//!
//! The hook runs on its own thread so it can never slow down the pointer.
//! It does no work beyond noting that the mouse moved: movements are
//! coalesced into at most one pending `WM_APP_MOUSE` message, and the window
//! manager reads the current cursor position when it handles it.

use crate::config::{MOD_ALT, MOD_WIN, VK_WHEEL_DOWN, VK_WHEEL_UP};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::Mutex;
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION, MSG,
    MSLLHOOKSTRUCT, WHEEL_DELTA, WH_MOUSE_LL, WM_APP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_QUIT,
};

static ENABLED: AtomicBool = AtomicBool::new(false);
/// A `WM_APP_MOUSE` is queued and not yet handled.
static PENDING: AtomicBool = AtomicBool::new(false);
static THREAD_ID: AtomicU32 = AtomicU32::new(0);
/// Pointer position from the latest mouse-move event.
static LAST_X: AtomicI32 = AtomicI32::new(0);
static LAST_Y: AtomicI32 = AtomicI32::new(0);

/// The pointer position reported by the most recent mouse-move event.
pub fn last_pos() -> (i32, i32) {
    (LAST_X.load(Ordering::Relaxed), LAST_Y.load(Ordering::Relaxed))
}

/// A wheel binding: modifiers, `VK_WHEEL_UP` or `VK_WHEEL_DOWN`, and the id
/// posted with `WM_APP_HOTKEY`.
#[derive(Clone, Copy, Debug)]
pub struct WheelBind {
    pub mods: u32,
    pub vk: u32,
    pub id: i32,
}

static WHEEL_BINDS: Mutex<Vec<WheelBind>> = Mutex::new(Vec::new());
/// Wheel movement not yet turned into a notch (precision touchpads send
/// many small deltas).
static WHEEL_ACCUMULATED: AtomicI32 = AtomicI32::new(0);
/// Asks the hook thread to send the menu-mask key.
const WM_MASK: u32 = WM_APP + 51;

/// Replaces the wheel bindings.
pub fn set_wheel_binds(binds: Vec<WheelBind>) {
    *WHEEL_BINDS.lock().unwrap_or_else(|e| e.into_inner()) = binds;
}

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
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            if msg.message == WM_MASK {
                crate::win::mask_menu_key();
            }
        }
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
    if code == HC_ACTION as i32 && wp as u32 == WM_MOUSEWHEEL && handle_wheel(lp) {
        return 1;
    }
    if code == HC_ACTION as i32 && wp as u32 == WM_MOUSEMOVE {
        let info = &*(lp as *const MSLLHOOKSTRUCT);
        LAST_X.store(info.pt.x, Ordering::Relaxed);
        LAST_Y.store(info.pt.y, Ordering::Relaxed);
    }
    if code == HC_ACTION as i32
        && wp as u32 == WM_MOUSEMOVE
        && ENABLED.load(Ordering::Relaxed)
        && !PENDING.swap(true, Ordering::Relaxed)
    {
        crate::post_to_ctrl(crate::WM_APP_MOUSE, 0, 0);
    }
    CallNextHookEx(null_mut(), code, wp, lp)
}

/// Runs a wheel binding if one matches the held modifiers. Returns whether
/// the wheel event should be swallowed.
unsafe fn handle_wheel(lp: LPARAM) -> bool {
    let mods = crate::keyboard::current_mods();
    if mods == 0 {
        return false;
    }
    let binds: Vec<WheelBind> =
        WHEEL_BINDS.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|b| b.mods == mods).copied().collect();
    if binds.is_empty() {
        return false;
    }
    let info = &*(lp as *const MSLLHOOKSTRUCT);
    let delta = (info.mouseData >> 16) as u16 as i16 as i32;
    let mut total = WHEEL_ACCUMULATED.fetch_add(delta, Ordering::Relaxed) + delta;
    let notch = WHEEL_DELTA as i32;
    while total.abs() >= notch {
        let vk = if total > 0 { VK_WHEEL_UP } else { VK_WHEEL_DOWN };
        if let Some(bind) = binds.iter().find(|b| b.vk == vk) {
            crate::post_to_ctrl(crate::WM_APP_HOTKEY, bind.id as usize, 0);
        }
        let step = notch * total.signum();
        total -= step;
        WHEEL_ACCUMULATED.fetch_sub(step, Ordering::Relaxed);
    }
    if mods & (MOD_WIN | MOD_ALT) != 0 {
        // Releasing Win/Alt after scrolling must not open Start or a menu bar.
        PostThreadMessageW(THREAD_ID.load(Ordering::Relaxed), WM_MASK, 0, 0);
    }
    true
}
