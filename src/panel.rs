//! The panels that open from the bar's widgets: Wi-Fi, Bluetooth, system
//! (CPU and memory) and sound. This module decides what they show,
//! lays them out and draws them; the popup window in [`crate::popup`] hosts
//! them and handles input. Data comes from [`crate::status`], [`crate::sys`]
//! and [`crate::audio`].
//!
//! ```text
//!  ┌──────────────────────────────────┐
//!  │ Wi-Fi                       (●─) │   title, state, switch
//!  │ sharma_5G                        │
//!  │──────────────────────────────────│
//!  │ ᯤ  sharma_5G         Disconnect  │   the connected network
//!  │ ᯤ  Neighbour                  🔒 │   click to join
//!  │──────────────────────────────────│
//!  │ Network settings       Scanning… │
//!  └──────────────────────────────────┘
//! ```

use crate::audio::Volume;
use crate::gfx::{label, rgba, rounded, Factories};
use crate::icons::Icon;
use crate::status::{Bluetooth, Details, Link, Status};
use crate::theme::Palette;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_BEZIER_SEGMENT, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_OPEN, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1DCRenderTarget, ID2D1SolidColorBrush, ID2D1StrokeStyle, D2D1_DRAW_TEXT_OPTIONS_NONE,
};
use windows::Win32::Graphics::DirectWrite::{
    IDWriteTextFormat, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_WORD_WRAPPING_WRAP,
};
use windows_numerics::Vector2;

/// Panel width, in logical pixels.
const WIDTH: f32 = 340.0;
const HEADER: f32 = 62.0;
const ROW: f32 = 50.0;
/// Rows shown at once; the list scrolls with the mouse wheel.
pub const MAX_ROWS: usize = 6;
const NOTE: f32 = 56.0;
const NOTE_WITH_BUTTON: f32 = 116.0;
const PASSWORD: f32 = 80.0;
const MESSAGE: f32 = 30.0;
const FOOTER: f32 = 46.0;
/// Space above and below the list.
const LIST_PAD: f32 = 6.0;
const SYSTEM_SECTION: f32 = 108.0;
const VOLUME_BODY: f32 = 64.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    Network,
    Bluetooth,
    System,
    Volume,
}

/// What a click on a panel element does.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Radio {
        bluetooth: bool,
        on: bool,
    },
    Connect(String),
    Disconnect,
    SubmitPassword,
    /// Open a Settings page or run a program (and close the panel).
    Open(&'static str),
    ToggleMute,
    /// The volume slider: the level follows the pointer along this track.
    VolumeSlider,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowIcon {
    /// Wi-Fi signal, 0-5 bars.
    Wifi(u8),
    Ethernet,
    Bluetooth,
}

struct Row {
    icon: RowIcon,
    title: String,
    subtitle: String,
    /// Connected: drawn highlighted.
    active: bool,
    secured: bool,
    /// What clicking the row does.
    action: Option<Action>,
    /// A button at the right end of the row.
    button: Option<(&'static str, Action)>,
}

/// A list panel (Wi-Fi, Bluetooth).
pub struct List {
    which: Which,
    title: &'static str,
    /// Under the title: the connected network, "Off", …
    state: String,
    /// The radio switch: `None` when there is no radio.
    radio: Option<bool>,
    rows: Vec<Row>,
    /// A centred explanation instead of (or after) the list, with an
    /// optional button.
    note: Option<(String, Option<(&'static str, Action)>)>,
    message: Option<String>,
    footer: (&'static str, Action),
    busy: Option<&'static str>,
}

/// Everything a panel shows, built from the latest data.
pub enum Content {
    List(List),
    System { cpu: Vec<f32>, memory: Vec<f32>, usage: crate::sys::Usage, uptime: std::time::Duration },
    Volume(Option<Volume>),
}

impl Content {
    pub fn rows(&self) -> usize {
        match self {
            Content::List(list) => list.rows.len(),
            _ => 0,
        }
    }
}

pub fn content(which: Which, status: Option<Status>, details: &Details) -> Content {
    let st = status.unwrap_or_default();
    match which {
        Which::Network => Content::List(network(&st, details)),
        Which::Bluetooth => Content::List(bluetooth(&st, details)),
        Which::System => {
            let usage = crate::sys::usage();
            let (cpu, memory) = crate::sys::history();
            Content::System { cpu, memory, usage, uptime: crate::sys::uptime() }
        }
        Which::Volume => Content::Volume(crate::audio::get()),
    }
}

fn network(st: &Status, d: &Details) -> List {
    let mut rows = Vec::new();
    if st.link == Link::Wired {
        rows.push(Row {
            icon: RowIcon::Ethernet,
            title: "Ethernet".into(),
            subtitle: if st.internet { "Connected" } else { "Connected, no internet" }.into(),
            active: true,
            secured: false,
            action: None,
            button: None,
        });
    }
    let wifi_on = st.wifi_radio == Some(true);
    if wifi_on {
        for n in &d.networks {
            let connecting = d.connecting.as_deref() == Some(n.ssid.as_str());
            let subtitle = if connecting {
                "Connecting…"
            } else if n.connected && !st.internet {
                "Connected, no internet"
            } else if n.connected {
                if n.secured {
                    "Connected, secured"
                } else {
                    "Connected"
                }
            } else if n.secured {
                "Secured"
            } else {
                "Open"
            };
            // Without access to the adapter nothing can be joined or left.
            let can_act = !d.scan_denied && d.connecting.is_none();
            rows.push(Row {
                icon: RowIcon::Wifi(n.bars),
                title: n.ssid.clone(),
                subtitle: subtitle.into(),
                active: n.connected,
                secured: n.secured,
                action: (can_act && !n.connected).then(|| Action::Connect(n.ssid.clone())),
                button: (can_act && n.connected).then_some(("Disconnect", Action::Disconnect)),
            });
        }
    }
    let note = match st.wifi_radio {
        None if rows.is_empty() => Some(("No Wi-Fi adapter".into(), None)),
        None => None,
        Some(false) => Some(("Wi-Fi is off".into(), None)),
        Some(true) if d.scan_denied => Some((
            "Windows only lists the Wi-Fi networks nearby when location services are on.".into(),
            Some(("Location settings", Action::Open("ms-settings:privacy-location"))),
        )),
        Some(true) if d.networks.is_empty() => {
            Some((if d.scanning || !d.loaded { "Looking for networks…" } else { "No networks found" }.into(), None))
        }
        Some(true) => None,
    };
    let state = match (st.link, st.wifi_radio) {
        (Link::Wired, _) => "Ethernet".to_string(),
        (_, Some(false)) => "Off".into(),
        (Link::Wifi(_), _) => {
            d.networks.iter().find(|n| n.connected).map_or_else(|| "Connected".to_string(), |n| n.ssid.clone())
        }
        _ => "Not connected".into(),
    };
    List {
        which: Which::Network,
        title: "Wi-Fi",
        state,
        radio: st.wifi_radio,
        rows,
        note,
        message: d.message.clone(),
        footer: ("Network settings", Action::Open("ms-settings:network")),
        busy: (wifi_on && d.scanning).then_some("Scanning…"),
    }
}

fn bluetooth(st: &Status, d: &Details) -> List {
    let on = matches!(st.bluetooth, Some(Bluetooth::On | Bluetooth::Connected));
    let rows: Vec<Row> = if on {
        d.devices
            .iter()
            .map(|dev| Row {
                icon: RowIcon::Bluetooth,
                title: dev.name.clone(),
                subtitle: if dev.connected { "Connected" } else { "Paired" }.into(),
                active: dev.connected,
                secured: false,
                action: None,
                button: None,
            })
            .collect()
    } else {
        Vec::new()
    };
    let note = match st.bluetooth {
        None => Some("No Bluetooth adapter"),
        Some(Bluetooth::Off) => Some("Bluetooth is off"),
        _ if rows.is_empty() && !d.loaded => Some("Looking for devices…"),
        _ if rows.is_empty() => Some("No paired devices"),
        _ => None,
    };
    let connected: Vec<&str> = rows.iter().filter(|r| r.active).map(|r| r.title.as_str()).collect();
    let state = match (on, connected.as_slice()) {
        (false, _) => "Off".to_string(),
        (true, []) => "On".into(),
        (true, [one]) => one.to_string(),
        (true, many) => format!("{} devices connected", many.len()),
    };
    List {
        which: Which::Bluetooth,
        title: "Bluetooth",
        state,
        radio: st.bluetooth.map(|b| b != Bluetooth::Off),
        rows,
        note: note.map(|n| (n.to_string(), None)),
        message: d.message.clone(),
        footer: ("Bluetooth settings", Action::Open("ms-settings:bluetooth")),
        busy: None,
    }
}

/// The panel's size in pixels.
pub fn size(c: &Content, password: bool, s: f32) -> (f32, f32) {
    let h = match c {
        Content::List(list) => {
            let mut h = HEADER + FOOTER + 2.0 * LIST_PAD + list.rows.len().min(MAX_ROWS) as f32 * ROW;
            if let Some((_, button)) = &list.note {
                h += if button.is_some() { NOTE_WITH_BUTTON } else { NOTE };
            }
            if password {
                h += PASSWORD;
            }
            if list.message.is_some() {
                h += MESSAGE;
            }
            h
        }
        Content::System { .. } => HEADER + 2.0 * SYSTEM_SECTION + FOOTER,
        Content::Volume(_) => HEADER + VOLUME_BODY + FOOTER,
    };
    (WIDTH * s, h * s)
}

/// Fonts from the popup.
pub struct Fonts<'a> {
    pub title: &'a IDWriteTextFormat,
    pub text: &'a IDWriteTextFormat,
    pub small: &'a IDWriteTextFormat,
    /// For numbers.
    pub mono: &'a IDWriteTextFormat,
}

/// Drawing resources from the popup.
pub struct Canvas<'a> {
    pub f: &'a Factories,
    pub target: &'a ID2D1DCRenderTarget,
    pub brush: &'a ID2D1SolidColorBrush,
    pub round: &'a ID2D1StrokeStyle,
    pub fonts: Fonts<'a>,
    pub palette: Palette,
    pub scale: f32,
}

/// What the popup keeps between frames.
pub struct View<'a> {
    /// The password being typed: (network, text).
    pub password: Option<&'a (String, String)>,
    pub mouse: Option<(f32, f32)>,
    /// First row shown.
    pub scroll: usize,
}

pub type Area = (f32, f32, f32, f32);

fn inside((l, t, r, b): Area, (x, y): (f32, f32)) -> bool {
    x >= l && x < r && y >= t && y < b
}

/// Everything here is flat: solid fills and hairlines, no gradients or
/// shadows.
impl Canvas<'_> {
    unsafe fn fill(&self, (l, t, r, b): Area, radius: f32, color: D2D1_COLOR_F) {
        self.brush.SetColor(&color);
        self.target.FillRoundedRectangle(&rounded(l, t, r, b, radius), self.brush);
    }

    fn width(&self, format: &IDWriteTextFormat, text: &str) -> f32 {
        unsafe { label(self.f, format, text, 1000.0 * self.scale, 40.0 * self.scale).map_or(0.0, |l| l.width) }
    }

    /// Single-line text, left-aligned at `x`, vertically centred on `cy`,
    /// trimmed to `max_w`. Returns its width.
    unsafe fn text(
        &self,
        format: &IDWriteTextFormat,
        text: &str,
        x: f32,
        cy: f32,
        max_w: f32,
        color: D2D1_COLOR_F,
    ) -> f32 {
        let line_h = 40.0 * self.scale;
        let Some(l) = label(self.f, format, text, max_w, line_h) else { return 0.0 };
        self.brush.SetColor(&color);
        self.target.DrawTextLayout(
            Vector2 { X: x, Y: cy - line_h / 2.0 },
            &l.layout,
            self.brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
        );
        l.width
    }

    /// Text ending at `right`.
    unsafe fn text_right(&self, format: &IDWriteTextFormat, text: &str, right: f32, cy: f32, color: D2D1_COLOR_F) {
        let w = self.width(format, text);
        self.text(format, text, right - w, cy, w + 1.0, color);
    }

    /// Wrapped, centred text in the box `(l, t, r, b)`.
    unsafe fn paragraph(&self, format: &IDWriteTextFormat, text: &str, (l, t, r, b): Area, color: D2D1_COLOR_F) {
        let utf16: Vec<u16> = text.encode_utf16().collect();
        let Ok(layout) = self.f.dwrite.CreateTextLayout(&utf16, format, r - l, b - t) else { return };
        let _ = layout.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
        let _ = layout.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
        self.brush.SetColor(&color);
        self.target.DrawTextLayout(Vector2 { X: l, Y: t }, &layout, self.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
    }

    /// A flat button with a centred label, placed by its right edge.
    unsafe fn button(&self, text: &str, right: f32, cy: f32, primary: bool, mouse: Option<(f32, f32)>) -> Area {
        let s = self.scale;
        let p = &self.palette;
        let width = self.width(self.fonts.small, text);
        let (h, pad) = (26.0 * s, 12.0 * s);
        let area = (right - width - 2.0 * pad, cy - h / 2.0, right, cy + h / 2.0);
        let hover = mouse.is_some_and(|m| inside(area, m));
        let (fill, color) = if primary {
            (rgba(p.accent, if hover { 0.85 } else { 1.0 }), rgba(p.background, 1.0))
        } else {
            (rgba(p.foreground, if hover { 0.14 } else { 0.07 }), rgba(p.foreground, 0.9))
        };
        self.fill(area, 6.0 * s, fill);
        self.text(self.fonts.small, text, area.0 + pad, cy, width + 1.0, color);
        area
    }

    /// The on/off switch, placed by its right edge.
    unsafe fn switch(&self, on: bool, right: f32, cy: f32, mouse: Option<(f32, f32)>) -> Area {
        let s = self.scale;
        let p = &self.palette;
        let (w, h) = (36.0 * s, 20.0 * s);
        let area = (right - w, cy - h / 2.0, right, cy + h / 2.0);
        let hover = mouse.is_some_and(|m| inside(area, m));
        let track = if on {
            rgba(p.accent, if hover { 0.85 } else { 1.0 })
        } else {
            rgba(p.foreground, if hover { 0.22 } else { 0.15 })
        };
        self.fill(area, h / 2.0, track);
        let knob_r = h / 2.0 - 3.0 * s;
        let knob_x = if on { area.2 - h / 2.0 } else { area.0 + h / 2.0 };
        let knob = if on { rgba(p.background, 1.0) } else { rgba(p.foreground, 0.7) };
        self.fill((knob_x - knob_r, cy - knob_r, knob_x + knob_r, cy + knob_r), knob_r, knob);
        area
    }

    fn painter(&self, x: f32, cy: f32, size: f32) -> Icon<'_> {
        Icon { f: self.f, target: self.target, brush: self.brush, round: self.round, x, cy, size }
    }

    unsafe fn row_icon(&self, icon: RowIcon, x: f32, cy: f32, color: D2D1_COLOR_F) {
        let painter = self.painter(x, cy, 18.0 * self.scale);
        match icon {
            RowIcon::Wifi(bars) => painter.wifi(bars, color, rgba(self.palette.foreground, 0.2)),
            RowIcon::Ethernet => painter.ethernet(color),
            RowIcon::Bluetooth => painter.rune(color),
        }
    }

    unsafe fn separator(&self, l: f32, r: f32, y: f32) {
        self.brush.SetColor(&rgba(self.palette.foreground, 0.08));
        self.target.FillRectangle(&D2D_RECT_F { left: l, top: y, right: r, bottom: y + 1.0 }, self.brush);
    }

    /// The panel header: title, a state line under it, and the switch.
    unsafe fn header(
        &self,
        x: f32,
        y: f32,
        title: &str,
        state: &str,
        radio: Option<bool>,
        mouse: Option<(f32, f32)>,
    ) -> Option<Area> {
        let s = self.scale;
        let p = &self.palette;
        let w = WIDTH * s;
        let pad = 18.0 * s;
        let cy = y + HEADER * s / 2.0;
        let switch_w = if radio.is_some() { 52.0 * s } else { 0.0 };
        let max = w - 2.0 * pad - switch_w;
        self.text(self.fonts.title, title, x + pad, cy - 9.0 * s, max, rgba(p.foreground, 1.0));
        self.text(self.fonts.small, state, x + pad, cy + 11.0 * s, max, rgba(p.foreground, 0.5));
        let area = radio.map(|on| self.switch(on, x + w - pad, cy, mouse));
        self.separator(x, x + w, y + HEADER * s);
        area
    }

    /// A line chart of `values` (0-1) in `area`, newest on the right.
    unsafe fn graph(&self, values: &[f32], (l, t, r, b): Area, color: D2D1_COLOR_F) {
        let s = self.scale;
        self.separator(l, r, b);
        if values.len() < 2 {
            return;
        }
        let slots = crate::sys::HISTORY.max(values.len()) - 1;
        let step = (r - l) / slots as f32;
        let x0 = r - step * (values.len() - 1) as f32;
        let point = |i: usize| Vector2 { X: x0 + step * i as f32, Y: b - (b - t) * values[i].clamp(0.0, 1.0) };
        let Ok(geometry) = self.f.d2d.CreatePathGeometry() else { return };
        let Ok(sink) = geometry.Open() else { return };
        sink.BeginFigure(point(0), D2D1_FIGURE_BEGIN_HOLLOW);
        for i in 1..values.len() {
            // A gentle curve through the samples.
            let (a, c) = (point(i - 1), point(i));
            let mid = (a.X + c.X) / 2.0;
            sink.AddBezier(&D2D1_BEZIER_SEGMENT {
                point1: Vector2 { X: mid, Y: a.Y },
                point2: Vector2 { X: mid, Y: c.Y },
                point3: c,
            });
        }
        sink.EndFigure(D2D1_FIGURE_END_OPEN);
        if sink.Close().is_ok() {
            self.brush.SetColor(&color);
            self.target.DrawGeometry(&geometry, self.brush, 1.5 * s, self.round);
        }
    }

    /// The footer: a link on the left and a status on the right.
    unsafe fn footer(
        &self,
        x: f32,
        top: f32,
        (link, action): (&str, &Action),
        busy: Option<&str>,
        mouse: Option<(f32, f32)>,
    ) -> (Area, Action) {
        let s = self.scale;
        let p = &self.palette;
        let w = WIDTH * s;
        let pad = 18.0 * s;
        self.separator(x, x + w, top);
        let cy = top + FOOTER * s / 2.0;
        let link_w = self.width(self.fonts.small, link);
        let area = (x + pad - 8.0 * s, cy - 13.0 * s, x + pad + link_w + 8.0 * s, cy + 13.0 * s);
        let hover = mouse.is_some_and(|m| inside(area, m));
        if hover {
            self.fill(area, 6.0 * s, rgba(p.foreground, 0.07));
        }
        self.text(
            self.fonts.small,
            link,
            x + pad,
            cy,
            link_w + 1.0,
            rgba(p.foreground, if hover { 0.95 } else { 0.6 }),
        );
        if let Some(busy) = busy {
            self.text_right(self.fonts.small, busy, x + w - pad, cy, rgba(p.foreground, 0.4));
        }
        (area, action.clone())
    }
}

/// Draws the panel whose top-left corner is `(x, y)`; returns the clickable
/// areas and their actions.
pub unsafe fn draw(cv: &Canvas, c: &Content, view: &View, x: f32, y: f32) -> Vec<(Area, Action)> {
    match c {
        Content::List(list) => draw_list(cv, list, view, x, y),
        Content::System { cpu, memory, usage, uptime } => draw_system(cv, cpu, memory, usage, *uptime, view, x, y),
        Content::Volume(volume) => draw_volume(cv, volume.as_ref(), view, x, y),
    }
}

unsafe fn draw_list(cv: &Canvas, c: &List, view: &View, x: f32, y: f32) -> Vec<(Area, Action)> {
    let s = cv.scale;
    let p = cv.palette;
    let w = WIDTH * s;
    let pad = 18.0 * s;
    let (l, r) = (x + pad, x + w - pad);
    let mut hits: Vec<(Area, Action)> = Vec::new();
    let mouse = view.mouse;
    let fg = p.foreground;

    if let (Some(area), Some(on)) = (cv.header(x, y, c.title, &c.state, c.radio, mouse), c.radio) {
        hits.push((area, Action::Radio { bluetooth: c.which == Which::Bluetooth, on: !on }));
    }
    let mut top = y + (HEADER + LIST_PAD) * s;

    // Rows: the connected one is tinted, the others highlight on hover.
    let first = view.scroll.min(c.rows.len().saturating_sub(MAX_ROWS));
    for row in c.rows.iter().skip(first).take(MAX_ROWS) {
        let area = (x + 8.0 * s, top + 2.0 * s, x + w - 8.0 * s, top + ROW * s - 2.0 * s);
        let cy = top + ROW * s / 2.0;
        let radius = 6.0 * s;
        if row.active {
            cv.fill(area, radius, rgba(p.accent, 0.12));
        }
        let button = row.button.as_ref().map(|(text, action)| (cv.button(text, r, cy, false, mouse), action.clone()));
        let over_button = button.as_ref().is_some_and(|(b, _)| mouse.is_some_and(|m| inside(*b, m)));
        if row.action.is_some() && !over_button && mouse.is_some_and(|m| inside(area, m)) {
            cv.fill(area, radius, rgba(fg, 0.06));
        }
        let icon_color = if row.active { rgba(p.accent, 1.0) } else { rgba(fg, 0.7) };
        cv.row_icon(row.icon, l, cy, icon_color);
        let text_l = l + 32.0 * s;
        let text_r = button.as_ref().map_or(r - if row.secured { 24.0 * s } else { 0.0 }, |(b, _)| b.0 - 8.0 * s);
        let title_color = rgba(fg, if row.active { 1.0 } else { 0.85 });
        cv.text(cv.fonts.text, &row.title, text_l, cy - 9.0 * s, text_r - text_l, title_color);
        let sub_color = if row.active { rgba(p.accent, 1.0) } else { rgba(fg, 0.45) };
        cv.text(cv.fonts.small, &row.subtitle, text_l, cy + 10.0 * s, text_r - text_l, sub_color);
        if row.secured && button.is_none() {
            let size = 14.0 * s;
            cv.painter(r - size, cy, size).lock(rgba(fg, 0.4));
        }
        if let Some((b, action)) = button {
            hits.push((b, action));
        }
        if let Some(action) = &row.action {
            hits.push((area, action.clone()));
        }
        top += ROW * s;
    }
    if c.rows.len() > MAX_ROWS {
        // A thin scroll indicator along the right edge of the list.
        let list_h = MAX_ROWS as f32 * ROW * s;
        let list_t = top - list_h;
        let thumb_h = list_h * MAX_ROWS as f32 / c.rows.len() as f32;
        let thumb_t = list_t + (list_h - thumb_h) * first as f32 / (c.rows.len() - MAX_ROWS) as f32;
        cv.fill((x + w - 4.0 * s, thumb_t, x + w - 2.0 * s, thumb_t + thumb_h), s, rgba(fg, 0.2));
    }

    // Note.
    if let Some((text, button)) = &c.note {
        let h = if button.is_some() { NOTE_WITH_BUTTON } else { NOTE } * s;
        let text_b = if button.is_some() { top + h - 48.0 * s } else { top + h };
        let line_h = 18.0 * s;
        let text_t = if button.is_some() { top + 10.0 * s } else { top + (h - line_h) / 2.0 - 4.0 * s };
        cv.paragraph(cv.fonts.text, text, (l, text_t, r, text_b), rgba(fg, 0.5));
        if let Some((label, action)) = button {
            // Centred: buttons are placed by their right edge.
            let width = cv.width(cv.fonts.small, label) + 24.0 * s;
            let area = cv.button(label, x + (w + width) / 2.0, top + h - 26.0 * s, true, mouse);
            hits.push((area, action.clone()));
        }
        top += h;
    }

    // Password.
    if let Some((ssid, text)) = view.password {
        cv.text(cv.fonts.small, &format!("Password for {ssid}"), l, top + 16.0 * s, r - l, rgba(fg, 0.55));
        let cy = top + 52.0 * s;
        let submit = cv.button("Connect", r, cy, true, mouse);
        hits.push((submit, Action::SubmitPassword));
        let field = (l, cy - 15.0 * s, submit.0 - 8.0 * s, cy + 15.0 * s);
        cv.fill(field, 6.0 * s, rgba(fg, 0.07));
        cv.fill((field.0, field.3 - 2.0 * s, field.2, field.3), 0.0, rgba(p.accent, 1.0));
        let inner_l = field.0 + 10.0 * s;
        let width = if text.is_empty() {
            cv.text(cv.fonts.text, "Enter the password", inner_l, cy, field.2 - inner_l, rgba(fg, 0.35));
            0.0
        } else {
            let dots: String = "•".repeat(text.chars().count());
            cv.text(cv.fonts.text, &dots, inner_l, cy, field.2 - inner_l - 10.0 * s, rgba(fg, 0.9))
        };
        let caret_x = inner_l + width + 1.0 * s;
        cv.fill((caret_x, cy - 8.0 * s, caret_x + 1.5 * s, cy + 8.0 * s), 0.0, rgba(p.accent, 1.0));
        top += PASSWORD * s;
    }

    // Message.
    if let Some(message) = &c.message {
        cv.text(cv.fonts.small, message, l, top + MESSAGE * s / 2.0, r - l, rgba(p.accent, 1.0));
        top += MESSAGE * s;
    }

    top += LIST_PAD * s;
    hits.push(cv.footer(x, top, (c.footer.0, &c.footer.1), c.busy, mouse));
    hits
}

fn gigabytes(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / (1u64 << 30) as f64)
}

fn uptime_text(d: std::time::Duration) -> String {
    let mins = d.as_secs() / 60;
    let (days, hours, mins) = (mins / 1440, (mins / 60) % 24, mins % 60);
    match (days, hours) {
        (0, 0) => format!("Up {mins} min"),
        (0, _) => format!("Up {hours} h {mins} min"),
        _ => format!("Up {days} d {hours} h"),
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn draw_system(
    cv: &Canvas,
    cpu: &[f32],
    memory: &[f32],
    usage: &crate::sys::Usage,
    uptime: std::time::Duration,
    view: &View,
    x: f32,
    y: f32,
) -> Vec<(Area, Action)> {
    let s = cv.scale;
    let p = cv.palette;
    let w = WIDTH * s;
    let pad = 18.0 * s;
    let fg = p.foreground;
    cv.header(x, y, "System", &uptime_text(uptime), None, view.mouse);
    let mut top = y + HEADER * s;
    let sections = [
        ("CPU", usage.cpu, String::new(), cpu),
        (
            "Memory",
            usage.memory,
            format!("{} / {} GB", gigabytes(usage.memory_used), gigabytes(usage.memory_total)),
            memory,
        ),
    ];
    for (name, value, detail, history) in sections {
        let (l, r) = (x + pad, x + w - pad);
        let cy = top + 24.0 * s;
        cv.text(cv.fonts.text, name, l, cy, 200.0 * s, rgba(fg, 0.6));
        let percent = format!("{}%", (value * 100.0).round());
        cv.text_right(cv.fonts.mono, &percent, r, cy, rgba(fg, 1.0));
        if !detail.is_empty() {
            let pw = cv.width(cv.fonts.mono, &percent);
            cv.text_right(cv.fonts.small, &detail, r - pw - 10.0 * s, cy + 1.0 * s, rgba(fg, 0.4));
        }
        cv.graph(history, (l, top + 44.0 * s, r, top + (SYSTEM_SECTION - 14.0) * s), rgba(p.accent, 1.0));
        top += SYSTEM_SECTION * s;
    }
    vec![cv.footer(x, top, ("Task Manager", &Action::Open("taskmgr")), None, view.mouse)]
}

unsafe fn draw_volume(cv: &Canvas, volume: Option<&Volume>, view: &View, x: f32, y: f32) -> Vec<(Area, Action)> {
    let s = cv.scale;
    let p = cv.palette;
    let w = WIDTH * s;
    let pad = 18.0 * s;
    let fg = p.foreground;
    let mut hits = Vec::new();
    let (level, muted) = volume.map_or((0.0, true), |v| (v.level, v.muted));
    let device =
        volume.map_or("No output device", |v| if v.device.is_empty() { "Speakers" } else { v.device.as_str() });
    cv.header(x, y, "Sound", device, None, view.mouse);

    let cy = y + (HEADER + VOLUME_BODY / 2.0) * s;
    // The speaker toggles mute.
    let icon = 18.0 * s;
    let mute_area = (x + pad - 8.0 * s, cy - 16.0 * s, x + pad + icon + 8.0 * s, cy + 16.0 * s);
    if view.mouse.is_some_and(|m| inside(mute_area, m)) {
        cv.fill(mute_area, 6.0 * s, rgba(fg, 0.08));
    }
    cv.painter(x + pad, cy, icon).speaker(level, muted, rgba(fg, 0.85), rgba(fg, 0.2));
    hits.push((mute_area, Action::ToggleMute));

    let percent = if muted { "Muted".to_string() } else { format!("{}%", (level * 100.0).round()) };
    let track = (x + pad + icon + 16.0 * s, x + w - pad - 56.0 * s);
    let track_h = 4.0 * s;
    cv.fill((track.0, cy - track_h / 2.0, track.1, cy + track_h / 2.0), track_h / 2.0, rgba(fg, 0.15));
    let end = track.0 + (track.1 - track.0) * level.clamp(0.0, 1.0);
    let color = if muted { rgba(fg, 0.4) } else { rgba(p.accent, 1.0) };
    cv.fill((track.0, cy - track_h / 2.0, end, cy + track_h / 2.0), track_h / 2.0, color);
    let knob = 7.0 * s;
    cv.fill((end - knob, cy - knob, end + knob, cy + knob), knob, color);
    cv.text_right(cv.fonts.mono, &percent, x + w - pad, cy, rgba(fg, if muted { 0.45 } else { 0.9 }));
    hits.push(((track.0 - 10.0 * s, cy - 16.0 * s, track.1 + 10.0 * s, cy + 16.0 * s), Action::VolumeSlider));
    let footer_top = y + (HEADER + VOLUME_BODY) * s;
    hits.push(cv.footer(x, footer_top, ("Sound settings", &Action::Open("ms-settings:sound")), None, view.mouse));
    hits
}

/// The action under `point`, if any.
pub fn action_at(hits: &[(Area, Action)], point: (f32, f32)) -> Option<(Area, Action)> {
    // Buttons are pushed before the rows they sit on, so the first match wins.
    hits.iter().find(|(area, _)| inside(*area, point)).cloned()
}

/// Index of the clickable area under `point` (to redraw on hover changes).
pub fn hover_at(hits: &[(Area, Action)], point: Option<(f32, f32)>) -> Option<usize> {
    point.and_then(|p| hits.iter().position(|(area, _)| inside(*area, p)))
}
