# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Dwindle tiling layout with inner and outer gaps.
- Ten global workspaces, one shown per monitor, with switching, moving
  windows (with or without following), and relative navigation.
- Hyprland-style dispatchers bound to configurable global hotkeys:
  `exec`, `killactive`, `togglefloating`, `fullscreen`, `togglesplit`,
  `movefocus`, `movewindow`, `resizeactive`, `cyclenext`/`cycleprev`,
  `workspace`, `movetoworkspace(silent)`, `reload`, and `exit`.
- Floating windows. Dialogs and fixed-size windows float automatically.
- Drag a tiled window onto another to swap them, or onto another monitor to move it.
- Per-monitor floating bar: translucent, rounded islands for workspaces, the
  focused window's icon and title, and the date and time, drawn with
  Direct2D/DirectWrite. Click or scroll it to switch workspaces. Configure it
  with `opacity` and `corner_radius`.
- `mod = "win"` support: Win-key bindings go through a low-level keyboard hook,
  so they override Windows' reserved shortcuts without opening the Start menu.
- `hide_taskbar` keeps the Windows taskbar hidden, even when Explorer re-shows it.
- App launcher (`launcher` dispatcher, `mod+Space`): fuzzy search over all
  installed apps with icons, usage ranking and a run-a-command fallback.
- Launcher calculator (type maths; Enter copies the result) and emoji picker
  (`:` prefix; Enter types the emoji), with a configurable emoji font
  (`[launcher] emoji_font`, including .ttf files such as Twemoji).
- orc.pictures GIF picker in the launcher (`gif ` prefix): thumbnails, Enter
  pastes the GIF as a file, Shift+Enter copies its link.
- Mouse-wheel bindings (`wheelup` / `wheeldown`): `mod`+scroll cycles windows,
  `mod+Shift`+scroll switches workspaces.
- Focus follows mouse (`general.focus_follows_mouse`), with the pointer
  following keyboard focus changes.
- Wallpaper-driven theming: colours are derived from the desktop wallpaper
  and follow it live, in dark or light mode (`[theme] source` and `mode`).
  The Windows accent colour or manual colours are also supported.
- Windows 11 accent border on the focused window.
- Window rules by exe, class, or title: float, ignore, or assign a workspace.
- TOML configuration with commented defaults, validation, and live reload.
- Multi-monitor and per-monitor DPI support.
- Crash safety: hidden windows are persisted and restored on exit, logoff,
  next start, or `tessera --restore`.
- CLI: `--list-windows`, `--check-config`, `--config-path`,
  `--default-config`, `--restore`, `--version`, and `--help`.
- Per-monitor workspaces: every monitor has its own workspaces 1–10.
- `mod`+drag moves windows (drop on a tile, a monitor or a bar workspace
  pill); `mod`+right-drag resizes floating windows or moves tile splits
  (`general.mouse_drag`).
- `mod+ctrl+1…0` sends the focused window to a workspace without following.
- `general.start_menu`: make a lone Win tap do nothing (`double_tap` opens
  Start on a quick double tap, `never` never does).
- Thinner bar and tighter gaps by default.

### Fixed

- Focus could disagree with the real keyboard focus (notably for Store
  apps); focus follows mouse used a stale pointer position; floating
  windows could end up behind tiles.
- Directional focus/move picked windows that did not line up instead of
  crossing to the next monitor.
- Windows could stay hidden after a monitor came back, after being moved to
  a visible workspace, or after being cloaked while hidden.
- tessera's own fullscreen could get stuck; resize drags were treated as
  moves; borders were left behind on unmanaged windows.
- The GIF picker lost its selection while thumbnails loaded, and failed
  thumbnails were re-downloaded continuously.
- The fuzzy matcher missed valid matches; the calculator could overflow its
  stack on huge input; wheel and keyboard hooks could keep stale state.
