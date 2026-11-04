//! Lengths, angles, percentages and colours as OpenDocument writes them.

use deckcraft_color::Rgba;

/// A length (`2.5cm`, `10mm`, `1in`, `12pt`, `1pc`, `96px`, bare number = cm in practice is not
/// allowed, so a bare number is read as points) in points. Non-finite or absurd values are `None`.
pub fn length(s: &str) -> Option<f64> {
    let s = s.trim();
    let split = s.find(|c: char| c.is_ascii_alphabetic() || c == '%').unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let v: f64 = num.trim().parse().ok()?;
    let pt = match unit.trim().to_ascii_lowercase().as_str() {
        "cm" => v * 72.0 / 2.54,
        "mm" => v * 72.0 / 25.4,
        "in" | "inch" => v * 72.0,
        "pt" | "" => v,
        "pc" => v * 12.0,
        "px" => v * 0.75,
        _ => return None,
    };
    (pt.is_finite() && pt.abs() < 1.0e7).then_some(pt)
}

/// A percentage (`45%`) as a fraction (0.45).
pub fn percent(s: &str) -> Option<f64> {
    let v: f64 = s.trim().strip_suffix('%')?.trim().parse().ok()?;
    (v.is_finite() && v.abs() < 1.0e6).then_some(v / 100.0)
}

/// An angle in degrees: `45deg`, `0.5rad`, `50grad`, or a bare number. `tenths` reads a bare
/// number as tenths of a degree (ODF 1.1 gradients and hatches write `draw:angle="450"`).
pub fn angle(s: &str, tenths: bool) -> Option<f64> {
    let s = s.trim();
    let (v, scale) = if let Some(n) = s.strip_suffix("deg") {
        (n, 1.0)
    } else if let Some(n) = s.strip_suffix("grad") {
        (n, 0.9)
    } else if let Some(n) = s.strip_suffix("rad") {
        (n, 180.0 / std::f64::consts::PI)
    } else {
        (s, if tenths { 0.1 } else { 1.0 })
    };
    let d = v.trim().parse::<f64>().ok()? * scale;
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
    format!("{}cm", trim(v, 4))
}

/// Points as `12pt`.
pub fn pt(v: f64) -> String {
    format!("{}pt", trim(if v.is_finite() { v } else { 0.0 }, 2))
}

pub fn pct(fraction: f64) -> String {
    format!("{}%", trim(if fraction.is_finite() { fraction * 100.0 } else { 0.0 }, 2))
}

pub fn trim(v: f64, places: usize) -> String {
    let s = format!("{v:.places$}");
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    if s == "-0" { "0".into() } else { s }
}

pub fn hex(c: Rgba) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

#[cfg(test)]
mod tests {
    use super::*;

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
