# Architecture

gromak is a single-threaded Win32 program. It does not inject code into other
processes. It watches the system through WinEvent hooks and moves windows with
ordinary `SetWindowPos` / `ShowWindow` calls.

```
                ┌────────────────────────── gromak.exe (one thread) ──────────────────────────┐
 WinEvent hooks │  win_event_proc ──PostMessage(WM_APP_EVENT)──┐                                │
 (out of        │                                              ▼                                │
  context)      │  RegisterHotKey ──────── WM_HOTKEY ────► control window ──► message loop ──► Wm │
                │  bar clicks/scroll ── WM_APP_BAR ──────┘     (hidden)          (main.rs)  (wm.rs)│
                │  WM_DISPLAYCHANGE ── WM_APP_DISPLAY ───┘                                      │
                └───────────────────────────────────────────────────────────────────────────────┘
```

## Modules

| File | Responsibility |
| --- | --- |
| `src/main.rs` | Entry point, CLI, single-instance guard, the hidden control window, WinEvent hooks, and the message loop. |
| `src/wm.rs` | The window manager state machine: monitors, workspaces, managed windows, dispatchers, and event handlers. Also tracks hidden windows on disk. |
| `src/layout.rs` | Pure geometry: `Rect`, the dwindle tree, and directional neighbour search. No Win32, fully unit-tested. |
| `src/config.rs` | TOML schema, defaults, validation, and parsing of hotkeys and dispatcher commands. No Win32, fully unit-tested. |
| `src/bar.rs` | The per-monitor bar: widgets (modules) laid out in three groups, drawn into a layered window with Direct2D and DirectWrite. |
| `src/keyboard.rs` | The low-level keyboard hook thread for Win-key bindings. |
| `src/mouse.rs` | The low-level mouse hook thread that reports pointer movement for focus follows mouse. |
| `src/launcher.rs` | The app launcher: app index, search panel window and launching. |
| `src/fuzzy.rs` | Fuzzy matching and scoring for the launcher. Pure, unit-tested. |
| `src/calc.rs` | The launcher's calculator: a small expression evaluator. Pure, unit-tested. |
| `src/emoji.rs` | The emoji list and search for the launcher's picker. |
| `src/orc.rs` | The orc.pictures catalog client: fetching, validation, search and the download cache. |
| `src/power.rs` | Power and session actions for the launcher (lock, sleep, hibernate, sign out, restart, shut down). |
| `src/gfx.rs` | Shared Direct2D / DirectWrite / WIC plumbing for the bar and the launcher. |
| `src/status.rs` | Network and Bluetooth status and actions (scan, connect, switch radios), on a background thread through WinRT. |
| `src/panel.rs` | The widget panels (Wi-Fi, Bluetooth, system, sound): what they show, layout and drawing. |
| `src/icons.rs` | Vector icons for the bar and panels (Wi-Fi, Ethernet, Bluetooth, usage rings, speaker, padlock). |
| `src/sys.rs` | CPU and memory usage sampling and history. |
| `src/audio.rs` | Default speaker volume and mute through Core Audio. |
| `src/custom.rs` | `custom/*` widgets: run their commands on background threads and keep the output. |
| `src/theme.rs` | Derives colour palettes from the wallpaper or accent colour, and detects changes. |
| `src/win.rs` | Thin wrappers over the Win32 / DWM APIs, plus the "is this a normal app window?" heuristic. |
| `src/log.rs` | File and stderr logger. |

## Threading and re-entrancy

Everything runs on the main thread. Win32 delivers out-of-context WinEvents and
window messages while the thread is inside `GetMessageW`, and also during some
cross-process calls. To keep `Wm` free of re-entrancy:

- Hook callbacks and window procedures **only post messages**. They never touch `Wm`.
- The message loop handles `WM_HOTKEY` and the `WM_APP_*` messages **directly**,
  without `DispatchMessage`. So `Wm` is only ever borrowed at one point.
- The bar paints from its own `thread_local` copy of `BarState`, which `Wm`
  pushes after every change. A bar repaint triggered while `Wm` is mid-update
  therefore never needs `Wm`.
- Each handled message runs inside `catch_unwind`. A bug in one handler is
  logged and does not leave the user's windows hidden.

## Workspaces and hiding

Every monitor owns ten workspaces (`workspaces[monitor * 10 + n]`), and each
monitor shows exactly one of its own. Workspace keys resolve against the
active monitor: the one under the pointer when focus follows mouse is on,
otherwise the one with focus. When monitors are added or removed, workspaces
follow their monitor (matched by handle, then by position). The workspaces
of a vanished monitor merge into the same-numbered workspaces of the first
monitor. A visibility sync then shows the windows of every shown workspace.

Windows on a workspace that no monitor shows are hidden with `SW_HIDE`. gromak prefers this
over cloaking or minimizing because it is reliable across apps and keeps those
windows out of Alt+Tab.

Hiding windows is risky: if gromak died, they would be invisible. So:

1. Every window gromak hides is recorded in `%LOCALAPPDATA%\gromak\hidden.txt`
   (handle plus window class), flushed after each handled message.
2. On a normal exit, Ctrl+C, logoff, or shutdown, every recorded window is shown again.
3. On startup, and with `gromak --restore`, windows still listed in the file
   are shown again. A window is only shown if its handle still exists and its
   class matches, so a recycled handle is never touched.

Windows Snap (`snap.rs`) gets the same treatment. `SPI_SETWINARRANGEMENT` is
system-wide and persistent, so before turning it off gromak writes the
original value to `%LOCALAPPDATA%\gromak\snap.txt`. Exit, logoff and
`--restore` put it back from that file and delete it. A start that finds the
file left by a crash keeps the recorded value instead of the current
(already off) one.

Show and hide are synchronous, except for hung windows, so that
`IsWindowVisible` is accurate when the queued SHOW/HIDE events are processed.
That lets the event handlers tell gromak's own visibility changes apart from
an app hiding or re-showing itself.

## Layout

`layout::Dwindle` is a binary tree. Leaves are windows. Each split stores a
direction and a ratio. Inserting a window replaces the target leaf with a split
of (old, new), oriented along the target tile's longer side. Removing a window
collapses its parent into the sibling. Resizing walks up from the leaf to the
nearest split on the relevant axis and nudges its ratio.

`Wm::layout_ws` turns the tree into rectangles for the monitor's tiling area:

- the work area, or the full monitor if `hide_taskbar` is set,
- minus the bar,
- minus `gaps_out`,
- with `gaps_in` between tiles.

All of these are scaled by the monitor's DPI.

`win::set_window_rect` positions a window by its **visible** frame. It measures
the invisible resize borders (`DWMWA_EXTENDED_FRAME_BOUNDS` versus
`GetWindowRect`) and compensates, so gaps look identical for every app. Moves
are `SWP_ASYNCWINDOWPOS`, so a hung app cannot freeze the WM. Moves that cross
to a monitor with a different DPI are issued twice, because the app rescales
itself after the first one.

## Which windows get managed

`win::is_manageable` accepts visible, unminimized, uncloaked, top-level windows
that have a caption or a sizing frame and a title. It rejects tool windows,
no-activate windows, and known shell classes (taskbar, desktop, Start menu, and
so on). On top of that:

- Windows of elevated processes are ignored unless gromak is elevated. UIPI
  would block moving them anyway.
- Windows with an owner, or that cannot be resized, float.
- User rules can float a window, ignore it, or assign its workspace.
- Windows that make themselves fullscreen (no caption, covering the monitor)
  are left in place, so browser F11 and video players work.

## Hotkeys

Bindings take one of two routes:

- **`RegisterHotKey`** on the control window, with `MOD_NOREPEAT`. Used for every
  binding without Win. It keeps working while an elevated window has focus.
- **A `WH_KEYBOARD_LL` hook** (`src/keyboard.rs`). Used for every Win binding,
  because Windows reserves most Win+key combinations and `RegisterHotKey` fails
  for them. Also used for any other binding whose registration failed because
  another app owns it. The hook runs on its own thread with its own message
  loop, so a busy window manager can never lag the keyboard. It matches the
  key against the binding table, posts `WM_APP_HOTKEY` to the control window,
  and swallows both the key-down and the matching key-up. Win+L cannot be
  intercepted, because the secure desktop handles it.

When a binding includes Alt or Win, gromak sends an unassigned virtual key
(`0xE8`) right after it fires. Otherwise, releasing the modifier would look
like a lone Alt or Win press to the focused app, which then opens its menu bar
or the Start menu.

## Focus follows mouse

`src/mouse.rs` installs a `WH_MOUSE_LL` hook on its own thread. It also handles wheel bindings: a
wheel event with a bound modifier combination held is swallowed. Small
precision-touchpad deltas are accumulated into whole notches, and the
Win/Alt release is masked like a keyboard binding.

For pointer movement, The hook does
no work beyond noting that the pointer moved. An atomic flag coalesces
movement into at most one queued `WM_APP_MOUSE` message, so a 1000 Hz mouse
costs the window manager one cheap check per message-loop turn, not a thousand.
`Wm::on_mouse_move` reads the cursor position, finds the root window under it
and focuses it if it is a managed window on a visible workspace. Nothing
happens while a mouse button is held. Keyboard-driven focus changes move the
pointer to the centre of the newly focused window's *target* rectangle,
because window moves are asynchronous. `SetCursorPos` does not go through the
input stream, so this pointer move never triggers the hook itself.

## The launcher

The app list comes from enumerating `FOLDERID_AppsFolder` with `IShellItem` and
`IEnumShellItems`. It runs on a background COM thread at startup, and again
when the launcher opens if the list is older than 30 seconds. Each entry keeps
its display name and its parsing name, which is an AppUserModelID or a path.
Launching runs `ShellExecuteW("shell:AppsFolder<parsing name>")`, which
works for both desktop and Store apps. Icons are fetched lazily through
`IShellItemImageFactory`, converted with WIC to keep their alpha channel, and
cached per render target.

The panel is a layered window like the bar, but it takes keyboard focus.
Input handlers work out an `Action` while they hold the launcher state, and
only carry it out (show, hide, launch) after releasing it. Those calls
re-enter the window procedure synchronously, through `WM_ACTIVATE` for
example. If the state happens to be busy, input messages are re-posted
instead of dropped.

### orc.pictures

`src/orc.rs` downloads `https://orc.pictures/catalog.json` with
`URLDownloadToFileW`, so no HTTP library is needed. Slugs, revisions and
asset paths are validated before use, because they become file names in the
cache. Downloads go to a uniquely named temporary file and are kept only if
they start with the right magic bytes (`GIF8…`, JPEG `FF D8 FF`). Posters are
fetched in the background as rows become visible, and the launcher redraws
when each one arrives. A chosen GIF is put on the clipboard as `CF_HDROP`
and pasted with a synthesized Ctrl+V into the window that had focus before
the launcher opened.

## Animations

DWM's own show/hide transition is disabled on managed windows
(`DWMWA_TRANSITIONS_FORCEDISABLED`), because hiding and showing windows on
every workspace switch would make it play constantly. Instead, every
placement goes through `Wm::place`. It starts an ease-out-quint glide from
the window's current rectangle (or its mid-animation one) to the target. A
10 ms timer on the control window advances all running glides and stops when
nothing moves. A workspace switch first parks the incoming tiles 6% to the
side while they are still hidden, using a synchronous move so they don't
flash. The normal layout then glides them into place. The bar's active
indicator and the launcher animate their own layered windows the same way.

## Theming

`src/theme.rs` turns the wallpaper into a palette. WIC decodes the image and
downsamples it to about 96 px. If the configured file can't be read, gromak
uses Windows' `TranscodedWallpaper` copy, then the solid desktop colour.
Pixels are binned by hue and weighted by chroma squared, so vivid colours
dominate and grey, near-black and near-white pixels are ignored. The
heaviest window of three adjacent bins becomes the accent hue. The image's
average colour gives a gentle tint for the neutrals. The palette is built in
HSL with fixed lightness levels, so contrast stays readable whatever the
wallpaper, in either dark or light mode.

The inputs (wallpaper path and modification time, the transcoded copy, light
mode, accent colour and desktop colour) are fingerprinted. The control window
re-checks that fingerprint when Windows broadcasts `SPI_SETDESKWALLPAPER` or
`ImmersiveColorSet`, and every 5 seconds as a fallback. Colours are only
recomputed when the fingerprint changes.

## The bar

Each monitor's bar is a layered window with per-pixel alpha
(`WS_EX_LAYERED` + `UpdateLayeredWindow`). There is no `WM_PAINT`. A frame is
drawn with Direct2D, through a DC render target bound to a 32-bit DIB, and
DirectWrite. The frame is then pushed to the screen in one call. Fully
transparent pixels are click-through, so only the islands receive mouse input.
App icons are converted from `HICON` through WIC to keep their alpha channel.
Which widgets the bar shows, and where, comes from `modules_left`,
`modules_center` and `modules_right`. Each frame measures the widgets of the
two sides first and gives the centre group (usually the window title) the
room left between them. On a one-second timer each bar gathers the live data
its widgets show (time, network status, CPU and memory, volume, custom
outputs) and redraws only if that changed. CPU and memory are sampled on the
main thread (`GetSystemTimes`, `GlobalMemoryStatusEx`, cached for a second),
volume comes from Core Audio, and each `custom/*` widget runs its command on
its own thread, which stops when a reload changes the widgets.

The bar redraws only when its state changes, the hover changes, a notice
expires, the live data changes.

Text uses the built-in Goeist fonts unless the config names other ones.
They are embedded with `include_bytes!` and loaded through DirectWrite's
in-memory font loader into private font collections, so they are never
installed system-wide.

That status comes from `src/status.rs`. A background thread asks the WinRT
APIs every 3 seconds: `Windows.Devices.Radios` for whether Wi-Fi and Bluetooth
are on, `Windows.Networking.Connectivity` for the connection type, internet
access and Wi-Fi signal bars, and `Windows.Devices.Enumeration` for connected
Bluetooth devices (classic and Low Energy). A poll takes about 25 ms, which is
why it never runs on the main thread. The thread only starts when a bar shows
one of the icons. Each bar compares the latest result with what it last drew
on its one-second clock timer. The icons (`src/icons.rs`) are Direct2D paths rather
than font glyphs, so they look the same with any bar font.

Clicking a widget opens its panel in the popup window
(`src/popup.rs` hosts it, `src/panel.rs` draws it). While a panel is open the
status thread also lists the Wi-Fi networks in range (`Windows.Devices.WiFi`,
rescanning every 12 seconds) and the paired Bluetooth devices, and bumps a
generation counter that the popup checks on a 100 ms timer. Clicks become
requests sent over a channel to the status thread, which switches radios,
joins or leaves networks, and reports back through the same snapshot. Joining
a secured network first tries its saved profile; if that fails, the panel asks
for the password. Recent Windows 11 builds only let apps see the networks in
range when location services are on. Without them, the panel shows the
connected network (from its connection profile) and a button to the location
settings. Windows has no public API for connecting a paired Bluetooth device,
so the Bluetooth panel lists devices but leaves connecting to Settings.
