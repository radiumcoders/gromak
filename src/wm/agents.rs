//! Agent workspaces: workspaces handed to AI agents through `gromak mcp`.
//!
//! An agent workspace behaves like any other while it is shown. While it is
//! not, its windows are *parked*: moved just past the right edge of the
//! desktop instead of hidden. Hidden windows are invisible to UI
//! Automation, so an agent could not drive them; parked windows keep
//! running, painting and answering accessibility queries, while staying out
//! of the user's sight. gromak never follows focus to a parked window: if
//! an agent's app grabs the foreground, focus goes straight back to the
//! user's window.

use super::{hidden, Wm};
use crate::agent::error;
use crate::config::WORKSPACES;
use crate::layout::Rect;
use crate::win::{self, Hwnd};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// How long after a launch a new window of the same program is routed to
/// the agent's workspace (for apps whose window belongs to another process,
/// like Store apps or a browser that is already running).
const LAUNCH_WINDOW: Duration = Duration::from_secs(20);
/// Parked windows sit this far past the right edge of the desktop.
const PARK_MARGIN: i32 = 200;

/// For this long after a launch, if the program has not shown a window of
/// its own yet, the next new window is taken to be its window. Store apps
/// (Calculator, the new Notepad, …) start through a stub whose window
/// belongs to another process, `ApplicationFrameHost.exe`.
const FIRST_WINDOW: Duration = Duration::from_secs(5);

/// A program an agent started, waiting for its windows.
pub(super) struct PendingLaunch {
    pid: u32,
    /// Lower-case executable name, e.g. "notepad.exe".
    exe: String,
    ws: usize,
    started: Instant,
    until: Instant,
    /// Windows routed to the agent for this launch, so that they go back
    /// there if they disappear and reappear (Store apps cloak while starting).
    routed: Vec<Hwnd>,
}

impl Wm {
    pub(super) fn is_agent(&self, ws: usize) -> bool {
        self.agents.contains_key(&ws)
    }

    /// The agent workspace a new window belongs to, if any: windows of a
    /// program an agent just launched, and dialogs owned by agent windows.
    pub(super) fn agent_ws_for(&mut self, info: &win::WindowInfo, h: Hwnd) -> Option<usize> {
        let now = Instant::now();
        self.pending.retain(|p| p.until > now);
        let exe = info.exe.to_lowercase();
        let pick = self
            .pending
            .iter()
            .position(|p| p.routed.contains(&h))
            .or_else(|| self.pending.iter().position(|p| p.pid == info.pid || (!p.exe.is_empty() && p.exe == exe)))
            .or_else(|| self.pending.iter().position(|p| p.routed.is_empty() && p.started.elapsed() < FIRST_WINDOW));
        if let Some(i) = pick {
            let p = &mut self.pending[i];
            if !p.routed.contains(&h) {
                p.routed.push(h);
            }
            let ws = p.ws;
            return Some(ws).filter(|ws| self.is_agent(*ws));
        }
        let owner = win::owner(h);
        self.windows.get(&owner).map(|m| m.ws).filter(|ws| self.is_agent(*ws))
    }

    /// Where parked windows go: past the right edge of every monitor.
    fn park_x(&self) -> i32 {
        self.monitors.iter().map(|m| m.info.rect.right()).max().unwrap_or(0) + PARK_MARGIN
    }

    /// Moves the windows of a hidden agent workspace off-screen, keeping
    /// their tiled sizes, and makes sure they are not hidden (hidden windows
    /// cannot be driven).
    pub(super) fn park_ws(&self, ws: usize) {
        let mi = (ws / WORKSPACES).min(self.monitors.len().saturating_sub(1));
        let Some(monitor) = self.monitors.get(mi).map(|m| m.info.rect) else { return };
        let dx = self.park_x() - monitor.x;
        let (area, gap) = (self.tiling_area(mi), self.gap(mi));
        let workspace = &self.workspaces[ws];
        let mut targets: Vec<(Hwnd, Rect)> =
            workspace.tree.layout(area, gap).into_iter().map(|(h, r)| (h, Rect { x: r.x + dx, ..r })).collect();
        for &f in &workspace.floating {
            let r = win::window_rect(f);
            let on_desktop = self.monitors.iter().any(|m| m.info.rect.intersects(&r));
            targets.push((f, if on_desktop { Rect { x: r.x + dx, ..r } } else { r }));
        }
        for (h, r) in targets {
            self.anims.borrow_mut().retain(|a| a.h != h);
            win::set_window_rect_now(h, r);
            self.placed.borrow_mut().insert(h, r);
            // Moved first, then shown, so it never flashes on screen.
            if hidden::contains(h) {
                hidden::show(h);
            }
        }
    }

    /// Takes a window off-screen for a workspace that is not shown: parked
    /// for agent workspaces, hidden for all others.
    pub(super) fn conceal(&self, h: Hwnd, ws: usize) {
        if self.is_agent(ws) {
            self.park_ws(ws);
        } else {
            hidden::hide(h);
        }
    }

    /// Puts an agent workspace's windows back on its monitor (it is being
    /// shown, or gromak is exiting).
    pub(super) fn unpark_ws(&self, ws: usize) {
        let mi = (ws / WORKSPACES).min(self.monitors.len().saturating_sub(1));
        let Some(area) = self.monitors.get(mi).map(|_| self.tiling_area(mi)) else { return };
        let gap = self.gap(mi);
        for (h, r) in self.workspaces[ws].tree.layout(area, gap) {
            self.anims.borrow_mut().retain(|a| a.h != h);
            win::set_window_rect_now(h, r);
            self.placed.borrow_mut().insert(h, r);
        }
        for &f in &self.workspaces[ws].floating {
            let r = win::window_rect(f);
            if !self.monitors.iter().any(|m| m.info.rect.intersects(&r)) {
                win::set_window_rect_now(f, area.centered(r.w.min(area.w), r.h.min(area.h)));
            }
        }
    }

    /// If the foreground window is a parked agent window, gives focus back
    /// to the user. Returns whether it did.
    pub(super) fn guard_focus(&mut self) -> bool {
        let fg = win::foreground();
        let parked =
            self.windows.get(&fg).map(|m| m.ws).is_some_and(|ws| self.is_agent(ws) && self.monitor_of_ws(ws).is_none());
        if parked {
            info!("agent window {fg:#x} took the foreground; giving it back to the user");
            self.refocus_user(fg);
        }
        parked
    }

    /// An agent's window took the foreground while its workspace is not
    /// shown: give focus back to the user's window.
    pub(super) fn refocus_user(&mut self, intruder: Hwnd) {
        match self.focused.filter(|&f| f != intruder && self.windows.contains_key(&f)) {
            Some(f) => self.focus_window(f),
            None => {
                let ws = self.current_ws();
                self.focus_in_ws(ws);
            }
        }
    }

    // ----- monitors by name ----------------------------------------------

    /// Names a monitor answers to: its number, "primary", and its position
    /// ("top", "bottom", "left", "right") when that tells it apart.
    fn monitor_names(&self, mi: usize) -> Vec<String> {
        let rect = |i: usize| self.monitors[i].info.rect;
        let r = rect(mi);
        let mut names = vec![(mi + 1).to_string()];
        if r.x == 0 && r.y == 0 {
            names.push("primary".into());
        }
        let n = self.monitors.len();
        if n > 1 {
            let (cx, cy) = r.center();
            let centers: Vec<(i32, i32)> = (0..n).map(|i| rect(i).center()).collect();
            let ys: Vec<i32> = centers.iter().map(|c| c.1).collect();
            let xs: Vec<i32> = centers.iter().map(|c| c.0).collect();
            let (min_y, max_y) = (*ys.iter().min().unwrap(), *ys.iter().max().unwrap());
            let (min_x, max_x) = (*xs.iter().min().unwrap(), *xs.iter().max().unwrap());
            let unique = |v: &[i32], x: i32| v.iter().filter(|&&o| o == x).count() == 1;
            if min_y != max_y && cy == min_y && unique(&ys, cy) {
                names.push("top".into());
            }
            if min_y != max_y && cy == max_y && unique(&ys, cy) {
                names.push("bottom".into());
            }
            if min_x != max_x && cx == min_x && unique(&xs, cx) {
                names.push("left".into());
            }
            if min_x != max_x && cx == max_x && unique(&xs, cx) {
                names.push("right".into());
            }
        }
        names
    }

    fn find_monitor(&self, spec: &str) -> Result<usize, String> {
        let spec = spec.trim().to_lowercase();
        let spec = spec.strip_suffix(" monitor").unwrap_or(&spec).trim();
        let spec = if spec == "main" { "primary" } else { spec };
        (0..self.monitors.len()).find(|&mi| self.monitor_names(mi).iter().any(|n| n == spec)).ok_or_else(|| {
            let known: Vec<String> = (0..self.monitors.len()).map(|mi| self.monitor_names(mi).join("/")).collect();
            format!("no monitor called \"{spec}\" (monitors: {})", known.join(", "))
        })
    }

    /// "monitor:number", e.g. "2:9" or "bottom:9" -> workspace index.
    fn parse_workspace(&self, value: &Value) -> Result<usize, String> {
        let text = value.as_str().ok_or("workspace must be a string like \"bottom:9\"")?;
        let (monitor, number) = text.rsplit_once(':').ok_or("workspace must look like \"monitor:number\"")?;
        let mi = self.find_monitor(monitor)?;
        let n: usize = number.trim().parse().map_err(|_| format!("bad workspace number in \"{text}\""))?;
        if !(1..=WORKSPACES).contains(&n) {
            return Err(format!("workspace numbers are 1-{WORKSPACES}"));
        }
        Ok(mi * WORKSPACES + n - 1)
    }

    fn workspace_id(&self, ws: usize) -> String {
        format!("{}:{}", ws / WORKSPACES + 1, ws % WORKSPACES + 1)
    }

    fn agent_ws(&self, value: &Value) -> Result<usize, String> {
        let ws = self.parse_workspace(value)?;
        if !self.is_agent(ws) {
            return Err(format!("workspace {} is not claimed by an agent; claim it first", self.workspace_id(ws)));
        }
        Ok(ws)
    }

    fn parse_window(&self, value: &Value) -> Result<Hwnd, String> {
        let text = value.as_str().ok_or("window must be an id like \"w-123456\"")?;
        let digits = text.strip_prefix("w-").unwrap_or(text);
        let h: Hwnd = digits.parse().map_err(|_| format!("bad window id \"{text}\""))?;
        if !self.windows.contains_key(&h) {
            return Err(format!("window {text} is not managed by gromak"));
        }
        Ok(h)
    }

    fn window_json(&self, h: Hwnd) -> Value {
        let info = win::WindowInfo::of(h);
        let m = &self.windows[&h];
        json!({
            "id": format!("w-{h}"),
            "title": info.title,
            "app": info.exe,
            "pid": info.pid,
            "workspace": self.workspace_id(m.ws),
            "floating": m.floating,
            "on_screen": self.monitor_of_ws(m.ws).is_some(),
        })
    }

    // ----- requests ------------------------------------------------------------

    /// Handles one request from `gromak mcp` (see [`crate::agent`]).
    pub fn on_agent(&mut self, request: &Value) -> Value {
        let result = match request["op"].as_str().unwrap_or("") {
            "monitors" => Ok(self.op_monitors()),
            "claim" => self.op_claim(request),
            "release" => self.op_release(request),
            "windows" => self.op_windows(request),
            "launch" => self.op_launch(request),
            "move_window" => self.op_move_window(request),
            "close_window" => self.op_close_window(request),
            other => Err(format!("unknown request \"{other}\"")),
        };
        self.refresh_bars();
        match result {
            Ok(mut value) => {
                value["ok"] = json!(true);
                value
            }
            Err(e) => error(e),
        }
    }

    fn op_monitors(&self) -> Value {
        let monitors: Vec<Value> = (0..self.monitors.len())
            .map(|mi| {
                let r = self.monitors[mi].info.rect;
                let base = mi * WORKSPACES;
                let workspaces: Vec<Value> = (0..WORKSPACES)
                    .filter(|n| !self.workspaces[base + n].is_empty() || self.is_agent(base + n))
                    .map(|n| {
                        json!({
                            "workspace": self.workspace_id(base + n),
                            "windows": self.workspaces[base + n].windows().len(),
                            "agent": self.agents.get(&(base + n)),
                        })
                    })
                    .collect();
                json!({
                    "monitor": mi + 1,
                    "names": self.monitor_names(mi),
                    "bounds": { "x": r.x, "y": r.y, "width": r.w, "height": r.h },
                    "shown_workspace": self.workspace_id(self.monitors[mi].ws),
                    "user_is_here": mi == self.active_monitor,
                    "busy_workspaces": workspaces,
                })
            })
            .collect();
        json!({ "monitors": monitors })
    }

    fn op_claim(&mut self, request: &Value) -> Result<Value, String> {
        let spec = request["monitor"].as_str().unwrap_or("primary");
        let mi = self.find_monitor(spec)?;
        let base = mi * WORKSPACES;
        let label = request["agent"].as_str().unwrap_or("agent").to_string();
        let ws = match request["workspace"].as_u64() {
            Some(n) if (1..=WORKSPACES as u64).contains(&n) => base + n as usize - 1,
            Some(n) => return Err(format!("workspace must be 1-{WORKSPACES}, got {n}")),
            // The highest-numbered empty workspace that is not on screen.
            None => (0..WORKSPACES)
                .rev()
                .map(|n| base + n)
                .find(|&w| self.workspaces[w].is_empty() && !self.is_agent(w) && self.monitors[mi].ws != w)
                .ok_or("this monitor has no free workspace")?,
        };
        if self.monitors[mi].ws == ws && !self.is_agent(ws) {
            return Err(format!(
                "workspace {} is the one the user is looking at on that monitor; pick another",
                self.workspace_id(ws)
            ));
        }
        self.agents.insert(ws, label.clone());
        info!("agent \"{label}\" claimed workspace {}", self.workspace_id(ws));
        if self.monitor_of_ws(ws).is_none() {
            self.park_ws(ws);
        }
        Ok(json!({
            "workspace": self.workspace_id(ws),
            "monitor": self.monitor_names(mi),
            "windows": self.workspaces[ws].windows().iter().map(|&h| self.window_json(h)).collect::<Vec<_>>(),
        }))
    }

    fn op_release(&mut self, request: &Value) -> Result<Value, String> {
        let ws = self.agent_ws(&request["workspace"])?;
        let label = self.agents.remove(&ws).unwrap_or_default();
        self.pending.retain(|p| p.ws != ws);
        // Its windows stay on the workspace, hidden the normal way now.
        if self.monitor_of_ws(ws).is_none() {
            for h in self.workspaces[ws].windows() {
                hidden::hide(h);
            }
        }
        info!("agent \"{label}\" released workspace {}", self.workspace_id(ws));
        Ok(json!({ "workspace": self.workspace_id(ws) }))
    }

    fn op_windows(&self, request: &Value) -> Result<Value, String> {
        let workspaces: Vec<usize> = match &request["workspace"] {
            Value::Null => self.agents.keys().copied().collect(),
            value => vec![self.parse_workspace(value)?],
        };
        let windows: Vec<Value> =
            workspaces.iter().flat_map(|&ws| self.workspaces[ws].windows()).map(|h| self.window_json(h)).collect();
        Ok(json!({ "windows": windows }))
    }

    fn op_launch(&mut self, request: &Value) -> Result<Value, String> {
        let ws = self.agent_ws(&request["workspace"])?;
        let command = request["command"].as_str().filter(|c| !c.trim().is_empty()).ok_or("command is required")?;
        let args: Vec<String> = request["args"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(quote_arg)).collect())
            .unwrap_or_default();
        let cwd = request["cwd"].as_str();
        let pid = win::shell_launch(command, &args.join(" "), cwd)?;
        let exe = std::path::Path::new(command)
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .filter(|n| !n.contains(':'))
            .map(|n| if n.ends_with(".exe") { n } else { format!("{n}.exe") })
            .unwrap_or_default();
        info!("agent launch on workspace {}: {command} (pid {pid})", self.workspace_id(ws));
        let now = Instant::now();
        self.pending.push(PendingLaunch { pid, exe, ws, started: now, until: now + LAUNCH_WINDOW, routed: Vec::new() });
        Ok(json!({ "workspace": self.workspace_id(ws), "pid": pid }))
    }

    fn op_move_window(&mut self, request: &Value) -> Result<Value, String> {
        let h = self.parse_window(&request["window"])?;
        let ws = self.agent_ws(&request["workspace"])?;
        let src = self.windows[&h].ws;
        if src != ws {
            let was_focused = self.focused == Some(h);
            let target = self.workspaces[ws].last_focus;
            self.transfer(h, ws, target);
            self.layout_ws(ws);
            if self.monitor_of_ws(ws).is_none() {
                self.conceal(h, ws);
            }
            self.after_removal(src, was_focused);
        }
        Ok(json!({ "window": self.window_json(h) }))
    }

    fn op_close_window(&mut self, request: &Value) -> Result<Value, String> {
        let h = self.parse_window(&request["window"])?;
        let ws = self.windows[&h].ws;
        if !self.is_agent(ws) {
            return Err("only windows on an agent workspace can be closed this way".into());
        }
        win::close(h);
        Ok(json!({ "closed": format!("w-{h}") }))
    }
}

/// Quotes a command-line argument for `ShellExecute` (Windows' rules).
fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

/// Moves windows left off-screen (by a crash while they were parked) back
/// onto the primary monitor. Run at startup and by `gromak --restore`.
pub fn rescue_offscreen() -> usize {
    let monitors = win::monitors();
    let Some(primary) = monitors.iter().find(|m| m.rect.x == 0 && m.rect.y == 0).or(monitors.first()) else {
        return 0;
    };
    let mut rescued = 0;
    for h in win::top_level_windows() {
        if !win::is_manageable(h) {
            continue;
        }
        let r = win::window_rect(h);
        if monitors.iter().any(|m| m.rect.intersects(&r)) {
            continue;
        }
        let area = primary.work;
        win::set_window_rect_now(h, area.centered(r.w.min(area.w), r.h.min(area.h)));
        rescued += 1;
    }
    rescued
}

#[cfg(test)]
mod tests {
    use super::quote_arg;

    #[test]
    fn quotes_arguments_like_windows() {
        assert_eq!(quote_arg("plain"), "plain");
        assert_eq!(quote_arg("two words"), "\"two words\"");
        assert_eq!(quote_arg(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote_arg(r"C:\dir with space\"), r#""C:\dir with space\\""#);
        assert_eq!(quote_arg(""), "\"\"");
    }
}
