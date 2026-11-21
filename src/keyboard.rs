//! A low-level keyboard hook for bindings `RegisterHotKey` cannot take.
//!
//! Windows reserves most Win+key combinations for itself (Win+1…9 belong to
//! the taskbar, Win+D shows the desktop, …), so `RegisterHotKey` fails for
//! them. A `WH_KEYBOARD_LL` hook sees keys before the shell does and can
//! swallow them. It runs on its own thread so that a busy window manager can
//! never make the keyboard lag.
//!
//! Limitations inherent to the mechanism:
//! - Win+L (lock) is handled by the secure desktop and cannot be overridden.
//! - While an elevated (administrator) window has focus, an unelevated
//!   tessera does not receive its keyboard input.

use crate::config::{MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU, VK_RCONTROL, VK_RMENU, VK_RSHIFT,
    VK_RWIN, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION,
    KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_APP, WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

/// A binding handled by the hook.
#[derive(Clone, Copy, Debug)]
pub struct HookBind {
    pub mods: u32,
    pub vk: u32,
    /// The id posted to the control window with `WM_APP_HOTKEY`.
    pub id: i32,
    /// Fire again while the key is held (auto-repeat).
    pub repeat: bool,
}

static BINDS: Mutex<Vec<HookBind>> = Mutex::new(Vec::new());
/// Keys whose key-down we swallowed, so we also swallow their key-up.
static HELD: [AtomicBool; 256] = [const { AtomicBool::new(false) }; 256];
static THREAD_ID: AtomicU32 = AtomicU32::new(0);
/// Asks the hook thread to send the menu-mask key.
const WM_MASK: u32 = WM_APP + 50;

/// Replaces the set of bindings the hook intercepts.
pub fn set_binds(binds: Vec<HookBind>) {
    *BINDS.lock().unwrap_or_else(|e| e.into_inner()) = binds;
}

/// Installs the hook on a dedicated thread.
pub fn start() {
    let spawned = std::thread::Builder::new().name("keyboard-hook".into()).spawn(|| unsafe {
        THREAD_ID.store(GetCurrentThreadId(), Ordering::Relaxed);
        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), GetModuleHandleW(null()), 0);
        if hook.is_null() {
            error!("could not install the keyboard hook; win-key bindings will not work");
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
        error!("could not start the keyboard hook thread: {e}");
    }
}

/// Removes the hook and ends its thread.
pub fn stop() {
    let id = THREAD_ID.load(Ordering::Relaxed);
    if id != 0 {
        unsafe { PostThreadMessageW(id, WM_QUIT, 0, 0) };
    }
}

fn is_down(vk: u16) -> bool {
    unsafe { GetAsyncKeyState(vk as i32) as u16 & 0x8000 != 0 }
}

fn current_mods() -> u32 {
    let mut mods = 0;
    if is_down(VK_MENU) {
        mods |= MOD_ALT;
    }
    if is_down(VK_CONTROL) {
        mods |= MOD_CONTROL;
    }
    if is_down(VK_SHIFT) {
        mods |= MOD_SHIFT;
    }
    if is_down(VK_LWIN) || is_down(VK_RWIN) {
        mods |= MOD_WIN;
    }
    mods
}

fn is_modifier(vk: u32) -> bool {
    [
        VK_MENU,
        VK_LMENU,
        VK_RMENU,
        VK_CONTROL,
        VK_LCONTROL,
        VK_RCONTROL,
        VK_SHIFT,
        VK_LSHIFT,
        VK_RSHIFT,
        VK_LWIN,
        VK_RWIN,
    ]
    .contains(&(vk as u16))
}

unsafe extern "system" fn hook_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let kb = &*(lp as *const KBDLLHOOKSTRUCT);
        let vk = kb.vkCode;
        let msg = wp as u32;
        if vk < 256 && !is_modifier(vk) {
            let held = &HELD[vk as usize];
            if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
                let mods = current_mods();
                let bind = BINDS
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .iter()
                    .find(|b| b.vk == vk && b.mods == mods)
                    .copied();
                if let Some(bind) = bind {
                    let repeated = held.swap(true, Ordering::Relaxed);
                    if !repeated || bind.repeat {
                        crate::post_to_ctrl(crate::WM_APP_HOTKEY, bind.id as usize, 0);
                    }
                    if !repeated && mods & (MOD_WIN | MOD_ALT) != 0 {
                        // Keep the Start menu / menu bar from opening when the
                        // modifier is released.
                        PostThreadMessageW(THREAD_ID.load(Ordering::Relaxed), WM_MASK, 0, 0);
                    }
                    return 1;
                }
            } else if (msg == WM_KEYUP || msg == WM_SYSKEYUP) && held.swap(false, Ordering::Relaxed) {
                return 1;
            }
        }
    }
    CallNextHookEx(null_mut(), code, wp, lp)
}
