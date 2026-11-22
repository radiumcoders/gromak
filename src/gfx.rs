//! Shared Direct2D / DirectWrite / WIC plumbing for gromak's own windows
//! (the bar and the launcher).
//!
//! Both are per-pixel-alpha layered windows: a frame is drawn with Direct2D
//! into a 32-bit DIB ([`Surface`]) through a DC render target, then pushed to
//! the screen with `UpdateLayeredWindow` ([`present`]).

use crate::layout::Rect;
use crate::win::wide;
use std::cell::RefCell;
use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::core::PCWSTR;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap, ID2D1DCRenderTarget, ID2D1Factory, ID2D1SolidColorBrush,
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, IDWriteTextLayout, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_METRICS, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmapSource, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapPaletteTypeMedianCut, WICBitmapUsePremultipliedAlpha,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
use windows_sys::Win32::Foundation::{HWND, POINT, RECT, SIZE};
use windows_sys::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject, AC_SRC_ALPHA,
    AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA};

/// Process-wide graphics factories (created once, on the main thread).
pub struct Factories {
    pub d2d: ID2D1Factory,
    pub dwrite: IDWriteFactory,
    pub wic: Option<IWICImagingFactory>,
}

thread_local! {
    static FACTORIES: RefCell<Option<Factories>> = const { RefCell::new(None) };
}

/// Initialises COM and the graphics factories for the current thread.
pub fn init() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let d2d: windows::core::Result<ID2D1Factory> = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None);
        let dwrite: windows::core::Result<IDWriteFactory> = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED);
        let wic: Option<IWICImagingFactory> =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok();
        match (d2d, dwrite) {
            (Ok(d2d), Ok(dwrite)) => FACTORIES.with(|f| *f.borrow_mut() = Some(Factories { d2d, dwrite, wic })),
            (Err(e), _) | (_, Err(e)) => error!("could not initialise Direct2D/DirectWrite: {e}"),
        }
    }
}

/// Releases the factories. Call last, after everything drawn with them is
/// gone: left to the thread-local destructor, they would be released during
/// process teardown, after the DLLs behind them have been unloaded.
pub fn shutdown() {
    BUILTIN.with(|b| b.borrow_mut().take());
    FACTORIES.with(|f| f.borrow_mut().take());
}

/// Runs `f` with the factories, if they were initialised.
pub fn with_factories<R>(f: impl FnOnce(&Factories) -> R) -> Option<R> {
    FACTORIES.with(|cell| cell.borrow().as_ref().map(f))
}

/// A 32-bit top-down DIB selected into a memory DC, the target of drawing.
pub struct Surface {
    pub dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
}

impl Surface {
    pub fn new(w: i32, h: i32) -> Option<Surface> {
        unsafe {
            let screen = GetDC(null_mut());
            let dc = CreateCompatibleDC(screen);
            ReleaseDC(null_mut(), screen);
            let mut info: BITMAPINFO = zeroed();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..zeroed()
            };
            let mut bits: *mut c_void = null_mut();
            let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if bitmap.is_null() {
                DeleteDC(dc);
                return None;
            }
            let old = SelectObject(dc, bitmap as HGDIOBJ);
            Some(Surface { dc, bitmap, old })
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.bitmap as HGDIOBJ);
            DeleteDC(self.dc);
        }
    }
}

/// A DC render target (pixels = DIPs) and a reusable solid brush.
pub unsafe fn render_target(f: &Factories) -> windows::core::Result<(ID2D1DCRenderTarget, ID2D1SolidColorBrush)> {
    let props = D2D1_RENDER_TARGET_PROPERTIES {
        r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 96.0,
        dpiY: 96.0,
        usage: D2D1_RENDER_TARGET_USAGE_NONE,
        minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
    };
    let target = f.d2d.CreateDCRenderTarget(&props)?;
    target.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
    target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
    // Crisper text: natural, symmetric rendering with full contrast.
    if let Ok(defaults) = f.dwrite.CreateRenderingParams() {
        if let Ok(params) = f.dwrite.CreateCustomRenderingParams(
            defaults.GetGamma(),
            1.0,
            0.0,
            windows::Win32::Graphics::DirectWrite::DWRITE_PIXEL_GEOMETRY_FLAT,
            windows::Win32::Graphics::DirectWrite::DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC,
        ) {
            target.SetTextRenderingParams(&params);
        }
    }
    let brush = target.CreateSolidColorBrush(&rgba(0, 1.0), None)?;
    Ok((target, brush))
}

/// Points `target` at the first `w`×`h` pixels of `surface`.
pub unsafe fn bind(target: &ID2D1DCRenderTarget, surface: &Surface, w: i32, h: i32) -> windows::core::Result<()> {
    let bounds = RECT { left: 0, top: 0, right: w, bottom: h };
    target.BindDC(windows::Win32::Graphics::Gdi::HDC(surface.dc as _), &bounds as *const RECT as *const _)
}

/// Pushes the top-left `rect.w`×`rect.h` pixels of `surface` to the layered
/// window `hwnd`, placing it at `rect.x`, `rect.y`, with an overall opacity of
/// `alpha` (255 = as drawn), used for fade animations.
pub unsafe fn present(hwnd: isize, surface: &Surface, rect: Rect, alpha: u8) {
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: alpha,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    let screen = GetDC(null_mut());
    UpdateLayeredWindow(
        hwnd as HWND,
        screen,
        &POINT { x: rect.x, y: rect.y },
        &SIZE { cx: rect.w, cy: rect.h },
        surface.dc,
        &POINT { x: 0, y: 0 },
        0,
        &blend,
        ULW_ALPHA,
    );
    ReleaseDC(null_mut(), screen);
}

/// `COLORREF` plus alpha to a Direct2D colour.
pub fn rgba(c: u32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: (c & 0xFF) as f32 / 255.0,
        g: ((c >> 8) & 0xFF) as f32 / 255.0,
        b: ((c >> 16) & 0xFF) as f32 / 255.0,
        a,
    }
}

/// Square corners, snapped to whole pixels (`corner_radius = 0`, the
/// default), or rounded ones.
static SHARP: AtomicBool = AtomicBool::new(true);

/// Turns sharp corners on or off for every surface gromak draws.
pub fn set_sharp(sharp: bool) {
    SHARP.store(sharp, Ordering::Relaxed);
}

pub fn sharp() -> bool {
    SHARP.load(Ordering::Relaxed)
}

/// A surface: square and snapped to whole pixels when corners are sharp,
/// otherwise rounded with `radius`.
pub fn rounded(l: f32, t: f32, r: f32, b: f32, radius: f32) -> D2D1_ROUNDED_RECT {
    if sharp() {
        return D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F { left: l.round(), top: t.round(), right: r.round(), bottom: b.round() },
            radiusX: 0.0,
            radiusY: 0.0,
        };
    }
    shape(l, t, r, b, radius)
}

/// A rounded rectangle that stays rounded (parts of icons).
pub fn shape(l: f32, t: f32, r: f32, b: f32, radius: f32) -> D2D1_ROUNDED_RECT {
    let radius = radius.min((b - t) / 2.0).min((r - l) / 2.0);
    D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: l, top: t, right: r, bottom: b }, radiusX: radius, radiusY: radius }
}

/// A 1 px border just inside `(l, t, r, b)`: exact pixel lines when corners
/// are sharp, a rounded outline otherwise.
pub unsafe fn border(
    target: &ID2D1DCRenderTarget,
    brush: &ID2D1SolidColorBrush,
    (l, t, r, b): (f32, f32, f32, f32),
    radius: f32,
    color: D2D1_COLOR_F,
) {
    brush.SetColor(&color);
    if sharp() {
        let (l, t, r, b) = (l.round(), t.round(), r.round(), b.round());
        for (x0, y0, x1, y1) in
            [(l, t, r, t + 1.0), (l, b - 1.0, r, b), (l, t + 1.0, l + 1.0, b - 1.0), (r - 1.0, t + 1.0, r, b - 1.0)]
        {
            target.FillRectangle(&D2D_RECT_F { left: x0, top: y0, right: x1, bottom: y1 }, brush);
        }
    } else {
        target.DrawRoundedRectangle(&shape(l + 0.5, t + 0.5, r - 0.5, b - 0.5, radius), brush, 1.0, None);
    }
}

/// A single-line, vertically centred text format that trims with "…".
pub unsafe fn text_format(f: &Factories, font: &str, weight: i32, px: f32) -> windows::core::Result<IDWriteTextFormat> {
    let name = wide(font);
    let format = f.dwrite.CreateTextFormat(
        PCWSTR(name.as_ptr()),
        None,
        DWRITE_FONT_WEIGHT(weight),
        DWRITE_FONT_STYLE_NORMAL,
        DWRITE_FONT_STRETCH_NORMAL,
        px,
        windows::core::w!("en-us"),
    )?;
    format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
    format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
    let ellipsis = f.dwrite.CreateEllipsisTrimmingSign(&format)?;
    let trimming =
        DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
    format.SetTrimming(&trimming, &ellipsis)?;
    Ok(format)
}

/// A measured, single-line piece of text.
pub struct Label {
    pub layout: IDWriteTextLayout,
    pub width: f32,
}

pub unsafe fn label(f: &Factories, format: &IDWriteTextFormat, text: &str, max_w: f32, h: f32) -> Option<Label> {
    let utf16: Vec<u16> = text.encode_utf16().collect();
    let layout = f.dwrite.CreateTextLayout(&utf16, format, max_w.max(1.0), h).ok()?;
    let mut metrics = DWRITE_TEXT_METRICS::default();
    layout.GetMetrics(&mut metrics).ok()?;
    Some(Label { layout, width: metrics.widthIncludingTrailingWhitespace.min(max_w).ceil() })
}

/// Converts a WIC image into a premultiplied Direct2D bitmap.
unsafe fn d2d_bitmap(
    wic: &IWICImagingFactory,
    target: &ID2D1DCRenderTarget,
    source: &IWICBitmapSource,
) -> Option<ID2D1Bitmap> {
    let converter = wic.CreateFormatConverter().ok()?;
    converter
        .Initialize(
            source,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeMedianCut,
        )
        .ok()?;
    target.CreateBitmapFromWicBitmap(&converter, None).ok()
}

/// Converts an `HICON` (with its alpha channel) into a Direct2D bitmap.
pub unsafe fn icon_bitmap(f: &Factories, target: &ID2D1DCRenderTarget, hicon: isize) -> Option<ID2D1Bitmap> {
    let wic = f.wic.as_ref()?;
    let source = wic.CreateBitmapFromHICON(windows::Win32::UI::WindowsAndMessaging::HICON(hicon as _)).ok()?;
    d2d_bitmap(wic, target, &source.into())
}

/// Converts a 32-bit `HBITMAP` with premultiplied alpha (as returned by the
/// shell's image factory) into a Direct2D bitmap.
pub unsafe fn hbitmap_bitmap(f: &Factories, target: &ID2D1DCRenderTarget, hbitmap: isize) -> Option<ID2D1Bitmap> {
    let wic = f.wic.as_ref()?;
    let source = wic
        .CreateBitmapFromHBITMAP(
            windows::Win32::Graphics::Gdi::HBITMAP(hbitmap as _),
            windows::Win32::Graphics::Gdi::HPALETTE::default(),
            WICBitmapUsePremultipliedAlpha,
        )
        .ok()?;
    d2d_bitmap(wic, target, &source.into())
}

/// Like [`text_format`], but for a font loaded from a `.ttf`/`.otf` file
/// rather than an installed family (needs Windows 10 1703+).
pub unsafe fn text_format_from_file(
    f: &Factories,
    path: &std::path::Path,
    weight: i32,
    px: f32,
) -> windows::core::Result<IDWriteTextFormat> {
    use windows::core::Interface;
    use windows::Win32::Graphics::DirectWrite::IDWriteFactory5;
    let factory: IDWriteFactory5 = f.dwrite.cast()?;
    let builder = factory.CreateFontSetBuilder()?;
    let file_path = wide(&path.to_string_lossy());
    builder.AddFontFile(&f.dwrite.CreateFontFileReference(PCWSTR(file_path.as_ptr()), None)?)?;
    let collection = factory.CreateFontCollectionFromFontSet(&builder.CreateFontSet()?)?;
    let names = collection.GetFontFamily(0)?.GetFamilyNames()?;
    let mut family = vec![0u16; names.GetStringLength(0)? as usize + 1];
    names.GetString(0, &mut family)?;
    let format = f.dwrite.CreateTextFormat(
        PCWSTR(family.as_ptr()),
        &collection,
        DWRITE_FONT_WEIGHT(weight),
        DWRITE_FONT_STYLE_NORMAL,
        DWRITE_FONT_STRETCH_NORMAL,
        px,
        windows::core::w!("en-us"),
    )?;
    format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
    format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
    let ellipsis = f.dwrite.CreateEllipsisTrimmingSign(&format)?;
    let trimming =
        DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
    format.SetTrimming(&trimming, &ellipsis)?;
    Ok(format)
}

/// Decodes an image file (PNG, JPEG, …) into a Direct2D bitmap.
pub unsafe fn file_bitmap(f: &Factories, target: &ID2D1DCRenderTarget, path: &std::path::Path) -> Option<ID2D1Bitmap> {
    use windows::Win32::Foundation::GENERIC_READ;
    use windows::Win32::Graphics::Imaging::WICDecodeMetadataCacheOnDemand;
    let wic = f.wic.as_ref()?;
    let name = wide(&path.to_string_lossy());
    let decoder = wic
        .CreateDecoderFromFilename(PCWSTR(name.as_ptr()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)
        .ok()?;
    let frame = decoder.GetFrame(0).ok()?;
    d2d_bitmap(wic, target, &frame.into())
}

/// Goeist and Goeist Mono, built into gromak (SIL Open Font License, see
/// `assets/fonts/OFL.txt`), so the default look needs no installed fonts.
static BUILTIN_FONTS: [(&str, &[u8]); 2] = [
    ("goeist", include_bytes!("../assets/fonts/Goeist-Variable.ttf")),
    ("goeist mono", include_bytes!("../assets/fonts/GoeistMono-Variable.ttf")),
];

/// (lower-case name, collection, family name as UTF-16).
type BuiltinFont = (String, windows::Win32::Graphics::DirectWrite::IDWriteFontCollection1, Vec<u16>);

thread_local! {
    /// The built-in fonts as font collections, by lower-case family name.
    static BUILTIN: RefCell<Option<Vec<BuiltinFont>>> =
        const { RefCell::new(None) };
}

/// Loads the built-in fonts from memory (needs Windows 10 1703+).
unsafe fn builtin_collections(f: &Factories) -> windows::core::Result<Vec<BuiltinFont>> {
    use windows::core::Interface;
    use windows::Win32::Graphics::DirectWrite::IDWriteFactory5;
    let factory: IDWriteFactory5 = f.dwrite.cast()?;
    let loader = factory.CreateInMemoryFontFileLoader()?;
    factory.RegisterFontFileLoader(&loader)?;
    let mut out = Vec::new();
    for (name, data) in BUILTIN_FONTS {
        let file = loader.CreateInMemoryFontFileReference(&factory, data.as_ptr().cast(), data.len() as u32, None)?;
        let builder = factory.CreateFontSetBuilder()?;
        builder.AddFontFile(&file)?;
        let collection = factory.CreateFontCollectionFromFontSet(&builder.CreateFontSet()?)?;
        let names = collection.GetFontFamily(0)?.GetFamilyNames()?;
        let mut family = vec![0u16; names.GetStringLength(0)? as usize + 1];
        names.GetString(0, &mut family)?;
        out.push((name.to_string(), collection, family));
    }
    Ok(out)
}

/// A text format for a built-in font, if `family` names one.
unsafe fn builtin_format(f: &Factories, family: &str, weight: i32, px: f32) -> Option<IDWriteTextFormat> {
    let wanted = family.trim().to_lowercase();
    if !BUILTIN_FONTS.iter().any(|(name, _)| *name == wanted) {
        return None;
    }
    BUILTIN.with(|cell| {
        let mut cell = cell.borrow_mut();
        if cell.is_none() {
            match builtin_collections(f) {
                Ok(fonts) => *cell = Some(fonts),
                Err(e) => {
                    warn!("could not load the built-in fonts: {e}");
                    *cell = Some(Vec::new());
                }
            }
        }
        let (_, collection, family) = cell.as_ref()?.iter().find(|(name, _, _)| *name == wanted)?;
        let format = f
            .dwrite
            .CreateTextFormat(
                PCWSTR(family.as_ptr()),
                collection,
                DWRITE_FONT_WEIGHT(weight),
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                px,
                windows::core::w!("en-us"),
            )
            .ok()?;
        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER).ok()?;
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP).ok()?;
        let ellipsis = f.dwrite.CreateEllipsisTrimmingSign(&format).ok()?;
        let trimming =
            DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
        format.SetTrimming(&trimming, &ellipsis).ok()?;
        Some(format)
    })
}

/// A text format for a font setting: a built-in font (Goeist, Goeist Mono),
/// an installed family, or a font file (falling back to Segoe UI if the file
/// cannot be loaded).
pub unsafe fn font_format(
    f: &Factories,
    font: &crate::config::FontSource,
    weight: i32,
    px: f32,
) -> windows::core::Result<IDWriteTextFormat> {
    match font {
        crate::config::FontSource::Family(family) => match builtin_format(f, family, weight, px) {
            Some(format) => Ok(format),
            None => text_format(f, family, weight, px),
        },
        crate::config::FontSource::File(path) => text_format_from_file(f, path, weight, px).or_else(|e| {
            warn!("could not load font {}: {e}", path.display());
            text_format(f, "Segoe UI Variable Text", weight, px)
        }),
    }
}
