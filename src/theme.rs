//! Dynamic theming: derives gromak's colours from the desktop wallpaper or
//! the Windows accent colour, and follows Windows' light/dark mode.
//!
//! The colour science is deliberately simple and lives in pure functions
//! ([`analyze`], [`palette`]) so it can be unit-tested; the rest of the module
//! reads the wallpaper, accent colour and app mode from Windows.

use std::path::PathBuf;

/// A full set of gromak colours, as `COLORREF`s (0x00BBGGRR).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub background: u32,
    pub foreground: u32,
    pub accent: u32,
    pub muted: u32,
    pub border_active: u32,
    pub border_inactive: u32,
}

pub type Rgb = [u8; 3];

/// Hue in degrees (0–360), saturation and lightness (0–1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hsl {
    pub h: f32,
    pub s: f32,
    pub l: f32,
}

pub fn rgb_to_hsl([r, g, b]: Rgb) -> Hsl {
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d < 1e-6 {
        return Hsl { h: 0.0, s: 0.0, l };
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    Hsl { h, s: s.clamp(0.0, 1.0), l }
}

pub fn hsl_to_rgb(Hsl { h, s, l }: Hsl) -> Rgb {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let to8 = |v: f32| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u8;
    [to8(r), to8(g), to8(b)]
}

fn colorref([r, g, b]: Rgb) -> u32 {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

fn hsl(h: f32, s: f32, l: f32) -> u32 {
    colorref(hsl_to_rgb(Hsl { h, s: s.clamp(0.0, 1.0), l: l.clamp(0.0, 1.0) }))
}

/// What an image looks like, colour-wise.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Analysis {
    /// Hue and saturation of the most prominent vivid colour, if the image
    /// has any real colour in it.
    pub accent: Option<(f32, f32)>,
    /// Hue and saturation of the image's overall (average) tone.
    pub tone: (f32, f32),
}

/// Finds the dominant vivid colour and the overall tone of an image.
///
/// Pixels are binned by hue (12° bins), weighted by chroma so that vivid
/// pixels count more than greyish ones; near-black and near-white pixels are
/// ignored. The heaviest window of three adjacent bins wins.
pub fn analyze(pixels: &[Rgb]) -> Analysis {
    const BINS: usize = 30;
    let mut weight = [0f32; BINS];
    let mut sums = [[0f32; 3]; BINS];
    let mut total = [0f32; 3];
    for &p in pixels {
        for (t, &c) in total.iter_mut().zip(&p) {
            *t += c as f32;
        }
        let max = *p.iter().max().unwrap() as f32 / 255.0;
        let min = *p.iter().min().unwrap() as f32 / 255.0;
        let chroma = max - min;
        let l = (max + min) / 2.0;
        if chroma < 0.1 || !(0.08..=0.95).contains(&l) {
            continue;
        }
        let bin = (rgb_to_hsl(p).h / 360.0 * BINS as f32) as usize % BINS;
        let w = chroma * chroma;
        weight[bin] += w;
        for (s, &c) in sums[bin].iter_mut().zip(&p) {
            *s += c as f32 * w;
        }
    }
    let n = pixels.len().max(1) as f32;
    let avg = total.map(|t| (t / n).round() as u8);
    let tone = rgb_to_hsl(avg);

    let total_weight: f32 = weight.iter().sum();
    let accent = (total_weight / n > 0.01).then(|| {
        let window = |i: usize| (0..3).map(move |k| (i + BINS - 1 + k) % BINS);
        let best = (0..BINS)
            .max_by(|&a, &b| {
                let wa: f32 = window(a).map(|i| weight[i]).sum();
                let wb: f32 = window(b).map(|i| weight[i]).sum();
                wa.total_cmp(&wb)
            })
            .unwrap_or(0);
        let w: f32 = window(best).map(|i| weight[i]).sum();
        let mut rgb = [0f32; 3];
        for i in window(best) {
            for (c, s) in rgb.iter_mut().zip(&sums[i]) {
                *c += s;
            }
        }
        let color = rgb_to_hsl(rgb.map(|c| (c / w).round().clamp(0.0, 255.0) as u8));
        (color.h, color.s)
    });
    Analysis { accent, tone: (tone.h, tone.s) }
}

/// Builds a palette from an accent hue/saturation and an overall tone.
pub fn palette(accent: (f32, f32), tone: (f32, f32), light: bool) -> Palette {
    let (ah, as_) = accent;
    let (th, ts) = tone;
    // Tint neutrals gently towards the wallpaper's overall tone.
    let tint = (ts * 0.6).min(0.28);
    if light {
        let accent = hsl(ah, as_.clamp(0.5, 0.9), 0.45);
        Palette {
            background: hsl(th, tint, 0.95),
            foreground: hsl(th, tint.min(0.25), 0.14),
            accent,
            muted: hsl(th, tint.min(0.15), 0.56),
            border_active: accent,
            border_inactive: hsl(th, tint.min(0.15), 0.80),
        }
    } else {
        let accent = hsl(ah, as_.clamp(0.45, 0.85), 0.72);
        Palette {
            background: hsl(th, tint, 0.10),
            foreground: hsl(th, tint.min(0.2), 0.92),
            accent,
            muted: hsl(th, tint.min(0.18), 0.48),
            border_active: accent,
            border_inactive: hsl(th, tint.min(0.18), 0.25),
        }
    }
}

/// A palette built around a single colour (e.g. the Windows accent).
pub fn palette_from_seed(seed: Rgb, light: bool) -> Palette {
    let c = rgb_to_hsl(seed);
    palette((c.h, c.s), (c.h, c.s * 0.5), light)
}

/// A palette derived from an image's pixels, or `None` if the image has no
/// usable colour (in which case callers fall back to the accent colour).
pub fn palette_from_pixels(pixels: &[Rgb], light: bool) -> Option<Palette> {
    let analysis = analyze(pixels);
    analysis.accent.map(|accent| palette(accent, analysis.tone, light))
}

/// `#rrggbb` for logging.
pub fn hex(c: u32) -> String {
    format!("#{:02x}{:02x}{:02x}", c & 0xFF, (c >> 8) & 0xFF, (c >> 16) & 0xFF)
}

// ----- reading the system ---------------------------------------------------

fn reg_dword(subkey: &str, value: &str) -> Option<u32> {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let (subkey, value) = (crate::win::wide(subkey), crate::win::wide(value));
    let mut data: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            &mut data as *mut u32 as *mut _,
            &mut size,
        )
    };
    (status == 0).then_some(data)
}

/// Whether Windows' shell (taskbar, Start) uses the light theme.
pub fn system_light_mode() -> bool {
    reg_dword(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize", "SystemUsesLightTheme") == Some(1)
}

/// The Windows accent colour (which Windows itself can derive from the
/// wallpaper when "Accent color: Automatic" is on).
pub fn windows_accent() -> Option<Rgb> {
    // Stored as 0xAABBGGRR.
    let v = reg_dword(r"Software\Microsoft\Windows\DWM", "AccentColor")?;
    Some([(v & 0xFF) as u8, ((v >> 8) & 0xFF) as u8, ((v >> 16) & 0xFF) as u8])
}

/// The current wallpaper image file, if any.
pub fn wallpaper_path() -> Option<PathBuf> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETDESKWALLPAPER};
    let mut buf = [0u16; 1024];
    let ok = unsafe { SystemParametersInfoW(SPI_GETDESKWALLPAPER, buf.len() as u32, buf.as_mut_ptr() as *mut _, 0) };
    let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
    let path = PathBuf::from(String::from_utf16_lossy(&buf[..len]));
    (ok != 0 && len > 0 && path.is_file()).then_some(path)
}

/// Windows keeps a copy of the current wallpaper here (also for slideshows
/// and Spotlight).
fn transcoded_wallpaper() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("APPDATA")?).join(r"Microsoft\Windows\Themes\TranscodedWallpaper");
    path.is_file().then_some(path)
}

/// The solid desktop colour, used when no wallpaper image is set.
fn desktop_color() -> Rgb {
    use windows_sys::Win32::Graphics::Gdi::GetSysColor;
    const COLOR_DESKTOP: i32 = 1;
    let c = unsafe { GetSysColor(COLOR_DESKTOP) };
    [(c & 0xFF) as u8, ((c >> 8) & 0xFF) as u8, ((c >> 16) & 0xFF) as u8]
}

/// Decodes an image file and returns its pixels, downscaled to ~96 px.
fn load_pixels(path: &std::path::Path) -> windows::core::Result<Vec<Rgb>> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::GENERIC_READ;
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, IWICImagingFactory, WICBitmapDitherTypeNone,
        WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let name = crate::win::wide(&path.to_string_lossy());
        let decoder =
            wic.CreateDecoderFromFilename(PCWSTR(name.as_ptr()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)?;
        let frame = decoder.GetFrame(0)?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h)?;
        let factor = 96.0 / w.max(h).max(1) as f32;
        let (sw, sh) = (((w as f32 * factor) as u32).max(1), ((h as f32 * factor) as u32).max(1));
        let scaler = wic.CreateBitmapScaler()?;
        scaler.Initialize(&frame, sw, sh, WICBitmapInterpolationModeFant)?;
        let converter = wic.CreateFormatConverter()?;
        converter.Initialize(
            &scaler,
            &GUID_WICPixelFormat32bppBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )?;
        let mut buf = vec![0u8; (sw * sh * 4) as usize];
        converter.CopyPixels(std::ptr::null(), sw * 4, &mut buf)?;
        Ok(buf.as_chunks::<4>().0.iter().map(|p| [p[2], p[1], p[0]]).collect())
    }
}

/// The wallpaper's pixels: the configured image, else Windows' transcoded
/// copy, else the solid desktop colour.
pub fn wallpaper_pixels() -> Vec<Rgb> {
    for path in [wallpaper_path(), transcoded_wallpaper()].into_iter().flatten() {
        match load_pixels(&path) {
            Ok(pixels) if !pixels.is_empty() => return pixels,
            Ok(_) => {}
            Err(e) => warn!("theme: could not read wallpaper {}: {e}", path.display()),
        }
    }
    vec![desktop_color()]
}

fn modified(path: Option<PathBuf>) -> String {
    path.and_then(|p| std::fs::metadata(&p).ok().map(|m| (p, m)))
        .map(|(p, m)| format!("{}@{:?}", p.display(), m.modified().ok()))
        .unwrap_or_default()
}

/// A cheap fingerprint of everything the theme depends on, used to notice
/// changes (new wallpaper, slideshow step, light/dark switch, new accent).
pub fn signature() -> String {
    format!(
        "{}|{}|{}|{:?}|{:?}",
        modified(wallpaper_path()),
        modified(transcoded_wallpaper()),
        system_light_mode(),
        windows_accent(),
        desktop_color()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lightness(c: u32) -> f32 {
        rgb_to_hsl([(c & 0xFF) as u8, ((c >> 8) & 0xFF) as u8, ((c >> 16) & 0xFF) as u8]).l
    }

    fn hue(c: u32) -> f32 {
        rgb_to_hsl([(c & 0xFF) as u8, ((c >> 8) & 0xFF) as u8, ((c >> 16) & 0xFF) as u8]).h
    }

    #[test]
    fn hsl_round_trips() {
        for rgb in [[255, 0, 0], [12, 200, 90], [40, 40, 200], [128, 128, 128], [250, 240, 10]] {
            let back = hsl_to_rgb(rgb_to_hsl(rgb));
            for (a, b) in rgb.iter().zip(back.iter()) {
                assert!((*a as i32 - *b as i32).abs() <= 1, "{rgb:?} -> {back:?}");
            }
        }
    }

    #[test]
    fn grayscale_has_no_accent() {
        let pixels: Vec<Rgb> = (0..=255).map(|v| [v, v, v]).collect();
        assert_eq!(analyze(&pixels).accent, None);
        assert_eq!(palette_from_pixels(&pixels, false), None);
    }

    #[test]
    fn finds_the_vivid_colour_among_grey() {
        // 80% grey, 20% blue: the accent is blue, the tone stays near-neutral.
        let mut pixels = vec![[90u8, 90, 95]; 800];
        pixels.extend(std::iter::repeat_n([40u8, 90, 230], 200));
        let a = analyze(&pixels);
        let (h, s) = a.accent.unwrap();
        assert!((200.0..240.0).contains(&h), "hue {h}");
        assert!(s > 0.6);
        assert!(a.tone.1 < 0.4);
    }

    #[test]
    fn dominant_colour_wins_over_minor_one() {
        let mut pixels = vec![[220u8, 60, 40]; 600]; // red-orange
        pixels.extend(std::iter::repeat_n([40u8, 200, 80], 150)); // green
        let (h, _) = analyze(&pixels).accent.unwrap();
        assert!(!(90.0..=150.0).contains(&h), "picked green: {h}");
    }

    #[test]
    fn dark_and_light_palettes_have_readable_contrast() {
        let dark = palette((210.0, 0.7), (220.0, 0.3), false);
        assert!(lightness(dark.background) < 0.15);
        assert!(lightness(dark.foreground) > 0.85);
        assert!(lightness(dark.accent) > 0.6);
        assert!((hue(dark.accent) - 210.0).abs() < 3.0);

        let light = palette((210.0, 0.7), (220.0, 0.3), true);
        assert!(lightness(light.background) > 0.9);
        assert!(lightness(light.foreground) < 0.2);
        assert!(lightness(light.accent) < 0.55);
    }

    #[test]
    fn seed_palette_keeps_the_seed_hue() {
        let p = palette_from_seed([0, 120, 215], false);
        assert!((hue(p.accent) - hue(colorref([0, 120, 215]))).abs() < 3.0);
        assert_eq!(p.border_active, p.accent);
    }

    #[test]
    fn hex_formats_colorref() {
        assert_eq!(hex(0x00332211), "#112233");
    }
}
