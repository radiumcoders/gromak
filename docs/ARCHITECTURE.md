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
| `src/bar.rs` | The per-monitor status bar window: GDI text plus GDI+ anti-aliased pills, double-buffered. |
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

Bindings are registered with `RegisterHotKey` on the control window, using
`MOD_NOREPEAT`. When a binding includes Alt or Win, tessera sends an unassigned
virtual key (`0xE8`) right after the hotkey fires. Otherwise, releasing the
modifier would look like a lone Alt or Win press to the focused app, which then
opens its menu bar or the Start menu.
