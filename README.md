# tessera

**A minimal, keyboard-driven tiling window manager for Windows, inspired by Hyprland.**

tessera arranges your windows into a clean dwindle layout with gaps, gives you
ten workspaces you can flip between instantly, and stays out of the way: one
floating bar per monitor, a coloured border on the focused window, and nothing else.

```
╭─────────╮       ╭─────────────────────────╮       ╭────────────────────╮
│ (1) 2 3 │       │ ▣ ~/projects — Terminal │       │ Sat 21 Nov • 21:27 │
╰─────────╯       ╰─────────────────────────╯       ╰────────────────────╯
╭──────────────────────────────╮ ╭──────────────────────────────╮
│                              │ │                              │
│          terminal            │ │           browser            │
│                              │ │                              │
│                              │ ╰──────────────────────────────╯
│                              │ ╭──────────────────────────────╮
│                              │ │            editor            │
╰──────────────────────────────╯ ╰──────────────────────────────╯
```

> **Status:** early (v0.1). It is usable day to day, but expect rough edges.
> If tessera ever exits unexpectedly, run `tessera --restore` to bring back
> windows that were on hidden workspaces.

## Features

- **Dwindle tiling.** Every new window splits the focused tile along its longer side.
- **Ten workspaces.** Switch with `mod+1…0`, send windows with `mod+shift+1…0`.
  Each monitor shows its own workspace.
- **Keyboard-first.** Move focus, swap, resize, float, fullscreen, and close
  windows without touching the mouse. The commands use Hyprland's names
  (`movefocus`, `togglefloating`, `workspace e+1`…).
- **Mouse-friendly too.** Drag a tiled window onto another to swap them, or onto
  another monitor to move it. Click or scroll the bar to change workspace.
- **Focus follows mouse (optional).** Focus moves to whatever window the pointer
  is over, and the pointer follows when you move focus with the keyboard, like
  Hyprland's `follow_mouse`. Turn it on with `focus_follows_mouse = true`.
- **App launcher.** `mod+Space` opens a rofi-style search panel with every
  installed app (desktop and Store), real icons, fuzzy matching and
  most-used-first ranking. If nothing matches, Enter runs what you typed.
  It also has a calculator, an emoji picker (`:`) and an
  [orc.pictures](https://orc.pictures) GIF picker (`gif `).
- **Floating island bar.** Translucent, rounded islands for workspaces, the
  focused window's icon and title, and the date and time. The space between
  them is see-through and click-through.
- **Themed by your wallpaper.** The bar and the focused window's border take
  their colours from your desktop wallpaper and follow it when it changes,
  in dark or light mode. You can also use your Windows accent colour, or
  your own colours.
- **Focus borders.** Uses Windows 11's native DWM window borders.
- **Window rules.** Float, ignore, or pin windows to workspaces by exe, class, or title.
- **Multi-monitor and per-monitor DPI aware.** Gaps and the bar scale with each display.
- **Safe.** Hidden windows are tracked on disk and restored on exit, logoff,
  crash recovery, or `tessera --restore`. No DLL injection and no admin rights required.
- **Small.** A single executable under 1 MB with no runtime dependencies.

## Requirements

- Windows 10 1809+ or Windows 11. Coloured focus borders need Windows 11.
- To build from source: [Rust](https://rustup.rs) (stable, MSVC toolchain).

## Install

```powershell
git clone https://github.com/radiumcoders/tessera
cd tessera
cargo build --release
.\target\release\tessera.exe
```

Copy `target\release\tessera.exe` anywhere you like (for example
`%LOCALAPPDATA%\Programs\tessera\`).

### Start with Windows

Press `Win+R`, run `shell:startup`, and create a shortcut to `tessera.exe` in
that folder.

## Default keybindings

`mod` is **Alt** by default. Set `general.mod` to `"win"` or `"ctrl+alt"` in
the config to change it. With `mod = "win"`, tessera takes over Windows' own
Win+key shortcuts (Win+1…9, Win+D, …) through a low-level keyboard hook, and
pressing Win on its own still opens the Start menu. The one exception is
**Win+L**, which always locks the PC, so use the arrow-key bindings for
moving focus right.

| Keys | Action |
| --- | --- |
| `mod+Enter` | Open a terminal (`general.terminal`, Windows Terminal by default) |
| `mod+Space` | Open the app launcher |
| `mod+Q`, `mod+W` | Close the focused window |
| `mod+H/J/K/L`, `mod+←↓↑→` | Move focus (crosses monitors at the edge) |
| `mod+Shift+H/J/K/L`, `mod+Shift+←↓↑→` | Swap with neighbour (or move to the next monitor) |
| `mod+Ctrl+H/J/K/L` | Move the nearest divider |
| `mod+1` … `mod+0` | Switch to workspace 1–10 |
| `mod+Shift+1` … `mod+Shift+0` | Move window to workspace 1–10 and follow it |
| `mod+Ctrl+1` … `mod+Ctrl+0` | Send window to workspace 1–10 without following it |
| `mod+.` / `mod+,` | Next / previous workspace |
| `mod+N` / `mod+Shift+N` | Focus next / previous window |
| `mod`+scroll | Focus next / previous window |
| `mod+Shift`+scroll | Next / previous workspace |
| `mod+V` | Toggle floating |
| `mod+F` | Fullscreen within the tiling area (keeps gaps and bar) |
| `mod+Shift+F` | True fullscreen |
| `mod+T` | Toggle the split direction |
| `mod+Shift+R` | Reload the config |
| `mod+Shift+E` | Exit tessera (restores all windows) |

## Configuration

The config lives at `%USERPROFILE%\.config\tessera\tessera.toml` and is created
with commented defaults on first launch. Edit it, then press `mod+Shift+R` to
reload. If the file has an error, tessera says so in the bar and keeps the
previous config.

```toml
[general]
gaps_in = 5
gaps_out = 10
mod = "alt"
terminal = "wt.exe"
hide_taskbar = false
focus_follows_mouse = false

[theme]
source = "wallpaper"  # or "accent" or "manual"
mode = "auto"          # follow Windows light/dark, or "dark" / "light"

[decoration]
borders = true
active_border = "#8aadf4"
inactive_border = "#363a4f"

[bar]
enabled = true
height = 32
opacity = 0.9        # island opacity, 0.0 - 1.0
corner_radius = 10
font = "Segoe UI Variable Text"
font_size = 13
background = "#181926"
foreground = "#cad3f5"
accent = "#8aadf4"

[[rules]]
exe = "Spotify.exe"
workspace = 9

[[rules]]
title = "Picture-in-Picture"
float = true

[binds]
"mod+return" = "exec $terminal"
"mod+b" = "exec firefox.exe"
"mod+1" = "workspace 1"
# ...
```

The full reference, including every dispatcher, is in
[`config/default.toml`](config/default.toml). You can also print it with
`tessera --default-config`.

### App launcher

`mod+Space` opens the launcher on the focused monitor. Start typing to
filter. Matching is fuzzy, so `vsc` finds Visual Studio Code and `term`
finds Terminal, and the matched letters are highlighted.

| Key | Action |
| --- | --- |
| `↑` `↓`, `Tab` `Shift+Tab`, `Ctrl+J` `Ctrl+K` | Move the selection |
| `PgUp` `PgDn` | Move a page |
| `Enter` | Open the selected app (or run the text as a command if nothing matches) |
| `Esc` | Close |
| `Ctrl+Backspace` | Delete a word |
| `Ctrl+V` | Paste |

The list is the same as Start's "All apps" (`shell:AppsFolder`), including
Microsoft Store apps. Apps you launch often move to the top. The launch
counts are kept in `%LOCALAPPDATA%\tessera\launcher_history.txt`.

#### Calculator

Type maths and the answer appears as the first result. Enter copies it to
the clipboard. Start with `=` to use only the calculator.

```
2+3*4            → 14
15% of 80        → 12
sqrt(2)^2 + 5!   → 122
(1,000 - 25) / 3 → 325
```

It supports `+ - * / ^`, parentheses, percentages, `mod`, factorials,
implicit multiplication (`2pi`), `pi`, `tau` and `e`, and the functions
`sqrt cbrt abs exp ln log log2 sin cos tan asin acos atan floor ceil round`.

#### Emoji picker

Type `:` followed by a name, such as `:fire` or `:thumbs`, to search
emoji. Enter types the emoji into the window you were using, then restores
your clipboard. To use a different emoji style, point `[launcher]
emoji_font` at an installed font or a `.ttf` file. For example, Twemoji, the
style used by Discord:

```toml
[launcher]
emoji_font = "fonts/Twemoji.Mozilla.ttf"   # relative to the config folder
```

Download `Twemoji.Mozilla.ttf` from
[mozilla/twemoji-colr](https://github.com/mozilla/twemoji-colr/releases).

#### GIF picker (orc.pictures)

Type `gif ` followed by a search, such as `gif angry` or `gif fire`, to
browse the [orc.pictures](https://orc.pictures) reaction-GIF catalog, with
thumbnails. **Enter** pastes the GIF into the window you were using.
**Shift+Enter** copies its link instead.
Every GIF comes in two styles, orc and Videorc. Press **Ctrl+S** in the picker
to switch between them, and set the default with `[launcher] gif_style =
"orc"` or `"videorc"`.

The GIF goes on the clipboard as a file, the same way Explorer copies one,
so it stays animated when pasted into Discord, Slack, a browser upload field
or a folder. The catalog, thumbnails and GIFs are cached in
`%LOCALAPPDATA%\tessera\orc\`. Each file is downloaded once, and the catalog
is refreshed every six hours. GIFs are made by
[OrcDev](https://orcdev.com) and fetched from the public site; tessera does
not bundle any of them.

### Theme

By default, tessera picks its colours from your wallpaper. It finds the most
vivid colour in the image and uses it as the accent (the active workspace
pill, the clock dot and the focused window's border). It then tints the bar
with the image's overall tone. When you change the wallpaper, or a slideshow
moves on, the colours update within a few seconds. Black-and-white wallpapers
fall back to your Windows accent colour.

| `[theme]` option | Values |
| --- | --- |
| `source` | `"wallpaper"` (default), `"accent"` (the Windows accent colour), or `"manual"` (the colours in `[bar]` and `[decoration]`) |
| `mode` | `"auto"` (follow Windows' light/dark setting, the default), `"dark"`, or `"light"` |

### Hiding the Windows taskbar

Set `hide_taskbar = true` under `[general]` to hide the Windows taskbar while
tessera runs (for example if you use another taskbar or launcher). Windows are
then tiled over the full screen, and the taskbar comes back when tessera exits.
If Explorer re-shows it, tessera hides it again.

### Window rules

A rule matches windows on any combination of `exe`, `class`, and `title`.
Matching ignores case, and `title` matches a substring. A matching rule can:

- `float = true`: never tile the window.
- `ignore = true`: leave the window completely alone.
- `workspace = N`: open the window on workspace N.

Run `tessera --list-windows` to see the exe, class, and title of every window
you could write a rule for.

### Dispatchers

| Dispatcher | Effect |
| --- | --- |
| `exec <command>` | Run a command (`$terminal` expands to `general.terminal`) |
| `launcher` | Open the app launcher (or close it if open) |
| `killactive` | Close the focused window |
| `togglefloating` | Float or tile the focused window |
| `fullscreen [0\|1]` | `0`: cover the monitor. `1`: fill the tiling area |
| `togglesplit` | Flip the split the focused window sits in |
| `movefocus <l\|r\|u\|d>` | Focus the neighbour in a direction |
| `movewindow <l\|r\|u\|d>` | Swap with the neighbour in a direction |
| `resizeactive <l\|r\|u\|d>` | Move the nearest divider in a direction |
| `cyclenext` / `cycleprev` | Focus the next or previous window |
| `workspace <1-10\|e+N\|e-N>` | Switch workspace |
| `movetoworkspace <n>` | Move the window there and follow it |
| `movetoworkspacesilent <n>` | Move the window there and stay |
| `reload` | Reload the config |
| `exit` | Quit, restoring every window |

## Command line

```
tessera                  start the window manager
tessera --list-windows   print exe, class and title of tileable windows
tessera --check-config   validate the config file
tessera --config-path    print where the config file lives
tessera --default-config print the default config
tessera --restore        show windows a crashed session left hidden
tessera --version
tessera --help
```

The log is written to `%LOCALAPPDATA%\tessera\tessera.log`. When you start
tessera from a terminal, the log also streams there.

## Compatibility with other apps

tessera only intercepts the exact key combinations in your `[binds]`, plus
the mouse wheel while a bound modifier is held. Every other shortcut reaches
your apps unchanged. With `mod = "win"`, that includes Ctrl/Alt/Shift
shortcuts, Ctrl+scroll zoom and Shift+scroll. Windows' own Win shortcuts keep
working unless you bind the same combination. To keep one, such as Win+V for
clipboard history or Win+N for notifications, remove or rebind that line in
`[binds]`.

## Known limitations

- **Elevated windows.** Windows running as administrator (Task Manager,
  installers) can only be managed if tessera itself runs as administrator.
  Otherwise they are left untouched.
- **Alt shortcuts.** With `mod = "alt"`, tessera's bindings take precedence
  over apps' Alt shortcuts. Use `win` or `ctrl+alt` if that bothers you.
- **Win key.** Win+L always locks Windows and cannot be rebound. While an
  elevated window has focus, Win-key bindings only work if tessera also runs as
  administrator.
- **No animations yet.** Windows move instantly.
- **Hidden workspaces hide taskbar buttons.** Windows on workspaces that aren't
  shown are hidden, so their taskbar buttons disappear until you switch back.

## Roadmap

- `mod`+drag to move and resize floating windows
- Resizing tiles by dragging their edges
- Smooth workspace and window animations
- Per-workspace layouts (master/stack, monocle)
- Named and persistent workspaces, scratchpad
- IPC and a `tessera msg` command for scripting
- Bar modules (battery, volume, network, media)

## Building and contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) to get started and
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for how tessera works inside.

## License

[MIT](LICENSE)
