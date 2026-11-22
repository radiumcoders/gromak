//! Speaker volume for the bar's `volume` widget and the volume panel,
//! through Core Audio's `IAudioEndpointVolume` on the default output device.
//!
//! The calls are in-process and cheap, so they run on the main thread (which
//! has COM initialised). The default device can change at any time, so it is
//! looked up again every few seconds and after any failure.

use std::cell::RefCell;
use std::time::{Duration, Instant};
use windows::core::PWSTR;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL, STGM_READ};

/// How long a looked-up default device is trusted.
const DEVICE_TTL: Duration = Duration::from_secs(3);
/// `PKEY_Device_FriendlyName`.
const FRIENDLY_NAME: PROPERTYKEY =
    PROPERTYKEY { fmtid: windows::core::GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0), pid: 14 };

#[derive(Clone, Debug, PartialEq)]
pub struct Volume {
    /// 0.0 - 1.0.
    pub level: f32,
    pub muted: bool,
    /// The output device, e.g. "Speakers (Realtek(R) Audio)".
    pub device: String,
}

struct Endpoint {
    volume: IAudioEndpointVolume,
    device: String,
    found: Instant,
}

thread_local! {
    static ENDPOINT: RefCell<Option<Endpoint>> = const { RefCell::new(None) };
}

fn default_endpoint() -> Option<Endpoint> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
        let volume: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None).ok()?;
        let name = device
            .OpenPropertyStore(STGM_READ)
            .and_then(|store| store.GetValue(&FRIENDLY_NAME))
            .and_then(|value| PropVariantToStringAlloc(&value))
            .map(|s: PWSTR| {
                let text = s.to_string().unwrap_or_default();
                CoTaskMemFree(Some(s.0 as _));
                text
            })
            .unwrap_or_default();
        Some(Endpoint { volume, device: name, found: Instant::now() })
    }
}

/// Runs `f` with the default output device's volume control.
fn with_endpoint<R>(f: impl Fn(&Endpoint) -> windows::core::Result<R>) -> Option<R> {
    ENDPOINT.with(|cell| {
        let mut cell = cell.borrow_mut();
        if cell.as_ref().is_none_or(|e| e.found.elapsed() >= DEVICE_TTL) {
            *cell = default_endpoint();
        }
        match f(cell.as_ref()?) {
            Ok(r) => Some(r),
            Err(_) => {
                // The device went away: look it up again and retry once.
                *cell = default_endpoint();
                f(cell.as_ref()?).ok()
            }
        }
    })
}

/// The default output device's volume, or `None` without one.
pub fn get() -> Option<Volume> {
    with_endpoint(|e| unsafe {
        Ok(Volume {
            level: e.volume.GetMasterVolumeLevelScalar()?,
            muted: e.volume.GetMute()?.as_bool(),
            device: e.device.clone(),
        })
    })
}

pub fn set_level(level: f32) {
    let level = level.clamp(0.0, 1.0);
    with_endpoint(|e| unsafe {
        e.volume.SetMasterVolumeLevelScalar(level, std::ptr::null())?;
        // Raising the volume unmutes, like Windows' own slider.
        if level > 0.0 {
            e.volume.SetMute(false, std::ptr::null())?;
        }
        Ok(())
    });
}

/// Changes the volume by `delta` (e.g. +0.02), rounding to whole percents.
pub fn change(delta: f32) {
    if let Some(v) = get() {
        set_level(((v.level + delta) * 100.0).round() / 100.0);
    }
}

pub fn toggle_mute() {
    with_endpoint(|e| unsafe {
        let muted = e.volume.GetMute()?.as_bool();
        e.volume.SetMute(!muted, std::ptr::null())
    });
}
