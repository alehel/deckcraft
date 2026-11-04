//! Lengths, angles, percentages and colours as OpenDocument writes them.

use deckcraft_color::Rgba;

/// Hundredths of a millimetre (ODF's drawing unit) per point.
pub const HMM_PER_PT: f64 = 2540.0 / 72.0;
/// The largest coordinate or length written (a huge one in a hostile file would otherwise be
/// written out to hundreds of digits).
pub const MAX_COORD: f64 = 1e9;

/// A coordinate or length as written: within [`MAX_COORD`].
pub fn coord(v: f64, places: usize) -> String {
    trim(if v.is_finite() { v.clamp(-MAX_COORD, MAX_COORD) } else { 0.0 }, places)
}

/// Points per centimetre.
pub const PT_PER_CM: f64 = 72.0 / 2.54;

/// Split a leading decimal number (sign, digits with an optional `.`, optional exponent `e±n`)
/// from the unit after it: `2.5E+2pt` → (`2.5E+2`, `pt`). An `e` that no exponent digits follow
/// belongs to the unit (`1em` → `1`, `em`). `None` when there is no number.
pub(crate) fn split_number(s: &str) -> Option<(&str, &str)> {
    let b = s.as_bytes();
    let digits_from = |mut i: usize| {
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        i
    };
    let mut i = usize::from(matches!(b.first(), Some(b'+' | b'-')));
    let int_end = digits_from(i);
    let mut has_digits = int_end > i;
    i = int_end;
    if b.get(i) == Some(&b'.') {
        let frac_end = digits_from(i + 1);
        has_digits |= frac_end > i + 1;
        i = frac_end;
    }
    if !has_digits {
        return None;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        let sign = usize::from(matches!(b.get(i + 1), Some(b'+' | b'-')));
        let exp_end = digits_from(i + 1 + sign);
        if exp_end > i + 1 + sign {
            i = exp_end;
        }
    }
    // Every byte consumed is ASCII, so `i` is a char boundary.
    Some((s.get(..i)?, s.get(i..)?))
}

/// A whole token as a number (no unit), finite values only.
pub(crate) fn number(t: &str) -> Option<f64> {
    let (num, rest) = split_number(t)?;
    if !rest.is_empty() {
        return None;
    }
    num.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// A number followed by an optional unit (whitespace between them is allowed).
fn number_unit(s: &str) -> Option<(f64, &str)> {
    let (num, unit) = split_number(s.trim())?;
    let v: f64 = num.parse().ok()?;
    v.is_finite().then_some((v, unit.trim()))
}

/// A length in points. ODF lengths are a number and a unit: `cm`, `mm`, `in` (or `inch`), `pt`,
/// `pc` or `px` (`2.5cm`, `-0.5in`, `1e-3cm`, `2.5E+2pt`). A bare number is not a valid ODF
/// length but some writers emit one; it is read as points. Unknown units, non-finite or absurd
/// values (beyond ±10⁷ pt) are `None`.
pub fn length(s: &str) -> Option<f64> {
    let (v, unit) = number_unit(s)?;
    let pt = match unit.to_ascii_lowercase().as_str() {
        "cm" => v * PT_PER_CM,
        "mm" => v * 72.0 / 25.4,
        "in" | "inch" => v * 72.0,
        "pt" | "" => v,
        "pc" => v * 12.0,
        "px" => v * 0.75,
        _ => return None,
    };
    (pt.is_finite() && pt.abs() < 1.0e7).then_some(pt)
}

/// A percentage (`45%`, `4.5e1%`) as a fraction (0.45).
pub fn percent(s: &str) -> Option<f64> {
    let (v, unit) = number_unit(s)?;
    (unit == "%" && v.abs() < 1.0e6).then_some(v / 100.0)
}

/// An angle in degrees: `45deg`, `0.5rad`, `50grad`, or a bare number (`4.5e1` included). `tenths`
/// reads a bare number as tenths of a degree (ODF 1.1 gradients and hatches write
/// `draw:angle="450"`).
pub fn angle(s: &str, tenths: bool) -> Option<f64> {
    let (v, unit) = number_unit(s)?;
    let scale = match unit {
        "deg" => 1.0,
        "grad" => 0.9,
        "rad" => 180.0 / std::f64::consts::PI,
        "" if tenths => 0.1,
        "" => 1.0,
        _ => return None,
    };
    let d = v * scale;
    d.is_finite().then_some(d)
}

/// `#rrggbb` (or `#rgb`).
pub fn color(s: &str) -> Option<Rgba> {
    let h = s.trim().strip_prefix('#')?;
    match h.len() {
        6 => Rgba::from_hex(h),
        3 => {
            let full: String = h.chars().flat_map(|c| [c, c]).collect();
            Rgba::from_hex(&full)
        }
        _ => None,
    }
}

/// Points as an ODF length in centimetres (`2.54cm`), trimmed.
pub fn cm(pt: f64) -> String {
    let v = if pt.is_finite() { pt * 2.54 / 72.0 } else { 0.0 };
    format!("{}cm", coord(v, 4))
}

/// Points as `12pt`.
pub fn pt(v: f64) -> String {
    format!("{}pt", coord(v, 2))
}

pub fn pct(fraction: f64) -> String {
    format!("{}%", trim(if fraction.is_finite() { fraction * 100.0 } else { 0.0 }, 2))
}

pub fn trim(v: f64, places: usize) -> String {
    let v = if v.is_finite() { v } else { 0.0 };
    let s = format!("{v:.places$}");
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    if s == "-0" { "0".into() } else { s }
}

pub fn hex(c: Rgba) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

/// A SMIL clock value (`1.5s`, `500ms`, `0.5min`, `1h`, `00:00:01.5`) in ms.
pub fn clock(s: &str) -> Option<u32> {
    let s = s.trim();
    let v = if let Some(ms) = s.strip_suffix("ms") {
        ms.trim().parse::<f64>().ok()?
    } else if let Some(min) = s.strip_suffix("min") {
        min.trim().parse::<f64>().ok()? * 60_000.0
    } else if let Some(h) = s.strip_suffix('h') {
        h.trim().parse::<f64>().ok()? * 3_600_000.0
    } else if let Some(sec) = s.strip_suffix('s') {
        sec.trim().parse::<f64>().ok()? * 1000.0
    } else if s.contains(':') {
        let mut v = 0.0;
        for part in s.split(':').take(3) {
            v = v * 60.0 + part.trim().parse::<f64>().ok()?;
        }
        v * 1000.0
    } else {
        s.parse::<f64>().ok()? * 1000.0
    };
    (v.is_finite() && v >= 0.0).then(|| v.min(f64::from(u32::MAX)).round() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: clamping lived in `trim`, so chart values past 1e9 (axis bounds, data) were
    /// written clamped. Only coordinates are.
    #[test]
    fn only_coordinates_are_clamped() {
        assert_eq!(trim(1e12, 2), "1000000000000");
        assert_eq!(coord(1e12, 2), "1000000000");
        assert_eq!(coord(-1e300, 2), "-1000000000");
        assert_eq!(coord(f64::NAN, 2), "0");
    }

    #[test]
    fn lengths_in_every_unit() {
        assert_eq!(length("2.54cm").map(|v| v.round()), Some(72.0));
        assert_eq!(length("25.4mm").map(|v| v.round()), Some(72.0));
        assert_eq!(length("1in"), Some(72.0));
        assert_eq!(length("12pt"), Some(12.0));
        assert_eq!(length("-0.5in"), Some(-36.0));
        assert_eq!(length("bogus"), None);
        assert_eq!(length("1e300cm"), None);
        assert_eq!(length("3furlongs"), None);
        assert_eq!(length("12"), Some(12.0));
        assert_eq!(length(" 2 pt "), Some(2.0));
    }

    /// Regression: the unit started at the first letter, so the `e` of an exponent was read as the
    /// unit and `1e-3cm` was rejected.
    #[test]
    fn numbers_with_exponents() {
        let close = |a: Option<f64>, b: f64| a.is_some_and(|a| (a - b).abs() < 1e-9);
        assert!(close(length("1e-3cm"), 72.0 / 2540.0));
        assert!(close(length("2.5E+2pt"), 250.0));
        assert!(close(length("1E2mm"), 100.0 * 72.0 / 25.4));
        assert!(close(length("-1.5e1pt"), -15.0));
        assert!(close(length(".5e1in"), 360.0));
        assert!(close(length("3e2"), 300.0));
        assert!(close(percent("4.5e1%"), 0.45));
        assert!(close(percent("1E-1 %"), 0.001));
        assert!(close(angle("4.5e1deg", false), 45.0));
        assert!(close(angle("4.5e2", true), 45.0));
        assert!(close(angle("1e2grad", false), 90.0));
        // An `e` with no exponent digits is the start of a unit, which lengths don't know.
        assert_eq!(length("1em"), None);
        assert_eq!(length("1e"), None);
        assert_eq!(length("1e+cm"), None);
        assert_eq!(length("e3pt"), None);
        assert_eq!(length("."), None);
        assert_eq!(length("inf"), None);
        assert_eq!(length("NaNpt"), None);
        assert_eq!(length("1e400pt"), None);
        assert_eq!(angle("1degx", false), None);
        assert_eq!(angle("", false), None);
        assert_eq!(percent("1e1"), None);
    }

    #[test]
    fn angles_and_percent() {
        assert_eq!(angle("450", true), Some(45.0));
        assert_eq!(angle("45deg", true), Some(45.0));
        assert_eq!(angle("100grad", false), Some(90.0));
        assert_eq!(percent("45%"), Some(0.45));
        assert_eq!(percent("45"), None);
        assert_eq!(color("#ff8000").map(hex), Some("#ff8000".into()));
        assert_eq!(color("#f80").map(hex), Some("#ff8800".into()));
        assert_eq!(cm(72.0), "2.54cm");
        assert_eq!(trim(-0.00001, 3), "0");
    }
}
