//! Custom bar widgets (`custom/<name>`): each runs its command on its own
//! background thread every `interval` seconds and keeps the first line of
//! the output (or the `"text"` of JSON output, as in waybar) for the bar.
//!
//! [`configure`] is called whenever bar styles are built. It restarts the
//! threads only when the custom widget settings actually changed; threads of
//! an old configuration notice the new epoch and stop.

use crate::config::CustomModule;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

static EPOCH: AtomicU64 = AtomicU64::new(0);
static OUTPUTS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
static CONFIG: Mutex<Option<BTreeMap<String, CustomModule>>> = Mutex::new(None);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// (Re)starts the widget threads for `modules`, unless they already run.
pub fn configure(modules: &BTreeMap<String, CustomModule>) {
    {
        let mut config = lock(&CONFIG);
        if config.as_ref() == Some(modules) {
            return;
        }
        *config = Some(modules.clone());
    }
    let epoch = EPOCH.fetch_add(1, Ordering::SeqCst) + 1;
    lock(&OUTPUTS).get_or_insert_with(HashMap::new).clear();
    for (name, module) in modules {
        let (name, module) = (name.clone(), module.clone());
        let spawned = std::thread::Builder::new().name(format!("custom/{name}")).spawn(move || loop {
            let text = run(&module.exec);
            if EPOCH.load(Ordering::SeqCst) != epoch {
                return;
            }
            lock(&OUTPUTS).get_or_insert_with(HashMap::new).insert(name.clone(), text);
            if module.interval == 0 {
                return;
            }
            // Sleep in short steps so that a reload stops the thread soon.
            for _ in 0..module.interval * 4 {
                std::thread::sleep(Duration::from_millis(250));
                if EPOCH.load(Ordering::SeqCst) != epoch {
                    return;
                }
            }
        });
        if let Err(e) = spawned {
            warn!("a custom widget could not start: {e}");
        }
    }
}

/// The text a custom widget shows (empty: hidden).
pub fn output(name: &str) -> String {
    lock(&OUTPUTS).as_ref().and_then(|o| o.get(name).cloned()).unwrap_or_default()
}

/// Runs `command` without a console window and returns what to show.
fn run(command: &str) -> String {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let output = std::process::Command::new("cmd.exe")
        // UTF-8 output, so that icons and accents survive.
        .raw_arg(format!("/D /C chcp 65001>nul & {command}"))
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .output();
    match output {
        Ok(out) => parse_output(&String::from_utf8_lossy(&out.stdout)),
        Err(e) => {
            warn!("custom widget: running {command}: {e}");
            String::new()
        }
    }
}

/// The first non-empty line, or `"text"` of a JSON object.
pub fn parse_output(stdout: &str) -> String {
    let trimmed = stdout.trim();
    if trimmed.starts_with('{') {
        if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(trimmed) {
            return map.get("text").and_then(|t| t.as_str()).unwrap_or_default().trim().to_string();
        }
    }
    trimmed.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests {
    use super::parse_output;

    #[test]
    fn output_is_first_line_or_json_text() {
        assert_eq!(parse_output("\r\n  22°C sunny \r\nmore\r\n"), "22°C sunny");
        assert_eq!(parse_output(r#"{"text": "♪ 3", "tooltip": "x"}"#), "♪ 3");
        assert_eq!(parse_output(""), "");
        assert_eq!(parse_output("{not json"), "{not json");
    }
}
