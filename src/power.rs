//! Power and session actions for the launcher: lock, sleep, hibernate,
//! sign out, restart and shut down.

use crate::fuzzy;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerAction {
    Lock,
    Sleep,
    Hibernate,
    SignOut,
    Restart,
    ShutDown,
}

pub const ALL: [PowerAction; 6] = [
    PowerAction::Lock,
    PowerAction::Sleep,
    PowerAction::Hibernate,
    PowerAction::SignOut,
    PowerAction::Restart,
    PowerAction::ShutDown,
];

impl PowerAction {
    pub fn name(self) -> &'static str {
        match self {
            PowerAction::Lock => "Lock",
            PowerAction::Sleep => "Sleep",
            PowerAction::Hibernate => "Hibernate",
            PowerAction::SignOut => "Sign out",
            PowerAction::Restart => "Restart",
            PowerAction::ShutDown => "Shut down",
        }
    }

    /// Other words people type for it.
    fn keywords(self) -> &'static [&'static str] {
        match self {
            PowerAction::Lock => &["lock", "screen", "lockscreen"],
            PowerAction::Sleep => &["sleep", "suspend", "standby"],
            PowerAction::Hibernate => &["hibernate", "hibernation"],
            PowerAction::SignOut => &["sign", "logout", "logoff", "log", "signout"],
            PowerAction::Restart => &["restart", "reboot"],
            PowerAction::ShutDown => &["shutdown", "shut", "poweroff", "power", "off", "turn"],
        }
    }

    /// A small glyph shown in the launcher (drawn with the emoji font).
    pub fn glyph(self) -> &'static str {
        match self {
            PowerAction::Lock => "🔒",
            PowerAction::Sleep => "💤",
            PowerAction::Hibernate => "🧊",
            PowerAction::SignOut => "🚪",
            PowerAction::Restart => "🔁",
            PowerAction::ShutDown => "🔌",
        }
    }

    /// Actions that end the session or lose state need a second Enter.
    pub fn needs_confirmation(self) -> bool {
        matches!(self, PowerAction::Hibernate | PowerAction::SignOut | PowerAction::Restart | PowerAction::ShutDown)
    }

    /// The prompt shown after the first Enter.
    pub fn confirm_prompt(self) -> &'static str {
        match self {
            PowerAction::Hibernate => "Press Enter again to hibernate",
            PowerAction::SignOut => "Press Enter again to sign out",
            PowerAction::Restart => "Press Enter again to restart",
            PowerAction::ShutDown => "Press Enter again to shut down",
            _ => "",
        }
    }

    /// Carries out the action.
    pub fn run(self) -> Result<(), String> {
        use windows_sys::Win32::System::Power::SetSuspendState;
        use windows_sys::Win32::System::Shutdown::{ExitWindowsEx, LockWorkStation, EWX_LOGOFF};
        let ok = |b: bool, what: &str| if b { Ok(()) } else { Err(format!("{what} failed")) };
        match self {
            PowerAction::Lock => ok(unsafe { LockWorkStation() } != 0, "LockWorkStation"),
            PowerAction::Sleep => ok(unsafe { SetSuspendState(0, 0, 0) } != 0, "sleep"),
            PowerAction::Hibernate => ok(unsafe { SetSuspendState(1, 0, 0) } != 0, "hibernate"),
            PowerAction::SignOut => ok(unsafe { ExitWindowsEx(EWX_LOGOFF, 0) } != 0, "sign out"),
            // shutdown.exe takes care of the shutdown privilege for us.
            PowerAction::Restart => run_shutdown("/r"),
            PowerAction::ShutDown => run_shutdown("/s"),
        }
    }
}

fn run_shutdown(flag: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("shutdown.exe")
        .args([flag, "/t", "0"])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Power actions matching `query` (at least two characters), best first.
pub fn search(query: &str) -> Vec<(PowerAction, fuzzy::Match)> {
    let q = query.trim().to_lowercase();
    if q.chars().count() < 2 {
        return Vec::new();
    }
    let mut results: Vec<(i32, PowerAction, fuzzy::Match)> = ALL
        .iter()
        .filter_map(|&a| {
            let name = a.name().to_lowercase();
            let compact: String = q.chars().filter(|c| !c.is_whitespace()).collect();
            let relevant = name.contains(&q)
                || name.split_whitespace().any(|w| w.starts_with(&q))
                || a.keywords().iter().any(|k| k.starts_with(&compact));
            if !relevant {
                return None;
            }
            let m = fuzzy::score(&q, a.name()).unwrap_or(fuzzy::Match { score: 0, positions: Vec::new() });
            let exact = a.keywords().iter().any(|k| *k == compact);
            Some((m.score + if exact { 40 } else { 0 }, a, m))
        })
        .collect();
    results.sort_by_key(|r| std::cmp::Reverse(r.0));
    results.into_iter().map(|(_, a, m)| (a, m)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first(q: &str) -> Option<PowerAction> {
        search(q).first().map(|(a, _)| *a)
    }

    #[test]
    fn finds_actions_by_name_and_keyword() {
        assert_eq!(first("shut"), Some(PowerAction::ShutDown));
        assert_eq!(first("shutdown"), Some(PowerAction::ShutDown));
        assert_eq!(first("power off"), Some(PowerAction::ShutDown));
        assert_eq!(first("reboot"), Some(PowerAction::Restart));
        assert_eq!(first("restart"), Some(PowerAction::Restart));
        assert_eq!(first("logout"), Some(PowerAction::SignOut));
        assert_eq!(first("suspend"), Some(PowerAction::Sleep));
        assert_eq!(first("hiber"), Some(PowerAction::Hibernate));
        assert_eq!(first("lock"), Some(PowerAction::Lock));
    }

    #[test]
    fn ignores_short_and_unrelated_queries() {
        assert!(search("s").is_empty());
        assert!(search("firefox").is_empty());
        assert!(search("terminal").is_empty());
    }

    #[test]
    fn only_destructive_actions_need_confirmation() {
        assert!(!PowerAction::Lock.needs_confirmation());
        assert!(!PowerAction::Sleep.needs_confirmation());
        assert!(PowerAction::ShutDown.needs_confirmation());
        assert!(PowerAction::Restart.needs_confirmation());
    }
}
