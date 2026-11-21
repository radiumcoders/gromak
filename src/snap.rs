//! Turns Windows' own window snapping off while gromak runs.
//!
//! Windows Snap (drag a window to a screen edge, the Snap Layouts flyout at
//! the top of the screen and on the maximize button, Snap Assist offering
//! the other windows) fights the tiler: it resizes windows behind gromak's
//! back. This is the "Snap windows" switch in Settings → System →
//! Multitasking.
//!
//! The setting is system-wide and persists, so the original value is kept in
//! `%LOCALAPPDATA%\gromak\snap.txt` until it has been put back. If gromak
//! dies without cleaning up, the next start (or `gromak --restore`) restores
//! it from there.

use std::path::PathBuf;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::BOOL;
use windows_sys::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPIF_SENDCHANGE, SPIF_UPDATEINIFILE};

const SPI_GETWINARRANGEMENT: u32 = 0x0082;
const SPI_SETWINARRANGEMENT: u32 = 0x0083;

fn path() -> PathBuf {
    crate::log::data_dir().join("snap.txt")
}

/// Whether Windows Snap is currently on.
fn is_enabled() -> bool {
    let mut on: BOOL = 1;
    unsafe { SystemParametersInfoW(SPI_GETWINARRANGEMENT, 0, &mut on as *mut BOOL as *mut _, 0) };
    on != 0
}

fn set_enabled(on: bool) {
    let ok = unsafe {
        SystemParametersInfoW(SPI_SETWINARRANGEMENT, on as u32, null_mut(), SPIF_UPDATEINIFILE | SPIF_SENDCHANGE)
    };
    if ok == 0 {
        warn!("could not turn Windows Snap {}", if on { "on" } else { "off" });
    }
}

/// Turns Windows Snap off, remembering whether it was on. Safe to call
/// repeatedly: the value recorded first (before gromak touched it) wins.
pub fn disable() {
    let path = path();
    if !path.exists() {
        let was_on = is_enabled();
        let saved = std::fs::create_dir_all(crate::log::data_dir())
            .and_then(|_| std::fs::write(&path, if was_on { "1" } else { "0" }));
        if let Err(e) = saved {
            // Without a record we could not put it back after a crash.
            warn!("could not save the Windows Snap setting, leaving it alone: {e}");
            return;
        }
    }
    if is_enabled() {
        set_enabled(false);
        info!("Windows Snap turned off while gromak runs");
    }
}

/// Puts Windows Snap back the way it was before gromak turned it off.
/// Does nothing if gromak never touched it.
pub fn restore() {
    let path = path();
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let was_on = text.trim() != "0";
    if is_enabled() != was_on {
        set_enabled(was_on);
    }
    let _ = std::fs::remove_file(&path);
    if was_on {
        info!("Windows Snap turned back on");
    }
}
