# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.2.0] - 2026-11-22

### Added

- Bar widgets, placed like waybar's modules with `modules_left`,
  `modules_center` and `modules_right`: `workspaces`, `window`, `clock`,
  `network`, `bluetooth`, `cpu`, `memory`, `volume`, and `custom/<name>`
  widgets that show a command's output (`[bar.custom.<name>]`: `exec`,
  `interval`, `on_click`, `on_right_click`, `on_scroll_up`, `on_scroll_down`).
- Panels for the widgets: Wi-Fi (switch it on or off, join a network, asking
  for its password when needed, or leave it), Bluetooth (switch it on or off,
  paired devices), system (live CPU and memory graphs) and sound (volume
  slider, mute). Scroll over the volume widget to change the volume,
  right-click it to mute.
- Goeist and Goeist Mono are built in and are the default fonts (`font`,
  `font_mono`).
- Workspaces 1-5 always show on the bar (`persistent_workspaces`); higher
  ones appear while active or holding windows.

### Changed

- The bar, its panels and the launcher are drawn flat: solid surfaces and
  hairline borders, no shadows.
- Sharp by default: `corner_radius = 0` gives square corners everywhere (bar,
  panels, buttons, switches, launcher), with surfaces snapped to whole pixels
  and exact 1 px borders. Text renders with higher contrast. Set
  `corner_radius` above 0 for rounded corners.
- The bar is solid by default (`opacity = 1.0`) and drawn as floating islands.
- Square corners on managed windows (`decoration.square_corners`, on by
  default), restored when gromak stops managing a window or exits.
- Window shadows are turned off while gromak runs (`general.window_shadows =
  false`, the default). Like Snap, the original setting is recorded in
  `%LOCALAPPDATA%\gromak\` and put back on exit, logoff, the next start
  after a crash, or `gromak --restore`.

## [0.1.0] - 2026-11-22

### Added

- Dwindle tiling layout with inner and outer gaps.
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
  next start, or `gromak --restore`.
- CLI: `--list-windows`, `--check-config`, `--config-path`,
  `--default-config`, `--restore`, `--version`, and `--help`.
- Full-width bar style with a subtle blurred (acrylic) backdrop, now the
  default; the floating islands remain as `style = "islands"`.
- Smooth animations: windows glide to their new place (ease-out quint),
  workspaces slide in, the active workspace indicator slides between square
  pills, and Windows' own show/hide scale-and-fade is turned off for managed
  windows (`[animations]`).
- The project is now called gromak (config and data folders migrate
  automatically from the old name).
- Launcher power actions: lock, sleep, hibernate, sign out, restart and shut
  down (destructive ones need a second Enter).
- Per-monitor workspaces: every monitor has its own workspaces 1–10.
- `mod`+drag moves windows (drop on a tile, a monitor or a bar workspace
  pill); `mod`+right-drag resizes floating windows or moves tile splits
  (`general.mouse_drag`).
- `mod+ctrl+1…0` sends the focused window to a workspace without following.
- `general.start_menu`: make a lone Win tap do nothing (`double_tap` opens
  Start on a quick double tap, `never` never does).
- Thinner bar and tighter gaps by default.
- `general.disable_snap` (on by default): Windows' own snapping (edge snap,
  Snap Layouts, Snap Assist) is turned off while gromak runs and restored
  when it exits, logs off, or the next start after a crash.
- `gromak --quit` asks the running instance to exit cleanly.
- Release builds are windowed apps: no console window flashes when gromak
  starts from the Start menu or at sign-in.
- Installer (`gromak-<version>-setup.exe`, built with Inno Setup): per-user
  install, Start menu entry, optional start at sign-in, uninstaller; plus a
  portable exe. gromak.exe now has an icon and version information.

### Fixed

- Focus could disagree with the real keyboard focus (notably for Store
  apps); focus follows mouse used a stale pointer position; floating
  windows could end up behind tiles.
- Directional focus/move picked windows that did not line up instead of
  crossing to the next monitor.
- Windows could stay hidden after a monitor came back, after being moved to
  a visible workspace, or after being cloaked while hidden.
- gromak's own fullscreen could get stuck; resize drags were treated as
  moves; borders were left behind on unmanaged windows.
- The GIF picker lost its selection while thumbnails loaded, and failed
  thumbnails were re-downloaded continuously.
- The fuzzy matcher missed valid matches; the calculator could overflow its
  stack on huge input; wheel and keyboard hooks could keep stale state.
- The Start menu could open after `mod`+drag or `mod`+scroll with
  `mod = "win"` when Win was released before the mouse button. With
  `start_menu = "never"`, every Win release is now masked, and Ctrl+Esc is
  blocked too.
- gromak crashed on exit (after its cleanup had run) because graphics
  objects were released during process teardown.

[Unreleased]: https://github.com/radiumcoders/gromak/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/radiumcoders/gromak/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/radiumcoders/gromak/releases/tag/v0.1.0
