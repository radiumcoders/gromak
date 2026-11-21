//! Configuration: the TOML schema, defaults, and parsing of keybindings
//! and dispatcher commands. Pure Rust — no Win32 calls.

use crate::layout::Dir;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The default configuration, written to disk on first launch.
pub const DEFAULT_CONFIG: &str = include_str!("../config/default.toml");

/// Number of workspaces (bound to keys 1–9 and 0).
pub const WORKSPACES: usize = 10;

// Hotkey modifier flags, identical to Win32's MOD_* values.
pub const MOD_ALT: u32 = 0x1;
pub const MOD_CONTROL: u32 = 0x2;
pub const MOD_SHIFT: u32 = 0x4;
pub const MOD_WIN: u32 = 0x8;

/// Pseudo key codes for the mouse wheel (outside the virtual-key range), usable
/// in bindings as `wheelup` and `wheeldown`.
pub const VK_WHEEL_UP: u32 = 0x1_0001;
pub const VK_WHEEL_DOWN: u32 = 0x1_0002;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub general: General,
    #[serde(default)]
    pub theme: Theme,
    #[serde(default)]
    pub decoration: Decoration,
    #[serde(default)]
    pub bar: Bar,
    #[serde(default)]
    pub launcher: Launcher,
    #[serde(default = "default_binds")]
    pub binds: BTreeMap<String, String>,
    #[serde(default = "default_rules")]
    pub rules: Vec<Rule>,
}

/// Where tessera's colours come from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeSource {
    /// Derived from the desktop wallpaper, following it as it changes.
    #[default]
    Wallpaper,
    /// Built around the Windows accent colour.
    Accent,
    /// The colours written in `[decoration]` and `[bar]`.
    Manual,
}

/// Light or dark colours.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    /// Follow Windows' light/dark setting.
    #[default]
    Auto,
    Dark,
    Light,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Theme {
    pub source: ThemeSource,
    pub mode: ThemeMode,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct General {
    pub gaps_in: i32,
    pub gaps_out: i32,
    #[serde(rename = "mod")]
    pub modifier: String,
    pub terminal: String,
    pub hide_taskbar: bool,
    /// Focus the window under the mouse pointer as it moves.
    pub focus_follows_mouse: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Decoration {
    pub borders: bool,
    pub active_border: String,
    pub inactive_border: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Bar {
    pub enabled: bool,
    pub height: i32,
    pub font: String,
    pub font_size: i32,
    pub background: String,
    pub foreground: String,
    pub accent: String,
    pub muted: String,
    /// Opacity of the bar islands, 0.0 (invisible) to 1.0 (solid).
    pub opacity: f32,
    /// Corner radius of the islands in logical pixels.
    pub corner_radius: i32,
    pub show_title: bool,
    pub show_clock: bool,
    pub clock_24h: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Launcher {
    /// An installed font family, or a path to a `.ttf`/`.otf` file (relative
    /// paths are relative to the config directory).
    pub emoji_font: String,
    /// Which orc.pictures variant the GIF picker shows first.
    pub gif_style: GifStyle,
}

/// orc.pictures offers every GIF in two styles.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GifStyle {
    /// The original orc GIFs.
    #[default]
    Orc,
    /// The Videorc versions.
    Videorc,
}

impl GifStyle {
    pub fn toggled(self) -> Self {
        match self {
            GifStyle::Orc => GifStyle::Videorc,
            GifStyle::Videorc => GifStyle::Orc,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            GifStyle::Orc => "orc",
            GifStyle::Videorc => "videorc",
        }
    }
}

impl Default for Launcher {
    fn default() -> Self {
        Launcher { emoji_font: "Segoe UI Emoji".into(), gif_style: GifStyle::Orc }
    }
}

/// Where the emoji font comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FontSource {
    Family(String),
    File(PathBuf),
}

impl Launcher {
    pub fn emoji_font_source(&self) -> FontSource {
        let value = self.emoji_font.trim();
        let lower = value.to_lowercase();
        if value.contains(['/', '\\']) || lower.ends_with(".ttf") || lower.ends_with(".otf") {
            let path = PathBuf::from(value);
            let path = if path.is_absolute() {
                path
            } else {
                config_path().parent().map(|dir| dir.join(&path)).unwrap_or(path)
            };
            FontSource::File(path)
        } else {
            FontSource::Family(value.to_string())
        }
    }
}

/// A window rule. All given matchers must match; a rule without any
/// matcher matches nothing.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Rule {
    pub exe: Option<String>,
    pub class: Option<String>,
    pub title: Option<String>,
    pub float: bool,
    pub ignore: bool,
    /// 1-based workspace number.
    pub workspace: Option<usize>,
}

impl Default for General {
    fn default() -> Self {
        General {
            gaps_in: 5,
            gaps_out: 10,
            modifier: "alt".into(),
            terminal: "wt.exe".into(),
            hide_taskbar: false,
            focus_follows_mouse: false,
        }
    }
}

impl Default for Decoration {
    fn default() -> Self {
        Decoration { borders: true, active_border: "#8aadf4".into(), inactive_border: "#363a4f".into() }
    }
}

impl Default for Bar {
    fn default() -> Self {
        Bar {
            enabled: true,
            height: 32,
            font: "Segoe UI Variable Text".into(),
            font_size: 13,
            background: "#181926".into(),
            foreground: "#cad3f5".into(),
            accent: "#8aadf4".into(),
            muted: "#5b6078".into(),
            opacity: 0.9,
            corner_radius: 10,
            show_title: true,
            show_clock: true,
            clock_24h: true,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            general: General::default(),
            theme: Theme::default(),
            decoration: Decoration::default(),
            bar: Bar::default(),
            launcher: Launcher::default(),
            binds: default_binds(),
            rules: default_rules(),
        }
    }
}

/// Parses one top-level section out of [`DEFAULT_CONFIG`].
fn default_section<T: serde::de::DeserializeOwned>(key: &str) -> T {
    let table: toml::Table = toml::from_str(DEFAULT_CONFIG).expect("default config is valid TOML");
    table
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("default config has [{key}]"))
        .try_into()
        .unwrap_or_else(|e| panic!("default [{key}] is valid: {e}"))
}

fn default_binds() -> BTreeMap<String, String> {
    default_section("binds")
}

fn default_rules() -> Vec<Rule> {
    default_section("rules")
}

impl Rule {
    pub fn matches(&self, exe: &str, class: &str, title: &str) -> bool {
        if self.exe.is_none() && self.class.is_none() && self.title.is_none() {
            return false;
        }
        let exe_ok = self.exe.as_ref().is_none_or(|e| e.eq_ignore_ascii_case(exe));
        let class_ok = self.class.as_ref().is_none_or(|c| c.eq_ignore_ascii_case(class));
        let title_ok = self.title.as_ref().is_none_or(|t| title.to_lowercase().contains(&t.to_lowercase()));
        exe_ok && class_ok && title_ok
    }
}

/// `%USERPROFILE%\.config\tessera\tessera.toml`
pub fn config_path() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").unwrap_or_else(|| ".".into());
    PathBuf::from(home).join(".config").join("tessera").join("tessera.toml")
}

/// Loads the config file, creating it with defaults if it does not exist.
pub fn load() -> Result<Config, String> {
    let path = config_path();
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        }
        std::fs::write(&path, DEFAULT_CONFIG).map_err(|e| format!("writing {}: {e}", path.display()))?;
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    parse(&text)
}

/// Parses and validates a config document.
pub fn parse(text: &str) -> Result<Config, String> {
    let cfg: Config = toml::from_str(text).map_err(|e| e.to_string())?;
    let (_, errors) = cfg.compile_binds();
    if let Some(first) = errors.first() {
        return Err(first.clone());
    }
    for (name, value) in [
        ("decoration.active_border", &cfg.decoration.active_border),
        ("decoration.inactive_border", &cfg.decoration.inactive_border),
        ("bar.background", &cfg.bar.background),
        ("bar.foreground", &cfg.bar.foreground),
        ("bar.accent", &cfg.bar.accent),
        ("bar.muted", &cfg.bar.muted),
    ] {
        if parse_color(value).is_none() {
            return Err(format!("{name}: expected a colour like \"#rrggbb\", got \"{value}\""));
        }
    }
    if !(0.0..=1.0).contains(&cfg.bar.opacity) {
        return Err(format!("bar.opacity must be between 0.0 and 1.0, got {}", cfg.bar.opacity));
    }
    for rule in &cfg.rules {
        if let Some(ws) = rule.workspace {
            if !(1..=WORKSPACES).contains(&ws) {
                return Err(format!("rules: workspace must be 1-{WORKSPACES}, got {ws}"));
            }
        }
    }
    Ok(cfg)
}

impl Config {
    /// The modifier flags `mod` expands to.
    pub fn main_mod(&self) -> Result<u32, String> {
        let mut mods = 0;
        for part in self.general.modifier.split('+') {
            mods |= parse_modifier(part.trim()).ok_or_else(|| format!("general.mod: unknown modifier \"{part}\""))?;
        }
        Ok(mods)
    }

    /// Parses every keybinding. Returns the valid ones plus an error
    /// message for each invalid one.
    pub fn compile_binds(&self) -> (Vec<Bind>, Vec<String>) {
        let mut ok = Vec::new();
        let mut errors = Vec::new();
        let main_mod = match self.main_mod() {
            Ok(m) => m,
            Err(e) => return (ok, vec![e]),
        };
        for (keys, action) in &self.binds {
            match (parse_hotkey(keys, main_mod), parse_command(action)) {
                (Ok(hotkey), Ok(command)) => ok.push(Bind { keys: keys.clone(), hotkey, command }),
                (Err(e), _) | (_, Err(e)) => errors.push(format!("binds.\"{keys}\": {e}")),
            }
        }
        (ok, errors)
    }
}

/// Parses `#rrggbb` into a Win32 `COLORREF` (0x00BBGGRR).
pub fn parse_color(s: &str) -> Option<u32> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    let (r, g, b) = ((v >> 16) & 0xff, (v >> 8) & 0xff, v & 0xff);
    Some(r | (g << 8) | (b << 16))
}

/// A parsed keybinding.
#[derive(Debug, Clone, PartialEq)]
pub struct Bind {
    /// The key combination as written in the config, for messages.
    pub keys: String,
    pub hotkey: Hotkey,
    pub command: Command,
}

/// A global hotkey: Win32 `MOD_*` flags plus a virtual-key code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hotkey {
    pub mods: u32,
    pub vk: u32,
}

fn parse_modifier(s: &str) -> Option<u32> {
    match s.to_ascii_lowercase().as_str() {
        "alt" => Some(MOD_ALT),
        "ctrl" | "control" => Some(MOD_CONTROL),
        "shift" => Some(MOD_SHIFT),
        "win" | "super" | "meta" => Some(MOD_WIN),
        _ => None,
    }
}

fn parse_key(s: &str) -> Option<u32> {
    let s = s.to_ascii_lowercase();
    let b = s.as_bytes();
    if b.len() == 1 && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit()) {
        return Some(b[0].to_ascii_uppercase() as u32);
    }
    if let Some(n) = s.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        return (1..=24).contains(&n).then_some(0x70 + n - 1);
    }
    Some(match s.as_str() {
        "return" | "enter" => 0x0D,
        "space" => 0x20,
        "tab" => 0x09,
        "escape" | "esc" => 0x1B,
        "backspace" => 0x08,
        "delete" => 0x2E,
        "insert" => 0x2D,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        "semicolon" => 0xBA,
        "equal" | "plus" => 0xBB,
        "comma" => 0xBC,
        "minus" => 0xBD,
        "period" => 0xBE,
        "slash" => 0xBF,
        "grave" => 0xC0,
        "bracketleft" => 0xDB,
        "backslash" => 0xDC,
        "bracketright" => 0xDD,
        "quote" => 0xDE,
        "wheelup" | "scrollup" => VK_WHEEL_UP,
        "wheeldown" | "scrolldown" => VK_WHEEL_DOWN,
        _ => return None,
    })
}

/// Parses `"mod+shift+h"` style key combinations. `mod` expands to `main_mod`.
pub fn parse_hotkey(s: &str, main_mod: u32) -> Result<Hotkey, String> {
    let parts: Vec<&str> = s.split('+').map(str::trim).collect();
    let (key, mods) = parts.split_last().ok_or("empty key combination")?;
    let mut flags = 0;
    for m in mods {
        flags |= if m.eq_ignore_ascii_case("mod") {
            main_mod
        } else {
            parse_modifier(m).ok_or_else(|| format!("unknown modifier \"{m}\""))?
        };
    }
    let vk = parse_key(key).ok_or_else(|| format!("unknown key \"{key}\""))?;
    Ok(Hotkey { mods: flags, vk })
}

/// Which workspace a command refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsTarget {
    /// 0-based index.
    Index(usize),
    /// Offset from the current workspace, wrapping around.
    Relative(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsMode {
    /// Cover the whole monitor, including the bar.
    Fullscreen,
    /// Fill the tiling area, keeping gaps and the bar.
    Maximized,
}

/// A dispatcher invocation, bound to a key.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Exec(String),
    KillActive,
    ToggleFloating,
    Fullscreen(FsMode),
    ToggleSplit,
    MoveFocus(Dir),
    MoveWindow(Dir),
    Resize(Dir),
    Cycle(bool),
    Workspace(WsTarget),
    MoveToWorkspace { target: WsTarget, follow: bool },
    Launcher,
    Reload,
    Exit,
}

fn parse_dir(s: &str) -> Result<Dir, String> {
    match s.to_ascii_lowercase().as_str() {
        "l" | "left" => Ok(Dir::Left),
        "r" | "right" => Ok(Dir::Right),
        "u" | "up" => Ok(Dir::Up),
        "d" | "down" => Ok(Dir::Down),
        _ => Err(format!("expected a direction (l, r, u, d), got \"{s}\"")),
    }
}

fn parse_ws(s: &str) -> Result<WsTarget, String> {
    let rel = s.strip_prefix('e').unwrap_or(s);
    if rel.starts_with('+') || rel.starts_with('-') {
        return rel.parse::<i32>().map(WsTarget::Relative).map_err(|_| format!("invalid relative workspace \"{s}\""));
    }
    match s.parse::<usize>() {
        Ok(n) if (1..=WORKSPACES).contains(&n) => Ok(WsTarget::Index(n - 1)),
        _ => Err(format!("workspace must be 1-{WORKSPACES} or e+N / e-N, got \"{s}\"")),
    }
}

/// Parses a dispatcher string such as `"movefocus l"` or `"exec wt.exe"`.
pub fn parse_command(s: &str) -> Result<Command, String> {
    let s = s.trim();
    let (name, arg) = match s.split_once(char::is_whitespace) {
        Some((n, a)) => (n, a.trim()),
        None => (s, ""),
    };
    let need_arg = || {
        if arg.is_empty() {
            Err(format!("\"{name}\" needs an argument"))
        } else {
            Ok(arg)
        }
    };
    Ok(match name.to_ascii_lowercase().as_str() {
        "exec" => Command::Exec(need_arg()?.to_string()),
        "killactive" => Command::KillActive,
        "togglefloating" => Command::ToggleFloating,
        "fullscreen" => match arg {
            "" | "0" => Command::Fullscreen(FsMode::Fullscreen),
            "1" => Command::Fullscreen(FsMode::Maximized),
            _ => return Err(format!("fullscreen takes 0 or 1, got \"{arg}\"")),
        },
        "togglesplit" => Command::ToggleSplit,
        "movefocus" => Command::MoveFocus(parse_dir(need_arg()?)?),
        "movewindow" => Command::MoveWindow(parse_dir(need_arg()?)?),
        "resizeactive" => Command::Resize(parse_dir(need_arg()?)?),
        "cyclenext" => Command::Cycle(true),
        "cycleprev" => Command::Cycle(false),
        "workspace" => Command::Workspace(parse_ws(need_arg()?)?),
        "movetoworkspace" => Command::MoveToWorkspace { target: parse_ws(need_arg()?)?, follow: true },
        "movetoworkspacesilent" => Command::MoveToWorkspace { target: parse_ws(need_arg()?)?, follow: false },
        "launcher" => Command::Launcher,
        "reload" => Command::Reload,
        "exit" => Command::Exit,
        _ => return Err(format!("unknown dispatcher \"{name}\"")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_file_matches_built_in_defaults() {
        assert_eq!(parse(DEFAULT_CONFIG).unwrap(), Config::default());
    }

    #[test]
    fn default_binds_all_compile() {
        let (binds, errors) = Config::default().compile_binds();
        assert!(errors.is_empty(), "{errors:?}");
        assert!(binds.len() > 40);
    }

    #[test]
    fn empty_file_uses_defaults() {
        assert_eq!(parse("").unwrap(), Config::default());
    }

    #[test]
    fn partial_sections_keep_other_defaults() {
        let cfg = parse("[general]\ngaps_in = 2\n[binds]\n\"alt+x\" = \"exit\"").unwrap();
        assert_eq!(cfg.general.gaps_in, 2);
        assert_eq!(cfg.general.gaps_out, 10);
        assert_eq!(cfg.binds.len(), 1);
        assert_eq!(cfg.rules, default_rules());
    }

    #[test]
    fn resolves_emoji_font_sources() {
        let family = Launcher { emoji_font: "Segoe UI Emoji".into(), ..Default::default() };
        assert_eq!(family.emoji_font_source(), FontSource::Family("Segoe UI Emoji".into()));
        let relative = Launcher { emoji_font: "fonts/Twemoji.ttf".into(), ..Default::default() };
        match relative.emoji_font_source() {
            FontSource::File(p) => assert!(p.ends_with("fonts/Twemoji.ttf") && p.is_absolute()),
            other => panic!("{other:?}"),
        }
        let absolute = Launcher { emoji_font: r"C:\Fonts\Noto.ttf".into(), ..Default::default() };
        assert_eq!(absolute.emoji_font_source(), FontSource::File(PathBuf::from(r"C:\Fonts\Noto.ttf")));
    }

    #[test]
    fn parses_theme_options() {
        let cfg = parse("[theme]\nsource = \"accent\"\nmode = \"light\"").unwrap();
        assert_eq!(cfg.theme, Theme { source: ThemeSource::Accent, mode: ThemeMode::Light });
        assert_eq!(parse("").unwrap().theme.source, ThemeSource::Wallpaper);
    }

    #[test]
    fn rejects_unknown_fields_and_bad_values() {
        assert!(parse("[general]\ngap = 1").is_err());
        assert!(parse("[bar]\naccent = \"blue\"").is_err());
        assert!(parse("[bar]\nopacity = 1.5").is_err());
        assert!(parse("[theme]\nsource = \"rainbow\"").is_err());
        assert!(parse("[binds]\n\"mod+x\" = \"fly\"").is_err());
        assert!(parse("[[rules]]\nexe = \"a.exe\"\nworkspace = 11").is_err());
    }

    #[test]
    fn parses_hotkeys() {
        assert_eq!(parse_hotkey("mod+shift+h", MOD_ALT), Ok(Hotkey { mods: MOD_ALT | MOD_SHIFT, vk: 'H' as u32 }));
        assert_eq!(parse_hotkey("win+return", 0), Ok(Hotkey { mods: MOD_WIN, vk: 0x0D }));
        assert_eq!(parse_hotkey("ctrl+alt+f12", 0).unwrap().vk, 0x7B);
        assert_eq!(parse_hotkey("mod+0", MOD_WIN).unwrap().vk, '0' as u32);
        assert_eq!(parse_hotkey("mod+wheelup", MOD_WIN), Ok(Hotkey { mods: MOD_WIN, vk: VK_WHEEL_UP }));
        assert_eq!(parse_hotkey("win+shift+scrolldown", 0).unwrap().vk, VK_WHEEL_DOWN);
        assert!(parse_hotkey("hyper+a", 0).is_err());
        assert!(parse_hotkey("mod+f25", 0).is_err());
    }

    #[test]
    fn parses_commands() {
        assert_eq!(parse_command("exec wt.exe -p PowerShell"), Ok(Command::Exec("wt.exe -p PowerShell".into())));
        assert_eq!(parse_command("movefocus l"), Ok(Command::MoveFocus(Dir::Left)));
        assert_eq!(parse_command("workspace 10"), Ok(Command::Workspace(WsTarget::Index(9))));
        assert_eq!(parse_command("workspace e-1"), Ok(Command::Workspace(WsTarget::Relative(-1))));
        assert_eq!(parse_command("workspace +2"), Ok(Command::Workspace(WsTarget::Relative(2))));
        assert_eq!(
            parse_command("movetoworkspacesilent 3"),
            Ok(Command::MoveToWorkspace { target: WsTarget::Index(2), follow: false })
        );
        assert_eq!(parse_command("launcher"), Ok(Command::Launcher));
        assert_eq!(parse_command("fullscreen"), Ok(Command::Fullscreen(FsMode::Fullscreen)));
        assert_eq!(parse_command("fullscreen 1"), Ok(Command::Fullscreen(FsMode::Maximized)));
        assert!(parse_command("workspace 11").is_err());
        assert!(parse_command("exec").is_err());
        assert!(parse_command("movefocus x").is_err());
    }

    #[test]
    fn parses_colors_as_colorref() {
        assert_eq!(parse_color("#112233"), Some(0x332211));
        assert_eq!(parse_color("112233"), None);
        assert_eq!(parse_color("#12345"), None);
    }

    #[test]
    fn rules_match_case_insensitively() {
        let rule = Rule { exe: Some("Spotify.exe".into()), title: Some("premium".into()), ..Default::default() };
        assert!(rule.matches("spotify.EXE", "Chrome_WidgetWin_0", "Spotify Premium"));
        assert!(!rule.matches("spotify.exe", "x", "Spotify Free"));
        assert!(!Rule::default().matches("a", "b", "c"));
    }
}
