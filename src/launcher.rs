//! The app launcher: a rofi/wofi-style search panel (Win+Space by default).
//!
//! ```text
//!  ╭──────────────────────────────────────────────╮
//!  │  ⌕  term|                                    │
//!  │ ──────────────────────────────────────────── │
//!  │ ▌▣  Terminal                               ↵ │
//!  │  ▣  Windows Terminal                         │
//!  │  ▣  Task Manager                             │
//!  ╰──────────────────────────────────────────────╯
//! ```
//!
//! Apps come from the shell's AppsFolder (`shell:AppsFolder`, the same list
//! as Start's "All apps"), so desktop programs and Store apps are both
//! included, with their real icons. The list is gathered on a background
//! thread and refreshed when the launcher opens. Typing filters with
//! [`crate::fuzzy`]; apps you launch often float to the top. When nothing
//! matches, Enter runs the typed text as a command.
//!
//! Like the bar, the panel is a per-pixel-alpha layered window drawn with
//! Direct2D. Unlike the bar it takes keyboard focus while open, and closes
//! on Escape or as soon as it loses focus.

use crate::calc;
use crate::config::{FontSource, GifStyle};
use crate::emoji::{self, Emoji};
use crate::fuzzy;
use crate::gfx::{hbitmap_bitmap, label, rgba, rounded, Factories, Surface};
use crate::layout::Rect;
use crate::orc;
use crate::theme::Palette;
use crate::win::{self, wide};
use std::cell::RefCell;
use std::collections::HashMap;
use std::mem::zeroed;
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows::core::{IUnknown, Interface, PCWSTR, PWSTR};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap, ID2D1DCRenderTarget, ID2D1SolidColorBrush, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
    D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE,
};
use windows::Win32::Graphics::DirectWrite::{IDWriteTextFormat, DWRITE_FONT_WEIGHT, DWRITE_TEXT_RANGE};
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, FOLDERID_AppsFolder, IEnumShellItems, IShellItem, IShellItemImageFactory,
    SHCreateItemFromParsingName, SHGetKnownFolderItem, KF_FLAG_DEFAULT, SIGDN_NORMALDISPLAY,
    SIGDN_PARENTRELATIVEPARSING, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY,
};
use windows_numerics::Vector2;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_NEXT, VK_PRIOR, VK_RETURN, VK_SHIFT, VK_TAB, VK_UP,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const CLASS: &str = "tessera_launcher";
/// Rows shown at once.
const MAX_ROWS: usize = 8;
/// Results kept after filtering (reachable by scrolling).
const MAX_RESULTS: usize = 200;
/// Re-scan the app list when opening if the last scan is older than this.
const REINDEX_AFTER: Duration = Duration::from_secs(30);
const CARET_TIMER: usize = 1;
const ANIM_TIMER: usize = 2;
/// Length of the fade-and-slide when the launcher opens.
const OPEN_ANIMATION: Duration = Duration::from_millis(140);
const CARET_BLINK_MS: u32 = 530;
/// Posted to the launcher window when a background re-index finishes.
const WM_APPS_INDEXED: u32 = WM_APP + 60;

/// An entry from the AppsFolder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct App {
    pub name: String,
    /// Parsing name inside `shell:AppsFolder` (an AppUserModelID or a path).
    pub target: String,
}

static INDEX: Mutex<Option<(Vec<App>, Instant)>> = Mutex::new(None);
static INDEXING: AtomicBool = AtomicBool::new(false);
static OPEN: AtomicBool = AtomicBool::new(false);
static HWND_LAUNCHER: AtomicIsize = AtomicIsize::new(0);

/// Whether the launcher is currently showing.
pub fn is_open() -> bool {
    OPEN.load(Ordering::Relaxed)
}

/// Colours and metrics for one opening, derived from the theme and monitor.
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    pub palette: Palette,
    pub font: String,
    pub font_px: f32,
    pub scale: f32,
    pub emoji_font: FontSource,
    /// The configured default orc.pictures style.
    pub gif_style: GifStyle,
}

/// Device-dependent resources.
struct Gfx {
    target: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
    accent: ID2D1SolidColorBrush,
    query: IDWriteTextFormat,
    item: IDWriteTextFormat,
    strong: IDWriteTextFormat,
    hint: IDWriteTextFormat,
    emoji: IDWriteTextFormat,
    /// App icons by target (None = no icon available).
    icons: HashMap<String, Option<ID2D1Bitmap>>,
}

enum Row {
    /// A calculation and its formatted result.
    Calc(String),
    Emoji(&'static Emoji, fuzzy::Match),
    /// An orc.pictures GIF (index into `Launcher::gifs`).
    Gif(usize, fuzzy::Match),
    App(usize, fuzzy::Match),
    Run(String),
    Message(&'static str),
}

struct Launcher {
    hwnd: isize,
    style: Option<Style>,
    /// Where the panel goes: the work area of the monitor it opened on.
    area: Rect,
    rect: Rect,
    surface: Option<Surface>,
    surface_size: (i32, i32),
    gfx: Option<Gfx>,
    apps: Vec<App>,
    /// orc.pictures catalog snapshot, used in GIF mode.
    gifs: Vec<orc::Gif>,
    /// The query starts with "gif ": results are GIFs (taller rows).
    gif_mode: bool,
    /// orc.pictures style in use (Ctrl+S toggles it while the picker is open).
    gif_style: GifStyle,
    /// The configured style `gif_style` was last reset from.
    gif_style_config: GifStyle,
    history: HashMap<String, u32>,
    query: String,
    pending_surrogate: Option<u16>,
    rows: Vec<Row>,
    selected: usize,
    scroll: usize,
    caret_on: bool,
    previous_foreground: isize,
    /// Cursor position when the launcher opened: Windows sends a mouse move
    /// when a window appears under the pointer, which must not change the
    /// selection.
    open_cursor: (i32, i32),
    /// When the open animation started (None once finished).
    anim_start: Option<Instant>,
}

thread_local! {
    static STATE: RefCell<Option<Launcher>> = const { RefCell::new(None) };
}

/// What to do after an event, executed once the state borrow is released
/// (showing/hiding windows re-enters the window procedure synchronously).
enum Action {
    Nothing,
    Render,
    Close {
        restore_focus: bool,
    },
    Launch {
        app: Option<App>,
        command: Option<String>,
    },
    /// Copy text to the clipboard and close.
    Copy(String),
    /// Type text into the previously focused window (via the clipboard).
    Type(String),
    /// Download a GIF and paste it (as a file) into the previous window.
    PasteGif(orc::Gif, GifStyle),
}

// ----- app index ------------------------------------------------------------

fn take_string(result: windows::core::Result<PWSTR>) -> String {
    match result {
        Ok(p) if !p.is_null() => {
            let s = unsafe { p.to_string() }.unwrap_or_default();
            unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
            s
        }
        _ => String::new(),
    }
}

/// Entries that are documents or links rather than apps.
fn is_app(app: &App) -> bool {
    let t = app.target.to_lowercase();
    const DOCS: [&str; 10] = [".txt", ".chm", ".pdf", ".htm", ".html", ".url", ".rtf", ".md", ".ini", ".log"];
    !app.name.is_empty()
        && !t.starts_with("http:")
        && !t.starts_with("https:")
        && !DOCS.iter().any(|ext| t.ends_with(ext))
        && !app.name.to_lowercase().starts_with("uninstall")
}

/// Lists everything in `shell:AppsFolder`. Must run on a COM thread.
fn enumerate_apps() -> windows::core::Result<Vec<App>> {
    let mut apps = Vec::new();
    unsafe {
        let folder: IShellItem = SHGetKnownFolderItem(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None)?;
        let items: IEnumShellItems = folder.BindToHandler(None, &BHID_EnumItems)?;
        loop {
            let mut batch: [Option<IShellItem>; 32] = std::array::from_fn(|_| None);
            let mut fetched = 0u32;
            items.Next(&mut batch, Some(&mut fetched))?;
            if fetched == 0 {
                break;
            }
            for item in batch.iter().take(fetched as usize).flatten() {
                let app = App {
                    name: take_string(item.GetDisplayName(SIGDN_NORMALDISPLAY)),
                    target: take_string(item.GetDisplayName(SIGDN_PARENTRELATIVEPARSING)),
                };
                if is_app(&app) {
                    apps.push(app);
                }
            }
        }
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps.dedup();
    Ok(apps)
}

/// Re-scans the app list on a background thread (if not already doing so).
fn refresh_index() {
    if INDEXING.swap(true, Ordering::Relaxed) {
        return;
    }
    let spawned = std::thread::Builder::new().name("app-index".into()).spawn(|| {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        match enumerate_apps() {
            Ok(apps) => {
                info!("launcher: indexed {} apps", apps.len());
                *INDEX.lock().unwrap_or_else(|e| e.into_inner()) = Some((apps, Instant::now()));
            }
            Err(e) => warn!("launcher: could not list apps: {e}"),
        }
        INDEXING.store(false, Ordering::Relaxed);
        let hwnd = HWND_LAUNCHER.load(Ordering::Relaxed);
        if hwnd != 0 {
            unsafe { PostMessageW(hwnd as HWND, WM_APPS_INDEXED, 0, 0) };
        }
    });
    if spawned.is_err() {
        INDEXING.store(false, Ordering::Relaxed);
    }
}

/// Asks the launcher to refresh its results (new catalog data or a
/// downloaded thumbnail); safe to call from any thread.
fn notify_launcher() {
    let hwnd = HWND_LAUNCHER.load(Ordering::Relaxed);
    if hwnd != 0 {
        unsafe { PostMessageW(hwnd as HWND, WM_APPS_INDEXED, 0, 0) };
    }
}

fn index_snapshot() -> (Vec<App>, bool) {
    let guard = INDEX.lock().unwrap_or_else(|e| e.into_inner());
    match guard.as_ref() {
        Some((apps, at)) => (apps.clone(), at.elapsed() > REINDEX_AFTER),
        None => (Vec::new(), true),
    }
}

// ----- launch history (frecency-lite) ----------------------------------------

fn history_path() -> PathBuf {
    crate::log::data_dir().join("launcher_history.txt")
}

fn load_history() -> HashMap<String, u32> {
    std::fs::read_to_string(history_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .filter_map(|(count, target)| Some((target.to_string(), count.parse().ok()?)))
        .collect()
}

fn save_history(history: &HashMap<String, u32>) {
    let mut entries: Vec<_> = history.iter().collect();
    entries.sort_by(|a, b| b.1.cmp(a.1));
    let text: String = entries.iter().take(500).map(|(t, c)| format!("{c}\t{t}\n")).collect();
    let _ = std::fs::create_dir_all(crate::log::data_dir());
    if let Err(e) = std::fs::write(history_path(), text) {
        warn!("launcher: could not save history: {e}");
    }
}

// ----- public API -----------------------------------------------------------

/// Registers the window class, creates the (hidden) window and starts
/// indexing apps in the background.
pub fn init() {
    let class = wide(CLASS);
    let title = wide("tessera launcher");
    let hwnd = unsafe {
        let mut wc: WNDCLASSW = zeroed();
        wc.lpfnWndProc = Some(launcher_proc);
        wc.hInstance = GetModuleHandleW(null());
        wc.hCursor = LoadCursorW(null_mut(), IDC_ARROW);
        wc.lpszClassName = class.as_ptr();
        RegisterClassW(&wc);
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            GetModuleHandleW(null()),
            null(),
        )
    };
    if hwnd.is_null() {
        error!("launcher: could not create its window");
        return;
    }
    HWND_LAUNCHER.store(hwnd as isize, Ordering::Relaxed);
    STATE.with(|s| {
        *s.borrow_mut() = Some(Launcher {
            hwnd: hwnd as isize,
            style: None,
            area: Rect::default(),
            rect: Rect::default(),
            surface: None,
            surface_size: (0, 0),
            gfx: None,
            apps: Vec::new(),
            gifs: Vec::new(),
            gif_mode: false,
            gif_style: GifStyle::Orc,
            gif_style_config: GifStyle::Orc,
            history: load_history(),
            query: String::new(),
            pending_surrogate: None,
            rows: Vec::new(),
            selected: 0,
            scroll: 0,
            caret_on: true,
            previous_foreground: 0,
            open_cursor: (0, 0),
            anim_start: None,
        })
    });
    refresh_index();
}

/// Opens the launcher on the given work area, or closes it if it is open.
pub fn toggle(area: Rect, style: Style) {
    if is_open() {
        close(true);
        return;
    }
    let (apps, stale) = index_snapshot();
    if stale {
        refresh_index();
    }
    let hwnd = with_state(|l| {
        if l.style.as_ref() != Some(&style) {
            l.gfx = None;
        }
        // Keep a style switched with Ctrl+S, unless the config default changed.
        if style.gif_style != l.gif_style_config {
            l.gif_style = style.gif_style;
            l.gif_style_config = style.gif_style;
        }
        l.style = Some(style);
        l.area = area;
        l.apps = apps;
        l.query.clear();
        l.pending_surrogate = None;
        l.caret_on = true;
        l.previous_foreground = win::foreground();
        l.open_cursor = win::cursor_pos();
        l.anim_start = Some(Instant::now());
        l.refilter();
        l.hwnd
    });
    let Some(hwnd) = hwnd else { return };
    OPEN.store(true, Ordering::Relaxed);
    render();
    unsafe {
        ShowWindow(hwnd as HWND, SW_SHOW);
        SetTimer(hwnd as HWND, CARET_TIMER, CARET_BLINK_MS, None);
        SetTimer(hwnd as HWND, ANIM_TIMER, 10, None);
    }
    win::focus(hwnd);
}

fn with_state<R>(f: impl FnOnce(&mut Launcher) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(f)))
}

fn close(restore_focus: bool) {
    if !OPEN.swap(false, Ordering::Relaxed) {
        return;
    }
    let Some((hwnd, previous)) = with_state(|l| (l.hwnd, l.previous_foreground)) else { return };
    unsafe {
        KillTimer(hwnd as HWND, CARET_TIMER);
        KillTimer(hwnd as HWND, ANIM_TIMER);
        ShowWindow(hwnd as HWND, SW_HIDE);
    }
    if restore_focus && previous != 0 && win::is_window(previous) {
        win::focus(previous);
    }
}

fn launch(app: Option<App>, command: Option<String>) {
    close(false);
    if let Some(app) = app {
        with_state(|l| {
            *l.history.entry(app.target.clone()).or_insert(0) += 1;
            save_history(&l.history);
        });
        let path = wide(&format!("shell:AppsFolder\\{}", app.target));
        let verb = wide("open");
        let result = unsafe { ShellExecuteW(null_mut(), verb.as_ptr(), path.as_ptr(), null(), null(), SW_SHOWNORMAL) };
        if result as isize > 32 {
            info!("launcher: opened {} ({})", app.name, app.target);
        } else {
            warn!("launcher: could not open {} ({}): code {}", app.name, app.target, result as isize);
        }
    } else if let Some(command) = command {
        match win::run_command(&command) {
            Ok(()) => info!("launcher: ran {command}"),
            Err(e) => warn!("launcher: could not run {command}: {e}"),
        }
    }
}

fn run(action: Action) {
    match action {
        Action::Nothing => {}
        Action::Render => render(),
        Action::Close { restore_focus } => close(restore_focus),
        Action::Launch { app, command } => launch(app, command),
        Action::Copy(text) => {
            close(true);
            if win::set_clipboard_text(&text) {
                info!("launcher: copied {text}");
            }
        }
        Action::PasteGif(gif, style) => {
            close(true);
            std::thread::spawn(move || match orc::fetch_gif(&gif, style) {
                Ok(path) => {
                    if win::set_clipboard_files(&[path]) {
                        std::thread::sleep(Duration::from_millis(80));
                        win::send_paste();
                        info!("launcher: pasted orc.pictures GIF {}", gif.slug);
                    }
                }
                Err(e) => warn!("launcher: could not get GIF {}: {e}", gif.slug),
            });
        }
        Action::Type(text) => {
            close(true);
            // Paste through the clipboard, then put back what was there.
            let previous = win::clipboard_text();
            if !win::set_clipboard_text(&text) {
                return;
            }
            std::thread::spawn(move || {
                // Give the previous window a moment to take focus back.
                std::thread::sleep(Duration::from_millis(120));
                win::send_paste();
                std::thread::sleep(Duration::from_millis(400));
                if let Some(previous) = previous {
                    win::set_clipboard_text(&previous);
                }
            });
        }
    }
}

// ----- state ----------------------------------------------------------------

impl Launcher {
    fn refilter(&mut self) {
        self.selected = 0;
        self.scroll = 0;
        let query = self.query.trim();
        // "gif ..." searches orc.pictures.
        let lower = query.to_lowercase();
        self.gif_mode = lower.starts_with("gif ") || lower == "gif";
        if self.gif_mode {
            let search = query.get(3..).unwrap_or("").trim();
            self.gifs = orc::catalog();
            orc::refresh_catalog(notify_launcher);
            self.rows =
                orc::search(&self.gifs, search).into_iter().take(MAX_RESULTS).map(|(i, m)| Row::Gif(i, m)).collect();
            if self.rows.is_empty() {
                self.rows.push(Row::Message(if self.gifs.is_empty() {
                    "Loading orc.pictures…"
                } else {
                    "No GIFs found"
                }));
            }
            return;
        }
        let calculation = calc::evaluate_query(query).map(|v| Row::Calc(calc::format(v)));
        // ":..." is the emoji picker.
        if let Some(rest) = query.strip_prefix(':') {
            self.rows = emoji::search(rest).into_iter().take(MAX_RESULTS).map(|(e, m)| Row::Emoji(e, m)).collect();
            if self.rows.is_empty() {
                self.rows.push(Row::Message("No emoji found"));
            }
            return;
        }
        // "=..." is calculator-only.
        if query.starts_with('=') {
            self.rows = vec![calculation.unwrap_or(Row::Message("Not a valid calculation yet"))];
            return;
        }
        let mut scored: Vec<(i32, usize, fuzzy::Match)> = self
            .apps
            .iter()
            .enumerate()
            .filter_map(|(i, app)| {
                let m = fuzzy::score(query, &app.name)?;
                let uses = self.history.get(&app.target).copied().unwrap_or(0).min(30) as i32;
                let bonus = if query.is_empty() { uses * 100 } else { uses * 3 };
                Some((m.score + bonus, i, m))
            })
            .collect();
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0).then_with(|| self.apps[a.1].name.to_lowercase().cmp(&self.apps[b.1].name.to_lowercase()))
        });
        self.rows = calculation.into_iter().collect();
        self.rows.extend(scored.into_iter().take(MAX_RESULTS).map(|(_, i, m)| Row::App(i, m)));
        if self.rows.is_empty() {
            if !query.is_empty() {
                self.rows.push(Row::Run(query.to_string()));
            } else if self.apps.is_empty() {
                self.rows.push(Row::Message("Indexing apps…"));
            }
        }
    }

    fn select(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let n = self.rows.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + MAX_ROWS {
            self.scroll = self.selected + 1 - MAX_ROWS;
        }
        self.caret_on = true;
    }

    fn activate(&self, index: usize) -> Action {
        match self.rows.get(index) {
            Some(Row::App(i, _)) => Action::Launch { app: Some(self.apps[*i].clone()), command: None },
            Some(Row::Run(cmd)) => Action::Launch { app: None, command: Some(cmd.clone()) },
            Some(Row::Calc(value)) => Action::Copy(value.clone()),
            Some(Row::Emoji(e, _)) => Action::Type(e.glyph.to_string()),
            Some(Row::Gif(i, _)) => Action::PasteGif(self.gifs[*i].clone(), self.gif_style),
            _ => Action::Nothing,
        }
    }

    fn type_text(&mut self, text: &str) {
        self.query.push_str(text);
        self.caret_on = true;
        self.refilter();
    }

    fn on_char(&mut self, unit: u16) -> Action {
        match unit {
            0x08 => {
                self.query.pop();
            }
            0x7F => {
                // Ctrl+Backspace: delete the last word.
                let trimmed = self.query.trim_end().len();
                self.query.truncate(trimmed);
                let cut = self.query.rfind(char::is_whitespace).map_or(0, |i| i + 1);
                self.query.truncate(cut);
            }
            0x16 => {
                if let Some(text) = win::clipboard_text() {
                    let line = text.lines().next().unwrap_or_default().to_string();
                    self.type_text(&line);
                }
                return Action::Render;
            }
            0xD800..=0xDBFF => {
                self.pending_surrogate = Some(unit);
                return Action::Nothing;
            }
            0xDC00..=0xDFFF => {
                if let Some(high) = self.pending_surrogate.take() {
                    let s = String::from_utf16_lossy(&[high, unit]);
                    self.type_text(&s);
                }
                return Action::Render;
            }
            u if u < 0x20 => return Action::Nothing,
            u => {
                let s = String::from_utf16_lossy(&[u]);
                self.type_text(&s);
                return Action::Render;
            }
        }
        self.caret_on = true;
        self.refilter();
        Action::Render
    }

    fn on_key(&mut self, vk: u16) -> Action {
        let ctrl = unsafe { GetKeyState(VK_CONTROL as i32) } < 0;
        let shift = unsafe { GetKeyState(VK_SHIFT as i32) } < 0;
        let step = match vk {
            VK_ESCAPE => return Action::Close { restore_focus: true },
            VK_RETURN => {
                // Shift+Enter on a GIF copies its link instead of pasting it.
                if let (true, Some(Row::Gif(i, _))) = (shift, self.rows.get(self.selected)) {
                    return Action::Copy(orc::gif_url(&self.gifs[*i], self.gif_style));
                }
                return self.activate(self.selected);
            }
            VK_DOWN => 1,
            VK_UP => -1,
            VK_TAB => {
                if shift {
                    -1
                } else {
                    1
                }
            }
            VK_NEXT => MAX_ROWS as isize,
            VK_PRIOR => -(MAX_ROWS as isize),
            v if ctrl && v == b'S' as u16 && self.gif_mode => {
                self.gif_style = self.gif_style.toggled();
                return Action::Render;
            }
            v if ctrl && (v == b'J' as u16 || v == b'N' as u16) => 1,
            v if ctrl && (v == b'K' as u16 || v == b'P' as u16) => -1,
            _ => return Action::Nothing,
        };
        self.select(step);
        Action::Render
    }

    /// Pushes the last drawn frame, applying the open animation.
    fn present(&self) {
        let Some(surface) = &self.surface else { return };
        let (alpha, slide) = self.animation();
        let rect = Rect { y: self.rect.y + slide, ..self.rect };
        unsafe { crate::gfx::present(self.hwnd, surface, rect, alpha) };
    }

    /// Layout metrics in pixels: (width, search height, row height, padding).
    fn metrics(&self) -> (f32, f32, f32, f32) {
        let s = self.style.as_ref().map_or(1.0, |st| st.scale);
        let width = (660.0 * s).min(self.area.w as f32 - 40.0 * s).max(200.0);
        let row_h = if self.gif_mode { 64.0 } else { 46.0 };
        (width.round(), (60.0 * s).round(), (row_h * s).round(), (10.0 * s).round())
    }

    fn visible_rows(&self) -> usize {
        self.rows.len().min(MAX_ROWS)
    }

    fn list_top(&self) -> f32 {
        let (_, search_h, _, pad) = self.metrics();
        pad + search_h + if self.rows.is_empty() { 0.0 } else { (6.0 * self.scale()).round() }
    }

    fn scale(&self) -> f32 {
        self.style.as_ref().map_or(1.0, |st| st.scale)
    }

    fn height(&self) -> f32 {
        let (_, _, row_h, pad) = self.metrics();
        self.list_top() + self.visible_rows() as f32 * row_h + pad
    }

    /// Transparent margin around the panel that holds its drop shadow.
    fn margin(&self) -> f32 {
        (28.0 * self.scale()).round()
    }

    /// The result row under a window-relative point.
    fn row_at(&self, y: f32) -> Option<usize> {
        let (_, _, row_h, _) = self.metrics();
        let y = y - self.margin();
        let top = self.list_top();
        if y < top {
            return None;
        }
        let i = ((y - top) / row_h) as usize;
        (i < self.visible_rows()).then_some(self.scroll + i)
    }
}

// ----- window procedure -----------------------------------------------------

unsafe extern "system" fn launcher_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let action = match msg {
        WM_ACTIVATE if (wp & 0xFFFF) as u32 == WA_INACTIVE => Some(Action::Close { restore_focus: false }),
        WM_KEYDOWN | WM_SYSKEYDOWN => with_state(|l| l.on_key(wp as u16)),
        WM_CHAR => with_state(|l| l.on_char(wp as u16)),
        WM_TIMER if wp == ANIM_TIMER => {
            with_state(|l| {
                l.present();
                if l.anim_start.is_some_and(|t| t.elapsed() >= OPEN_ANIMATION) {
                    l.anim_start = None;
                    KillTimer(hwnd, ANIM_TIMER);
                }
            });
            return 0;
        }
        WM_TIMER if wp == CARET_TIMER => with_state(|l| {
            l.caret_on = !l.caret_on;
            Action::Render
        }),
        WM_MOUSEMOVE => with_state(|l| {
            if win::cursor_pos() == l.open_cursor {
                return Action::Nothing;
            }
            l.open_cursor = (i32::MIN, i32::MIN);
            let y = ((lp >> 16) & 0xFFFF) as i16 as f32;
            match l.row_at(y) {
                Some(i) if i != l.selected => {
                    l.selected = i;
                    Action::Render
                }
                _ => Action::Nothing,
            }
        }),
        WM_LBUTTONDOWN => with_state(|l| {
            let y = ((lp >> 16) & 0xFFFF) as i16 as f32;
            l.row_at(y).map_or(Action::Nothing, |i| l.activate(i))
        }),
        WM_MOUSEWHEEL => with_state(|l| {
            let delta = ((wp >> 16) & 0xFFFF) as u16 as i16;
            l.select(if delta > 0 { -1 } else { 1 });
            Action::Render
        }),
        WM_APPS_INDEXED => {
            let (apps, _) = index_snapshot();
            with_state(|l| {
                if !is_open() {
                    return Action::Nothing;
                }
                let selected_target = match l.rows.get(l.selected) {
                    Some(Row::App(i, _)) => Some(l.apps[*i].target.clone()),
                    _ => None,
                };
                l.apps = apps;
                l.refilter();
                // Keep the selection on the same app if it is still listed.
                if let Some(target) = selected_target {
                    if let Some(pos) =
                        l.rows.iter().position(|r| matches!(r, Row::App(i, _) if l.apps[*i].target == target))
                    {
                        l.select(pos as isize);
                    }
                }
                Action::Render
            })
        }
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    };
    match action {
        Some(action) => run(action),
        // The state is busy (re-entrant call); retry input later.
        None if matches!(msg, WM_KEYDOWN | WM_CHAR | WM_LBUTTONDOWN) => {
            PostMessageW(hwnd, msg, wp, lp);
        }
        None => {}
    }
    0
}

// ----- drawing --------------------------------------------------------------

fn render() {
    crate::gfx::with_factories(|f| {
        with_state(|l| unsafe {
            if let Err(e) = l.draw(f) {
                warn!("launcher: draw failed: {e}");
                l.gfx = None;
                return;
            }
            l.present();
        });
    });
}

unsafe fn create_gfx(f: &Factories, style: &Style) -> windows::core::Result<Gfx> {
    let (target, brush) = crate::gfx::render_target(f)?;
    let accent = target.CreateSolidColorBrush(&rgba(style.palette.accent, 1.0), None)?;
    Ok(Gfx {
        target,
        brush,
        accent,
        query: crate::gfx::text_format(f, &style.font, 400, style.font_px * 1.35)?,
        item: crate::gfx::text_format(f, &style.font, 450, style.font_px * 1.12)?,
        strong: crate::gfx::text_format(f, &style.font, 600, style.font_px * 1.25)?,
        hint: crate::gfx::text_format(f, &style.font, 450, style.font_px * 0.95)?,
        emoji: emoji_format(f, style)?,
        icons: HashMap::new(),
    })
}

/// The emoji text format: the configured font file or family, falling back
/// to Segoe UI Emoji if a font file cannot be loaded.
unsafe fn emoji_format(f: &Factories, style: &Style) -> windows::core::Result<IDWriteTextFormat> {
    let px = style.font_px * 1.6;
    match &style.emoji_font {
        FontSource::File(path) => crate::gfx::text_format_from_file(f, path, 400, px).or_else(|e| {
            warn!("launcher: could not load emoji font {}: {e}", path.display());
            crate::gfx::text_format(f, "Segoe UI Emoji", 400, px)
        }),
        FontSource::Family(family) => crate::gfx::text_format(f, family, 400, px),
    }
}

/// Extracts an app's icon from the shell at `px` pixels.
unsafe fn app_icon(f: &Factories, target: &ID2D1DCRenderTarget, app: &str, px: i32) -> Option<ID2D1Bitmap> {
    let path = wide(&format!("shell:AppsFolder\\{app}"));
    let factory: IShellItemImageFactory = SHCreateItemFromParsingName(PCWSTR(path.as_ptr()), None).ok()?;
    let size = windows::Win32::Foundation::SIZE { cx: px, cy: px };
    let hbitmap = factory.GetImage(size, SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK).ok()?;
    let bitmap = hbitmap_bitmap(f, target, hbitmap.0 as isize);
    windows_sys::Win32::Graphics::Gdi::DeleteObject(hbitmap.0 as _);
    bitmap
}

/// Converts char positions in `text` to UTF-16 offsets (for DirectWrite).
fn utf16_offsets(text: &str, positions: &[usize]) -> Vec<u32> {
    let mut offsets = Vec::with_capacity(positions.len());
    let mut utf16 = 0u32;
    for (i, c) in text.chars().enumerate() {
        if positions.contains(&i) {
            offsets.push(utf16);
        }
        utf16 += c.len_utf16() as u32;
    }
    offsets
}

impl Launcher {
    /// Opacity (0–255) and upward slide offset (pixels) of the open animation.
    fn animation(&self) -> (u8, i32) {
        let Some(start) = self.anim_start else { return (255, 0) };
        let t = (start.elapsed().as_secs_f32() / OPEN_ANIMATION.as_secs_f32()).clamp(0.0, 1.0);
        let eased = 1.0 - (1.0 - t).powi(3);
        ((eased * 255.0).round() as u8, ((1.0 - eased) * 10.0 * self.scale()).round() as i32)
    }

    unsafe fn draw(&mut self, f: &Factories) -> windows::core::Result<()> {
        let Some(style) = self.style.clone() else { return Ok(()) };
        let s = style.scale;
        let (w, search_h, row_h, pad) = self.metrics();
        let h = self.height();
        let top = self.list_top();
        let visible = self.visible_rows();
        let m = self.margin();
        let max_h = pad + search_h + 6.0 * s + MAX_ROWS as f32 * row_h + pad;

        // The window covers the panel plus a margin for the shadow. The
        // panel sits about a fifth of the way down the monitor.
        let area = self.area;
        self.rect = Rect::new(
            area.x + ((area.w as f32 - w) / 2.0 - m) as i32,
            area.y + (area.h as f32 * 0.2 - m) as i32,
            (w + 2.0 * m) as i32,
            (h + 2.0 * m).ceil() as i32,
        );
        let size = ((w + 2.0 * m) as i32, (max_h + 2.0 * m).ceil() as i32);
        if self.surface.is_none() || self.surface_size != size {
            self.surface = Surface::new(size.0, size.1);
            self.surface_size = size;
        }
        let Some(surface) = &self.surface else { return Ok(()) };
        if self.gfx.is_none() {
            self.gfx = Some(create_gfx(f, &style)?);
        }
        let gfx = self.gfx.as_mut().expect("just created");
        let p = &style.palette;
        let light = crate::theme::rgb_to_hsl([
            (p.background & 0xFF) as u8,
            ((p.background >> 8) & 0xFF) as u8,
            ((p.background >> 16) & 0xFF) as u8,
        ])
        .l > 0.5;

        crate::gfx::bind(&gfx.target, surface, size.0, size.1)?;
        gfx.target.BeginDraw();
        gfx.target.Clear(Some(&rgba(0, 0.0)));
        let target = &gfx.target;
        let brush = &gfx.brush;
        let radius = (16.0 * s).round();

        // Soft drop shadow: stacked, expanding, faint rounded rectangles.
        let shadow_alpha = if light { 0.012 } else { 0.03 };
        let steps = 14;
        for i in 0..steps {
            let grow = m * 0.85 * (i + 1) as f32 / steps as f32;
            let dy = 6.0 * s;
            brush.SetColor(&rgba(0, shadow_alpha));
            target.FillRoundedRectangle(
                &rounded(m - grow, m - grow + dy, m + w + grow, m + h + grow + dy, radius + grow),
                brush,
            );
        }

        // Panel.
        let panel = rounded(m + 0.5, m + 0.5, m + w - 0.5, m + h - 0.5, radius);
        brush.SetColor(&rgba(p.background, 1.0));
        target.FillRoundedRectangle(&panel, brush);
        brush.SetColor(&rgba(p.foreground, if light { 0.10 } else { 0.08 }));
        target.DrawRoundedRectangle(&panel, brush, 1.0, None);

        // Search field: magnifier, query or placeholder, blinking caret.
        let center_y = m + pad + search_h / 2.0;
        let glass_x = m + pad + 22.0 * s;
        let glass_r = 6.5 * s;
        brush.SetColor(&rgba(p.foreground, 0.45));
        let glass_cy = center_y - 1.5 * s;
        let ellipse = D2D1_ELLIPSE { point: Vector2 { X: glass_x, Y: glass_cy }, radiusX: glass_r, radiusY: glass_r };
        target.DrawEllipse(&ellipse, brush, 1.6 * s, None);
        target.DrawLine(
            Vector2 { X: glass_x + glass_r * 0.72, Y: glass_cy + glass_r * 0.72 },
            Vector2 { X: glass_x + glass_r * 1.5, Y: glass_cy + glass_r * 1.5 },
            brush,
            1.6 * s,
            None,
        );
        let text_x = m + pad + 46.0 * s;
        let mut text_w = w - (text_x - m) - pad - 16.0 * s;
        // GIF mode: show which orc.pictures style is active.
        if self.gif_mode {
            let text = format!("{}  ·  Ctrl+S", if self.gif_style == GifStyle::Videorc { "Videorc" } else { "Orc" });
            if let Some(style_label) = label(f, &gfx.hint, &text, text_w / 2.0, search_h) {
                let x = m + w - pad - 16.0 * s - style_label.width;
                brush.SetColor(&rgba(p.accent, 0.9));
                target.DrawTextLayout(
                    Vector2 { X: x, Y: m + pad },
                    &style_label.layout,
                    brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
                text_w -= style_label.width + 16.0 * s;
            }
        }
        let mut caret_x = text_x;
        let query_origin = Vector2 { X: text_x, Y: m + pad };
        if self.query.is_empty() {
            if let Some(placeholder) =
                label(f, &gfx.query, "Search apps  ·  2+2  ·  :emoji  ·  gif orc", text_w, search_h)
            {
                brush.SetColor(&rgba(p.foreground, 0.32));
                target.DrawTextLayout(query_origin, &placeholder.layout, brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
            }
        } else if let Some(query) = label(f, &gfx.query, &self.query, text_w, search_h) {
            brush.SetColor(&rgba(p.foreground, 1.0));
            target.DrawTextLayout(query_origin, &query.layout, brush, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
            caret_x = text_x + query.width + 1.0;
        }
        if self.caret_on {
            let caret_h = style.font_px * 1.55;
            brush.SetColor(&rgba(p.accent, 1.0));
            target.FillRoundedRectangle(
                &rounded(caret_x, center_y - caret_h / 2.0, caret_x + (2.0 * s).max(1.5), center_y + caret_h / 2.0, s),
                brush,
            );
        }

        if !self.rows.is_empty() {
            let y = m + pad + search_h + (3.0 * s).round() - 0.5;
            brush.SetColor(&rgba(p.foreground, 0.07));
            target.DrawLine(
                Vector2 { X: m + pad + 12.0 * s, Y: y },
                Vector2 { X: m + w - pad - 12.0 * s, Y: y },
                brush,
                1.0,
                None,
            );
        }

        // Results.
        let icon_size = (26.0 * s).round();
        let icon_x = m + pad + 14.0 * s;
        let name_x = icon_x + icon_size + 14.0 * s;
        let scrollbar = self.rows.len() > MAX_ROWS;
        let right = m + w - pad - if scrollbar { 18.0 * s } else { 12.0 * s };
        let name_w = right - name_x;
        for (slot, index) in (self.scroll..self.scroll + visible).enumerate() {
            let y0 = m + top + slot as f32 * row_h;
            let selected = index == self.selected;
            if selected {
                gfx.brush.SetColor(&rgba(p.accent, if light { 0.16 } else { 0.18 }));
                gfx.target.FillRoundedRectangle(
                    &rounded(m + pad, y0 + 1.0, m + w - pad, y0 + row_h - 1.0, (10.0 * s).round()),
                    &gfx.brush,
                );
            }
            let text_alpha = if selected { 1.0 } else { 0.86 };
            match &self.rows[index] {
                Row::App(i, matched) => {
                    let app = &self.apps[*i];
                    if !gfx.icons.contains_key(&app.target) {
                        let icon = app_icon(f, &gfx.target, &app.target, (32.0 * s).round() as i32);
                        gfx.icons.insert(app.target.clone(), icon);
                    }
                    if let Some(Some(icon)) = gfx.icons.get(&app.target) {
                        let iy = y0 + (row_h - icon_size) / 2.0;
                        let dest =
                            D2D_RECT_F { left: icon_x, top: iy, right: icon_x + icon_size, bottom: iy + icon_size };
                        gfx.target.DrawBitmap(icon, Some(&dest), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
                    }
                    if let Some(name) = label(f, &gfx.item, &app.name, name_w, row_h) {
                        // Matched characters: accent-coloured and a touch bolder.
                        let accent_effect: IUnknown = gfx.accent.cast()?;
                        for start in utf16_offsets(&app.name, &matched.positions) {
                            let range = DWRITE_TEXT_RANGE { startPosition: start, length: 1 };
                            name.layout.SetFontWeight(DWRITE_FONT_WEIGHT(600), range)?;
                            name.layout.SetDrawingEffect(&accent_effect, range)?;
                        }
                        gfx.brush.SetColor(&rgba(p.foreground, text_alpha));
                        gfx.target.DrawTextLayout(
                            Vector2 { X: name_x, Y: y0 },
                            &name.layout,
                            &gfx.brush,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                    }
                }
                Row::Calc(value) => {
                    if let Some(eq) = label(f, &gfx.strong, "=", icon_size, row_h) {
                        gfx.brush.SetColor(&rgba(p.accent, 1.0));
                        let x = icon_x + (icon_size - eq.width) / 2.0;
                        gfx.target.DrawTextLayout(
                            Vector2 { X: x, Y: y0 },
                            &eq.layout,
                            &gfx.brush,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                    }
                    let hint = label(f, &gfx.hint, "Enter to copy", name_w, row_h);
                    let hint_w = hint.as_ref().map_or(0.0, |h| h.width + 12.0 * s);
                    if let Some(result) = label(f, &gfx.strong, value, name_w - hint_w, row_h) {
                        gfx.brush.SetColor(&rgba(p.foreground, 1.0));
                        gfx.target.DrawTextLayout(
                            Vector2 { X: name_x, Y: y0 },
                            &result.layout,
                            &gfx.brush,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                    }
                    if let (Some(hint), true) = (hint, selected) {
                        gfx.brush.SetColor(&rgba(p.foreground, 0.42));
                        gfx.target.DrawTextLayout(
                            Vector2 { X: right - hint.width, Y: y0 },
                            &hint.layout,
                            &gfx.brush,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                    }
                }
                Row::Emoji(e, matched) => {
                    if let Some(glyph) = label(f, &gfx.emoji, e.glyph, icon_size * 2.0, row_h) {
                        let x = icon_x + (icon_size - glyph.width) / 2.0;
                        gfx.brush.SetColor(&rgba(p.foreground, 1.0));
                        gfx.target.DrawTextLayout(
                            Vector2 { X: x, Y: y0 },
                            &glyph.layout,
                            &gfx.brush,
                            D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
                        );
                    }
                    if let Some(name) = label(f, &gfx.item, e.name, name_w, row_h) {
                        let accent_effect: IUnknown = gfx.accent.cast()?;
                        for start in utf16_offsets(e.name, &matched.positions) {
                            let range = DWRITE_TEXT_RANGE { startPosition: start, length: 1 };
                            name.layout.SetFontWeight(DWRITE_FONT_WEIGHT(600), range)?;
                            name.layout.SetDrawingEffect(&accent_effect, range)?;
                        }
                        gfx.brush.SetColor(&rgba(p.foreground, text_alpha));
                        gfx.target.DrawTextLayout(
                            Vector2 { X: name_x, Y: y0 },
                            &name.layout,
                            &gfx.brush,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                    }
                }
                Row::Gif(i, matched) => {
                    let gif = &self.gifs[*i];
                    let thumb = (row_h - 12.0 * s).round();
                    let ty = y0 + (row_h - thumb) / 2.0;
                    let style = self.gif_style;
                    let key = format!("orc:{}:{}:{}", style.name(), gif.slug, gif.asset(style).revision);
                    if !gfx.icons.contains_key(&key) {
                        let poster = orc::poster_path(gif, style);
                        if poster.exists() {
                            let bitmap = crate::gfx::file_bitmap(f, &gfx.target, &poster);
                            gfx.icons.insert(key.clone(), bitmap);
                        } else {
                            orc::ensure_poster(gif, style, notify_launcher);
                        }
                    }
                    match gfx.icons.get(&key) {
                        Some(Some(bitmap)) => {
                            let dest = D2D_RECT_F { left: icon_x, top: ty, right: icon_x + thumb, bottom: ty + thumb };
                            gfx.target.DrawBitmap(
                                bitmap,
                                Some(&dest),
                                1.0,
                                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                                None,
                            );
                        }
                        _ => {
                            gfx.brush.SetColor(&rgba(p.foreground, 0.06));
                            gfx.target.FillRoundedRectangle(
                                &rounded(icon_x, ty, icon_x + thumb, ty + thumb, 6.0 * s),
                                &gfx.brush,
                            );
                        }
                    }
                    let text_x = icon_x + thumb + 14.0 * s;
                    let hint = selected
                        .then(|| label(f, &gfx.hint, "Enter paste  ·  Shift+Enter link", name_w, row_h))
                        .flatten();
                    let hint_w = hint.as_ref().map_or(0.0, |h| h.width + 16.0 * s);
                    let mut x = text_x;
                    if let Some(title) = label(f, &gfx.item, &gif.title, right - text_x - hint_w, row_h) {
                        let accent_effect: IUnknown = gfx.accent.cast()?;
                        for start in utf16_offsets(&gif.title, &matched.positions) {
                            let range = DWRITE_TEXT_RANGE { startPosition: start, length: 1 };
                            title.layout.SetFontWeight(DWRITE_FONT_WEIGHT(600), range)?;
                            title.layout.SetDrawingEffect(&accent_effect, range)?;
                        }
                        gfx.brush.SetColor(&rgba(p.foreground, text_alpha));
                        gfx.target.DrawTextLayout(
                            Vector2 { X: x, Y: y0 },
                            &title.layout,
                            &gfx.brush,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                        x += title.width + 12.0 * s;
                    }
                    let tags = gif.tags.iter().take(4).cloned().collect::<Vec<_>>().join(" · ");
                    let tags_w = right - x - hint_w;
                    if !tags.is_empty() && tags_w > 40.0 * s {
                        if let Some(tags) = label(f, &gfx.hint, &tags, tags_w, row_h) {
                            gfx.brush.SetColor(&rgba(p.foreground, 0.4));
                            gfx.target.DrawTextLayout(
                                Vector2 { X: x, Y: y0 },
                                &tags.layout,
                                &gfx.brush,
                                D2D1_DRAW_TEXT_OPTIONS_NONE,
                            );
                        }
                    }
                    if let Some(hint) = hint {
                        gfx.brush.SetColor(&rgba(p.foreground, 0.42));
                        gfx.target.DrawTextLayout(
                            Vector2 { X: right - hint.width, Y: y0 },
                            &hint.layout,
                            &gfx.brush,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                    }
                }
                Row::Run(command) => {
                    if let Some(prefix) = label(f, &gfx.item, "Run", name_w, row_h) {
                        gfx.brush.SetColor(&rgba(p.foreground, 0.5));
                        gfx.target.DrawTextLayout(
                            Vector2 { X: icon_x, Y: y0 },
                            &prefix.layout,
                            &gfx.brush,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                        let cx = icon_x + prefix.width + 8.0 * s;
                        if let Some(cmd) = label(f, &gfx.item, command, right - cx, row_h) {
                            gfx.brush.SetColor(&rgba(p.foreground, text_alpha));
                            gfx.target.DrawTextLayout(
                                Vector2 { X: cx, Y: y0 },
                                &cmd.layout,
                                &gfx.brush,
                                D2D1_DRAW_TEXT_OPTIONS_NONE,
                            );
                        }
                    }
                }
                Row::Message(text) => {
                    if let Some(msg) = label(f, &gfx.item, text, right - icon_x, row_h) {
                        gfx.brush.SetColor(&rgba(p.foreground, 0.45));
                        gfx.target.DrawTextLayout(
                            Vector2 { X: icon_x, Y: y0 },
                            &msg.layout,
                            &gfx.brush,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                    }
                }
            }
        }

        // Scrollbar, when there are more results than rows.
        if scrollbar {
            let track_top = m + top + 4.0 * s;
            let track_h = visible as f32 * row_h - 8.0 * s;
            let total = self.rows.len() as f32;
            let thumb_h = (track_h * visible as f32 / total).max(18.0 * s);
            let thumb_y = track_top + (track_h - thumb_h) * self.scroll as f32 / (total - visible as f32).max(1.0);
            let x = m + w - pad - 6.0 * s;
            gfx.brush.SetColor(&rgba(p.foreground, 0.16));
            gfx.target.FillRoundedRectangle(&rounded(x, thumb_y, x + 3.0 * s, thumb_y + thumb_h, 1.5 * s), &gfx.brush);
        }
        gfx.target.EndDraw(None, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_out_documents_and_links() {
        let app = |name: &str, target: &str| App { name: name.into(), target: target.into() };
        assert!(is_app(&app("Terminal", "Microsoft.WindowsTerminal_8wekyb3d8bbwe!App")));
        assert!(is_app(&app("Git Bash", r"{6D809377-6AF0-444B-8957-A3773F02200E}\Git\git-bash.exe")));
        assert!(!is_app(&app("Readme", r"C:\Program Files\App\readme.txt")));
        assert!(!is_app(&app("Website", "https://example.com")));
        assert!(!is_app(&app("Uninstall App", r"C:\App\uninst.exe")));
        assert!(!is_app(&app("", "x.exe")));
    }

    #[test]
    fn maps_char_positions_to_utf16() {
        assert_eq!(utf16_offsets("abc", &[0, 2]), vec![0, 2]);
        // '𝄞' is two UTF-16 units.
        assert_eq!(utf16_offsets("𝄞ab", &[1, 2]), vec![2, 3]);
    }
}
