//! The window manager: monitors, workspaces, window events and dispatchers.
//!
//! Everything here runs on the main thread, driven by the message loop in
//! `main.rs`. Win32 callbacks only *post* messages, so `Wm` is never
//! re-entered and needs no locking.
//!
//! Workspaces are global (1–10). Each monitor shows exactly one; windows of
//! workspaces that are not shown anywhere are hidden with `SW_HIDE`. Hidden
//! windows are tracked in [`hidden`] and persisted to disk so they can be
//! restored after a crash.

use crate::bar::{Bar, BarState, Style, WsItem};
use crate::config::{
    self, Bind, Command, Config, FsMode, ThemeMode, ThemeSource, WsTarget, MOD_ALT, MOD_WIN, WORKSPACES,
};
use crate::keyboard::{self, HookBind};
use crate::launcher;
use crate::layout::{self, Dir, Dwindle, Rect};
use crate::mouse;
use crate::theme::{self, Palette};
use crate::win::{self, Hwnd};
use std::collections::{HashMap, HashSet};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, MOD_NOREPEAT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    PostQuitMessage, EVENT_OBJECT_CLOAKED, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, EVENT_OBJECT_NAMECHANGE,
    EVENT_OBJECT_SHOW, EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND,
    EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MOVESIZEEND,
};

pub use hidden::{flush as flush_hidden, recover as recover_hidden, restore_all as restore_hidden};

/// Fraction of a split that one resize step moves the divider.
const RESIZE_STEP: f32 = 0.05;
/// Logical pixels a floating window moves or grows per step.
const FLOAT_STEP: i32 = 50;

#[derive(Default)]
struct Workspace {
    tree: Dwindle,
    floating: Vec<Hwnd>,
    fullscreen: Option<(Hwnd, FsMode)>,
    /// Where a floating window was before it went fullscreen.
    restore_rect: Option<Rect>,
    last_focus: Option<Hwnd>,
}

impl Workspace {
    fn windows(&self) -> Vec<Hwnd> {
        let mut all = self.tree.windows();
        all.extend(&self.floating);
        all
    }

    fn is_empty(&self) -> bool {
        self.tree.is_empty() && self.floating.is_empty()
    }

    /// Removes a window from this workspace's bookkeeping.
    fn detach(&mut self, h: Hwnd) {
        self.tree.remove(h);
        self.floating.retain(|&f| f != h);
        if self.fullscreen.is_some_and(|(f, _)| f == h) {
            self.fullscreen = None;
            self.restore_rect = None;
        }
        if self.last_focus == Some(h) {
            self.last_focus = None;
        }
    }
}

struct Monitor {
    info: win::MonitorInfo,
    /// The workspace shown on this monitor.
    ws: usize,
    bar: Option<Bar>,
}

struct Managed {
    ws: usize,
    floating: bool,
}

pub struct Wm {
    cfg: Config,
    ctrl: Hwnd,
    monitors: Vec<Monitor>,
    workspaces: Vec<Workspace>,
    windows: HashMap<Hwnd, Managed>,
    /// Windows that matched an `ignore` rule or cannot be managed (elevated).
    ignored: HashSet<Hwnd>,
    focused: Option<Hwnd>,
    /// `HICON` of the focused window, shown in the bar.
    focused_icon: isize,
    active_monitor: usize,
    binds: Vec<Bind>,
    active_border: u32,
    inactive_border: u32,
    /// The current colours (from the wallpaper, accent or config).
    palette: Palette,
    /// Fingerprint of the inputs `palette` was derived from.
    theme_signature: String,
    elevated: bool,
}

impl Wm {
    pub fn new(cfg: Config, ctrl: Hwnd) -> Self {
        let mut wm = Wm {
            cfg,
            ctrl,
            monitors: Vec::new(),
            workspaces: (0..WORKSPACES).map(|_| Workspace::default()).collect(),
            windows: HashMap::new(),
            ignored: HashSet::new(),
            focused: None,
            focused_icon: 0,
            active_monitor: 0,
            binds: Vec::new(),
            active_border: 0,
            inactive_border: 0,
            palette: theme::palette_from_seed([0x8a, 0xad, 0xf4], false),
            theme_signature: String::new(),
            elevated: win::self_elevated(),
        };
        wm.load_colors();
        wm
    }

    /// Detects monitors, adopts existing windows and registers hotkeys.
    pub fn start(&mut self) {
        self.apply_taskbar();
        self.detect_monitors();
        let foreground = win::foreground();
        // Bottom of the z-order first, so recently used windows split last.
        for h in win::top_level_windows().into_iter().rev() {
            let ws = self.monitors[self.monitor_index(win::monitor_of_window(h))].ws;
            self.manage(h, Some(ws));
        }
        if self.windows.contains_key(&foreground) {
            self.set_focused(Some(foreground));
        }
        self.layout_all();
        self.register_hotkeys();
        mouse::set_enabled(self.cfg.general.focus_follows_mouse);
        self.refresh_bars();
        info!(
            "managing {} windows on {} monitor(s){}",
            self.windows.len(),
            self.monitors.len(),
            if self.elevated { " (elevated)" } else { "" }
        );
    }

    /// Restores everything to how Windows had it.
    pub fn shutdown(&mut self) {
        self.unregister_hotkeys();
        hidden::restore_all();
        for &h in self.windows.keys() {
            win::set_border_color(h, win::BORDER_DEFAULT);
        }
        self.monitors.clear();
    }

    /// Shows a transient message on every bar.
    pub fn notify(&self, text: &str) {
        for bar in self.monitors.iter().filter_map(|m| m.bar.as_ref()) {
            bar.notify(text);
        }
    }

    // ----- configuration -------------------------------------------------

    /// Works out the colours from the `[theme]` settings.
    fn compute_palette(&self) -> Palette {
        let light = match self.cfg.theme.mode {
            ThemeMode::Auto => theme::system_light_mode(),
            ThemeMode::Dark => false,
            ThemeMode::Light => true,
        };
        let from_accent = || theme::palette_from_seed(theme::windows_accent().unwrap_or([0x8a, 0xad, 0xf4]), light);
        match self.cfg.theme.source {
            ThemeSource::Manual => {
                let color = |s: &str| config::parse_color(s).unwrap_or(0);
                let (bar, deco) = (&self.cfg.bar, &self.cfg.decoration);
                Palette {
                    background: color(&bar.background),
                    foreground: color(&bar.foreground),
                    accent: color(&bar.accent),
                    muted: color(&bar.muted),
                    border_active: color(&deco.active_border),
                    border_inactive: color(&deco.inactive_border),
                }
            }
            ThemeSource::Accent => from_accent(),
            ThemeSource::Wallpaper => {
                theme::palette_from_pixels(&theme::wallpaper_pixels(), light).unwrap_or_else(from_accent)
            }
        }
    }

    fn load_colors(&mut self) {
        self.theme_signature = theme::signature();
        self.palette = self.compute_palette();
        self.active_border = self.palette.border_active;
        self.inactive_border = self.palette.border_inactive;
        info!(
            "theme ({:?}): accent {}, background {}",
            self.cfg.theme.source,
            theme::hex(self.palette.accent),
            theme::hex(self.palette.background)
        );
    }

    /// Re-derives the colours if the wallpaper, accent colour or light/dark
    /// mode changed since the last time. Cheap when nothing changed.
    pub fn on_theme_check(&mut self) {
        if self.cfg.theme.source == ThemeSource::Manual || theme::signature() == self.theme_signature {
            return;
        }
        self.load_colors();
        self.apply_borders();
        for m in &self.monitors {
            if let Some(bar) = &m.bar {
                bar.set_style(Style::new(&self.cfg.bar, &self.palette, m.info.scale));
            }
        }
    }

    fn apply_borders(&self) {
        for &h in self.windows.keys() {
            let color = match (self.cfg.decoration.borders, Some(h) == self.focused) {
                (false, _) => win::BORDER_DEFAULT,
                (true, true) => self.active_border,
                (true, false) => self.inactive_border,
            };
            win::set_border_color(h, color);
        }
    }

    fn apply_taskbar(&self) {
        let hide = self.cfg.general.hide_taskbar;
        for taskbar in win::taskbars() {
            if hide {
                hidden::hide(taskbar);
            } else if hidden::contains(taskbar) {
                hidden::show(taskbar);
            }
        }
    }

    fn register_hotkeys(&mut self) {
        let (binds, errors) = self.cfg.compile_binds();
        for e in &errors {
            warn!("config: {e}");
        }
        self.binds = binds;
        // Win-key bindings always go through the keyboard hook, because
        // Windows reserves most of them. Everything else uses RegisterHotKey
        // (which also works while an elevated window has focus) and falls
        // back to the hook if another app already owns the combination.
        let mut hooked = Vec::new();
        for (i, bind) in self.binds.iter().enumerate() {
            let id = i as i32 + 1;
            let (mods, vk) = (bind.hotkey.mods, bind.hotkey.vk);
            let use_hook =
                mods & MOD_WIN != 0 || unsafe { RegisterHotKey(self.ctrl as _, id, mods | MOD_NOREPEAT, vk) } == 0;
            if use_hook {
                if mods & MOD_WIN != 0 && vk == 'L' as u32 {
                    warn!("binds.\"{}\": win+l always locks Windows and cannot be overridden", bind.keys);
                }
                hooked.push(HookBind { mods, vk, id, repeat: matches!(bind.command, Command::Resize(_)) });
            }
        }
        if !hooked.is_empty() {
            info!("{} binding(s) handled by the keyboard hook", hooked.len());
        }
        keyboard::set_binds(hooked);
    }

    fn unregister_hotkeys(&mut self) {
        for i in 0..self.binds.len() {
            unsafe { UnregisterHotKey(self.ctrl as _, i as i32 + 1) };
        }
        keyboard::set_binds(Vec::new());
        self.binds.clear();
    }

    fn reload(&mut self) {
        match config::load() {
            Ok(cfg) => {
                self.unregister_hotkeys();
                self.cfg = cfg;
                self.load_colors();
                self.apply_taskbar();
                self.detect_monitors();
                self.register_hotkeys();
                mouse::set_enabled(self.cfg.general.focus_follows_mouse);
                self.apply_borders();
                self.layout_all();
                self.refresh_bars();
                info!("config reloaded");
                self.notify("config reloaded");
            }
            Err(e) => {
                error!("config: {e}");
                self.notify(&format!("config error: {e}"));
            }
        }
    }

    // ----- monitors and geometry ----------------------------------------

    /// (Re)builds the monitor list, keeping each monitor's workspace where
    /// possible, and recreates the bars.
    fn detect_monitors(&mut self) {
        let previous: Vec<usize> = self.monitors.iter().map(|m| m.ws).collect();
        self.monitors.clear();
        let mut shown: Vec<usize> = Vec::new();
        for (i, info) in win::monitors().into_iter().enumerate() {
            let preferred = previous.get(i).copied().unwrap_or(i % WORKSPACES);
            let ws = if shown.contains(&preferred) {
                (0..WORKSPACES).find(|w| !shown.contains(w)).unwrap_or(0)
            } else {
                preferred
            };
            shown.push(ws);
            self.monitors.push(Monitor { info, ws, bar: None });
        }
        if self.monitors.is_empty() {
            error!("no monitors found");
            return;
        }
        // Workspaces that lost their monitor disappear from view.
        for ws in previous.into_iter().filter(|ws| !shown.contains(ws)) {
            for h in self.workspaces[ws].windows() {
                hidden::hide(h);
            }
        }
        self.active_monitor = self.active_monitor.min(self.monitors.len() - 1);
        for i in 0..self.monitors.len() {
            let bar = if self.cfg.bar.enabled {
                let style = Style::new(&self.cfg.bar, &self.palette, self.monitors[i].info.scale);
                Bar::new(self.bar_rect(i), style)
            } else {
                None
            };
            self.monitors[i].bar = bar;
        }
    }

    pub fn on_display_change(&mut self) {
        info!("display configuration changed");
        self.detect_monitors();
        self.layout_all();
        self.refresh_bars();
    }

    fn scaled(&self, mi: usize, v: i32) -> i32 {
        (v as f32 * self.monitors[mi].info.scale).round() as i32
    }

    /// The part of the monitor tessera lays out into, before gaps.
    fn base_rect(&self, mi: usize) -> Rect {
        let info = &self.monitors[mi].info;
        if self.cfg.general.hide_taskbar {
            info.rect
        } else {
            info.work
        }
    }

    fn bar_rect(&self, mi: usize) -> Rect {
        let base = self.base_rect(mi);
        let go = self.scaled(mi, self.cfg.general.gaps_out);
        Rect::new(base.x + go, base.y + go, base.w - 2 * go, self.scaled(mi, self.cfg.bar.height))
    }

    fn tiling_area(&self, mi: usize) -> Rect {
        let mut area = self.base_rect(mi);
        let go = self.scaled(mi, self.cfg.general.gaps_out);
        if self.cfg.bar.enabled {
            let top = self.scaled(mi, self.cfg.bar.height) + go;
            area.y += top;
            area.h -= top;
        }
        area.inset(go)
    }

    fn gap(&self, mi: usize) -> i32 {
        self.scaled(mi, self.cfg.general.gaps_in)
    }

    fn monitor_index(&self, handle: isize) -> usize {
        self.monitors.iter().position(|m| m.info.handle == handle).unwrap_or(0)
    }

    fn monitor_of_ws(&self, ws: usize) -> Option<usize> {
        self.monitors.iter().position(|m| m.ws == ws)
    }

    fn monitor_in_dir(&self, from: usize, dir: Dir) -> Option<usize> {
        let rects: Vec<(isize, Rect)> =
            self.monitors.iter().enumerate().map(|(i, m)| (i as isize, m.info.rect)).collect();
        layout::neighbor(&rects, from as isize, dir).map(|i| i as usize)
    }

    fn current_ws(&self) -> usize {
        self.monitors[self.active_monitor].ws
    }

    fn resolve(&self, target: WsTarget) -> usize {
        match target {
            WsTarget::Index(i) => i.min(WORKSPACES - 1),
            WsTarget::Relative(d) => (self.current_ws() as i32 + d).rem_euclid(WORKSPACES as i32) as usize,
        }
    }

    // ----- window bookkeeping -------------------------------------------

    /// Starts managing `h` if it is a regular window. Returns whether it did.
    fn manage(&mut self, h: Hwnd, ws_hint: Option<usize>) -> bool {
        if self.windows.contains_key(&h) || self.ignored.contains(&h) || !win::is_manageable(h) {
            return false;
        }
        let info = win::WindowInfo::of(h);
        if !self.elevated && win::process_elevated(info.pid) {
            info!(
                "ignoring elevated window \"{}\" ({}); run tessera as administrator to manage it",
                info.title, info.exe
            );
            self.ignored.insert(h);
            return false;
        }
        let mut floating = info.has_owner || !info.resizable;
        let mut ws = ws_hint;
        for rule in self.cfg.rules.iter().filter(|r| r.matches(&info.exe, &info.class, &info.title)) {
            if rule.ignore {
                self.ignored.insert(h);
                return false;
            }
            floating |= rule.float;
            if let Some(n) = rule.workspace {
                ws = Some(n - 1);
            }
        }
        let ws = ws.unwrap_or_else(|| self.current_ws());
        self.windows.insert(h, Managed { ws, floating });
        if floating {
            self.workspaces[ws].floating.push(h);
        } else {
            let target = self.workspaces[ws].last_focus;
            self.tile_into(h, ws, target);
        }
        if self.cfg.decoration.borders {
            win::set_border_color(h, self.inactive_border);
        }
        if self.monitor_of_ws(ws).is_none() {
            hidden::hide(h);
        }
        info!(
            "manage {h:#x} {} [{}] \"{}\" -> workspace {}{}",
            info.exe,
            info.class,
            info.title,
            ws + 1,
            if floating { " (floating)" } else { "" }
        );
        true
    }

    /// Stops managing `h`. Returns the workspace it was on.
    fn unmanage(&mut self, h: Hwnd) -> Option<usize> {
        let m = self.windows.remove(&h)?;
        self.workspaces[m.ws].detach(h);
        hidden::forget(h);
        if self.focused == Some(h) {
            self.focused = None;
        }
        info!("unmanage {h:#x}");
        Some(m.ws)
    }

    /// Inserts a tiled window into a workspace's tree, splitting `target`.
    fn tile_into(&mut self, h: Hwnd, ws: usize, target: Option<Hwnd>) {
        let mi = self.monitor_of_ws(ws).unwrap_or(self.active_monitor);
        let (area, gap) = (self.tiling_area(mi), self.gap(mi));
        self.workspaces[ws].tree.insert(h, target.filter(|&t| t != h), area, gap);
    }

    /// Moves a managed window to another workspace's bookkeeping.
    fn transfer(&mut self, h: Hwnd, to: usize, target: Option<Hwnd>) {
        let Some(m) = self.windows.get_mut(&h) else { return };
        let (from, floating) = (m.ws, m.floating);
        m.ws = to;
        self.workspaces[from].detach(h);
        if floating {
            self.workspaces[to].floating.push(h);
            // Keep floating windows on the monitor that shows their workspace.
            if let Some(mi) = self.monitor_of_ws(to) {
                if win::monitor_of_window(h) != self.monitors[mi].info.handle {
                    let area = self.tiling_area(mi);
                    let r = win::window_rect(h);
                    win::set_window_rect(h, area.centered(r.w.min(area.w), r.h.min(area.h)));
                }
            }
        } else {
            self.tile_into(h, to, target);
        }
        self.workspaces[to].last_focus = Some(h);
    }

    /// Applies the layout of a workspace if it is visible.
    fn layout_ws(&self, ws: usize) {
        let Some(mi) = self.monitor_of_ws(ws) else { return };
        let (area, gap) = (self.tiling_area(mi), self.gap(mi));
        let workspace = &self.workspaces[ws];
        let fullscreen = workspace.fullscreen;
        for (h, r) in workspace.tree.layout(area, gap) {
            if fullscreen.is_some_and(|(f, _)| f == h) || win::is_app_fullscreen(h) {
                continue;
            }
            win::set_window_rect(h, r);
        }
        if let Some((h, mode)) = fullscreen {
            let r = match mode {
                FsMode::Fullscreen => self.monitors[mi].info.rect,
                FsMode::Maximized => area,
            };
            win::set_window_rect(h, r);
            win::raise(h);
        }
        if let Some(bar) = &self.monitors[mi].bar {
            bar.set_visible(!matches!(fullscreen, Some((_, FsMode::Fullscreen))));
        }
    }

    fn layout_all(&self) {
        for m in &self.monitors {
            self.layout_ws(m.ws);
        }
    }

    /// Tiled rectangles plus the real rectangles of floating windows.
    fn ws_rects(&self, ws: usize) -> Vec<(Hwnd, Rect)> {
        let mi = self.monitor_of_ws(ws).unwrap_or(self.active_monitor);
        let mut rects = self.workspaces[ws].tree.layout(self.tiling_area(mi), self.gap(mi));
        rects.extend(self.workspaces[ws].floating.iter().map(|&f| (f, win::window_rect(f))));
        rects
    }

    fn set_focused(&mut self, new: Option<Hwnd>) {
        let old = std::mem::replace(&mut self.focused, new);
        if old != new {
            self.focused_icon = new.map(win::window_icon).unwrap_or(0);
        }
        if self.cfg.decoration.borders && old != new {
            if let Some(o) = old.filter(|o| self.windows.contains_key(o)) {
                win::set_border_color(o, self.inactive_border);
            }
            if let Some(n) = new {
                win::set_border_color(n, self.active_border);
            }
        }
        if let Some(ws) = new.and_then(|n| self.windows.get(&n)).map(|m| m.ws) {
            self.workspaces[ws].last_focus = new;
            if let Some(mi) = self.monitor_of_ws(ws) {
                self.active_monitor = mi;
            }
        }
    }

    fn focus_window(&mut self, h: Hwnd) {
        win::focus(h);
        self.set_focused(Some(h));
    }

    /// Focuses the workspace's last focused window, or the desktop if empty.
    fn focus_in_ws(&mut self, ws: usize) {
        let windows = self.workspaces[ws].windows();
        let pick = self.workspaces[ws].last_focus.filter(|h| windows.contains(h)).or_else(|| windows.first().copied());
        match pick {
            Some(h) => self.focus_window(h),
            None => {
                win::focus_desktop();
                self.set_focused(None);
            }
        }
    }

    /// After a window left a workspace: re-tile, and move focus within the
    /// workspace if the window had it.
    fn after_removal(&mut self, ws: usize, was_focused: bool) {
        self.layout_ws(ws);
        if was_focused && self.monitor_of_ws(ws).is_some() {
            self.focus_in_ws(ws);
        }
    }

    fn refresh_bars(&self) {
        let focused_ws = self.focused.and_then(|f| self.windows.get(&f)).map(|m| m.ws);
        let title = self.focused.map(win::title).unwrap_or_default();
        for (i, mon) in self.monitors.iter().enumerate() {
            let Some(bar) = &mon.bar else { continue };
            let workspaces = (0..WORKSPACES)
                .filter(|&w| w == mon.ws || !self.workspaces[w].is_empty())
                .map(|w| WsItem { index: w, active: w == mon.ws, occupied: !self.workspaces[w].is_empty() })
                .collect();
            bar.set_state(BarState {
                workspaces,
                title: if focused_ws == Some(mon.ws) { title.clone() } else { String::new() },
                icon: if focused_ws == Some(mon.ws) { self.focused_icon } else { 0 },
                focused_monitor: i == self.active_monitor,
            });
        }
    }

    // ----- workspaces -----------------------------------------------------

    /// Shows workspace `target` on the active monitor (or focuses the
    /// monitor already showing it).
    fn switch_workspace(&mut self, target: usize) {
        let mi = self.active_monitor;
        let old = self.monitors[mi].ws;
        if old == target {
            return;
        }
        if let Some(other) = self.monitor_of_ws(target) {
            self.active_monitor = other;
            self.focus_in_ws(target);
            return;
        }
        self.monitors[mi].ws = target;
        // Recenter floating windows that were last shown on another monitor.
        let handle = self.monitors[mi].info.handle;
        let area = self.tiling_area(mi);
        for &f in &self.workspaces[target].floating {
            if win::monitor_of_window(f) != handle {
                let r = win::window_rect(f);
                win::set_window_rect(f, area.centered(r.w.min(area.w), r.h.min(area.h)));
            }
        }
        // Lay out while still hidden, show the new windows, then hide the old.
        self.layout_ws(target);
        for h in self.workspaces[target].windows() {
            hidden::show(h);
        }
        for &f in &self.workspaces[target].floating {
            win::raise(f);
        }
        for h in self.workspaces[old].windows() {
            hidden::hide(h);
        }
        self.focus_in_ws(target);
    }

    fn move_to_workspace(&mut self, target: usize, follow: bool) {
        let Some(h) = self.focused else { return };
        let Some(src) = self.windows.get(&h).map(|m| m.ws) else { return };
        if src == target {
            return;
        }
        let tile_target = self.workspaces[target].last_focus;
        self.transfer(h, target, tile_target);
        let visible = self.monitor_of_ws(target).is_some();
        if visible {
            self.layout_ws(target);
        } else if !follow {
            hidden::hide(h);
        }
        self.layout_ws(src);
        if follow {
            if visible {
                self.focus_window(h);
            } else {
                self.switch_workspace(target);
            }
        } else {
            self.focused = None;
            self.focus_in_ws(src);
        }
    }

    // ----- dispatchers ---------------------------------------------------

    /// Runs the binding with this id. `from_hook` is true for bindings the
    /// keyboard hook handled; the hook masks the menu key itself.
    pub fn on_hotkey(&mut self, id: i32, from_hook: bool) {
        let Some(bind) = usize::try_from(id - 1).ok().and_then(|i| self.binds.get(i)).cloned() else {
            return;
        };
        if !from_hook && bind.hotkey.mods & (MOD_ALT | MOD_WIN) != 0 {
            win::mask_menu_key();
        }
        let moves_focus = matches!(
            bind.command,
            Command::MoveFocus(_)
                | Command::MoveWindow(_)
                | Command::Cycle(_)
                | Command::Workspace(_)
                | Command::MoveToWorkspace { .. }
                | Command::ToggleFloating
        );
        self.dispatch(bind.command);
        if moves_focus {
            if let Some(h) = self.focused {
                self.warp_pointer_to(h);
            }
        }
    }

    /// With focus follows mouse, moves the pointer into `h` (unless it is
    /// already there) so that keyboard focus changes stick: otherwise the
    /// next nudge of the mouse would focus whatever is under the pointer.
    fn warp_pointer_to(&self, h: Hwnd) {
        if !self.cfg.general.focus_follows_mouse {
            return;
        }
        let Some(ws) = self.windows.get(&h).map(|m| m.ws) else { return };
        // Use the layout's target rectangle: window moves are asynchronous,
        // so the window's current rectangle may still be the old one.
        let Some(r) = self.ws_rects(ws).into_iter().find(|(w, _)| *w == h).map(|(_, r)| r) else { return };
        let (x, y) = win::cursor_pos();
        if !r.contains(x, y) {
            let (cx, cy) = r.center();
            win::set_cursor_pos(cx, cy);
        }
    }

    /// The pointer moved: focus the managed window under it.
    pub fn on_mouse_move(&mut self) {
        if !self.cfg.general.focus_follows_mouse || win::mouse_button_down() || launcher::is_open() {
            return;
        }
        let (x, y) = win::cursor_pos();
        let h = win::window_at(x, y);
        if h == 0 || Some(h) == self.focused {
            return;
        }
        let Some((ws, floating)) = self.windows.get(&h).map(|m| (m.ws, m.floating)) else { return };
        if self.monitor_of_ws(ws).is_none() {
            return;
        }
        self.focus_window(h);
        if !floating {
            // Focusing raises the window; keep floating windows on top.
            for &f in &self.workspaces[ws].floating {
                win::raise(f);
            }
        }
        self.refresh_bars();
    }

    /// Handles clicks and scrolls on a bar.
    pub fn on_bar(&mut self, kind: usize, value: isize) {
        let (x, y) = win::cursor_pos();
        self.active_monitor = self.monitor_index(win::monitor_at(x, y));
        let target = match kind {
            crate::BAR_WORKSPACE => WsTarget::Index(value.max(0) as usize),
            _ => WsTarget::Relative(value.signum() as i32),
        };
        self.dispatch(Command::Workspace(target));
    }

    pub fn dispatch(&mut self, cmd: Command) {
        match cmd {
            Command::Exec(command) => self.exec(&command),
            Command::KillActive => {
                if let Some(h) = self.focused {
                    win::close(h);
                }
            }
            Command::ToggleFloating => self.toggle_floating(),
            Command::Fullscreen(mode) => self.toggle_fullscreen(mode),
            Command::ToggleSplit => {
                if let Some((h, ws)) = self.focused_tiled() {
                    if self.workspaces[ws].tree.toggle_split(h) {
                        self.layout_ws(ws);
                    }
                }
            }
            Command::MoveFocus(dir) => self.move_focus(dir),
            Command::MoveWindow(dir) => self.move_window(dir),
            Command::Resize(dir) => self.resize(dir),
            Command::Cycle(forward) => self.cycle(forward),
            Command::Workspace(t) => {
                let ws = self.resolve(t);
                self.switch_workspace(ws);
            }
            Command::MoveToWorkspace { target, follow } => {
                let ws = self.resolve(target);
                self.move_to_workspace(ws, follow);
            }
            Command::Launcher => self.toggle_launcher(),
            Command::Reload => self.reload(),
            Command::Exit => unsafe { PostQuitMessage(0) },
        }
        self.refresh_bars();
    }

    fn focused_tiled(&self) -> Option<(Hwnd, usize)> {
        let h = self.focused?;
        let m = self.windows.get(&h)?;
        (!m.floating).then_some((h, m.ws))
    }

    fn exec(&self, command: &str) {
        let command = command.replace("$terminal", &self.cfg.general.terminal);
        match win::run_command(&command) {
            Ok(()) => info!("exec {command}"),
            Err(e) => error!("exec {command}: {e}"),
        }
    }

    fn toggle_launcher(&self) {
        let mi = self.active_monitor;
        let style = launcher::Style {
            palette: self.palette,
            font: self.cfg.bar.font.clone(),
            font_px: self.cfg.bar.font_size as f32 * self.monitors[mi].info.scale,
            scale: self.monitors[mi].info.scale,
        };
        launcher::toggle(self.base_rect(mi), style);
    }

    fn toggle_floating(&mut self) {
        let Some(h) = self.focused else { return };
        let Some(m) = self.windows.get_mut(&h) else { return };
        let ws = m.ws;
        m.floating = !m.floating;
        let now_floating = m.floating;
        if self.workspaces[ws].fullscreen.is_some_and(|(f, _)| f == h) {
            self.workspaces[ws].fullscreen = None;
            self.workspaces[ws].restore_rect = None;
        }
        if now_floating {
            self.workspaces[ws].tree.remove(h);
            self.workspaces[ws].floating.push(h);
            let mi = self.monitor_of_ws(ws).unwrap_or(self.active_monitor);
            let area = self.tiling_area(mi);
            win::set_window_rect(h, area.centered(area.w * 3 / 5, area.h * 7 / 10));
            win::raise(h);
        } else {
            self.workspaces[ws].floating.retain(|&f| f != h);
            let target = self.workspaces[ws].tree.windows().last().copied();
            self.tile_into(h, ws, target);
        }
        self.layout_ws(ws);
    }

    fn toggle_fullscreen(&mut self, mode: FsMode) {
        let Some(h) = self.focused else { return };
        let Some(m) = self.windows.get(&h) else { return };
        let (ws, floating) = (m.ws, m.floating);
        let workspace = &mut self.workspaces[ws];
        let previous = workspace.fullscreen.take();
        if let Some((prev, _)) = previous {
            if let Some(r) = workspace.restore_rect.take() {
                win::set_window_rect(prev, r);
            }
        }
        if previous != Some((h, mode)) {
            workspace.restore_rect = floating.then(|| win::window_rect(h));
            workspace.fullscreen = Some((h, mode));
        }
        self.layout_ws(ws);
    }

    fn move_focus(&mut self, dir: Dir) {
        if let Some(h) = self.focused {
            if let Some(ws) = self.windows.get(&h).map(|m| m.ws) {
                if let Some(next) = layout::neighbor(&self.ws_rects(ws), h, dir) {
                    self.focus_window(next);
                    return;
                }
            }
        }
        if let Some(mi) = self.monitor_in_dir(self.active_monitor, dir) {
            self.active_monitor = mi;
            self.focus_in_ws(self.monitors[mi].ws);
        }
    }

    fn move_window(&mut self, dir: Dir) {
        let Some(h) = self.focused else { return };
        let Some(m) = self.windows.get(&h) else { return };
        let (ws, floating) = (m.ws, m.floating);
        let Some(mi) = self.monitor_of_ws(ws) else { return };
        if floating {
            let step = self.scaled(mi, FLOAT_STEP);
            let r = win::window_rect(h);
            let (dx, dy) = match dir {
                Dir::Left => (-step, 0),
                Dir::Right => (step, 0),
                Dir::Up => (0, -step),
                Dir::Down => (0, step),
            };
            win::set_window_rect(h, Rect { x: r.x + dx, y: r.y + dy, ..r });
            return;
        }
        let tiled = self.workspaces[ws].tree.layout(self.tiling_area(mi), self.gap(mi));
        if let Some(other) = layout::neighbor(&tiled, h, dir) {
            self.workspaces[ws].tree.swap(h, other);
            self.layout_ws(ws);
        } else if let Some(target_mi) = self.monitor_in_dir(mi, dir) {
            // No neighbour on this side: move to the adjacent monitor.
            let target_ws = self.monitors[target_mi].ws;
            let tile_target = self.workspaces[target_ws].last_focus;
            self.transfer(h, target_ws, tile_target);
            self.layout_ws(ws);
            self.layout_ws(target_ws);
            self.focus_window(h);
        }
    }

    fn resize(&mut self, dir: Dir) {
        let Some(h) = self.focused else { return };
        let Some(m) = self.windows.get(&h) else { return };
        let ws = m.ws;
        if !m.floating {
            if self.workspaces[ws].tree.resize(h, dir, RESIZE_STEP) {
                self.layout_ws(ws);
            }
            return;
        }
        let mi = self.monitor_of_ws(ws).unwrap_or(self.active_monitor);
        let step = self.scaled(mi, FLOAT_STEP);
        let min = self.scaled(mi, 100);
        let r = win::window_rect(h);
        let (w, hgt) = match dir {
            Dir::Right => (r.w + step, r.h),
            Dir::Left => ((r.w - step).max(min), r.h),
            Dir::Down => (r.w, r.h + step),
            Dir::Up => (r.w, (r.h - step).max(min)),
        };
        win::set_window_rect(h, Rect { w, h: hgt, ..r });
    }

    fn cycle(&mut self, forward: bool) {
        let ws = self.focused.and_then(|f| self.windows.get(&f)).map(|m| m.ws).unwrap_or(self.current_ws());
        let windows = self.workspaces[ws].windows();
        if windows.is_empty() {
            return;
        }
        let n = windows.len();
        let next = match self.focused.and_then(|f| windows.iter().position(|&w| w == f)) {
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
            None => 0,
        };
        self.focus_window(windows[next]);
        if self.workspaces[ws].floating.contains(&windows[next]) {
            win::raise(windows[next]);
        }
    }

    // ----- window events -------------------------------------------------

    pub fn on_event(&mut self, event: u32, h: Hwnd) {
        match event {
            EVENT_OBJECT_SHOW | EVENT_OBJECT_UNCLOAKED => self.on_show(h),
            EVENT_OBJECT_HIDE | EVENT_OBJECT_CLOAKED => self.on_hide(h, event == EVENT_OBJECT_CLOAKED),
            EVENT_OBJECT_DESTROY => {
                self.ignored.remove(&h);
                let was_focused = self.focused == Some(h);
                if let Some(ws) = self.unmanage(h) {
                    self.after_removal(ws, was_focused);
                }
            }
            EVENT_SYSTEM_MINIMIZESTART => {
                let was_focused = self.focused == Some(h);
                if let Some(ws) = self.unmanage(h) {
                    self.after_removal(ws, was_focused);
                }
            }
            EVENT_SYSTEM_MINIMIZEEND => self.manage_and_layout(h),
            EVENT_SYSTEM_FOREGROUND => self.on_foreground(h),
            EVENT_SYSTEM_MOVESIZEEND => self.on_move_end(h),
            EVENT_OBJECT_NAMECHANGE => {
                // Some apps only get a title (and become manageable) after
                // they are shown.
                if !self.windows.contains_key(&h) {
                    self.manage_and_layout(h);
                }
            }
            _ => {}
        }
        self.refresh_bars();
    }

    fn manage_and_layout(&mut self, h: Hwnd) {
        if self.manage(h, None) {
            let ws = self.windows[&h].ws;
            self.layout_ws(ws);
            if win::foreground() == h {
                self.set_focused(Some(h));
                self.warp_pointer_to(h);
            }
        }
    }

    fn on_show(&mut self, h: Hwnd) {
        if self.cfg.general.hide_taskbar && win::is_taskbar(h) && win::is_visible(h) {
            // Explorer re-shows the taskbar now and then (Start menu, explorer
            // restarts, display changes); keep it hidden.
            hidden::hide(h);
            return;
        }
        let Some(ws) = self.windows.get(&h).map(|m| m.ws) else {
            self.manage_and_layout(h);
            return;
        };
        if self.monitor_of_ws(ws).is_none() && win::is_visible(h) {
            // The app re-showed a window on a hidden workspace (e.g. restored
            // from its tray icon): follow it there.
            hidden::forget(h);
            self.workspaces[ws].last_focus = Some(h);
            self.switch_workspace(ws);
        }
    }

    fn on_hide(&mut self, h: Hwnd, cloaked: bool) {
        if !self.windows.contains_key(&h) {
            return;
        }
        let gone = if cloaked { win::is_cloaked(h) } else { !hidden::contains(h) && !win::is_visible(h) };
        if gone {
            let was_focused = self.focused == Some(h);
            if let Some(ws) = self.unmanage(h) {
                self.after_removal(ws, was_focused);
            }
        }
    }

    fn on_foreground(&mut self, h: Hwnd) {
        if win::foreground() != h {
            return; // stale event
        }
        if !self.windows.contains_key(&h) && !self.manage(h, None) {
            self.set_focused(None);
            return;
        }
        let ws = self.windows[&h].ws;
        if self.monitor_of_ws(ws).is_none() {
            // Activated while on a hidden workspace: bring that workspace up.
            self.workspaces[ws].last_focus = Some(h);
            self.switch_workspace(ws);
            return;
        }
        self.set_focused(Some(h));
    }

    /// A window was dragged with the mouse: dropping a tiled window onto
    /// another swaps them; dropping onto another monitor moves it there.
    fn on_move_end(&mut self, h: Hwnd) {
        let Some(m) = self.windows.get(&h) else { return };
        let (src, floating) = (m.ws, m.floating);
        let (cx, cy) = win::cursor_pos();
        let mi = self.monitor_index(win::monitor_at(cx, cy));
        let dst = self.monitors[mi].ws;
        let under = self
            .ws_rects(dst)
            .into_iter()
            .find(|(o, r)| *o != h && !self.workspaces[dst].floating.contains(o) && r.contains(cx, cy))
            .map(|(o, _)| o);
        if dst != src {
            self.transfer(h, dst, under);
            self.layout_ws(src);
        } else if let (false, Some(other)) = (floating, under) {
            self.workspaces[dst].tree.swap(h, other);
        }
        self.layout_ws(dst);
        self.set_focused(Some(h));
    }
}

/// Windows tessera has hidden, mirrored to `%LOCALAPPDATA%\tessera\hidden.txt`
/// so they can be shown again if tessera dies without cleaning up.
mod hidden {
    use crate::win::{self, Hwnd};
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, MutexGuard};

    /// hwnd -> window class (to avoid showing an unrelated window that
    /// reused the handle after a crash).
    static SET: Mutex<BTreeMap<Hwnd, String>> = Mutex::new(BTreeMap::new());
    static DIRTY: AtomicBool = AtomicBool::new(false);

    fn set() -> MutexGuard<'static, BTreeMap<Hwnd, String>> {
        SET.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn path() -> PathBuf {
        crate::log::data_dir().join("hidden.txt")
    }

    pub fn hide(h: Hwnd) {
        set().insert(h, win::class(h));
        DIRTY.store(true, Ordering::Relaxed);
        win::hide(h);
    }

    pub fn show(h: Hwnd) {
        forget(h);
        win::show(h);
    }

    pub fn contains(h: Hwnd) -> bool {
        set().contains_key(&h)
    }

    /// Stops tracking `h` without touching it.
    pub fn forget(h: Hwnd) {
        if set().remove(&h).is_some() {
            DIRTY.store(true, Ordering::Relaxed);
        }
    }

    /// Shows every window tessera has hidden.
    pub fn restore_all() {
        let all = std::mem::take(&mut *set());
        for &h in all.keys() {
            if win::is_window(h) {
                win::show(h);
            }
        }
        DIRTY.store(true, Ordering::Relaxed);
        flush();
    }

    /// Writes the set to disk if it changed.
    pub fn flush() {
        if !DIRTY.swap(false, Ordering::Relaxed) {
            return;
        }
        let content: String = set().iter().map(|(h, class)| format!("{h}\t{class}\n")).collect();
        let path = path();
        let result = if content.is_empty() {
            std::fs::remove_file(&path)
                .or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) })
        } else {
            std::fs::create_dir_all(crate::log::data_dir()).and_then(|_| std::fs::write(&path, content))
        };
        if let Err(e) = result {
            warn!("could not save hidden window list: {e}");
        }
    }

    /// Shows windows a previous, crashed session left hidden.
    /// Returns how many were restored.
    pub fn recover() -> usize {
        let path = path();
        let Ok(text) = std::fs::read_to_string(&path) else { return 0 };
        let mut restored = 0;
        for line in text.lines() {
            let Some((h, class)) = line.split_once('\t') else { continue };
            let Ok(h) = h.parse::<Hwnd>() else { continue };
            if win::is_window(h) && win::class(h) == class && !win::is_visible(h) {
                win::show(h);
                restored += 1;
            }
        }
        let _ = std::fs::remove_file(&path);
        restored
    }
}
