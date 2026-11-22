//! System-wide Windows settings that gromak switches off while it runs:
//!
//! - **Snap** (drag a window to a screen edge, the Snap Layouts flyout,
//!   Snap Assist), which fights the tiler by resizing windows behind
//!   gromak's back. This is the "Snap windows" switch in Settings → System →
//!   Multitasking.
//! - **Window shadows**, the "Show shadows under windows" visual effect, for
//!   a flat look between tiles.
//!
//! These settings are system-wide and persist, so each original value is
//! kept in a file in `%LOCALAPPDATA%\gromak\` until it has been put back. If
//! gromak dies without cleaning up, the next start (or `gromak --restore`)
//! restores it from there.

use std::path::PathBuf;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::BOOL;
use windows_sys::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPIF_SENDCHANGE, SPIF_UPDATEINIFILE};

/// An on/off setting read and written with `SystemParametersInfo`.
pub struct Setting {
    name: &'static str,
    /// Where the original value is recorded.
    file: &'static str,
    get: u32,
    set: u32,
    /// The new value goes in `pvParam` (as a pointer-sized BOOL) rather than
    /// `uiParam`.
    value_in_pv: bool,
}

pub const SNAP: Setting = Setting {
    name: "Windows Snap",
    file: "snap.txt",
    get: 0x0082, // SPI_GETWINARRANGEMENT
    set: 0x0083, // SPI_SETWINARRANGEMENT
    value_in_pv: false,
};

pub const SHADOWS: Setting = Setting {
    name: "Window shadows",
    file: "shadows.txt",
    get: 0x1024, // SPI_GETDROPSHADOW
    set: 0x1025, // SPI_SETDROPSHADOW
    value_in_pv: true,
};

/// Puts every setting back the way it was before gromak changed it.
pub fn restore_all() {
    SNAP.restore();
    SHADOWS.restore();
}

impl Setting {
    fn path(&self) -> PathBuf {
        crate::log::data_dir().join(self.file)
    }

    fn is_enabled(&self) -> bool {
        let mut on: BOOL = 1;
        unsafe { SystemParametersInfoW(self.get, 0, &mut on as *mut BOOL as *mut _, 0) };
        on != 0
    }

    fn set_enabled(&self, on: bool) {
        let (ui, pv) = if self.value_in_pv { (0, on as usize as *mut _) } else { (on as u32, null_mut()) };
        let ok = unsafe { SystemParametersInfoW(self.set, ui, pv, SPIF_UPDATEINIFILE | SPIF_SENDCHANGE) };
        if ok == 0 {
            warn!("could not turn {} {}", self.name, if on { "on" } else { "off" });
        }
    }

    /// Turns the setting off, remembering whether it was on. Safe to call
    /// repeatedly: the value recorded first (before gromak touched it) wins.
    pub fn disable(&self) {
        let path = self.path();
        if !path.exists() {
            let was_on = self.is_enabled();
            let saved = std::fs::create_dir_all(crate::log::data_dir())
                .and_then(|_| std::fs::write(&path, if was_on { "1" } else { "0" }));
            if let Err(e) = saved {
                // Without a record we could not put it back after a crash.
                warn!("could not save the {} setting, leaving it alone: {e}", self.name);
                return;
            }
        }
        if self.is_enabled() {
            self.set_enabled(false);
            info!("{} turned off while gromak runs", self.name);
        }
    }

    /// Puts the setting back the way it was before gromak turned it off.
    /// Does nothing if gromak never touched it.
    pub fn restore(&self) {
        let path = self.path();
        let Ok(text) = std::fs::read_to_string(&path) else { return };
        let was_on = text.trim() != "0";
        if self.is_enabled() != was_on {
            self.set_enabled(was_on);
        }
        let _ = std::fs::remove_file(&path);
        if was_on {
            info!("{} turned back on", self.name);
        }
    }
}
