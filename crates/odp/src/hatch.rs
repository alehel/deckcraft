//! Hatches (`draw:hatch`: parallel lines at an angle and spacing) as a small tile picture, so they
//! draw as lines rather than as the nearest DrawingML pattern.
//!
//! The model has no hatch fill, so the tile's media name records the hatch and a save writes the
//! hatch back from it. The name is `odp-hatch_<style>_<rrggbb>_<distance>_<rotation>_<bg>.png`:
//! `_` separates the fields because none of them can contain it (the style is a keyword, colours
//! are hex, numbers are unsigned decimals since the rotation is normalised to `[0, 360)`), and a
//! name is accepted only when it is exactly what [`Hatch::media_name`] writes.

use std::f64::consts::{FRAC_1_SQRT_2, SQRT_2};

use deckcraft_color::Rgba;

/// Rasterised pixels per point.
const PX_PER_PT: f64 = 4.0;
/// Smallest and largest tile edge in pixels.
const MIN_TILE_PX: f64 = 4.0;
const MAX_TILE_PX: f64 = 512.0;
/// Line spacing kept, in points.
const MIN_DISTANCE: f64 = 0.5;
const MAX_DISTANCE: f64 = 200.0;
/// Line width in points.
const LINE_PT: f64 = 0.6;

const PREFIX: &str = "odp-hatch_";
const SUFFIX: &str = ".png";

/// `draw:style`: one family of parallel lines, two crossing at 90°, or three (the third at 45°).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HatchStyle {
    Single,
    Double,
    Triple,
}

impl HatchStyle {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "single" => Some(Self::Single),
            "double" => Some(Self::Double),
            "triple" => Some(Self::Triple),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Double => "double",
            Self::Triple => "triple",
        }
    }
}

/// A hatch's parameters, normalised (see [`Hatch::new`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Hatch {
    pub style: HatchStyle,
    pub color: Rgba,
    /// Line spacing in points, within `[0.5, 200]`, to 3 decimals.
    pub distance: f64,
    /// Line angle in degrees, counter-clockwise, within `[0, 360)`, to 2 decimals.
    pub rotation: f64,
    /// The background under the lines, when the hatch is drawn over a solid fill.
    pub background: Option<Rgba>,
}

/// The rendered tile.
pub struct Tile {
    pub png: Vec<u8>,
    /// Tile edge in points.
    pub edge_pt: f64,
    /// Tile edge in pixels.
    pub px: u32,
}

fn round_to(v: f64, places: i32) -> f64 {
    let f = 10f64.powi(places);
    (v * f).round() / f
}

fn norm_distance(d: f64) -> f64 {
    if d.is_finite() { round_to(d.clamp(MIN_DISTANCE, MAX_DISTANCE), 3) } else { 5.0 }
}

fn norm_rotation(r: f64) -> f64 {
    if !r.is_finite() {
        return 0.0;
    }
    let r = round_to(r.rem_euclid(360.0), 2);
    // Rounding can land on 360 (359.999); `+ 0.0` turns -0 into 0.
    if r >= 360.0 { 0.0 } else { r + 0.0 }
}

fn hex6(c: Rgba) -> String {
    format!("{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

/// Exactly six lowercase hex digits.
fn parse_hex6(s: &str) -> Option<Rgba> {
    (s.len() == 6 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))).then(|| Rgba::from_hex(s)).flatten()
}

/// An unsigned decimal (`12`, `5.67`): no sign, exponent, or empty part.
fn parse_decimal(s: &str) -> Option<f64> {
    let (int, frac) = s.split_once('.').unwrap_or((s, "0"));
    let digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    (digits(int) && digits(frac)).then(|| s.parse().ok()).flatten()
}

/// One family of parallel lines in a tile.
struct Family {
    /// Unit normal (pixel space, y down).
    nx: f64,
    ny: f64,
    /// Spacing between lines in pixels.
    spacing: f64,
}

/// The tile's geometry before rasterising.
struct Raster {
    px: u32,
    edge_pt: f64,
    line_px: f64,
    families: Vec<Family>,
}

impl Raster {
    /// Line coverage (0–1) at a point in pixel space; periodic with the tile edge.
    fn cover(&self, fx: f64, fy: f64) -> f64 {
        let mut cover: f64 = 0.0;
        for f in &self.families {
            let v = (fx * f.nx + fy * f.ny) / f.spacing;
            let dist = (v - v.round()).abs() * f.spacing;
            cover = cover.max((0.5 + self.line_px / 2.0 - dist).clamp(0.0, 1.0));
        }
        cover
    }
}

impl Hatch {
    /// A hatch with its numbers normalised: the distance clamped to what draws and rounded, the
    /// rotation wrapped into `[0, 360)` and rounded, so the media name records it exactly.
    pub fn new(style: HatchStyle, color: Rgba, distance: f64, rotation: f64, background: Option<Rgba>) -> Self {
        let opaque = |c: Rgba| Rgba::rgb(c.r, c.g, c.b);
        Hatch {
            style,
            color: opaque(color),
            distance: norm_distance(distance),
            rotation: norm_rotation(rotation),
            background: background.map(opaque),
        }
    }

    /// The media name that records this hatch.
    pub fn media_name(&self) -> String {
        let bg = self.background.map(hex6).unwrap_or_else(|| "none".into());
        format!(
            "{PREFIX}{}_{}_{}_{}_{bg}{SUFFIX}",
            self.style.as_str(),
            hex6(self.color),
            crate::units::trim(norm_distance(self.distance), 3),
            crate::units::trim(norm_rotation(self.rotation), 2)
        )
    }

    /// The hatch a media name records, if it is exactly one [`Hatch::media_name`] writes.
    pub fn from_media_name(name: &str) -> Option<Hatch> {
        let rest = name.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?;
        let mut parts = rest.split('_');
        let (style, color, dist, rot, bg) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?, parts.next()?);
        if parts.next().is_some() {
            return None;
        }
        let distance = parse_decimal(dist).filter(|d| (MIN_DISTANCE..=MAX_DISTANCE).contains(d))?;
        let rotation = parse_decimal(rot).filter(|r| (0.0..360.0).contains(r))?;
        let background = if bg == "none" { None } else { Some(parse_hex6(bg)?) };
        let h = Hatch { style: HatchStyle::parse(style)?, color: parse_hex6(color)?, distance, rotation, background };
        // Only the canonical spelling (no `05`, `1.50`): a name means one hatch.
        (h.media_name() == name).then_some(h)
    }

    /// The tile's geometry.
    ///
    /// The rotation snaps to a multiple of 45°, so each family is axis-aligned or diagonal. A tile
    /// repeats seamlessly when every family fits a whole number of lines along its edges: an
    /// axis-aligned family repeats every `d` along an edge, a diagonal one every `d·√2`. Single and
    /// double hatches use the one period they need. A triple hatch has both kinds, and `√2` is
    /// irrational, so its tile holds 7 axis-aligned periods and 5 diagonal ones (7/5 ≈ √2), sized
    /// at their geometric mean so both spacings stay within 0.5% of `d`.
    fn raster(&self) -> Raster {
        let d = norm_distance(self.distance);
        // Multiples of 45° (mod 180°: lines are undirected).
        let base = (norm_rotation(self.rotation) / 45.0).round() as i64;
        let mut steps = vec![base];
        if self.style != HatchStyle::Single {
            steps.push(base + 2);
        }
        if self.style == HatchStyle::Triple {
            steps.push(base + 1);
        }
        let diagonal = |k: i64| k.rem_euclid(2) == 1;
        let any_diag = steps.iter().any(|k| diagonal(*k));
        let any_axis = steps.iter().any(|k| !diagonal(*k));
        let edge_pt = match (any_axis, any_diag) {
            (true, true) => d * (7.0 * 5.0 * SQRT_2).sqrt(),
            (false, true) => d * SQRT_2,
            _ => d,
        };
        let px = (edge_pt * PX_PER_PT).round().clamp(MIN_TILE_PX, MAX_TILE_PX);
        let k = px / edge_pt;
        let families = steps
            .iter()
            .map(|s| {
                // A line at `a` (counter-clockwise, y down) runs along (cos a, -sin a); its normal
                // is (sin a, cos a), written exactly for the four undirected multiples of 45°.
                let (nx, ny) = match s.rem_euclid(4) {
                    0 => (0.0, 1.0),
                    1 => (FRAC_1_SQRT_2, FRAC_1_SQRT_2),
                    2 => (1.0, 0.0),
                    _ => (FRAC_1_SQRT_2, -FRAC_1_SQRT_2),
                };
                // The normal's largest component: how far the lines' phase advances per pixel
                // along an edge, so a whole number of lines fits in the tile.
                let m = if diagonal(*s) { FRAC_1_SQRT_2 } else { 1.0 };
                let count = (edge_pt * m / d).round().max(1.0);
                Family { nx, ny, spacing: px * m / count }
            })
            .collect();
        Raster { px: px as u32, edge_pt, line_px: (LINE_PT * k).max(1.0), families }
    }

    /// The PNG tile.
    pub fn tile(&self) -> Option<Tile> {
        let r = self.raster();
        let mut img = image::RgbaImage::new(r.px, r.px);
        let (bg, ba) = match self.background {
            Some(b) => (b, 1.0),
            None => (self.color, 0.0),
        };
        for (x, y, p) in img.enumerate_pixels_mut() {
            let cover = r.cover(f64::from(x) + 0.5, f64::from(y) + 0.5);
            let mix = |a: u8, b: u8| (f64::from(a) * (1.0 - cover) + f64::from(b) * cover).round() as u8;
            let alpha = (ba * (1.0 - cover) + cover) * 255.0;
            *p = image::Rgba([mix(bg.r, self.color.r), mix(bg.g, self.color.g), mix(bg.b, self.color.b), alpha.round() as u8]);
        }
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).ok()?;
        Some(Tile { png: out.into_inner(), edge_pt: r.edge_pt, px: r.px })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STYLES: [HatchStyle; 3] = [HatchStyle::Single, HatchStyle::Double, HatchStyle::Triple];

    #[test]
    fn names_round_trip_and_tiles_draw() {
        let h = Hatch::new(HatchStyle::Double, Rgba::rgb(0, 0, 0x80), 5.67, 45.0, Some(Rgba::WHITE));
        assert_eq!(Hatch::from_media_name(&h.media_name()), Some(h.clone()));
        assert_eq!(Hatch::from_media_name("photo.png"), None);
        let tile = h.tile().unwrap();
        assert!(tile.png.starts_with(b"\x89PNG"));
        assert!((tile.edge_pt - 5.67 * SQRT_2).abs() < 1e-9);
        let single = Hatch { style: HatchStyle::Single, rotation: 0.0, background: None, ..h };
        assert!((single.tile().unwrap().edge_pt - 5.67).abs() < 1e-9);
    }

    /// Regression: a negative angle wrote `--45` into the `-`-separated name, which then failed to
    /// parse, and the hatch was lost on save.
    #[test]
    fn names_round_trip_every_angle_and_style() {
        for style in STYLES {
            for (angle, wrapped) in
                [(-45.0, 315.0), (0.0, 0.0), (90.0, 90.0), (359.0, 359.0), (725.0, 5.0), (-0.0, 0.0), (359.999, 0.0), (-720.5, 359.5)]
            {
                for bg in [None, Some(Rgba::rgb(0xff, 0x80, 0x00))] {
                    let h = Hatch::new(style, Rgba::rgb(0x12, 0xab, 0xef), 2.5, angle, bg);
                    assert_eq!(h.rotation, wrapped, "{angle}");
                    let name = h.media_name();
                    assert!(!name.contains("--"), "{name}");
                    let back = Hatch::from_media_name(&name).unwrap_or_else(|| panic!("{name}"));
                    assert_eq!(back, h, "{name}");
                    assert_eq!(back.media_name(), name);
                }
            }
        }
        // A hostile rotation or distance still names a hatch that reads back.
        for (d, r) in [(f64::NAN, f64::INFINITY), (-3.0, -1e300), (1e9, 1e-9)] {
            let h = Hatch::new(HatchStyle::Triple, Rgba::BLACK, d, r, None);
            assert_eq!(Hatch::from_media_name(&h.media_name()), Some(h));
        }
    }

    #[test]
    fn malformed_names_are_rejected() {
        for name in [
            "odp-hatch_single_000000_5_0_none",
            "odp-hatch-single-000000-5-0-none.png",
            "odp-hatch_dotted_000000_5_0_none.png",
            "odp-hatch_single_00000g_5_0_none.png",
            "odp-hatch_single_000000_-5_0_none.png",
            "odp-hatch_single_000000_5_-45_none.png",
            "odp-hatch_single_000000_5_360_none.png",
            "odp-hatch_single_000000_5_1e2_none.png",
            "odp-hatch_single_000000_5_inf_none.png",
            "odp-hatch_single_000000_5_05_none.png",
            "odp-hatch_single_000000_5_.5_none.png",
            "odp-hatch_single_000000_5_5._none.png",
            "odp-hatch_single_000000_5_0_none_extra.png",
            "odp-hatch_single_000000_5_0.png",
            "odp-hatch_single_000000_0.1_0_none.png",
            "odp-hatch_single_FFFFFF_5_0_none.png",
            "odp-hatch_single_ffffff_5_0_ffffff00.png",
            "odp-hatch_single_000000_5_0_.png",
            "odp-hatch_____.png",
        ] {
            assert_eq!(Hatch::from_media_name(name), None, "{name}");
        }
        assert!(Hatch::from_media_name("odp-hatch_single_000000_5_0_none.png.png").is_none());
        // The canonical spelling is accepted.
        assert_eq!(Hatch::from_media_name("odp-hatch_single_000000_5_0_none.png"), Some(Hatch::new(HatchStyle::Single, Rgba::BLACK, 5.0, 0.0, None)));
    }

    /// The spacing of each family's lines in points.
    fn spacings(h: &Hatch) -> Vec<f64> {
        let r = h.raster();
        let k = f64::from(r.px) / r.edge_pt;
        r.families.iter().map(|f| f.spacing / k).collect()
    }

    /// Regression: a triple hatch's tile was sized for the diagonal family only, so its horizontal
    /// and vertical lines were `√2·d` apart instead of `d`.
    #[test]
    fn every_family_keeps_the_distance() {
        for style in STYLES {
            for rot in [0.0, 30.0, 45.0, 90.0, -45.0, 135.0] {
                for d in [0.5, 2.0, 5.67, 40.0, 200.0] {
                    let h = Hatch::new(style, Rgba::BLACK, d, rot, None);
                    let sp = spacings(&h);
                    let want = match style {
                        HatchStyle::Single => 1,
                        HatchStyle::Double => 2,
                        HatchStyle::Triple => 3,
                    };
                    assert_eq!(sp.len(), want);
                    for s in sp {
                        assert!((s / d - 1.0).abs() < 0.006, "{style:?} {rot}° d={d}: spacing {s}");
                    }
                }
            }
        }
    }

    /// The tile, repeated 2×2, matches the pattern drawn directly over the larger area: the pixel
    /// columns and rows at opposite edges continue each other.
    #[test]
    fn triple_hatch_tiles_seamlessly() {
        for rot in [0.0, 45.0, 90.0, 300.0] {
            for d in [3.0, 5.67] {
                let h = Hatch::new(HatchStyle::Triple, Rgba::BLACK, d, rot, None);
                let tile = h.tile().unwrap();
                let img = image::load_from_memory(&tile.png).unwrap().to_rgba8();
                let n = tile.px;
                assert_eq!((img.width(), img.height()), (n, n));
                let r = h.raster();
                let mut worst = 0i32;
                for y in 0..2 * n {
                    for x in 0..2 * n {
                        let tiled = i32::from(img.get_pixel(x % n, y % n)[3]);
                        let direct = (r.cover(f64::from(x) + 0.5, f64::from(y) + 0.5) * 255.0).round() as i32;
                        worst = worst.max((tiled - direct).abs());
                    }
                }
                assert!(worst <= 1, "{rot}° d={d}: seam off by {worst}");
            }
        }
    }
}
