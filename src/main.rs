//! tessera — a minimal, keyboard-driven tiling window manager for Windows.
//!
//! The window manager runs on the main thread: a hidden control window
//! receives global hotkeys, WinEvent hooks and the keyboard hook thread post
//! events to it, and the message loop below feeds them into [`wm::Wm`].

#[macro_use]
mod log;
mod bar;
mod config;
mod fuzzy;
mod gfx;
mod keyboard;
mod launcher;
mod layout;
mod mouse;
mod theme;
mod win;
mod wm;

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicIsize, Ordering};
use windows_sys::Win32::Foundation::{GetLastError, BOOL, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::Console::{FreeConsole, GetConsoleProcessList, SetConsoleCtrlHandler};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

/// A window event from the WinEvent hook: `wparam` = event, `lparam` = hwnd.
pub const WM_APP_EVENT: u32 = WM_APP + 1;
/// Monitors or the work area changed.
pub const WM_APP_DISPLAY: u32 = WM_APP + 2;
/// A bar asks the window manager to act: `wparam` is a `BAR_*` kind.
pub const WM_APP_BAR: u32 = WM_APP + 3;
/// Leave the message loop and clean up (Ctrl+C, console close).
pub const WM_APP_QUIT: u32 = WM_APP + 4;
/// The keyboard hook matched a binding: `wparam` is the binding id.
pub const WM_APP_HOTKEY: u32 = WM_APP + 5;
/// The wallpaper, accent colour or light/dark mode may have changed.
pub const WM_APP_THEME: u32 = WM_APP + 6;
/// The mouse moved (coalesced; see `mouse.rs`).
pub const WM_APP_MOUSE: u32 = WM_APP + 7;

/// How often the control window checks whether the theme inputs changed
/// (slideshows and some wallpaper tools do not broadcast a change).
const THEME_CHECK_MS: u32 = 5000;
const THEME_TIMER: usize = 1;

/// `lparam` is the 0-based workspace index that was clicked.
pub const BAR_WORKSPACE: usize = 0;
/// `lparam` is +1 / -1 from scrolling over the bar.
pub const BAR_SCROLL: usize = 1;

/// The hidden control window that receives hotkeys and internal messages.
static CTRL_HWND: AtomicIsize = AtomicIsize::new(0);

/// Posts a message to the control window, safe to call from any callback.
pub fn post_to_ctrl(msg: u32, wparam: usize, lparam: isize) {
    let ctrl = CTRL_HWND.load(Ordering::Relaxed);
    if ctrl != 0 {
        unsafe { PostMessageW(ctrl as HWND, msg, wparam, lparam) };
    }
}

const USAGE: &str = "\
tessera - a minimal tiling window manager for Windows

USAGE:
    tessera                  start the window manager
    tessera --list-windows   print exe, class and title of tileable windows
    tessera --check-config   validate the config file
    tessera --config-path    print where the config file lives
    tessera --default-config print the default config
    tessera --restore        show windows a crashed session left hidden
    tessera --version        print the version
    tessera --help           print this help
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        std::process::exit(run_cli(&args));
    }
    detach_own_console();

    // Check for a running instance before opening (and truncating) the log,
    // so a second launch does not wipe the running instance's log.
    if !acquire_single_instance() {
        error!("tessera is already running");
        return;
    }
    log::init(&log::data_dir().join("tessera.log"));
    std::panic::set_hook(Box::new(|info| error!("{info}")));
    win::set_dpi_awareness();
    info!("tessera {} starting", env!("CARGO_PKG_VERSION"));

    let restored = wm::recover_hidden();
    if restored > 0 {
        warn!("showed {restored} window(s) left hidden by a previous session");
    }
    let (cfg, cfg_error) = match config::load() {
        Ok(cfg) => (cfg, None),
        Err(e) => {
            error!("config: {e}");
            (config::Config::default(), Some(e))
        }
    };

    gfx::init();
    bar::init();
    launcher::init();
    let ctrl = create_ctrl_window();
    if ctrl == 0 {
        error!("could not create the control window");
        return;
    }
    CTRL_HWND.store(ctrl, Ordering::Relaxed);
    unsafe { SetTimer(ctrl as HWND, THEME_TIMER, THEME_CHECK_MS, None) };
    let hooks = install_hooks();
    keyboard::start();
    mouse::start();
    unsafe { SetConsoleCtrlHandler(Some(console_handler), 1) };

    let mut wm = wm::Wm::new(cfg, ctrl);
    wm.start();
    if let Some(e) = cfg_error {
        wm.notify(&format!("config error, using defaults: {e}"));
    }
    wm::flush_hidden();

    run_message_loop(&mut wm);

    info!("shutting down");
    wm.shutdown();
    keyboard::stop();
    mouse::stop();
    for hook in hooks {
        unsafe { UnhookWinEvent(hook) };
    }
}

fn run_cli(args: &[String]) -> i32 {
    match args[0].as_str() {
        "-h" | "--help" => print!("{USAGE}"),
        "-V" | "--version" => println!("tessera {}", env!("CARGO_PKG_VERSION")),
        "--default-config" => print!("{}", config::DEFAULT_CONFIG),
        "--config-path" => println!("{}", config::config_path().display()),
        "--check-config" => match config::load() {
            Ok(_) => println!("{}: ok", config::config_path().display()),
            Err(e) => {
                eprintln!("{}: {e}", config::config_path().display());
                return 1;
            }
        },
        "--list-windows" => {
            win::set_dpi_awareness();
            println!("{:<28} {:<36} TITLE", "EXE", "CLASS");
            for h in win::top_level_windows().into_iter().filter(|&h| win::is_manageable(h)) {
                let info = win::WindowInfo::of(h);
                println!("{:<28} {:<36} {}", info.exe, info.class, info.title);
            }
        }
        "--restore" => println!("restored {} window(s)", wm::recover_hidden()),
        other => {
            eprintln!("unknown argument: {other}\n\n{USAGE}");
            return 2;
        }
    }
    0
}

/// When started from Explorer or autostart, Windows creates a console
/// window just for us; release it. When started from a terminal, keep it so
/// the log streams there.
fn detach_own_console() {
    let mut pids = [0u32; 2];
    if unsafe { GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) } == 1 {
        unsafe { FreeConsole() };
        log::disable_stderr();
    }
}

/// Returns false if another tessera instance owns the mutex.
fn acquire_single_instance() -> bool {
    let name = win::wide("Local\\tessera-window-manager");
    unsafe {
        let mutex = CreateMutexW(null(), 1, name.as_ptr());
        // The handle is intentionally leaked: it must live as long as we do.
        !mutex.is_null() && GetLastError() != ERROR_ALREADY_EXISTS
    }
}

unsafe extern "system" fn ctrl_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_DISPLAYCHANGE => {
            PostMessageW(hwnd, WM_APP_DISPLAY, 0, 0);
            0
        }
        WM_SETTINGCHANGE if wp == SPI_SETWORKAREA as usize => {
            PostMessageW(hwnd, WM_APP_DISPLAY, 0, 0);
            0
        }
        WM_SETTINGCHANGE if wp == SPI_SETDESKWALLPAPER as usize || is_color_set_change(lp) => {
            PostMessageW(hwnd, WM_APP_THEME, 0, 0);
            0
        }
        WM_TIMER if wp == THEME_TIMER => {
            PostMessageW(hwnd, WM_APP_THEME, 0, 0);
            0
        }
        WM_ENDSESSION if wp != 0 => {
            // Logging off or shutting down: make sure no window stays hidden.
            wm::restore_hidden();
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// `WM_SETTINGCHANGE` with "ImmersiveColorSet": light/dark mode or the accent
/// colour changed.
fn is_color_set_change(lp: LPARAM) -> bool {
    if lp == 0 {
        return false;
    }
    let ptr = lp as *const u16;
    let len = (0..64).take_while(|&i| unsafe { *ptr.add(i) } != 0).count();
    let name = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(ptr, len) });
    name == "ImmersiveColorSet"
}

/// An invisible top-level window: the target for hotkeys and posted
/// messages, and a receiver for display-change broadcasts.
fn create_ctrl_window() -> isize {
    let class = win::wide("tessera_ctrl");
    unsafe {
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(ctrl_proc);
        wc.hInstance = GetModuleHandleW(null());
        wc.lpszClassName = class.as_ptr();
        RegisterClassW(&wc);
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class.as_ptr(),
            class.as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            wc.hInstance,
            null(),
        ) as isize
    }
}

unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if !hwnd.is_null() && id_object == OBJID_WINDOW && id_child == CHILDID_SELF as i32 {
        post_to_ctrl(WM_APP_EVENT, event as usize, hwnd as isize);
    }
}

fn install_hooks() -> Vec<HWINEVENTHOOK> {
    let ranges = [
        (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
        (EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZEEND),
        (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
        (EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE),
        (EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_NAMECHANGE),
        (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
    ];
    ranges
        .iter()
        .map(|&(min, max)| unsafe {
            SetWinEventHook(
                min,
                max,
                null_mut(),
                Some(win_event_proc),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            )
        })
        .filter(|hook| !hook.is_null())
        .collect()
}

unsafe extern "system" fn console_handler(_ctrl_type: u32) -> BOOL {
    post_to_ctrl(WM_APP_QUIT, 0, 0);
    // Give the main thread time to restore hidden windows before the
    // process is torn down.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    1
}

fn run_message_loop(wm: &mut wm::Wm) {
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        let r = unsafe { GetMessageW(&mut msg, null_mut(), 0, 0) };
        if r == 0 || r == -1 || msg.message == WM_APP_QUIT {
            break;
        }
        let m = msg;
        let handled = matches!(
            m.message,
            WM_HOTKEY | WM_APP_HOTKEY | WM_APP_EVENT | WM_APP_DISPLAY | WM_APP_BAR | WM_APP_THEME | WM_APP_MOUSE
        );
        if !handled {
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            continue;
        }
        // A bug in one handler must not take the whole desktop down with it.
        let result = catch_unwind(AssertUnwindSafe(|| match m.message {
            WM_HOTKEY => wm.on_hotkey(m.wParam as i32, false),
            WM_APP_HOTKEY => wm.on_hotkey(m.wParam as i32, true),
            WM_APP_EVENT => wm.on_event(m.wParam as u32, m.lParam),
            WM_APP_DISPLAY => wm.on_display_change(),
            WM_APP_BAR => wm.on_bar(m.wParam, m.lParam),
            WM_APP_THEME => wm.on_theme_check(),
            WM_APP_MOUSE => {
                mouse::handled();
                wm.on_mouse_move();
            }
            _ => {}
        }));
        if result.is_err() {
            error!("recovered from a panic while handling message {:#x}", m.message);
        }
        wm::flush_hidden();
    }
}
