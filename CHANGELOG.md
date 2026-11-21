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
