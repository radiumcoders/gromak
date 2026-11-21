//! [orc.pictures](https://orc.pictures) integration for the launcher: search
//! the orc reaction-GIF catalog (type `gif ` then a search) and paste a GIF
//! into the window you were using.
//!
//! The catalog comes from `https://orc.pictures/catalog.json` and is cached
//! in `%LOCALAPPDATA%\tessera\orc\`, as are poster thumbnails and GIFs (keyed
//! by revision, so each file is downloaded once). Like the site's own Omarchy
//! picker, a GIF is put on the clipboard as a *file*, so it stays animated
//! when pasted into Discord, Slack, a browser upload field, and so on.

use crate::config::GifStyle;
use crate::fuzzy;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

const ORIGIN: &str = "https://orc.pictures";
/// Re-download the catalog when the cached copy is older than this.
const CATALOG_MAX_AGE: Duration = Duration::from_secs(6 * 60 * 60);

/// One GIF in the catalog.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Gif {
    pub slug: String,
    pub title: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Site-relative path of the GIF, e.g. `/gifs/angry.gif`.
    pub file: String,
    /// Site-relative path of the still poster (JPEG).
    pub poster: String,
    /// Content hash, changes when the GIF is redone.
    #[serde(default)]
    pub revision: String,
    /// The Videorc version of the same GIF, if there is one.
    #[serde(default)]
    pub videorc: Option<Asset>,
}

/// One rendition of a GIF: its file, poster and revision.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Asset {
    pub file: String,
    pub poster: String,
    #[serde(default)]
    pub revision: String,
}

impl Gif {
    /// The rendition for `style`, falling back to the orc one when the GIF
    /// has no Videorc version.
    pub fn asset(&self, style: GifStyle) -> Asset {
        match (style, &self.videorc) {
            (GifStyle::Videorc, Some(v)) => v.clone(),
            _ => Asset { file: self.file.clone(), poster: self.poster.clone(), revision: self.revision.clone() },
        }
    }

    /// The style actually used for `style` (after the fallback).
    fn effective(&self, style: GifStyle) -> GifStyle {
        if self.videorc.is_some() {
            style
        } else {
            GifStyle::Orc
        }
    }
}

static CATALOG: Mutex<Vec<Gif>> = Mutex::new(Vec::new());
static FETCHING: AtomicBool = AtomicBool::new(false);
static POSTERS_IN_FLIGHT: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Posters that failed to download this session; not retried (the launcher
/// redraws often, and each redraw would otherwise start a new download).
static POSTERS_FAILED: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Makes temporary download names unique across concurrent downloads.
static DOWNLOAD_COUNTER: AtomicU64 = AtomicU64::new(0);

/// `^[a-z0-9]+(-[a-z0-9]+)*$`, at most 80 characters: slugs end up in file
/// names, so anything else is rejected.
pub fn valid_slug(slug: &str) -> bool {
    // Reserved device names cannot be used as file names on Windows.
    const RESERVED: [&str; 22] = [
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1",
        "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    !slug.is_empty()
        && !RESERVED.contains(&slug)
        && slug.len() <= 80
        && slug
            .split('-')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()))
}

fn valid_revision(revision: &str) -> bool {
    revision.is_empty() || (revision.len() <= 64 && revision.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn valid_asset(path: &str, ext: &str) -> bool {
    path.starts_with("/gifs/") && path.ends_with(ext) && !path.contains("..") && !path.contains("//")
}

/// Parses `catalog.json`, dropping entries with unexpected slugs or paths.
pub fn parse_catalog(json: &str) -> Result<Vec<Gif>, String> {
    let gifs: Vec<Gif> = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let valid =
        |a: &Asset| valid_revision(&a.revision) && valid_asset(&a.file, ".gif") && valid_asset(&a.poster, ".jpg");
    Ok(gifs
        .into_iter()
        .filter(|g| {
            valid_slug(&g.slug)
                && valid(&Asset { file: g.file.clone(), poster: g.poster.clone(), revision: g.revision.clone() })
        })
        .map(|mut g| {
            // A bad Videorc rendition only loses that rendition.
            if g.videorc.as_ref().is_some_and(|v| !valid(v)) {
                g.videorc = None;
            }
            g
        })
        .collect())
}

fn cache_dir() -> PathBuf {
    crate::log::data_dir().join("orc")
}

fn catalog_path() -> PathBuf {
    cache_dir().join("catalog.json")
}

fn revision_dir(asset: &Asset) -> &str {
    if asset.revision.is_empty() {
        "current"
    } else {
        &asset.revision
    }
}

/// `posters/<style>/<slug>.<revision>.jpg`
pub fn poster_path(gif: &Gif, style: GifStyle) -> PathBuf {
    let asset = gif.asset(style);
    cache_dir().join("posters").join(gif.effective(style).name()).join(format!(
        "{}.{}.jpg",
        gif.slug,
        revision_dir(&asset)
    ))
}

/// `gifs/<style>/<revision>/<slug>.gif`: the file keeps a clean name
/// (`angry.gif`) when pasted, while styles and revisions still get separate
/// cache entries.
fn gif_path(gif: &Gif, style: GifStyle) -> PathBuf {
    let asset = gif.asset(style);
    cache_dir()
        .join("gifs")
        .join(gif.effective(style).name())
        .join(revision_dir(&asset))
        .join(format!("{}.gif", gif.slug))
}

/// The public URL of a GIF in `style`.
pub fn gif_url(gif: &Gif, style: GifStyle) -> String {
    format!("{ORIGIN}{}", gif.asset(style).file)
}

fn asset_url(path: &str, revision: &str) -> String {
    if revision.is_empty() {
        format!("{ORIGIN}{path}")
    } else {
        format!("{ORIGIN}{path}?v={revision}")
    }
}

/// Downloads `url` to `dest` (via a temporary file), accepting it only if it
/// starts with one of `magic`.
fn download(url: &str, dest: &Path, magic: &[&[u8]]) -> Result<(), String> {
    use windows_sys::Win32::System::Com::Urlmon::URLDownloadToFileW;
    let dir = dest.parent().ok_or("bad destination")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let n = DOWNLOAD_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".download-{}-{n}.tmp", std::process::id()));
    let (wurl, wtmp) = (crate::win::wide(url), crate::win::wide(&tmp.to_string_lossy()));
    let hr = unsafe { URLDownloadToFileW(std::ptr::null_mut(), wurl.as_ptr(), wtmp.as_ptr(), 0, std::ptr::null_mut()) };
    if hr != 0 {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("download of {url} failed (0x{hr:08x})"));
    }
    let head = std::fs::read(&tmp).map(|b| b.into_iter().take(8).collect::<Vec<u8>>()).unwrap_or_default();
    if !magic.iter().any(|m| head.starts_with(m)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("{url} did not return the expected file type"));
    }
    std::fs::rename(&tmp, dest).map_err(|e| e.to_string())
}

fn load_cached_catalog() -> Option<(Vec<Gif>, bool)> {
    let path = catalog_path();
    let text = std::fs::read_to_string(&path).ok()?;
    let gifs = parse_catalog(&text).ok()?;
    let age =
        std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| SystemTime::now().duration_since(t).ok());
    Some((gifs, age.is_none_or(|a| a > CATALOG_MAX_AGE)))
}

/// The catalog as currently known (possibly empty while loading).
pub fn catalog() -> Vec<Gif> {
    let mut guard = CATALOG.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_empty() {
        if let Some((gifs, _)) = load_cached_catalog() {
            *guard = gifs;
        }
    }
    guard.clone()
}

/// Loads the catalog from the cache and refreshes it from the site in the
/// background when missing or stale; `done` runs when new data arrived.
pub fn refresh_catalog(done: impl Fn() + Send + 'static) {
    let stale = match load_cached_catalog() {
        Some((gifs, stale)) => {
            *CATALOG.lock().unwrap_or_else(|e| e.into_inner()) = gifs;
            stale
        }
        None => true,
    };
    if !stale || FETCHING.swap(true, Ordering::Relaxed) {
        return;
    }
    std::thread::spawn(move || {
        let tmp = cache_dir().join("catalog.download.json");
        let result = download(&format!("{ORIGIN}/catalog.json"), &tmp, &[b"[", b"\xEF\xBB\xBF["])
            .and_then(|_| std::fs::read_to_string(&tmp).map_err(|e| e.to_string()))
            .and_then(|text| parse_catalog(&text).map(|gifs| (text, gifs)));
        match result {
            Ok((text, gifs)) if !gifs.is_empty() => {
                let _ = std::fs::write(catalog_path(), text);
                info!("orc.pictures: catalog has {} GIFs", gifs.len());
                *CATALOG.lock().unwrap_or_else(|e| e.into_inner()) = gifs;
                done();
            }
            Ok(_) => warn!("orc.pictures: catalog was empty"),
            Err(e) => warn!("orc.pictures: could not load the catalog: {e}"),
        }
        let _ = std::fs::remove_file(&tmp);
        FETCHING.store(false, Ordering::Relaxed);
    });
}

/// Downloads a GIF's poster in the background if it is not cached yet;
/// `done` runs when it arrives.
pub fn ensure_poster(gif: &Gif, style: GifStyle, done: impl Fn() + Send + 'static) {
    let dest = poster_path(gif, style);
    if dest.exists() {
        return;
    }
    let key = format!("{}:{}", gif.effective(style).name(), gif.slug);
    {
        if POSTERS_FAILED.lock().unwrap_or_else(|e| e.into_inner()).contains(&key) {
            return;
        }
        let mut in_flight = POSTERS_IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner());
        if in_flight.contains(&key) {
            return;
        }
        in_flight.push(key.clone());
    }
    let asset = gif.asset(style);
    let url = asset_url(&asset.poster, &asset.revision);
    std::thread::spawn(move || {
        match download(&url, &dest, &[b"\xFF\xD8\xFF"]) {
            Ok(()) => done(),
            Err(e) => {
                warn!("orc.pictures: {e}");
                POSTERS_FAILED.lock().unwrap_or_else(|e| e.into_inner()).push(key.clone());
            }
        }
        POSTERS_IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner()).retain(|k| *k != key);
    });
}

/// Returns the cached GIF file in `style`, downloading it first if needed
/// (blocking).
pub fn fetch_gif(gif: &Gif, style: GifStyle) -> Result<PathBuf, String> {
    let dest = gif_path(gif, style);
    if !dest.exists() {
        let asset = gif.asset(style);
        download(&asset_url(&asset.file, &asset.revision), &dest, &[b"GIF87a", b"GIF89a"])?;
        // Older revisions of the same GIF are dead weight now.
        let file_name = format!("{}.gif", gif.slug);
        let style_dir = cache_dir().join("gifs").join(gif.effective(style).name());
        for entry in std::fs::read_dir(style_dir).into_iter().flatten().flatten() {
            if entry.file_name() != revision_dir(&asset) {
                let _ = std::fs::remove_file(entry.path().join(&file_name));
            }
        }
    }
    Ok(dest)
}

/// GIFs matching `query` by title or tag, best first.
pub fn search(gifs: &[Gif], query: &str) -> Vec<(usize, fuzzy::Match)> {
    let query = query.trim().to_lowercase();
    let mut results: Vec<(i32, usize, fuzzy::Match)> = gifs
        .iter()
        .enumerate()
        .filter_map(|(i, g)| {
            if query.is_empty() {
                return Some((0, i, fuzzy::Match { score: 0, positions: Vec::new() }));
            }
            let title_lower = g.title.to_lowercase();
            let tag_hit = g.tags.iter().map(|t| t.to_lowercase()).find(|t| t.starts_with(&query) || t.contains(&query));
            let title_hit = title_lower.contains(&query)
                || title_lower.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(&query));
            if !title_hit && tag_hit.is_none() {
                return None;
            }
            let by_title = fuzzy::score(&query, &g.title).filter(|_| title_hit);
            let tag_score = tag_hit.map(|t| if t == query { 60 } else { 30 });
            match by_title {
                Some(m) => Some((m.score.max(tag_score.unwrap_or(0)), i, m)),
                None => Some((tag_score.unwrap_or(0), i, fuzzy::Match { score: 0, positions: Vec::new() })),
            }
        })
        .collect();
    results.sort_by(|a, b| b.0.cmp(&a.0).then(gifs[a.1].title.cmp(&gifs[b.1].title)));
    results.into_iter().map(|(_, i, m)| (i, m)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"[
        {"file":"/gifs/angry.gif","poster":"/gifs/angry.jpg","slug":"angry","tags":["angry","mad","rage"],"title":"Angry","revision":"e83cea76e831","width":544,"height":544,
         "videorc":{"file":"/gifs/videorc/angry.gif","poster":"/gifs/videorc/angry.jpg","revision":"e9e990775e87","width":640,"height":640}},
        {"file":"/gifs/burn-it-with-fire.gif","poster":"/gifs/burn-it-with-fire.jpg","slug":"burn-it-with-fire","tags":["fire","burn"],"title":"Burn It With Fire","revision":"0a1b2c3d4e5f",
         "videorc":{"file":"/gifs/../oops.gif","poster":"/gifs/videorc/x.jpg"}},
        {"file":"/gifs/../../evil.gif","poster":"/gifs/evil.jpg","slug":"evil","tags":[],"title":"Evil"},
        {"file":"/gifs/x.gif","poster":"/gifs/x.jpg","slug":"Bad Slug","tags":[],"title":"Bad"},
        {"file":"/gifs/y.gif","poster":"/gifs/y.jpg","slug":"y","tags":[],"title":"Y","revision":"not-hex!"}
    ]"#;

    #[test]
    fn parses_and_validates_the_catalog() {
        let gifs = parse_catalog(SAMPLE).unwrap();
        let slugs: Vec<&str> = gifs.iter().map(|g| g.slug.as_str()).collect();
        assert_eq!(slugs, vec!["angry", "burn-it-with-fire"]);
        assert_eq!(gifs[0].tags, vec!["angry", "mad", "rage"]);
        assert!(parse_catalog("not json").is_err());
    }

    #[test]
    fn validates_slugs() {
        assert!(valid_slug("burn-it-with-fire"));
        assert!(valid_slug("a1"));
        assert!(!valid_slug(""));
        assert!(!valid_slug("-lead"));
        assert!(!valid_slug("double--dash"));
        assert!(!valid_slug("../etc"));
        assert!(!valid_slug("UPPER"));
        assert!(!valid_slug("con"));
        assert!(!valid_slug("nul"));
    }

    #[test]
    fn searches_titles_and_tags() {
        let gifs = parse_catalog(SAMPLE).unwrap();
        let first = |q: &str| search(&gifs, q).first().map(|(i, _)| gifs[*i].slug.clone());
        assert_eq!(first("angry").as_deref(), Some("angry"));
        assert_eq!(first("mad").as_deref(), Some("angry"));
        assert_eq!(first("fire").as_deref(), Some("burn-it-with-fire"));
        assert!(search(&gifs, "zzz").is_empty());
        assert_eq!(search(&gifs, "").len(), 2);
    }

    #[test]
    fn builds_urls_and_cache_paths() {
        let gif = parse_catalog(SAMPLE).unwrap().remove(0);
        assert_eq!(gif_url(&gif, GifStyle::Orc), "https://orc.pictures/gifs/angry.gif");
        assert_eq!(asset_url(&gif.file, &gif.revision), "https://orc.pictures/gifs/angry.gif?v=e83cea76e831");
        assert!(poster_path(&gif, GifStyle::Orc).ends_with(r"orc\posters\orc\angry.e83cea76e831.jpg"));
        assert!(gif_path(&gif, GifStyle::Orc).ends_with(r"orc\gifs\orc\e83cea76e831\angry.gif"));
    }

    #[test]
    fn supports_both_styles_with_fallback() {
        let gifs = parse_catalog(SAMPLE).unwrap();
        let angry = &gifs[0];
        assert_eq!(gif_url(angry, GifStyle::Videorc), "https://orc.pictures/gifs/videorc/angry.gif");
        assert!(poster_path(angry, GifStyle::Videorc).ends_with(r"orc\posters\videorc\angry.e9e990775e87.jpg"));
        assert!(gif_path(angry, GifStyle::Videorc).ends_with(r"orc\gifs\videorc\e9e990775e87\angry.gif"));
        // An invalid Videorc rendition is dropped; the GIF falls back to orc.
        let fire = &gifs[1];
        assert_eq!(fire.videorc, None);
        assert_eq!(gif_url(fire, GifStyle::Videorc), "https://orc.pictures/gifs/burn-it-with-fire.gif");
        assert!(gif_path(fire, GifStyle::Videorc).ends_with(r"orc\gifs\orc\0a1b2c3d4e5f\burn-it-with-fire.gif"));
    }
}
