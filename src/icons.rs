//! Vector icons for the bar and its panels: Wi-Fi, Ethernet, Bluetooth,
//! usage rings, a speaker, a padlock.
//!
//! They are Direct2D shapes rather than font glyphs, so they look the same
//! whatever font the bar uses and stay crisp at every scale.

use crate::gfx::{rgba, Factories};
use crate::status::{Bluetooth, Link, Status};
use std::f32::consts::PI;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_CLOSED, D2D1_FIGURE_END_OPEN,
    D2D_SIZE_F,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1DCRenderTarget, ID2D1SolidColorBrush, ID2D1StrokeStyle, D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE,
    D2D1_ARC_SIZE_SMALL, D2D1_ELLIPSE, D2D1_SWEEP_DIRECTION_CLOCKWISE,
};
use windows_numerics::Vector2;

/// Draws into the `size`×`size` square whose left edge is `x` and whose
/// vertical centre is `cy`.
pub struct Icon<'a> {
    pub f: &'a Factories,
    pub target: &'a ID2D1DCRenderTarget,
    pub brush: &'a ID2D1SolidColorBrush,
    /// A stroke style with round caps and joins.
    pub round: &'a ID2D1StrokeStyle,
    pub x: f32,
    pub cy: f32,
    pub size: f32,
}

impl Icon<'_> {
    /// A point given in units of the icon's size, relative to its centre.
    fn at(&self, dx: f32, dy: f32) -> Vector2 {
        Vector2 { X: self.x + self.size * (0.5 + dx), Y: self.cy + self.size * dy }
    }

    unsafe fn line(&self, from: Vector2, to: Vector2, width: f32, color: D2D1_COLOR_F) {
        self.brush.SetColor(&color);
        self.target.DrawLine(from, to, self.brush, width, self.round);
    }

    unsafe fn dot(&self, center: Vector2, radius: f32, color: D2D1_COLOR_F) {
        self.brush.SetColor(&color);
        self.target.FillEllipse(&D2D1_ELLIPSE { point: center, radiusX: radius, radiusY: radius }, self.brush);
    }

    /// Strokes (or, with `fill`, fills) a path through `points`; `arc` makes
    /// each segment a circular arc of that radius instead of a straight line.
    unsafe fn shape(&self, points: &[Vector2], arc: Option<(f32, bool)>, width: f32, fill: bool, color: D2D1_COLOR_F) {
        let Some((first, rest)) = points.split_first() else { return };
        let Ok(geometry) = self.f.d2d.CreatePathGeometry() else { return };
        let Ok(sink) = geometry.Open() else { return };
        sink.BeginFigure(*first, if fill { D2D1_FIGURE_BEGIN_FILLED } else { D2D1_FIGURE_BEGIN_HOLLOW });
        for &point in rest {
            match arc {
                Some((r, large)) => sink.AddArc(&D2D1_ARC_SEGMENT {
                    point,
                    size: D2D_SIZE_F { width: r, height: r },
                    rotationAngle: 0.0,
                    sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                    arcSize: if large { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
                }),
                None => sink.AddLine(point),
            }
        }
        sink.EndFigure(if fill { D2D1_FIGURE_END_CLOSED } else { D2D1_FIGURE_END_OPEN });
        if sink.Close().is_ok() {
            self.brush.SetColor(&color);
            if fill {
                self.target.FillGeometry(&geometry, self.brush, None);
            } else {
                self.target.DrawGeometry(&geometry, self.brush, width, self.round);
            }
        }
    }

    unsafe fn path(&self, points: &[Vector2], arc: Option<f32>, width: f32, color: D2D1_COLOR_F) {
        self.shape(points, arc.map(|r| (r, false)), width, false, color);
    }

    unsafe fn strike(&self, color: D2D1_COLOR_F) {
        self.line(self.at(-0.42, -0.42), self.at(0.42, 0.42), 0.12 * self.size, color);
    }

    /// Wi-Fi signal arcs, an Ethernet symbol, or a dimmed (and, with the
    /// Wi-Fi radio off, struck-through) Wi-Fi symbol when disconnected.
    pub unsafe fn network(&self, st: &Status, fg: u32) {
        // Without internet access the lit part is drawn dimmer.
        let lit = rgba(fg, if st.internet { 0.92 } else { 0.55 });
        let dim = rgba(fg, 0.22);
        match st.link {
            Link::Wifi(bars) => self.wifi(bars, lit, dim),
            Link::Wired => self.ethernet(lit),
            Link::None => match st.wifi_radio {
                Some(on) => {
                    self.wifi(0, lit, dim);
                    if !on {
                        self.strike(rgba(fg, 0.7));
                    }
                }
                None => self.ethernet(dim),
            },
        }
    }

    /// The Wi-Fi fan: a dot and three arcs, lit for Windows' 0-5 bars.
    pub unsafe fn wifi(&self, bars: u8, lit: D2D1_COLOR_F, dim: D2D1_COLOR_F) {
        let level = (bars.min(5) * 3).div_ceil(5);
        let s = self.size;
        let (bottom, spread) = (0.34, 48f32.to_radians());
        for (i, r) in [0.26f32, 0.48, 0.70].into_iter().enumerate() {
            let (dx, dy) = (r * spread.sin(), bottom - r * spread.cos());
            let color = if (i as u8) < level { lit } else { dim };
            self.path(&[self.at(-dx, dy), self.at(dx, dy)], Some(r * s), 0.12 * s, color);
        }
        self.dot(self.at(0.0, bottom - 0.02), 0.09 * s, if level > 0 { lit } else { dim });
    }

    /// Three connected nodes.
    pub unsafe fn ethernet(&self, color: D2D1_COLOR_F) {
        let s = self.size;
        let node = |cx: f32, cy: f32| {
            let half = 0.14;
            let (a, b) = (self.at(cx - half, cy - half), self.at(cx + half, cy + half));
            self.brush.SetColor(&color);
            self.target.FillRoundedRectangle(&crate::gfx::shape(a.X, a.Y, b.X, b.Y, 0.06 * s), self.brush);
        };
        node(0.0, -0.28);
        node(-0.3, 0.28);
        node(0.3, 0.28);
        let w = 0.1 * s;
        self.line(self.at(0.0, -0.2), self.at(0.0, 0.0), w, color);
        self.path(&[self.at(-0.3, 0.2), self.at(-0.3, 0.0), self.at(0.3, 0.0), self.at(0.3, 0.2)], None, w, color);
    }

    /// The Bluetooth rune: dim when off (and struck through), brighter when
    /// on, and flanked by two accent dots while a device is connected.
    pub unsafe fn bluetooth(&self, state: Option<Bluetooth>, fg: u32, accent: u32) {
        let s = self.size;
        let color = match state {
            Some(Bluetooth::Connected) => rgba(fg, 0.92),
            Some(Bluetooth::On) => rgba(fg, 0.6),
            _ => rgba(fg, 0.25),
        };
        self.rune(color);
        match state {
            Some(Bluetooth::Connected) => {
                for dx in [-0.42, 0.42] {
                    self.dot(self.at(dx, 0.0), 0.07 * s, rgba(accent, 1.0));
                }
            }
            Some(Bluetooth::Off) => self.strike(rgba(fg, 0.7)),
            _ => {}
        }
    }

    /// The bare Bluetooth rune.
    pub unsafe fn rune(&self, color: D2D1_COLOR_F) {
        let (l, r, top, mid) = (-0.2, 0.2, 0.44, 0.2);
        let rune = [
            self.at(l, -mid),
            self.at(r, mid),
            self.at(0.0, top),
            self.at(0.0, -top),
            self.at(r, -mid),
            self.at(l, mid),
        ];
        self.path(&rune, None, 0.1 * self.size, color);
    }

    /// A small padlock (secured Wi-Fi networks).
    pub unsafe fn lock(&self, color: D2D1_COLOR_F) {
        let s = self.size;
        let (a, b) = (self.at(-0.26, -0.02), self.at(0.26, 0.38));
        self.brush.SetColor(&color);
        self.target.FillRoundedRectangle(&crate::gfx::shape(a.X, a.Y, b.X, b.Y, 0.07 * s), self.brush);
        let r = 0.16;
        self.path(&[self.at(-r, -0.02), self.at(-r, -0.2)], None, 0.09 * s, color);
        self.path(&[self.at(-r, -0.2), self.at(r, -0.2)], Some(r * s), 0.09 * s, color);
        self.path(&[self.at(r, -0.2), self.at(r, -0.02)], None, 0.09 * s, color);
    }

    /// A gauge ring filled clockwise from the top to `fraction` (0-1).
    pub unsafe fn ring(&self, fraction: f32, width: f32, lit: D2D1_COLOR_F, dim: D2D1_COLOR_F) {
        let r = self.size / 2.0 - width / 2.0;
        let center = self.at(0.0, 0.0);
        self.brush.SetColor(&dim);
        self.target.DrawEllipse(&D2D1_ELLIPSE { point: center, radiusX: r, radiusY: r }, self.brush, width, None);
        let fraction = fraction.clamp(0.0, 1.0);
        if fraction >= 0.999 {
            self.brush.SetColor(&lit);
            self.target.DrawEllipse(&D2D1_ELLIPSE { point: center, radiusX: r, radiusY: r }, self.brush, width, None);
        } else if fraction > 0.01 {
            let angle = fraction * 2.0 * PI;
            let start = Vector2 { X: center.X, Y: center.Y - r };
            let end = Vector2 { X: center.X + r * angle.sin(), Y: center.Y - r * angle.cos() };
            self.shape(&[start, end], Some((r, fraction > 0.5)), width, false, lit);
        }
    }

    /// A speaker with 0-3 sound waves for the level, or a cross when muted.
    pub unsafe fn speaker(&self, level: f32, muted: bool, color: D2D1_COLOR_F, dim: D2D1_COLOR_F) {
        let s = self.size;
        let body = [
            self.at(-0.42, -0.14),
            self.at(-0.22, -0.14),
            self.at(0.0, -0.36),
            self.at(0.0, 0.36),
            self.at(-0.22, 0.14),
            self.at(-0.42, 0.14),
        ];
        self.shape(&body, None, 0.0, true, color);
        if muted {
            self.line(self.at(0.14, -0.16), self.at(0.44, 0.16), 0.1 * s, color);
            self.line(self.at(0.14, 0.16), self.at(0.44, -0.16), 0.1 * s, color);
            return;
        }
        let waves = if level <= 0.0 { 0 } else { 1 + (level * 2.99) as usize };
        for (i, r) in [0.16f32, 0.30, 0.44].into_iter().enumerate() {
            let spread = 50f32.to_radians();
            // An arc around the speaker's mouth, from upper right to lower right.
            let (dx, dy) = (0.04 + r * spread.cos(), r * spread.sin());
            let c = if i < waves { color } else { dim };
            self.path(&[self.at(dx, -dy), self.at(dx, dy)], Some(r * s), 0.09 * s, c);
        }
    }
}
