//! Fills, lines, shadows and backgrounds of graphic styles.

use deckcraft_color::{ColorTransform, Rgba};
use deckcraft_model::style::{
    ColorRef, Dash, Effects, Fill, Glow, Gradient, GradientShape, GradientStop, Line, LineEnd, PatternFill, PictureAdjust, PictureFill, PictureMode,
    Shadow,
};
use deckcraft_model::{Background, MediaId};

use super::Reader;
use crate::dom::{El, attr, kids};
use crate::styles::Props;
use crate::{units, vocab};

impl Reader<'_> {
    pub(super) fn background(&mut self, page: &Props) -> Option<Background> {
        let fill = page.get("draw:fill")?;
        if fill == "none" {
            return None;
        }
        // A page style names no colour of its own: the graphic default's applies.
        let mut props = self.store.default_of("graphic").graphic;
        props.extend(page.iter().map(|(k, v)| (k.clone(), v.clone())));
        Some(Background::Fill { fill: self.fill(&props) })
    }

    pub(super) fn fill(&mut self, g: &Props) -> Fill {
        // An opacity gradient wins over a flat opacity.
        let store = std::rc::Rc::clone(&self.store);
        let og = g.get("draw:opacity-name").and_then(|n| store.opacities.get(n));
        // An opacity gradient's start and end; fills that can't vary take their average.
        let og_ends = og.map(|og| {
            let at = |k: &str| attr(og, k).and_then(units::percent).unwrap_or(1.0).clamp(0.0, 1.0);
            (at("draw:start"), at("draw:end"))
        });
        let alpha = match og_ends {
            Some((a, b)) => Some((a + b) / 2.0),
            None => g.get("draw:opacity").and_then(|v| units::percent(v)).map(|v| v.clamp(0.0, 1.0)),
        };
        let with_alpha = |c: Rgba| crate::styles::color_ref(c, alpha);
        // Resolved styles carry the built-in defaults; a page style without them is unfilled.
        let color = g.get("draw:fill-color").and_then(|c| units::color(c)).unwrap_or(Rgba::WHITE);
        match g.get("draw:fill").map(String::as_str).unwrap_or("none") {
            "none" => Fill::None,
            "solid" => match &og {
                // A solid colour under an opacity gradient: the colour, faded by the gradient.
                Some(og) => Fill::Gradient(gradient(og, &|c| ColorRef::rgb(c), Some(color))),
                None => Fill::solid(with_alpha(color)),
            },
            "gradient" => match g.get("draw:fill-gradient-name").and_then(|n| self.store.gradients.get(n)) {
                // Under an opacity gradient each stop takes the opacity where it is.
                Some(gr) => Fill::Gradient(match og_ends {
                    Some((a, b)) => {
                        let mut out = gradient(gr, &|c| ColorRef::rgb(c), None);
                        for s in &mut out.stops {
                            s.color.mods.retain(|m| !matches!(m, ColorTransform::Alpha(_)));
                            let o = a + (b - a) * s.pos;
                            if o < 1.0 {
                                s.color.mods.push(ColorTransform::Alpha((o * 100_000.0).round() as i32));
                            }
                        }
                        out
                    }
                    None => gradient(gr, &with_alpha, None),
                }),
                None => Fill::solid(with_alpha(color)),
            },
            "hatch" => {
                let h = g.get("draw:fill-hatch-name").and_then(|n| self.store.hatches.get(n));
                let ha = |k: &str| h.and_then(|h| attr(h, k));
                let hc = ha("draw:color").and_then(units::color).unwrap_or(Rgba::BLACK);
                let solid = g.get("draw:fill-hatch-solid").map(String::as_str) == Some("true");
                // Lines at the hatch's angle and spacing, as a tile picture.
                let hatch = crate::hatch::Hatch::new(
                    ha("draw:style").and_then(crate::hatch::HatchStyle::parse).unwrap_or(crate::hatch::HatchStyle::Single),
                    hc,
                    ha("draw:distance").and_then(units::length).unwrap_or(5.0),
                    ha("draw:rotation").and_then(|a| units::angle(a, true)).unwrap_or(0.0),
                    solid.then_some(color),
                );
                // Drawn once per hatch, however many shapes use it.
                let key = hatch.media_name();
                let tile = match self.hatch_tiles.get(&key) {
                    Some(t) => *t,
                    None => {
                        let t = hatch.tile().and_then(|tile| {
                            let px = f64::from(tile.px);
                            let id = self.package_media.add_bytes(&mut self.p, &key, "image/png", tile.png)?;
                            Some((id, tile.edge_pt / (0.75 * px)))
                        });
                        self.hatch_tiles.insert(key, t);
                        t
                    }
                };
                match tile {
                    Some((media, s)) => Fill::Picture(PictureFill {
                        media,
                        mode: PictureMode::Tile { tx: 0.0, ty: 0.0, sx: s, sy: s, flip: "none".into(), align: "tl".into() },
                        alpha: alpha.filter(|a| *a < 1.0),
                        ..Default::default()
                    }),
                    None => {
                        let bg = if solid { with_alpha(color) } else { ColorRef::rgb(Rgba::WHITE).with(ColorTransform::Alpha(0)) };
                        Fill::Pattern(PatternFill { preset: hatch.preset().into(), fg: with_alpha(hc), bg })
                    }
                }
            }
            "bitmap" => {
                let media = g.get("draw:fill-image-name").and_then(|n| self.fill_image(&store, n));
                match media {
                    Some(m) => {
                        let repeat = g.get("style:repeat").map(String::as_str).unwrap_or("repeat");
                        let mode = if repeat == "stretch" {
                            PictureMode::Stretch { fill_rect: [0.0; 4] }
                        } else {
                            PictureMode::Tile { tx: 0.0, ty: 0.0, sx: 1.0, sy: 1.0, flip: "none".into(), align: "tl".into() }
                        };
                        Fill::Picture(PictureFill { media: m, crop: [0.0; 4], mode, alpha: alpha.filter(|a| *a < 1.0), ..Default::default() })
                    }
                    None => Fill::solid(with_alpha(color)),
                }
            }
            _ => Fill::None,
        }
    }

    /// A named fill image as media, decoded once however many shapes use it.
    fn fill_image(&mut self, store: &crate::styles::Store, name: &str) -> Option<MediaId> {
        if let Some(m) = self.fill_images.get(name) {
            return *m;
        }
        let m = store.fill_images.get(name).and_then(|img| self.image_media(img));
        self.fill_images.insert(name.to_string(), m);
        m
    }

    pub(super) fn line(&self, g: &Props) -> Line {
        let stroke = g.get("draw:stroke").map(String::as_str).unwrap_or("none");
        if stroke == "none" {
            return Line::none();
        }
        let color = crate::styles::color_prop(g, "svg:stroke-color", "svg:stroke-opacity").unwrap_or(ColorRef::rgb(Rgba::BLACK));
        // A zero width is a hairline.
        let width = g.get("svg:stroke-width").and_then(|v| units::length(v)).filter(|w| *w > 0.0).unwrap_or(0.5).min(1000.0);
        let mut l = Line::solid(color, width);
        l.join = g.get("draw:stroke-linejoin").and_then(|j| vocab::read(vocab::LINE_JOINS, j));
        l.cap = g.get("svg:stroke-linecap").and_then(|c| vocab::read(vocab::LINE_CAPS, c));
        if stroke == "dash" {
            // A dash with no definition is LibreOffice's default dashing.
            l.dash = Some(match g.get("draw:stroke-dash").and_then(|n| self.store.dashes.get(n)) {
                Some(d) => dash(d, width),
                None => Dash::Preset { name: "dash".into() },
            });
        }
        let end = |name: Option<&String>, w: Option<&String>| -> Option<LineEnd> {
            let n = name?;
            let m = self.store.markers.get(n);
            let kind = marker_kind(n, m);
            let mw = w.and_then(|v| units::length(v)).unwrap_or(0.3 * units::PT_PER_CM);
            let ratio = mw / width.max(0.75);
            let size = if ratio <= 2.5 {
                "sm"
            } else if ratio <= 4.0 {
                "med"
            } else {
                "lg"
            };
            Some(LineEnd { kind: kind.into(), w: size.into(), len: size.into() })
        };
        l.head = end(g.get("draw:marker-start"), g.get("draw:marker-start-width"));
        l.tail = end(g.get("draw:marker-end"), g.get("draw:marker-end-width"));
        l
    }

    /// Shadow, glow and soft edges.
    pub(super) fn effects(&self, g: &Props) -> Option<Effects> {
        let len = |k: &str| g.get(k).and_then(|v| units::length(v));
        let glow = len("loext:glow-radius").filter(|r| *r > 0.0).map(|radius| {
            let opacity = g.get("loext:glow-transparency").and_then(|v| units::percent(v)).map(|t| 1.0 - t);
            let c = g.get("loext:glow-color").and_then(|c| units::color(c)).unwrap_or(Rgba::rgb(0x72, 0x9f, 0xcf));
            Glow { color: crate::styles::color_ref(c, opacity), radius }
        });
        let soft_edge = len("loext:softedge-radius").filter(|r| *r > 0.0);
        let shadow = self.shadow(g);
        (shadow.is_some() || glow.is_some() || soft_edge.is_some()).then(|| Effects { outer_shadow: shadow, glow, soft_edge, ..Default::default() })
    }

    fn shadow(&self, g: &Props) -> Option<Shadow> {
        if g.get("draw:shadow").map(String::as_str) != Some("visible") {
            return None;
        }
        let len = |k: &str| g.get(k).and_then(|v| units::length(v));
        let (dx, dy) = (len("draw:shadow-offset-x").unwrap_or(0.0), len("draw:shadow-offset-y").unwrap_or(0.0));
        let color = crate::styles::color_prop(g, "draw:shadow-color", "draw:shadow-opacity").unwrap_or(ColorRef::rgb(Rgba::rgb(0x80, 0x80, 0x80)));
        Some(Shadow {
            color,
            blur: len("loext:shadow-blur").unwrap_or(0.0),
            dist: dx.hypot(dy),
            dir: dy.atan2(dx).to_degrees().rem_euclid(360.0),
            inner: false,
            sx: 1.0,
            sy: 1.0,
            kx: 0.0,
            ky: 0.0,
            align: String::new(),
            rotate_with_shape: false,
        })
    }
}

/// The colour a fill mostly shows (for automatic text colour): `None` for no fill or a picture.
pub(super) fn fill_rgb(f: &Fill) -> Option<Rgba> {
    match f {
        Fill::Solid { color } if color.alpha() > 0.3 => Some(color.resolve(&deckcraft_model::Theme::default().colors, None)),
        Fill::Gradient(g) => {
            let (a, b) = (g.stops.first()?, g.stops.last()?);
            let scheme = deckcraft_model::Theme::default().colors;
            let (a, b) = (a.color.resolve(&scheme, None), b.color.resolve(&scheme, None));
            let mix = |x: u8, y: u8| ((u16::from(x) + u16::from(y)) / 2) as u8;
            Some(Rgba::rgb(mix(a.r, b.r), mix(a.g, b.g), mix(a.b, b.b)))
        }
        _ => None,
    }
}

pub(super) fn background_rgb(b: &Background) -> Option<Rgba> {
    match b {
        Background::Fill { fill } => fill_rgb(fill),
        Background::Ref { .. } => None,
    }
}

/// A gradient fill from a `draw:gradient`, or (with `opacity_of`) a single colour faded by a
/// `draw:opacity` gradient.
pub(super) fn gradient(gr: &El, with_alpha: &dyn Fn(Rgba) -> ColorRef, opacity_of: Option<Rgba>) -> Gradient {
    let intensity = |c: Rgba, k: &str| {
        let f = attr(gr, k).and_then(units::percent).unwrap_or(1.0).clamp(0.0, 1.0);
        Rgba::rgb((f64::from(c.r) * f).round() as u8, (f64::from(c.g) * f).round() as u8, (f64::from(c.b) * f).round() as u8)
    };
    let (start, end) = match opacity_of {
        Some(c) => {
            let a = |k: &str| (attr(gr, k).and_then(units::percent).unwrap_or(1.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            (c.with_alpha(a("draw:start")), c.with_alpha(a("draw:end")))
        }
        None => (
            intensity(attr(gr, "draw:start-color").and_then(units::color).unwrap_or(Rgba::BLACK), "draw:start-intensity"),
            intensity(attr(gr, "draw:end-color").and_then(units::color).unwrap_or(Rgba::WHITE), "draw:end-intensity"),
        ),
    };
    let border = attr(gr, "draw:border").and_then(units::percent).unwrap_or(0.0).clamp(0.0, 1.0);
    let angle = attr(gr, "draw:angle").and_then(|a| units::angle(a, true)).unwrap_or(0.0);
    let cx = attr(gr, "draw:cx").and_then(units::percent).unwrap_or(0.5).clamp(0.0, 1.0);
    let cy = attr(gr, "draw:cy").and_then(units::percent).unwrap_or(0.5).clamp(0.0, 1.0);
    let style = attr(gr, "draw:style").unwrap_or("linear");
    // LibreOffice 7.6+ multi-colour gradients list their stops.
    let mut stops: Vec<(f64, Rgba)> = kids(gr, "loext:gradient-stop")
        .take(256)
        .filter_map(|s| {
            let pos = attr(s, "svg:offset").and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite())?;
            let c = attr(s, "loext:color-value").and_then(units::color)?;
            Some((pos.clamp(0.0, 1.0), c))
        })
        .collect();
    if stops.len() < 2 {
        stops = vec![(0.0, start), (1.0, end)];
    }
    let stops = srgb_steps(&stops);
    let stop = |pos: f64, c: Rgba| {
        // The stop's own opacity (from an opacity gradient) times the fill's.
        let fill_alpha = with_alpha(Rgba::BLACK).alpha();
        let color = crate::styles::color_ref(c.with_alpha(255), Some(f64::from(c.a) / 255.0 * fill_alpha));
        GradientStop { pos: pos.clamp(0.0, 1.0), color }
    };
    // `border` is the part at the start with the start colour.
    let squeeze = |stops: &[(f64, Rgba)]| -> Vec<GradientStop> {
        let mut out = vec![];
        if border > 0.0 {
            out.push(stop(0.0, stops.first().map(|s| s.1).unwrap_or(start)));
        }
        out.extend(stops.iter().map(|(p, c)| stop(border + p * (1.0 - border), *c)));
        out
    };
    match style {
        "axial" => {
            // Start colour at both edges, end colour in the middle.
            let half: Vec<GradientStop> = squeeze(&stops).into_iter().map(|s| GradientStop { pos: s.pos / 2.0, color: s.color }).collect();
            let mut all = half.clone();
            for s in half.iter().rev() {
                all.push(GradientStop { pos: 1.0 - s.pos, color: s.color.clone() });
            }
            Gradient {
                stops: all,
                shape: GradientShape::Linear { angle: 90.0 - angle, scaled: false },
                flip: None,
                rotate_with_shape: true,
                tile: [0.0; 4],
            }
        }
        "radial" | "ellipsoid" | "square" | "rectangular" => {
            // The start colour is outside, the end colour at the centre: reverse into DrawingML
            // order (position 0 at the centre).
            let st = squeeze(&stops);
            let rev: Vec<GradientStop> = st.iter().rev().map(|s| GradientStop { pos: 1.0 - s.pos, color: s.color.clone() }).collect();
            let path = if style == "radial" || style == "ellipsoid" { "circle" } else { "rect" };
            Gradient {
                stops: rev,
                shape: GradientShape::Path { path: path.into(), focus: [cx, cy, 1.0 - cx, 1.0 - cy] },
                flip: None,
                rotate_with_shape: true,
                // The circle is centred on (cx, cy): move the tile, keeping its size.
                tile: if path == "circle" { [cx - 0.5, cy - 0.5, 0.5 - cx, 0.5 - cy] } else { [0.0; 4] },
            }
        }
        _ => Gradient {
            stops: squeeze(&stops),
            shape: GradientShape::Linear { angle: (90.0 - angle).rem_euclid(360.0), scaled: false },
            flip: None,
            rotate_with_shape: true,
            tile: [0.0; 4],
        },
    }
}

/// LibreOffice blends gradient colours in sRGB: pin that blend with intermediate stops, so a
/// renderer that interpolates otherwise draws the same ramp.
pub(super) fn srgb_steps(stops: &[(f64, Rgba)]) -> Vec<(f64, Rgba)> {
    const STEPS: usize = 8;
    let mut out = vec![];
    for w in stops.windows(2) {
        let ((p0, a), (p1, b)) = (w[0], w[1]);
        let differ = [a.r.abs_diff(b.r), a.g.abs_diff(b.g), a.b.abs_diff(b.b), a.a.abs_diff(b.a)].iter().any(|d| *d > 24);
        out.push((p0, a));
        if differ && p1 > p0 {
            for i in 1..STEPS {
                let t = i as f64 / STEPS as f64;
                let mix = |x: u8, y: u8| (f64::from(x) + (f64::from(y) - f64::from(x)) * t).round() as u8;
                out.push((p0 + (p1 - p0) * t, Rgba::rgba(mix(a.r, b.r), mix(a.g, b.g), mix(a.b, b.b), mix(a.a, b.a))));
            }
        }
    }
    if let Some(last) = stops.last() {
        out.push(*last);
    }
    out
}

pub(super) fn dash(d: &El, width: f64) -> Dash {
    let w = width.max(0.5);
    let rel = |k: &str, default: f64| -> f64 {
        match attr(d, k) {
            Some(v) if v.ends_with('%') => units::percent(v).unwrap_or(default),
            Some(v) => units::length(v).map(|l| l / w).unwrap_or(default),
            None => default,
        }
    };
    let n = |k: &str| attr(d, k).and_then(|v| v.parse::<usize>().ok()).unwrap_or(0).min(16);
    let gap = rel("draw:distance", 1.0).max(0.01);
    let mut pattern = vec![];
    for _ in 0..n("draw:dots1") {
        pattern.push((rel("draw:dots1-length", 1.0).max(0.01), gap));
    }
    for _ in 0..n("draw:dots2") {
        pattern.push((rel("draw:dots2-length", 1.0).max(0.01), gap));
    }
    if pattern.is_empty() {
        return Dash::Preset { name: "dash".into() };
    }
    Dash::Custom { pattern }
}

pub(super) fn marker_kind(name: &str, m: Option<&El>) -> &'static str {
    let n = name.to_ascii_lowercase();
    let d = m.and_then(|m| attr(m, "svg:d")).unwrap_or("");
    if n.contains("circle") || n.contains("oval") || d.contains('c') || d.contains('C') || d.contains('a') || d.contains('A') {
        "oval"
    } else if n.contains("square") || n.contains("diamond") {
        "diamond"
    } else if n.contains("line") && n.contains("arrow") {
        "arrow"
    } else if n.contains("stealth") || n.contains("concave") {
        "stealth"
    } else {
        "triangle"
    }
}

pub(super) fn picture_adjust(g: &Props) -> PictureAdjust {
    let pct = |k: &str| g.get(k).and_then(|v| units::percent(v)).unwrap_or(0.0).clamp(-1.0, 1.0);
    PictureAdjust {
        brightness: pct("draw:luminance"),
        contrast: pct("draw:contrast"),
        grayscale: g.get("draw:color-mode").map(String::as_str) == Some("greyscale"),
        bilevel: (g.get("draw:color-mode").map(String::as_str) == Some("mono")).then_some(0.5),
        ..Default::default()
    }
}
