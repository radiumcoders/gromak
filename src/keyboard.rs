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
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::Mutex;
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::SystemInformation::GetTickCount64;
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
/// Asks the hook thread to replay a swallowed Win release behind a mask key.
const WM_SUPPRESS_WIN: u32 = WM_APP + 52;

/// What a lone press of the Win key does (`general.start_menu`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartMenu {
    /// Windows' default: a tap opens Start.
    Tap,
    /// A tap does nothing; two taps in quick succession open Start.
    DoubleTap,
    /// Taps never open Start.
    Never,
}

/// Two Win taps closer together than this count as a double tap.
const DOUBLE_TAP_MS: u64 = 400;

static START_MENU: AtomicU8 = AtomicU8::new(0);
/// The Win key is down.
static WIN_DOWN: AtomicBool = AtomicBool::new(false);
/// Something else was pressed (or the mouse used) while Win was down, so the
/// release is part of a combination, not a lone tap.
static WIN_COMBO: AtomicBool = AtomicBool::new(false);
/// When the last suppressed lone tap happened (GetTickCount64).
static LAST_TAP: AtomicU64 = AtomicU64::new(0);

pub fn set_start_menu(mode: StartMenu) {
    START_MENU.store(mode as u8, Ordering::Relaxed);
}

/// Marks the current Win press as a combination (used by the mouse hook for
/// Win+wheel and Win+drag).
pub fn note_win_combo() {
    if WIN_DOWN.load(Ordering::Relaxed) {
        WIN_COMBO.store(true, Ordering::Relaxed);
    }
}

fn is_win_key(vk: u32) -> bool {
    vk == VK_LWIN as u32 || vk == VK_RWIN as u32
}

/// On a Win release: should this lone tap be kept from opening Start?
fn suppress_lone_win_tap() -> bool {
    if WIN_COMBO.load(Ordering::Relaxed) {
        return false;
    }
    match START_MENU.load(Ordering::Relaxed) {
        x if x == StartMenu::Never as u8 => true,
        x if x == StartMenu::DoubleTap as u8 => {
            let now = unsafe { GetTickCount64() };
            let last = LAST_TAP.swap(now, Ordering::Relaxed);
            if now.saturating_sub(last) <= DOUBLE_TAP_MS {
                // Second tap: let it through, and do not chain a third.
                LAST_TAP.store(0, Ordering::Relaxed);
                false
            } else {
                true
            }
        }
        _ => false,
    }
}

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
            match msg.message {
                WM_MASK => crate::win::mask_menu_key(),
                WM_SUPPRESS_WIN => crate::win::replay_masked_key_up(msg.wParam as u16),
                _ => {}
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

pub fn current_mods() -> u32 {
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
        let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        // Our own injected keys (the Win-tap suppression) pass untouched.
        if kb.dwExtraInfo == crate::win::INJECTED_MARK {
            return CallNextHookEx(null_mut(), code, wp, lp);
        }
        if is_win_key(vk) {
            if down {
                if !WIN_DOWN.swap(true, Ordering::Relaxed) {
                    WIN_COMBO.store(false, Ordering::Relaxed);
                }
            } else {
                WIN_DOWN.store(false, Ordering::Relaxed);
                if suppress_lone_win_tap() {
                    // Swallow this release; the hook thread replays it behind
                    // an unassigned key so the shell does not see a lone tap.
                    PostThreadMessageW(THREAD_ID.load(Ordering::Relaxed), WM_SUPPRESS_WIN, vk as usize, 0);
                    return 1;
                }
            }
        } else if down && WIN_DOWN.load(Ordering::Relaxed) {
            WIN_COMBO.store(true, Ordering::Relaxed);
        }
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
                // Not a binding: whatever we thought was held is not (a
                // missed key-up would otherwise eat this key's release).
                held.store(false, Ordering::Relaxed);
            } else if (msg == WM_KEYUP || msg == WM_SYSKEYUP) && held.swap(false, Ordering::Relaxed) {
                return 1;
            }
        }
    }
    CallNextHookEx(null_mut(), code, wp, lp)
}
