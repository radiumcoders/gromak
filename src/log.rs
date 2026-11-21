//! A tiny file + stderr logger.
//!
//! Release builds have no console, so everything also goes to
//! `%LOCALAPPDATA%\tessera\tessera.log` (truncated on every start).

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static FILE: Mutex<Option<File>> = Mutex::new(None);

/// `%LOCALAPPDATA%\tessera`, where runtime state and logs live.
pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("TEMP"))
        .unwrap_or_else(|| ".".into());
    PathBuf::from(base).join("tessera")
}

pub fn init(path: &Path) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(file) = File::create(path) {
        *FILE.lock().unwrap_or_else(|e| e.into_inner()) = Some(file);
    }
}

pub fn write(level: &str, args: std::fmt::Arguments) {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    let line = format!("{h:02}:{m:02}:{s:02}Z {level:<5} {args}\n");
    let _ = std::io::stderr().write_all(line.as_bytes());
    if let Some(file) = FILE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        let _ = file.write_all(line.as_bytes());
    }
}

#[macro_export]
macro_rules! info {
    ($($t:tt)*) => { $crate::log::write("INFO", format_args!($($t)*)) };
}

#[macro_export]
macro_rules! warn {
    ($($t:tt)*) => { $crate::log::write("WARN", format_args!($($t)*)) };
}

#[macro_export]
macro_rules! error {
    ($($t:tt)*) => { $crate::log::write("ERROR", format_args!($($t)*)) };
}
