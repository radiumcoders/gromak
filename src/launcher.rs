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

use crate::fuzzy;
use crate::gfx::{hbitmap_bitmap, label, rgba, rounded, Factories, Surface};
use crate::layout::Rect;
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
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE,
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
use windows_sys::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{GlobalLock, GlobalUnlock};
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
}

/// Device-dependent resources.
struct Gfx {
    target: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
    accent: ID2D1SolidColorBrush,
    query: IDWriteTextFormat,
    item: IDWriteTextFormat,
    hint: IDWriteTextFormat,
    /// App icons by target (None = no icon available).
    icons: HashMap<String, Option<ID2D1Bitmap>>,
}

enum Row {
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
}

thread_local! {
    static STATE: RefCell<Option<Launcher>> = const { RefCell::new(None) };
}

/// What to do after an event, executed once the state borrow is released
/// (showing/hiding windows re-enters the window procedure synchronously).
enum Action {
    Nothing,
    Render,
    Close { restore_focus: bool },
    Launch { app: Option<App>, command: Option<String> },
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
            history: load_history(),
            query: String::new(),
            pending_surrogate: None,
            rows: Vec::new(),
            selected: 0,
            scroll: 0,
            caret_on: true,
            previous_foreground: 0,
            open_cursor: (0, 0),
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
        l.style = Some(style);
        l.area = area;
        l.apps = apps;
        l.query.clear();
        l.pending_surrogate = None;
        l.caret_on = true;
        l.previous_foreground = win::foreground();
        l.open_cursor = win::cursor_pos();
        l.refilter();
        l.hwnd
    });
    let Some(hwnd) = hwnd else { return };
    OPEN.store(true, Ordering::Relaxed);
    render();
    unsafe {
        ShowWindow(hwnd as HWND, SW_SHOW);
        SetTimer(hwnd as HWND, CARET_TIMER, CARET_BLINK_MS, None);
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
    }
}

// ----- state ----------------------------------------------------------------

impl Launcher {
    fn refilter(&mut self) {
        let query = self.query.trim();
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
        self.rows = scored.into_iter().take(MAX_RESULTS).map(|(_, i, m)| Row::App(i, m)).collect();
        if self.rows.is_empty() {
            if !query.is_empty() {
                self.rows.push(Row::Run(query.to_string()));
            } else if self.apps.is_empty() {
                self.rows.push(Row::Message("Indexing apps…"));
            }
        }
        self.selected = 0;
        self.scroll = 0;
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
                if let Some(text) = clipboard_text() {
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
            VK_RETURN => return self.activate(self.selected),
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
            v if ctrl && (v == b'J' as u16 || v == b'N' as u16) => 1,
            v if ctrl && (v == b'K' as u16 || v == b'P' as u16) => -1,
            _ => return Action::Nothing,
        };
        self.select(step);
        Action::Render
    }

    /// Layout metrics in pixels: (width, search height, row height, padding).
    fn metrics(&self) -> (f32, f32, f32, f32) {
        let s = self.style.as_ref().map_or(1.0, |st| st.scale);
        let width = (640.0 * s).min(self.area.w as f32 - 40.0 * s).max(200.0);
        (width.round(), (54.0 * s).round(), (42.0 * s).round(), (8.0 * s).round())
    }

    fn visible_rows(&self) -> usize {
        self.rows.len().min(MAX_ROWS)
    }

    fn list_top(&self) -> f32 {
        let (_, search_h, _, pad) = self.metrics();
        pad + search_h + if self.rows.is_empty() { 0.0 } else { (4.0 * self.scale()).round() }
    }

    fn scale(&self) -> f32 {
        self.style.as_ref().map_or(1.0, |st| st.scale)
    }

    fn height(&self) -> f32 {
        let (_, _, row_h, pad) = self.metrics();
        self.list_top() + self.visible_rows() as f32 * row_h + pad
    }

    /// The result row under a window-relative point.
    fn row_at(&self, y: f32) -> Option<usize> {
        let (_, _, row_h, _) = self.metrics();
        let top = self.list_top();
        if y < top {
            return None;
        }
        let i = ((y - top) / row_h) as usize;
        (i < self.visible_rows()).then_some(self.scroll + i)
    }
}

fn clipboard_text() -> Option<String> {
    const CF_UNICODETEXT: u32 = 13;
    unsafe {
        if OpenClipboard(null_mut()) == 0 {
            return None;
        }
        let handle = GetClipboardData(CF_UNICODETEXT);
        let text = if handle.is_null() {
            None
        } else {
            let ptr = GlobalLock(handle) as *const u16;
            let text = (!ptr.is_null()).then(|| {
                let len = (0..).take_while(|&i| *ptr.add(i) != 0).count();
                String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
            });
            GlobalUnlock(handle);
            text
        };
        CloseClipboard();
        text
    }
}

// ----- window procedure -----------------------------------------------------

unsafe extern "system" fn launcher_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let action = match msg {
        WM_ACTIVATE if (wp & 0xFFFF) as u32 == WA_INACTIVE => Some(Action::Close { restore_focus: false }),
        WM_KEYDOWN | WM_SYSKEYDOWN => with_state(|l| l.on_key(wp as u16)),
        WM_CHAR => with_state(|l| l.on_char(wp as u16)),
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
            if let Some(surface) = &l.surface {
                crate::gfx::present(l.hwnd, surface, l.rect);
            }
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
        query: crate::gfx::text_format(f, &style.font, 450, style.font_px * 1.25)?,
        item: crate::gfx::text_format(f, &style.font, 450, style.font_px * 1.1)?,
        hint: crate::gfx::text_format(f, &style.font, 450, style.font_px * 0.95)?,
        icons: HashMap::new(),
    })
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
    unsafe fn draw(&mut self, f: &Factories) -> windows::core::Result<()> {
        let Some(style) = self.style.clone() else { return Ok(()) };
        let s = style.scale;
        let (w, search_h, row_h, pad) = self.metrics();
        let h = self.height();
        let top = self.list_top();
        let visible = self.visible_rows();
        let max_h = top.max(pad + search_h + 4.0 * s) + MAX_ROWS as f32 * row_h + pad;

        // Place the panel: centred, about a fifth of the way down.
        let area = self.area;
        self.rect = Rect::new(
            area.x + ((area.w as f32 - w) / 2.0) as i32,
            area.y + (area.h as f32 * 0.2) as i32,
            w as i32,
            h.ceil() as i32,
        );
        let size = (w as i32, max_h.ceil() as i32);
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

        crate::gfx::bind(&gfx.target, surface, size.0, size.1)?;
        gfx.target.BeginDraw();
        gfx.target.Clear(Some(&rgba(0, 0.0)));
        let target = &gfx.target;
        let brush = &gfx.brush;

        // Panel.
        let radius = (14.0 * s).round();
        let panel = rounded(0.5, 0.5, w - 0.5, h - 0.5, radius);
        brush.SetColor(&rgba(p.background, 1.0));
        target.FillRoundedRectangle(&panel, brush);
        brush.SetColor(&rgba(p.foreground, 0.12));
        target.DrawRoundedRectangle(&panel, brush, 1.0, None);

        // Search field: magnifier, query (or placeholder), caret.
        let center_y = pad + search_h / 2.0;
        let glass_x = pad + 20.0 * s;
        let glass_r = 6.0 * s;
        brush.SetColor(&rgba(p.foreground, 0.55));
        let ellipse =
            D2D1_ELLIPSE { point: Vector2 { X: glass_x, Y: center_y - 1.5 * s }, radiusX: glass_r, radiusY: glass_r };
        target.DrawEllipse(&ellipse, brush, 1.8 * s, None);
        target.DrawLine(
            Vector2 { X: glass_x + glass_r * 0.72, Y: center_y - 1.5 * s + glass_r * 0.72 },
            Vector2 { X: glass_x + glass_r * 1.55, Y: center_y - 1.5 * s + glass_r * 1.55 },
            brush,
            1.8 * s,
            None,
        );
        let text_x = pad + 42.0 * s;
        let text_w = w - text_x - pad - 14.0 * s;
        let query_h = search_h;
        let mut caret_x = text_x;
        if self.query.is_empty() {
            if let Some(placeholder) = label(f, &gfx.query, "Search apps", text_w, query_h) {
                brush.SetColor(&rgba(p.foreground, 0.38));
                target.DrawTextLayout(
                    Vector2 { X: text_x, Y: pad },
                    &placeholder.layout,
                    brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
            }
        } else if let Some(query) = label(f, &gfx.query, &self.query, text_w, query_h) {
            brush.SetColor(&rgba(p.foreground, 1.0));
            target.DrawTextLayout(Vector2 { X: text_x, Y: pad }, &query.layout, brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
            caret_x = text_x + query.width + 1.0;
        }
        if self.caret_on {
            let caret_h = style.font_px * 1.45;
            brush.SetColor(&rgba(p.accent, 1.0));
            target.FillRectangle(
                &D2D_RECT_F {
                    left: caret_x,
                    top: center_y - caret_h / 2.0,
                    right: caret_x + (2.0 * s).max(1.5),
                    bottom: center_y + caret_h / 2.0,
                },
                brush,
            );
        }

        // Divider and results.
        if !self.rows.is_empty() {
            let y = pad + search_h + (2.0 * s).round() - 0.5;
            brush.SetColor(&rgba(p.foreground, 0.08));
            target.DrawLine(
                Vector2 { X: pad + 10.0 * s, Y: y },
                Vector2 { X: w - pad - 10.0 * s, Y: y },
                brush,
                1.0,
                None,
            );
        }
        let icon_size = (24.0 * s).round();
        let icon_x = pad + 14.0 * s;
        let name_x = icon_x + icon_size + 12.0 * s;
        let name_w = w - name_x - pad - 36.0 * s;
        for (slot, index) in (self.scroll..self.scroll + visible).enumerate() {
            let y0 = top + slot as f32 * row_h;
            let selected = index == self.selected;
            if selected {
                brush.SetColor(&rgba(p.accent, 0.20));
                target
                    .FillRoundedRectangle(&rounded(pad, y0 + 1.0, w - pad, y0 + row_h - 1.0, (9.0 * s).round()), brush);
                brush.SetColor(&rgba(p.accent, 1.0));
                let bar_h = row_h * 0.42;
                target.FillRoundedRectangle(
                    &rounded(
                        pad + 3.0 * s,
                        y0 + (row_h - bar_h) / 2.0,
                        pad + 6.0 * s,
                        y0 + (row_h + bar_h) / 2.0,
                        2.0 * s,
                    ),
                    brush,
                );
            }
            match &self.rows[index] {
                Row::App(i, m) => {
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
                        // Matched characters: bold and accent-coloured.
                        let accent_effect: IUnknown = gfx.accent.cast()?;
                        for start in utf16_offsets(&app.name, &m.positions) {
                            let range = DWRITE_TEXT_RANGE { startPosition: start, length: 1 };
                            name.layout.SetFontWeight(DWRITE_FONT_WEIGHT(700), range)?;
                            name.layout.SetDrawingEffect(&accent_effect, range)?;
                        }
                        gfx.brush.SetColor(&rgba(p.foreground, if selected { 1.0 } else { 0.86 }));
                        gfx.target.DrawTextLayout(
                            Vector2 { X: name_x, Y: y0 },
                            &name.layout,
                            &gfx.brush,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                    }
                }
                Row::Run(command) => {
                    let text = format!("Run “{command}”");
                    if let Some(run) = label(f, &gfx.item, &text, w - icon_x - pad - 36.0 * s, row_h) {
                        gfx.brush.SetColor(&rgba(p.foreground, 0.9));
                        gfx.target.DrawTextLayout(
                            Vector2 { X: icon_x, Y: y0 },
                            &run.layout,
                            &gfx.brush,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                    }
                }
                Row::Message(text) => {
                    if let Some(msg) = label(f, &gfx.item, text, w - icon_x - pad, row_h) {
                        gfx.brush.SetColor(&rgba(p.foreground, 0.5));
                        gfx.target.DrawTextLayout(
                            Vector2 { X: icon_x, Y: y0 },
                            &msg.layout,
                            &gfx.brush,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                    }
                }
            }
            if selected && !matches!(self.rows[index], Row::Message(_)) {
                if let Some(enter) = label(f, &gfx.hint, "↵", 40.0 * s, row_h) {
                    gfx.brush.SetColor(&rgba(p.foreground, 0.45));
                    gfx.target.DrawTextLayout(
                        Vector2 { X: w - pad - 14.0 * s - enter.width, Y: y0 },
                        &enter.layout,
                        &gfx.brush,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                    );
                }
            }
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
