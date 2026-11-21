//! Shared Direct2D / DirectWrite / WIC plumbing for tessera's own windows
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
    WICBitmapDitherTypeNone, WICBitmapPaletteTypeMedianCut,
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
    let brush = target.CreateSolidColorBrush(&rgba(0, 1.0), None)?;
    Ok((target, brush))
}

/// Points `target` at the first `w`×`h` pixels of `surface`.
pub unsafe fn bind(target: &ID2D1DCRenderTarget, surface: &Surface, w: i32, h: i32) -> windows::core::Result<()> {
    let bounds = RECT { left: 0, top: 0, right: w, bottom: h };
    target.BindDC(windows::Win32::Graphics::Gdi::HDC(surface.dc as _), &bounds as *const RECT as *const _)
}

/// Pushes the top-left `rect.w`×`rect.h` pixels of `surface` to the layered
/// window `hwnd`, placing it at `rect.x`, `rect.y`.
pub unsafe fn present(hwnd: isize, surface: &Surface, rect: Rect) {
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
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

pub fn rounded(l: f32, t: f32, r: f32, b: f32, radius: f32) -> D2D1_ROUNDED_RECT {
    let radius = radius.min((b - t) / 2.0).min((r - l) / 2.0);
    D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: l, top: t, right: r, bottom: b }, radiusX: radius, radiusY: radius }
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
