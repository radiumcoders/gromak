#[macro_use]
mod log;
mod bar;
mod config;
mod layout;
mod win;

use std::sync::atomic::{AtomicIsize, Ordering};
use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// A bar asks the window manager to act: `wparam` is a `BAR_*` kind.
pub const WM_APP_BAR: u32 = WM_APP + 3;
/// `lparam` is the 0-based workspace index that was clicked.
pub const BAR_WORKSPACE: usize = 0;
/// `lparam` is +1 / -1 from scrolling over the bar.
pub const BAR_SCROLL: usize = 1;

/// The hidden control window that receives hotkeys and internal messages.
static CTRL_HWND: AtomicIsize = AtomicIsize::new(0);

/// Posts a message to the control window, safe to call from any callback.
pub fn post_to_ctrl(msg: u32, wparam: usize, lparam: isize) {
    let ctrl = CTRL_HWND.load(Ordering::Relaxed);
    if ctrl != 0 {
        unsafe { PostMessageW(ctrl as _, msg, wparam, lparam) };
    }
}

fn main() {
    println!("tessera: a minimal tiling window manager for Windows");
}
