//! Hatches (`draw:hatch`: parallel lines at an angle and spacing) as a small tile picture, so they
//! draw as lines rather than as the nearest DrawingML pattern. The tile's media name records the
//! hatch, so a save writes the hatch back.

use deckcraft_color::Rgba;

/// Rasterised pixels per point.
const PX_PER_PT: f64 = 4.0;
/// Largest tile edge in pixels.
const MAX_TILE_PX: f64 = 512.0;

/// A hatch's parameters.
#[derive(Clone, Debug, PartialEq)]
pub struct Hatch {
    /// `single`, `double` or `triple`.
    pub style: String,
    pub color: Rgba,
    /// Line spacing in points.
    pub distance: f64,
    /// Line angle in degrees, counter-clockwise.
    pub rotation: f64,
    /// The background under the lines, when the hatch is drawn over a solid fill.
    pub background: Option<Rgba>,
}

const PREFIX: &str = "odp-hatch-";

impl Hatch {
    /// The media name that records this hatch.
    pub fn media_name(&self) -> String {
        let bg = self.background.map(|c| format!("{:02x}{:02x}{:02x}", c.r, c.g, c.b)).unwrap_or_else(|| "none".into());
        format!(
            "{PREFIX}{}-{:02x}{:02x}{:02x}-{}-{}-{bg}.png",
            self.style,
            self.color.r,
            self.color.g,
            self.color.b,
            crate::units::trim(self.distance, 3),
            crate::units::trim(self.rotation, 2)
        )
    }

    /// The hatch a media name records, if it is one.
    pub fn from_media_name(name: &str) -> Option<Hatch> {
        let rest = name.strip_prefix(PREFIX)?.strip_suffix(".png")?;
        let parts: Vec<&str> = rest.split('-').collect();
        let [style, color, dist, rot, bg] = parts.as_slice() else { return None };
        Some(Hatch {
            style: (*style).to_string(),
            color: Rgba::from_hex(color)?,
            distance: dist.parse().ok().filter(|d: &f64| d.is_finite() && *d > 0.0)?,
            rotation: rot.parse().ok().filter(|r: &f64| r.is_finite())?,
            background: if *bg == "none" { None } else { Some(Rgba::from_hex(bg)?) },
        })
    }

    /// A PNG tile and its edge in points.
    pub fn tile(&self) -> Option<(Vec<u8>, f64)> {
        let d = self.distance.clamp(0.5, 200.0);
        // Snap to a multiple of 45° so the tile repeats seamlessly.
        let rot = (self.rotation / 45.0).round() * 45.0;
        let mut angles = vec![rot];
        if self.style != "single" {
            angles.push(rot + 90.0);
        }
        if self.style == "triple" {
            angles.push(rot + 45.0);
        }
        let diagonal = angles.iter().any(|a| (a.rem_euclid(90.0) - 45.0).abs() < 1.0);
        let t = if diagonal { d * std::f64::consts::SQRT_2 } else { d };
        let px = (t * PX_PER_PT).round().clamp(4.0, MAX_TILE_PX);
        let k = px / t;
        let n = px as u32;
        let width = (0.6 * k).max(1.0);
        let families: Vec<(f64, f64, f64)> = angles
            .iter()
            .map(|a| {
                // A line at `a` (counter-clockwise, y down) runs along (cos a, -sin a); its normal:
                let r = a.to_radians();
                let (nx, ny) = (r.sin(), r.cos());
                // A spacing that fits a whole number of lines into the tile.
                let m = nx.abs().max(ny.abs());
                let count = (px * m / (d * k)).round().max(1.0);
                (nx, ny, px * m / count)
            })
            .collect();
        let mut img = image::RgbaImage::new(n, n);
        for (x, y, p) in img.enumerate_pixels_mut() {
            let (fx, fy) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
            let mut cover: f64 = 0.0;
            for (nx, ny, sp) in &families {
                let v = (fx * nx + fy * ny) / sp;
                let dist = (v - v.round()).abs() * sp;
                cover = cover.max((0.5 + width / 2.0 - dist).clamp(0.0, 1.0));
            }
            let (bg, ba) = match self.background {
                Some(b) => (b, 1.0),
                None => (self.color, 0.0),
            };
            let mix = |a: u8, b: u8| (f64::from(a) * (1.0 - cover) + f64::from(b) * cover).round() as u8;
            let alpha = (ba * (1.0 - cover) + cover) * 255.0;
            *p = image::Rgba([mix(bg.r, self.color.r), mix(bg.g, self.color.g), mix(bg.b, self.color.b), alpha.round() as u8]);
        }
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).ok()?;
        Some((out.into_inner(), t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_and_tiles_draw() {
        let h = Hatch { style: "double".into(), color: Rgba::rgb(0, 0, 0x80), distance: 5.67, rotation: 45.0, background: Some(Rgba::WHITE) };
        assert_eq!(Hatch::from_media_name(&h.media_name()), Some(h.clone()));
        assert_eq!(Hatch::from_media_name("photo.png"), None);
        let (png, t) = h.tile().unwrap();
        assert!(png.starts_with(b"\x89PNG"));
        assert!((t - 5.67 * std::f64::consts::SQRT_2).abs() < 1e-9);
        let single = Hatch { style: "single".into(), rotation: 0.0, background: None, ..h };
        assert!((single.tile().unwrap().1 - 5.67).abs() < 1e-9);
    }
}
