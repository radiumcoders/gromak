//! Network and Bluetooth status, and the actions behind the bar's Wi-Fi and
//! Bluetooth panels.
//!
//! A background thread asks Windows every few seconds (through the WinRT
//! `Windows.Devices.Radios`, `Windows.Networking.Connectivity`,
//! `Windows.Devices.WiFi` and `Windows.Devices.Enumeration` APIs) whether
//! there is a Wi-Fi or wired connection, how strong the Wi-Fi signal is, and
//! whether Bluetooth is on and has a device connected. Those queries can take
//! tens of milliseconds, and scanning or connecting takes seconds, so none of
//! it runs on the main thread.
//!
//! The bar reads the latest [`Status`] with [`current`] on its one-second
//! timer. While a panel is open ([`Request::Watch`]) the thread also keeps
//! [`Details`] up to date: the Wi-Fi networks in range and the paired
//! Bluetooth devices. Panels send [`Request`]s (connect, switch a radio, …)
//! and redraw when [`generation`] changes.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, Once};
use std::time::{Duration, Instant};
use windows::core::HSTRING;
use windows::Devices::Bluetooth::{BluetoothConnectionStatus, BluetoothDevice, BluetoothLEDevice};
use windows::Devices::Enumeration::DeviceInformation;
use windows::Devices::Radios::{Radio, RadioAccessStatus, RadioKind, RadioState};
use windows::Devices::WiFi::{
    WiFiAccessStatus, WiFiAdapter, WiFiAvailableNetwork, WiFiConnectionStatus, WiFiReconnectionKind,
};
use windows::Networking::Connectivity::{
    ConnectionProfile, NetworkAuthenticationType, NetworkConnectivityLevel, NetworkInformation,
};
use windows::Security::Credentials::PasswordCredential;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows_future::{AsyncActionCompletedHandler, AsyncStatus, IAsyncAction};

/// The thread wakes this often.
const TICK: Duration = Duration::from_millis(1500);
/// The network and Bluetooth status is refreshed every this many ticks
/// (every tick while a panel is open).
const POLL_TICKS: u32 = 2;
/// How often networks are rescanned while the Wi-Fi panel is open.
const SCAN_INTERVAL: Duration = Duration::from_secs(12);
/// `IanaInterfaceType` of Wi-Fi adapters (IEEE 802.11).
const IF_TYPE_WIFI: u32 = 71;

/// The active network connection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Link {
    /// Not connected to any network.
    #[default]
    None,
    /// Connected over Wi-Fi, with Windows' signal strength (0-5 bars).
    Wifi(u8),
    /// Connected over anything else: Ethernet, cellular, USB tethering, …
    Wired,
}

/// The Bluetooth radio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bluetooth {
    Off,
    On,
    /// On, with at least one device connected.
    Connected,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub link: Link,
    /// The connection reaches the internet (not just the local network).
    pub internet: bool,
    /// `Some(on)` if the machine has a Wi-Fi radio.
    pub wifi_radio: Option<bool>,
    /// `None` if the machine has no Bluetooth radio.
    pub bluetooth: Option<Bluetooth>,
}

/// A Wi-Fi network in range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Network {
    pub ssid: String,
    /// Signal strength, 0-5 bars.
    pub bars: u8,
    /// Needs a password (or other credentials) to join.
    pub secured: bool,
    pub connected: bool,
}

/// A paired Bluetooth device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub name: String,
    pub connected: bool,
}

/// What the panels list. Only kept up to date while one is open.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Details {
    pub has_wifi_adapter: bool,
    /// Windows does not let gromak see the networks in range (on recent
    /// Windows 11, desktop apps need location access for that).
    pub scan_denied: bool,
    /// A scan is running.
    pub scanning: bool,
    /// Networks in range: the connected one first, then by signal strength.
    pub networks: Vec<Network>,
    /// Paired Bluetooth devices: connected ones first, then by name.
    pub devices: Vec<Device>,
    /// The network a connection attempt is running for.
    pub connecting: Option<String>,
    /// This network needs a password: the panel asks for it.
    pub needs_password: Option<String>,
    /// The lists were refreshed since the panel opened.
    pub loaded: bool,
    /// The outcome of the last action, when it went wrong.
    pub message: Option<String>,
}

/// Something a panel asks the status thread to do.
#[derive(Debug)]
pub enum Request {
    /// A panel opened (`true`) or closed: keep [`Details`] fresh meanwhile.
    Watch(bool),
    /// Switch the Wi-Fi (`bluetooth == false`) or Bluetooth radio on or off.
    SetRadio {
        bluetooth: bool,
        on: bool,
    },
    /// Join a network, with its saved profile or the given password.
    Connect {
        ssid: String,
        password: Option<String>,
    },
    Disconnect,
    /// The panel stopped asking for a password.
    CancelPassword,
    /// Something finished in the background (a scan): refresh now.
    Refresh,
}

static STATUS: Mutex<Option<Status>> = Mutex::new(None);
static DETAILS: Mutex<Option<Details>> = Mutex::new(None);
static SENDER: Mutex<Option<Sender<Request>>> = Mutex::new(None);
static GENERATION: AtomicU64 = AtomicU64::new(0);
static START: Once = Once::new();

/// Starts the status thread (once; later calls do nothing).
pub fn start() {
    START.call_once(|| {
        let (tx, rx) = channel();
        let worker_tx = tx.clone();
        let spawned = std::thread::Builder::new().name("status".into()).spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            Worker::new(worker_tx).run(rx);
        });
        match spawned {
            Ok(_) => *lock(&SENDER) = Some(tx),
            Err(e) => warn!("could not start the status thread: {e}"),
        }
    });
}

/// Asks the status thread to do something (starting it if needed).
pub fn request(request: Request) {
    start();
    if let Some(tx) = lock(&SENDER).as_ref() {
        let _ = tx.send(request);
    }
}

/// The latest status, or `None` before the first poll finished.
pub fn current() -> Option<Status> {
    *lock(&STATUS)
}

/// The latest panel details.
pub fn details() -> Details {
    lock(&DETAILS).clone().unwrap_or_default()
}

/// Changes whenever the status or the details change.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Stores `value` in `slot` and bumps the generation if it changed.
fn publish<T: PartialEq>(slot: &Mutex<Option<T>>, value: T) {
    let mut guard = lock(slot);
    if guard.as_ref() != Some(&value) {
        *guard = Some(value);
        GENERATION.fetch_add(1, Ordering::Relaxed);
    }
}

/// State owned by the status thread.
struct Worker {
    /// For background completions (scans) to wake the loop.
    tx: Sender<Request>,
    watching: bool,
    details: Details,
    adapter: Option<WiFiAdapter>,
    /// The networks of the last scan report, by SSID (strongest BSSID).
    found: BTreeMap<String, WiFiAvailableNetwork>,
    scan: Option<IAsyncAction>,
    last_scan: Option<Instant>,
    /// When Wi-Fi access was last asked for.
    access_checked: Option<Instant>,
}

impl Worker {
    fn new(tx: Sender<Request>) -> Self {
        Worker {
            tx,
            watching: false,
            details: Details::default(),
            adapter: None,
            found: BTreeMap::new(),
            scan: None,
            last_scan: None,
            access_checked: None,
        }
    }

    fn run(mut self, rx: Receiver<Request>) {
        let mut tick = 0u32;
        let mut status;
        loop {
            if tick.is_multiple_of(POLL_TICKS) || self.watching {
                status = poll();
                publish(&STATUS, status);
                if self.watching {
                    self.refresh_details(&status);
                }
            }
            tick = tick.wrapping_add(1);
            let deadline = Instant::now() + TICK;
            // Wait for the next poll, but poll right away after a request so
            // that its effect shows quickly.
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(request) => {
                    self.handle(request);
                    // Take whatever else queued up meanwhile, too.
                    while let Ok(request) = rx.try_recv() {
                        self.handle(request);
                    }
                    tick = 0;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn handle(&mut self, request: Request) {
        match request {
            Request::Watch(on) => {
                self.watching = on;
                if on {
                    // Start each panel fresh, and scan as soon as possible.
                    self.details.message = None;
                    self.details.needs_password = None;
                    self.details.loaded = false;
                    self.last_scan = None;
                    self.access_checked = None;
                }
            }
            Request::SetRadio { bluetooth, on } => {
                let kind = if bluetooth { RadioKind::Bluetooth } else { RadioKind::WiFi };
                self.details.message = set_radio(kind, on).err();
                if !bluetooth && on {
                    self.last_scan = None;
                }
            }
            Request::Connect { ssid, password } => self.connect(ssid, password),
            Request::Disconnect => {
                if let Some(adapter) = &self.adapter {
                    let _ = adapter.Disconnect();
                }
            }
            Request::CancelPassword => self.details.needs_password = None,
            Request::Refresh => {}
        }
        self.publish_details();
    }

    fn publish_details(&self) {
        publish(&DETAILS, self.details.clone());
    }

    fn refresh_details(&mut self, status: &Status) {
        let wifi_on = status.wifi_radio == Some(true);
        self.details.has_wifi_adapter = status.wifi_radio.is_some();
        // Without location access Windows refuses even to list the Wi-Fi
        // adapters. Ask again now and then: the user may turn it on.
        if self.adapter.is_none() && wifi_on && self.access_checked.is_none_or(|t| t.elapsed() >= SCAN_INTERVAL) {
            self.access_checked = Some(Instant::now());
            let access = WiFiAdapter::RequestAccessAsync().and_then(|op| op.join());
            self.details.scan_denied = access.ok() != Some(WiFiAccessStatus::Allowed);
            if !self.details.scan_denied {
                self.adapter = WiFiAdapter::FindAllAdaptersAsync()
                    .and_then(|op| op.join())
                    .ok()
                    .and_then(|adapters| adapters.into_iter().next());
                self.last_scan = None;
            }
        }
        if self.scan.as_ref().is_some_and(|s| s.Status() != Ok(AsyncStatus::Started)) {
            self.scan = None;
        }
        if let (Some(adapter), true) = (&self.adapter, wifi_on) {
            if self.scan.is_none() && self.last_scan.is_none_or(|t| t.elapsed() >= SCAN_INTERVAL) {
                self.last_scan = Some(Instant::now());
                if let Ok(scan) = adapter.ScanAsync() {
                    let tx = self.tx.clone();
                    let _ = scan.SetCompleted(&AsyncActionCompletedHandler::new(move |_, _| {
                        let _ = tx.send(Request::Refresh);
                        Ok(())
                    }));
                    self.scan = Some(scan);
                }
            }
            self.found = report(adapter);
        } else {
            self.found.clear();
        }
        self.details.scanning = self.scan.is_some();
        let connected = if let Link::Wifi(_) = status.link { connected_ssid() } else { None };
        let mut networks: Vec<Network> = self
            .found
            .iter()
            .map(|(ssid, n)| Network {
                ssid: ssid.clone(),
                bars: n.SignalBars().unwrap_or(0).min(5),
                secured: secured(n),
                connected: connected.as_deref() == Some(ssid.as_str()),
            })
            .collect();
        // The connected network may be missing from a fresh or partial scan.
        if let (Some(ssid), Link::Wifi(bars)) = (&connected, status.link) {
            if !networks.iter().any(|n| n.connected) {
                networks.push(Network { ssid: ssid.clone(), bars, secured: true, connected: true });
            }
        }
        networks.sort_by(|a, b| b.connected.cmp(&a.connected).then(b.bars.cmp(&a.bars)).then(a.ssid.cmp(&b.ssid)));
        self.details.networks = networks;
        self.details.devices =
            if matches!(status.bluetooth, Some(Bluetooth::On | Bluetooth::Connected)) { devices() } else { Vec::new() };
        self.details.loaded = true;
        self.publish_details();
    }

    fn connect(&mut self, ssid: String, password: Option<String>) {
        let (Some(adapter), Some(network)) = (self.adapter.clone(), self.found.get(&ssid).cloned()) else {
            self.details.message = Some(format!("{ssid} is out of range"));
            return;
        };
        self.details.connecting = Some(ssid.clone());
        self.details.needs_password = None;
        self.details.message = None;
        self.publish_details();
        let kind = WiFiReconnectionKind::Automatic;
        let result = match &password {
            Some(password) => PasswordCredential::new()
                .and_then(|credential| {
                    credential.SetPassword(&HSTRING::from(password.as_str()))?;
                    adapter.ConnectWithPasswordCredentialAsync(&network, kind, &credential)
                })
                .and_then(|op| op.join()),
            None => adapter.ConnectAsync(&network, kind).and_then(|op| op.join()),
        }
        .and_then(|r| r.ConnectionStatus());
        self.details.connecting = None;
        match result {
            Ok(WiFiConnectionStatus::Success) => info!("wifi: connected to {ssid}"),
            // No saved password (or a wrong one): ask for it.
            Ok(WiFiConnectionStatus::InvalidCredential | WiFiConnectionStatus::UnspecifiedFailure)
                if secured(&network) =>
            {
                if password.is_some() {
                    self.details.message = Some("Wrong password".into());
                }
                self.details.needs_password = Some(ssid);
            }
            Ok(status) => {
                let reason = match status {
                    WiFiConnectionStatus::AccessRevoked => "access was revoked",
                    WiFiConnectionStatus::NetworkNotAvailable => "the network is not available",
                    WiFiConnectionStatus::Timeout => "it timed out",
                    WiFiConnectionStatus::UnsupportedAuthenticationProtocol => "its sign-in method is not supported",
                    _ => "it failed",
                };
                self.details.message = Some(format!("Could not connect to {ssid}: {reason}"));
            }
            Err(e) => {
                warn!("wifi: connecting to {ssid}: {e}");
                self.details.message = Some(format!("Could not connect to {ssid}"));
            }
        }
    }
}

fn poll() -> Status {
    let mut status = Status::default();
    let radios = Radio::GetRadiosAsync().and_then(|op| op.join());
    if let Ok(radios) = radios {
        for radio in radios {
            let (Ok(kind), Ok(state)) = (radio.Kind(), radio.State()) else { continue };
            let on = state == RadioState::On;
            if kind == RadioKind::WiFi {
                status.wifi_radio = Some(status.wifi_radio.unwrap_or(false) || on);
            } else if kind == RadioKind::Bluetooth && status.bluetooth != Some(Bluetooth::On) {
                status.bluetooth = Some(if on { Bluetooth::On } else { Bluetooth::Off });
            }
        }
    }
    if status.bluetooth == Some(Bluetooth::On) && bluetooth_connected() {
        status.bluetooth = Some(Bluetooth::Connected);
    }
    (status.link, status.internet) = network();
    status
}

/// The best current connection and whether anything reaches the internet.
/// A Wi-Fi connection wins over others, so that a VPN or a virtual adapter
/// carrying the internet traffic does not hide the Wi-Fi signal.
fn network() -> (Link, bool) {
    let mut profiles: Vec<ConnectionProfile> = Vec::new();
    if let Ok(p) = NetworkInformation::GetInternetConnectionProfile() {
        profiles.push(p);
    }
    if let Ok(all) = NetworkInformation::GetConnectionProfiles() {
        profiles.extend(all);
    }
    let level = |p: &ConnectionProfile| p.GetNetworkConnectivityLevel().unwrap_or(NetworkConnectivityLevel::None);
    let connected: Vec<&ConnectionProfile> =
        profiles.iter().filter(|p| level(p) != NetworkConnectivityLevel::None).collect();
    let internet = connected.iter().any(|p| level(p) == NetworkConnectivityLevel::InternetAccess);
    let is_wifi = |p: &ConnectionProfile| {
        p.IsWlanConnectionProfile().unwrap_or(false)
            || p.NetworkAdapter().and_then(|a| a.IanaInterfaceType()).is_ok_and(|t| t == IF_TYPE_WIFI)
    };
    let link = if let Some(wifi) = connected.iter().find(|p| is_wifi(p)) {
        let bars = wifi.GetSignalBars().and_then(|b| b.Value()).unwrap_or(5);
        Link::Wifi(bars.min(5))
    } else if connected.is_empty() {
        Link::None
    } else {
        Link::Wired
    };
    (link, internet)
}

/// The name of the connected Wi-Fi network: its SSID or, when Windows hides
/// the SSID (without location access), its profile name, which is usually
/// the same.
fn connected_ssid() -> Option<String> {
    for profile in NetworkInformation::GetConnectionProfiles().ok()? {
        let level = profile.GetNetworkConnectivityLevel().unwrap_or(NetworkConnectivityLevel::None);
        if !profile.IsWlanConnectionProfile().unwrap_or(false) || level == NetworkConnectivityLevel::None {
            continue;
        }
        let ssid = profile.WlanConnectionProfileDetails().and_then(|d| d.GetConnectedSsid()).map(|s| s.to_string());
        let name = match ssid {
            Ok(ssid) if !ssid.is_empty() => ssid,
            _ => profile.ProfileName().map(|s| s.to_string()).unwrap_or_default(),
        };
        if !name.is_empty() {
            return Some(name);
        }
    }
    None
}

/// The networks in the adapter's latest scan report, one per SSID (the
/// strongest access point). Hidden networks (no SSID) are left out.
fn report(adapter: &WiFiAdapter) -> BTreeMap<String, WiFiAvailableNetwork> {
    let mut found: BTreeMap<String, WiFiAvailableNetwork> = BTreeMap::new();
    let Ok(networks) = adapter.NetworkReport().and_then(|r| r.AvailableNetworks()) else { return found };
    for network in networks {
        let Ok(ssid) = network.Ssid().map(|s| s.to_string()) else { continue };
        if ssid.is_empty() {
            continue;
        }
        let bars = |n: &WiFiAvailableNetwork| n.SignalBars().unwrap_or(0);
        if found.get(&ssid).is_none_or(|old| bars(&network) > bars(old)) {
            found.insert(ssid, network);
        }
    }
    found
}

fn secured(network: &WiFiAvailableNetwork) -> bool {
    let auth = network.SecuritySettings().and_then(|s| s.NetworkAuthenticationType());
    !matches!(
        auth,
        Ok(NetworkAuthenticationType::None | NetworkAuthenticationType::Open80211 | NetworkAuthenticationType::Owe)
    )
}

/// Switches every radio of `kind` on or off.
fn set_radio(kind: RadioKind, on: bool) -> Result<(), String> {
    let name = if kind == RadioKind::Bluetooth { "Bluetooth" } else { "Wi-Fi" };
    let refused = || format!("Windows did not let gromak turn {name} {}", if on { "on" } else { "off" });
    let access = Radio::RequestAccessAsync().and_then(|op| op.join()).map_err(|_| refused())?;
    if access != RadioAccessStatus::Allowed {
        return Err(refused());
    }
    let state = if on { RadioState::On } else { RadioState::Off };
    let radios = Radio::GetRadiosAsync().and_then(|op| op.join()).map_err(|_| refused())?;
    for radio in radios {
        if radio.Kind().ok() == Some(kind) {
            let result = radio.SetStateAsync(state).and_then(|op| op.join());
            if result.ok() != Some(RadioAccessStatus::Allowed) {
                return Err(refused());
            }
        }
    }
    info!("turned {name} {}", if on { "on" } else { "off" });
    Ok(())
}

/// Device information ids and names matching an AQS selector.
fn find(selector: windows::core::Result<HSTRING>) -> Vec<(String, String)> {
    selector
        .and_then(|s| DeviceInformation::FindAllAsyncAqsFilter(&s))
        .and_then(|op| op.join())
        .map(|devices| {
            devices.into_iter().filter_map(|d| Some((d.Id().ok()?.to_string(), d.Name().ok()?.to_string()))).collect()
        })
        .unwrap_or_default()
}

fn connected_selectors() -> [windows::core::Result<HSTRING>; 2] {
    [
        BluetoothDevice::GetDeviceSelectorFromConnectionStatus(BluetoothConnectionStatus::Connected),
        BluetoothLEDevice::GetDeviceSelectorFromConnectionStatus(BluetoothConnectionStatus::Connected),
    ]
}

/// Whether any classic or Low Energy Bluetooth device is connected.
fn bluetooth_connected() -> bool {
    connected_selectors().into_iter().any(|selector| !find(selector).is_empty())
}

/// Paired devices. A device that pairs over both classic Bluetooth and Low
/// Energy shows up twice with the same name; it is listed once.
fn devices() -> Vec<Device> {
    let connected: Vec<String> = connected_selectors().into_iter().flat_map(find).map(|(id, _)| id).collect();
    let mut by_name: BTreeMap<String, bool> = BTreeMap::new();
    let paired = [
        BluetoothDevice::GetDeviceSelectorFromPairingState(true),
        BluetoothLEDevice::GetDeviceSelectorFromPairingState(true),
    ];
    for selector in paired {
        for (id, name) in find(selector) {
            if name.trim().is_empty() {
                continue;
            }
            *by_name.entry(name).or_insert(false) |= connected.contains(&id);
        }
    }
    let mut devices: Vec<Device> = by_name.into_iter().map(|(name, connected)| Device { name, connected }).collect();
    devices.sort_by(|a, b| b.connected.cmp(&a.connected).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    devices
}
