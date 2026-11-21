# Architecture

tessera is a single-threaded Win32 program. It does not inject code into other
processes. It watches the system through WinEvent hooks and moves windows with
ordinary `SetWindowPos` / `ShowWindow` calls.

```
                ┌────────────────────────── tessera.exe (one thread) ──────────────────────────┐
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
| `src/bar.rs` | The per-monitor floating bar: a layered window drawn with Direct2D and DirectWrite. |
| `src/keyboard.rs` | The low-level keyboard hook thread for Win-key bindings. |
| `src/mouse.rs` | The low-level mouse hook thread that reports pointer movement for focus follows mouse. |
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

There are ten global workspaces. Each monitor shows exactly one. Windows on a
workspace that no monitor shows are hidden with `SW_HIDE`. tessera prefers this
over cloaking or minimizing because it is reliable across apps and keeps those
windows out of Alt+Tab.

Hiding windows is risky: if tessera died, they would be invisible. So:

1. Every window tessera hides is recorded in `%LOCALAPPDATA%\tessera\hidden.txt`
   (handle plus window class), flushed after each handled message.
2. On a normal exit, Ctrl+C, logoff, or shutdown, every recorded window is shown again.
3. On startup, and with `tessera --restore`, windows still listed in the file
   are shown again. A window is only shown if its handle still exists and its
   class matches, so a recycled handle is never touched.

Show and hide are synchronous, except for hung windows, so that
`IsWindowVisible` is accurate when the queued SHOW/HIDE events are processed.
That lets the event handlers tell tessera's own visibility changes apart from
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

- Windows of elevated processes are ignored unless tessera is elevated. UIPI
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

When a binding includes Alt or Win, tessera sends an unassigned virtual key
(`0xE8`) right after it fires. Otherwise, releasing the modifier would look
like a lone Alt or Win press to the focused app, which then opens its menu bar
or the Start menu.

## Focus follows mouse

`src/mouse.rs` installs a `WH_MOUSE_LL` hook on its own thread. The hook does
no work beyond noting that the pointer moved. An atomic flag coalesces
movement into at most one queued `WM_APP_MOUSE` message, so a 1000 Hz mouse
costs the window manager one cheap check per message-loop turn, not a thousand.
`Wm::on_mouse_move` reads the cursor position, finds the root window under it
and focuses it if it is a managed window on a visible workspace. Nothing
happens while a mouse button is held. Keyboard-driven focus changes move the
pointer to the centre of the newly focused window's *target* rectangle,
because window moves are asynchronous. `SetCursorPos` does not go through the
input stream, so this pointer move never triggers the hook itself.

## Theming

`src/theme.rs` turns the wallpaper into a palette. WIC decodes the image and
downsamples it to about 96 px. If the configured file can't be read, tessera
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
The bar redraws only when its state changes, the hover changes, a notice
expires, or the clock's minute changes.
