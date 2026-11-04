//! Shape geometry: `draw:transform`, SVG path data, and `draw:enhanced-geometry` (custom shapes:
//! equations, modifiers and the enhanced path language of ODF 1.3 §19.145) evaluated into model
//! paths.

use std::collections::HashMap;

use deckcraft_geom::preset::FillMode;
use deckcraft_model::CustomPath;
use kurbo::{Affine, Arc, BezPath, PathEl, Point, Vec2};

use crate::dom::{El, attr, kids};
use crate::units;

/// A parsed `draw:transform`: the affine from the shape's own (unrotated) box at the origin to the
/// page, in points.
pub fn transform(s: &str) -> Option<Affine> {
    let mut m = Affine::IDENTITY;
    let mut rest = s.trim();
    let mut any = false;
    let mut guard = 0;
    while !rest.is_empty() && guard < 32 {
        guard += 1;
        let open = rest.find('(')?;
        let close = rest.find(')')?;
        if close < open {
            return None;
        }
        let name = rest[..open].trim().to_ascii_lowercase();
        let args: Vec<&str> = rest[open + 1..close].split(|c: char| c == ',' || c.is_whitespace()).filter(|t| !t.is_empty()).collect();
        rest = rest[close + 1..].trim_start_matches([',', ' ']).trim();
        let num = |i: usize| args.get(i).and_then(|a| a.trim().parse::<f64>().ok()).filter(|v| v.is_finite());
        let len = |i: usize| args.get(i).and_then(|a| units::length(a));
        // ODF applies the list left to right: each step acts on the result of the previous one.
        let step = match name.as_str() {
            "rotate" => {
                let a = num(0)?;
                // Positive is counter-clockwise on the page (y down).
                Affine::new([a.cos(), -a.sin(), a.sin(), a.cos(), 0.0, 0.0])
            }
            "translate" => Affine::translate((len(0)?, len(1).unwrap_or(0.0))),
            "scale" => {
                let sx = num(0)?;
                Affine::scale_non_uniform(sx, num(1).unwrap_or(sx))
            }
            "skewx" => Affine::new([1.0, 0.0, -num(0)?.tan(), 1.0, 0.0, 0.0]),
            "skewy" => Affine::new([1.0, -num(0)?.tan(), 0.0, 1.0, 0.0, 0.0]),
            "matrix" => {
                let v: Vec<f64> = (0..6).map(|i| if i >= 4 { len(i).or(num(i)).unwrap_or(0.0) } else { num(i).unwrap_or(0.0) }).collect();
                Affine::new([v[0], v[1], v[2], v[3], v[4], v[5]])
            }
            _ => return None,
        };
        m = step * m;
        any = true;
    }
    any.then_some(m)
}

/// The box of a shape of size `w`×`h` placed by `m`: rotation clockwise in degrees, and a
/// mirrored transform as a rotation plus a vertical flip.
pub fn placed_box(m: Affine, w: f64, h: f64) -> deckcraft_model::Xfrm {
    let c = m * Point::new(w / 2.0, h / 2.0);
    let [a, b, c2, d, _, _] = m.as_coeffs();
    let sx = a.hypot(b);
    let det = a * d - b * c2;
    let sy = if sx > 0.0 { det.abs() / sx } else { c2.hypot(d) };
    let (w2, h2) = (w * if sx > 0.0 { sx } else { 1.0 }, h * if sy > 0.0 { sy } else { 1.0 });
    let rot = b.atan2(a).to_degrees();
    let rot = if rot.is_finite() { rot.rem_euclid(360.0) } else { 0.0 };
    let rot = if (rot - 360.0).abs() < 1e-9 { 0.0 } else { rot };
    deckcraft_model::Xfrm { x: c.x - w2 / 2.0, y: c.y - h2 / 2.0, w: w2, h: h2, rot, flip_h: false, flip_v: det < 0.0 }
}

/// The shape's geometry (preset or custom) at its size, without the renderer.
pub fn shape_geometry(geom: &deckcraft_model::Geom, w: f64, h: f64) -> deckcraft_geom::preset::Geometry {
    use deckcraft_geom::preset::{Geometry, SubPath, build};
    match geom {
        deckcraft_model::Geom::Preset { name, adj } => build(name, w, h, adj).or_else(|| build("rect", w, h, &[])).unwrap_or(Geometry {
            paths: vec![],
            text_rect: kurbo::Rect::new(0.0, 0.0, w, h),
            handles: vec![],
            sites: vec![],
        }),
        deckcraft_model::Geom::Custom { paths } => {
            let mut out = vec![];
            for p in paths {
                let (pw, ph) = (if p.w > 0.0 { p.w } else { w }, if p.h > 0.0 { p.h } else { h });
                let Ok(mut bp) = BezPath::from_svg(&p.d) else { continue };
                bp.apply_affine(Affine::scale_non_uniform(w / pw.max(1e-9), h / ph.max(1e-9)));
                out.push(SubPath { path: bp, fill: p.fill, stroke: p.stroke, even_odd: true });
            }
            Geometry { paths: out, text_rect: kurbo::Rect::new(0.0, 0.0, w, h), handles: vec![], sites: vec![] }
        }
    }
}

/// LibreOffice's default glue points 0–3: the middles of the top, right, bottom and left edges,
/// as fractions of the box.
const EDGE_MIDDLES: [(f64, f64); 4] = [(0.5, 0.0), (1.0, 0.5), (0.5, 1.0), (0.0, 0.5)];

/// A shape's connection sites as fractions of its box (none for custom outlines, or no shape).
fn site_fractions(target: Option<&deckcraft_model::Shape>) -> Vec<(f64, f64)> {
    let Some((t, x)) = target.and_then(|t| Some((t, t.xfrm?))) else { return vec![] };
    // A custom outline has no sites (and its path can be long: don't build it to learn that).
    if matches!(t.geom, deckcraft_model::Geom::Custom { .. }) {
        return vec![];
    }
    let (w, h) = (x.w.max(1e-6), x.h.max(1e-6));
    shape_geometry(&t.geom, w, h).sites.iter().map(|p| (p.x / w, p.y / h)).collect()
}

fn nearest(from: (f64, f64), to: impl Iterator<Item = (f64, f64)>) -> Option<usize> {
    let d = |p: (f64, f64)| (p.0 - from.0).powi(2) + (p.1 - from.1).powi(2);
    to.enumerate().min_by(|a, b| d(a.1).total_cmp(&d(b.1))).map(|(i, _)| i)
}

/// The default glue point nearest a shape's connection site. Shapes without sites (and missing
/// ones) are boxes, whose sites run top, left, bottom, right.
pub fn glue_point_of_site(target: Option<&deckcraft_model::Shape>, site: u32) -> u32 {
    let sites = site_fractions(target);
    let at = sites.get(site as usize).copied();
    let by_position = at.and_then(|p| nearest(p, EDGE_MIDDLES.into_iter()));
    by_position.or_else(|| crate::GLUE_POINTS.iter().position(|s| *s == site)).unwrap_or(0) as u32
}

/// The connection site nearest a default glue point.
pub fn site_of_glue_point(target: Option<&deckcraft_model::Shape>, glue: u32) -> u32 {
    let sites = site_fractions(target);
    let mid = EDGE_MIDDLES.get(glue as usize).copied();
    let by_position = mid.and_then(|m| nearest(m, sites.into_iter()));
    by_position.map(|i| i as u32).or_else(|| crate::GLUE_POINTS.get(glue as usize).copied()).unwrap_or(0)
}

/// Serialise a path in the model's syntax.
pub fn path_d(p: &BezPath) -> String {
    path_d_at(p, 3)
}

/// [`path_d`] with coordinates to `places` decimals (the same syntax as an enhanced path's).
pub fn path_d_at(p: &BezPath, places: usize) -> String {
    let f = |v: f64| units::trim(v, places);
    let mut out = String::new();
    for el in p.elements() {
        if !out.is_empty() {
            out.push(' ');
        }
        match el {
            PathEl::MoveTo(a) => out.push_str(&format!("M {} {}", f(a.x), f(a.y))),
            PathEl::LineTo(a) => out.push_str(&format!("L {} {}", f(a.x), f(a.y))),
            PathEl::QuadTo(a, b) => out.push_str(&format!("Q {} {} {} {}", f(a.x), f(a.y), f(b.x), f(b.y))),
            PathEl::CurveTo(a, b, c) => out.push_str(&format!("C {} {} {} {} {} {}", f(a.x), f(a.y), f(b.x), f(b.y), f(c.x), f(c.y))),
            PathEl::ClosePath => out.push('Z'),
        }
    }
    out
}

/// A `svg:viewBox` (`x y w h`).
pub fn view_box(s: Option<&str>) -> Option<[f64; 4]> {
    let v: Vec<f64> = s?.split(|c: char| c == ',' || c.is_whitespace()).filter(|t| !t.is_empty()).filter_map(|t| t.parse().ok()).collect();
    match v.as_slice() {
        [x, y, w, h] if w.is_finite() && h.is_finite() && x.is_finite() && y.is_finite() => Some([*x, *y, *w, *h]),
        _ => None,
    }
}

/// Longest `svg:d` read.
pub const MAX_PATH_TEXT: usize = 4 << 20;

/// SVG path data in a view box → one model path in view-box units (origin moved to 0,0).
pub fn svg_path(d: &str, vb: [f64; 4], closed_fill: bool) -> Option<CustomPath> {
    if d.len() > MAX_PATH_TEXT {
        return None;
    }
    let mut p = BezPath::from_svg(d).ok()?;
    p.apply_affine(Affine::translate((-vb[0], -vb[1])));
    // An open path (no close) is a curve: it is never filled.
    let closed = d.contains(['z', 'Z']);
    Some(CustomPath {
        w: vb[2].abs().max(1e-6),
        h: vb[3].abs().max(1e-6),
        d: path_d(&p),
        fill: if closed_fill && closed { FillMode::Norm } else { FillMode::None },
        stroke: true,
    })
}

/// `svg:points` (`x,y x,y …`) as a polygon or polyline path.
pub fn points_path(points: &str, vb: [f64; 4], close: bool) -> Option<CustomPath> {
    let nums: Vec<f64> = points.split(|c: char| c == ',' || c.is_whitespace()).filter(|t| !t.is_empty()).filter_map(|t| t.parse().ok()).collect();
    if nums.len() < 4 || nums.len() > 2_000_000 {
        return None;
    }
    let mut p = BezPath::new();
    for (i, xy) in nums.chunks_exact(2).enumerate() {
        let pt = Point::new(xy[0] - vb[0], xy[1] - vb[1]);
        if i == 0 { p.move_to(pt) } else { p.line_to(pt) }
    }
    if close {
        p.close_path();
    }
    Some(CustomPath {
        w: vb[2].abs().max(1e-6),
        h: vb[3].abs().max(1e-6),
        d: path_d(&p),
        fill: if close { FillMode::Norm } else { FillMode::None },
        stroke: true,
    })
}

// ---------------------------------------------------------------------------------------------
// Enhanced geometry

/// The evaluated geometry of a `draw:custom-shape`.
pub struct Enhanced {
    pub kind: String,
    pub modifiers: Vec<f64>,
    pub paths: Vec<CustomPath>,
    /// The first text area as fractions of the shape box (left, top, right, bottom).
    pub text_area: Option<[f64; 4]>,
    pub mirror_h: bool,
    pub mirror_v: bool,
}

const MAX_EQUATIONS: usize = 4096;
/// Nesting allowed across one whole evaluation: equation references and nested sub-expressions
/// (parentheses, signs, function arguments) all count against this one depth.
const MAX_DEPTH: usize = 256;
/// Steps (equations entered, expressions read) one shape's evaluation may take in total, over all
/// its equations, path parameters and text areas.
const MAX_WORK: usize = 1_000_000;
/// Steps all the shapes of one document may take together.
pub const DOCUMENT_WORK: usize = 50_000_000;
/// [`Eval::low`] when the value being computed depends on nothing unfinished.
const COMPLETE: usize = usize::MAX;

/// What is known about an equation during an evaluation.
enum Slot {
    /// Being evaluated, entered at this depth: a reference back to it is a cycle.
    InProgress(usize),
    /// Computed completely.
    Done(f64),
}

/// Evaluates parameters and equations of one `draw:enhanced-geometry`.
struct Eval<'a> {
    /// `svg:viewBox` (x, y, w, h).
    vb: [f64; 4],
    /// The shape's size in 1/100 mm (`logwidth`, `logheight`).
    log: [f64; 2],
    /// `draw:path-stretchpoint-x`/`-y` (`xstretch`, `ystretch`).
    stretch: [Option<f64>; 2],
    modifiers: &'a [f64],
    formulas: &'a HashMap<&'a str, Vec<Tok>>,
    slots: HashMap<&'a str, Slot>,
    /// Current nesting, shared by every equation and sub-expression.
    depth: usize,
    /// Steps taken so far, shared by the whole evaluation, and the most it may take.
    work: usize,
    max_work: usize,
    /// The shallowest depth of an unfinished equation the value being computed has read (a
    /// cycle), 0 if a depth or work cut-off fired, [`COMPLETE`] otherwise. Only values whose
    /// own evaluation leaves this at or below their depth are cached.
    low: usize,
}

impl<'a> Eval<'a> {
    fn new(vb: [f64; 4], size: (f64, f64), stretch: [Option<f64>; 2], modifiers: &'a [f64], formulas: &'a HashMap<&'a str, Vec<Tok>>) -> Self {
        let log = |v: f64| if v.is_finite() && v > 0.0 { v * units::HMM_PER_PT } else { 0.0 };
        Eval {
            vb,
            log: [log(size.0), log(size.1)],
            stretch,
            modifiers,
            formulas,
            slots: HashMap::new(),
            depth: 0,
            work: 0,
            max_work: MAX_WORK,
            low: COMPLETE,
        }
    }

    /// The identifiers of ODF 1.3 §19.171 (`draw:formula`), also the constants of handle
    /// positions, path parameters and text areas.
    fn identifier(&self, k: &str) -> Option<f64> {
        let [x, y, w, h] = self.vb;
        Some(match k {
            "pi" => std::f64::consts::PI,
            // The view box.
            "left" => x,
            "top" => y,
            "right" => x + w,
            "bottom" => y + h,
            "width" => w,
            "height" => h,
            // The shape's own size (`svg:width`/`svg:height`) in 1/100 mm.
            "logwidth" => self.log[0],
            "logheight" => self.log[1],
            // The stretch points' values.
            "xstretch" => self.stretch[0].unwrap_or(0.0),
            "ystretch" => self.stretch[1].unwrap_or(0.0),
            // Line and fill styles live outside the geometry; a shape is taken to have both.
            "hasstroke" | "hasfill" => 1.0,
            _ => return None,
        })
    }

    /// Go one level deeper and take a step; false (and the result marked cut off) past the
    /// shared depth or work budget.
    fn enter(&mut self) -> bool {
        self.work = self.work.saturating_add(1);
        if self.depth >= MAX_DEPTH || self.work > self.max_work {
            self.low = 0;
            return false;
        }
        self.depth += 1;
        true
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    /// A parameter of the path, a text area or a handle: number, `?name`, `$n` or identifier.
    fn param(&mut self, t: &str) -> f64 {
        let t = t.trim();
        if let Some(name) = t.strip_prefix('?') {
            return self.equation(name);
        }
        if let Some(n) = t.strip_prefix('$') {
            return self.modifier(n);
        }
        if let Some(v) = self.identifier(&t.to_ascii_lowercase()) {
            return v;
        }
        parse_number(t).unwrap_or(0.0)
    }

    fn modifier(&self, n: &str) -> f64 {
        n.parse::<usize>().ok().and_then(|i| self.modifiers.get(i).copied()).filter(|v| v.is_finite()).unwrap_or(0.0)
    }

    /// The value of the equation `name` (0 if there is none, it is part of a cycle being
    /// evaluated, or the budget ran out). Cached only when computed completely.
    fn equation(&mut self, name: &str) -> f64 {
        let formulas = self.formulas;
        let Some((&key, toks)) = formulas.get_key_value(name) else { return 0.0 };
        match self.slots.get(key) {
            Some(Slot::Done(v)) => return *v,
            Some(Slot::InProgress(d)) => {
                self.low = self.low.min(*d);
                return 0.0;
            }
            None => {}
        }
        if !self.enter() {
            return 0.0;
        }
        let depth = self.depth;
        self.slots.insert(key, Slot::InProgress(depth));
        let outer = std::mem::replace(&mut self.low, COMPLETE);
        let v = Parser { toks, i: 0 }.expr(self);
        let v = if v.is_finite() { v } else { 0.0 };
        self.leave();
        let own = self.low;
        if own >= depth {
            // Complete, or every cycle it met closes on itself.
            self.slots.insert(key, Slot::Done(v));
            self.low = outer;
        } else {
            // Read a value cut off or still unfinished further out: evaluate it afresh next time.
            self.slots.remove(key);
            self.low = outer.min(own);
        }
        v
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Ref(String),
    Op(char),
}

/// Length in bytes of the ODF number at the start of `s` (0 if none).
fn scan_number(s: &str) -> usize {
    units::split_number(s).map_or(0, |(n, _)| n.len())
}

/// A whole token as an ODF number; finite values only.
fn parse_number(t: &str) -> Option<f64> {
    units::number(t)
}

fn tokenize(s: &str) -> Vec<Tok> {
    let s = s.char_indices().nth(4096).and_then(|(i, _)| s.get(..i)).unwrap_or(s);
    let name_len = |r: &str| r.char_indices().find(|(_, c)| !(c.is_alphanumeric() || *c == '_')).map_or(r.len(), |(i, _)| i);
    let mut out = vec![];
    let mut i = 0;
    while let Some(rest) = s.get(i..) {
        let Some(c) = rest.chars().next() else { break };
        if c.is_whitespace() {
            i += c.len_utf8();
        } else if c.is_ascii_digit() || c == '.' {
            let n = scan_number(rest);
            if n == 0 {
                out.push(Tok::Op(c));
                i += 1;
            } else {
                out.push(Tok::Num(rest.get(..n).and_then(|t| t.parse::<f64>().ok()).filter(|v| v.is_finite()).unwrap_or(0.0)));
                i += n;
            }
        } else if c == '?' || c == '$' {
            let n = 1 + rest.get(1..).map_or(0, name_len);
            out.push(Tok::Ref(rest.get(..n).unwrap_or_default().to_string()));
            i += n;
        } else if c.is_alphabetic() {
            let n = name_len(rest);
            out.push(Tok::Ident(rest.get(..n).unwrap_or_default().to_ascii_lowercase()));
            i += n;
        } else {
            out.push(Tok::Op(c));
            i += c.len_utf8();
        }
    }
    out
}

struct Parser<'t> {
    toks: &'t [Tok],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.i)
    }
    fn expr(&mut self, ev: &mut Eval) -> f64 {
        let mut v = self.term(ev);
        while let Some(Tok::Op(c @ ('+' | '-'))) = self.peek().cloned() {
            self.i += 1;
            let r = self.term(ev);
            v = if c == '+' { v + r } else { v - r };
        }
        v
    }
    fn term(&mut self, ev: &mut Eval) -> f64 {
        let mut v = self.unary(ev);
        while let Some(Tok::Op(c @ ('*' | '/'))) = self.peek().cloned() {
            self.i += 1;
            let r = self.unary(ev);
            v = if c == '*' {
                v * r
            } else if r != 0.0 {
                v / r
            } else {
                0.0
            };
        }
        v
    }
    /// Every nesting (signs, parentheses, arguments, references) passes through here, so this
    /// is where the shared depth and work budget are charged.
    fn unary(&mut self, ev: &mut Eval) -> f64 {
        if !ev.enter() {
            // Out of budget: stop reading this formula.
            self.i = self.toks.len();
            return 0.0;
        }
        let v = match self.peek().cloned() {
            Some(Tok::Op('-')) => {
                self.i += 1;
                -self.unary(ev)
            }
            Some(Tok::Op('+')) => {
                self.i += 1;
                self.unary(ev)
            }
            _ => self.primary(ev),
        };
        ev.leave();
        v
    }
    fn args(&mut self, ev: &mut Eval) -> Vec<f64> {
        let mut out = vec![];
        if self.peek() != Some(&Tok::Op('(')) {
            return out;
        }
        self.i += 1;
        if self.peek() == Some(&Tok::Op(')')) {
            self.i += 1;
            return out;
        }
        loop {
            out.push(self.expr(ev));
            match self.peek() {
                Some(Tok::Op(',')) => self.i += 1,
                Some(Tok::Op(')')) => {
                    self.i += 1;
                    break;
                }
                _ => break,
            }
            if out.len() > 8 {
                break;
            }
        }
        out
    }
    fn primary(&mut self, ev: &mut Eval) -> f64 {
        let Some(t) = self.peek().cloned() else { return 0.0 };
        self.i += 1;
        match t {
            Tok::Num(v) => v,
            Tok::Ref(r) => ev.param(&r),
            Tok::Op('(') => {
                let v = self.expr(ev);
                if self.peek() == Some(&Tok::Op(')')) {
                    self.i += 1;
                }
                v
            }
            Tok::Ident(name) => {
                if self.peek() == Some(&Tok::Op('(')) {
                    let a = self.args(ev);
                    let g = |i: usize| a.get(i).copied().unwrap_or(0.0);
                    match name.as_str() {
                        "abs" => g(0).abs(),
                        "sqrt" => g(0).max(0.0).sqrt(),
                        "sin" => g(0).sin(),
                        "cos" => g(0).cos(),
                        "tan" => g(0).tan(),
                        "atan" => g(0).atan(),
                        "atan2" => g(0).atan2(g(1)),
                        "min" => g(0).min(g(1)),
                        "max" => g(0).max(g(1)),
                        "if" => {
                            if g(0) > 0.0 {
                                g(1)
                            } else {
                                g(2)
                            }
                        }
                        _ => 0.0,
                    }
                } else {
                    ev.identifier(&name).unwrap_or(0.0)
                }
            }
            Tok::Op(_) => 0.0,
        }
    }
}

/// Where view-box points land in the evaluated paths: shifted to the origin and, when a stretch
/// point applies (ODF 1.3 §19.204–19.205), scaled uniformly into the shape box with the part past
/// the stretch point moved to the box's far edge.
struct Frame {
    origin: Point,
    scale: f64,
    /// The coordinate space of the output: the view box's size, or the shape's when stretched.
    size: (f64, f64),
    /// Stretch point (view-box units) and how far points past it move, per axis.
    split_x: Option<(f64, f64)>,
    split_y: Option<(f64, f64)>,
}

impl Frame {
    fn new(vb: [f64; 4], w: f64, h: f64, stretch: [Option<f64>; 2]) -> Frame {
        let plain = Frame { origin: Point::new(vb[0], vb[1]), scale: 1.0, size: (vb[2], vb[3]), split_x: None, split_y: None };
        if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
            return plain;
        }
        let (kx, ky) = (w / vb[2], h / vb[3]);
        let k = kx.min(ky);
        if !(k.is_finite() && k > 0.0) {
            return plain;
        }
        // The box is relatively wider than the view box: stretch horizontally (and vice versa).
        let split_x = stretch[0].filter(|_| kx > k * (1.0 + 1e-9)).map(|s| (s, w - vb[2] * k));
        let split_y = stretch[1].filter(|_| ky > k * (1.0 + 1e-9)).map(|s| (s, h - vb[3] * k));
        if split_x.is_none() && split_y.is_none() {
            return plain;
        }
        Frame { scale: k, size: (w, h), split_x, split_y, ..plain }
    }

    /// A view-box point in the output space.
    fn map(&self, p: Point) -> Point {
        let axis = |v: f64, o: f64, split: Option<(f64, f64)>| {
            let m = (v - o) * self.scale;
            match split {
                Some((s, shift)) if v > s => m + shift,
                _ => m,
            }
        };
        Point::new(axis(p.x, self.origin.x, self.split_x), axis(p.y, self.origin.y, self.split_y))
    }

    fn map_path(&self, p: &BezPath) -> BezPath {
        let m = |a: Point| self.map(a);
        p.elements()
            .iter()
            .map(|el| match *el {
                PathEl::MoveTo(a) => PathEl::MoveTo(m(a)),
                PathEl::LineTo(a) => PathEl::LineTo(m(a)),
                PathEl::QuadTo(a, b) => PathEl::QuadTo(m(a), m(b)),
                PathEl::CurveTo(a, b, c) => PathEl::CurveTo(m(a), m(b), m(c)),
                PathEl::ClosePath => PathEl::ClosePath,
            })
            .collect()
    }

    /// A rectangle in view-box units (left, top, right, bottom) as fractions of the output.
    fn fractions(&self, r: [f64; 4]) -> [f64; 4] {
        let (a, b) = (self.map(Point::new(r[0], r[1])), self.map(Point::new(r[2], r[3])));
        let (w, h) = self.size;
        [a.x.min(b.x) / w, a.y.min(b.y) / h, a.x.max(b.x) / w, a.y.max(b.y) / h]
    }
}

/// Parameters each enhanced-path command takes (per repetition).
fn arity(cmd: char) -> Option<usize> {
    Some(match cmd {
        'M' | 'L' | 'X' | 'Y' => 2,
        'C' => 6,
        'Q' | 'G' => 4,
        'T' | 'U' => 6,
        'A' | 'B' | 'W' | 'V' => 8,
        'Z' | 'N' | 'F' | 'S' => 0,
        _ => return None,
    })
}

/// Read a `draw:enhanced-geometry` for a shape of `w`×`h` points, its formulas' steps taken from
/// the document's `budget` (none left: `None`, the shape is its box).
pub fn enhanced(g: &El, w: f64, h: f64, budget: &mut usize) -> Option<Enhanced> {
    if *budget == 0 {
        return None;
    }
    let kind = attr(g, "draw:type").unwrap_or("non-primitive").to_string();
    let vb = view_box(attr(g, "svg:viewBox")).filter(|v| v[2] > 0.0 && v[3] > 0.0).unwrap_or([0.0, 0.0, 21600.0, 21600.0]);
    let modifiers: Vec<f64> = attr(g, "draw:modifiers").unwrap_or("").split_whitespace().take(64).map(|t| parse_number(t).unwrap_or(0.0)).collect();
    let mut order = vec![];
    let mut formulas = HashMap::new();
    for e in kids(g, "draw:equation").take(MAX_EQUATIONS) {
        if let (Some(n), Some(f)) = (attr(e, "draw:name"), attr(e, "draw:formula")) {
            order.push(n);
            formulas.insert(n, tokenize(f));
        }
    }
    // Stretch points are plain numbers (data type double).
    let stretch = ["draw:path-stretchpoint-x", "draw:path-stretchpoint-y"].map(|k| attr(g, k).and_then(parse_number));
    let mut ev = Eval::new(vb, (w, h), stretch, &modifiers, &formulas);
    ev.max_work = MAX_WORK.min(*budget);
    // In document order first, so chains of equations that build on earlier ones are computed
    // one link at a time instead of all at once from the deepest reference.
    for n in order {
        ev.equation(n);
    }
    let frame = Frame::new(vb, w, h, stretch);
    let paths = eval_path(attr(g, "draw:enhanced-path").unwrap_or(""), &mut ev, &frame);
    let text_area = attr(g, "draw:text-areas").and_then(|t| {
        let toks: Vec<&str> = t.split_whitespace().take(4).collect();
        let [l, t, r, b] = toks.as_slice() else { return None };
        Some(frame.fractions([l, t, r, b].map(|p| ev.param(p))))
    });
    *budget = budget.saturating_sub(ev.work);
    Some(Enhanced {
        kind,
        modifiers,
        paths,
        text_area,
        mirror_h: attr(g, "draw:mirror-horizontal") == Some("true"),
        mirror_v: attr(g, "draw:mirror-vertical") == Some("true"),
    })
}

/// The path being drawn: the open subpath and its pen position, and the paths finished so far.
struct Pen<'f> {
    frame: &'f Frame,
    out: Vec<CustomPath>,
    bp: BezPath,
    cur: Point,
    /// Where the open subpath began (`Z` returns there).
    start: Point,
    open: bool,
    fill: bool,
    stroke: bool,
}

impl<'f> Pen<'f> {
    fn new(frame: &'f Frame) -> Self {
        Pen { frame, out: vec![], bp: BezPath::new(), cur: Point::ZERO, start: Point::ZERO, open: false, fill: true, stroke: true }
    }

    /// Start a subpath at the pen if none is open (curves continue from where the pen is).
    fn ensure_open(&mut self) {
        if !self.open {
            self.bp.move_to(self.cur);
            self.start = self.cur;
            self.open = true;
        }
    }

    /// To `p`: a new subpath there (when asked, or when none is open), else a line.
    fn to(&mut self, p: Point, new_subpath: bool) {
        if new_subpath || !self.open {
            self.bp.move_to(p);
            self.start = p;
            self.open = true;
        } else {
            self.bp.line_to(p);
        }
        self.cur = p;
    }

    fn arc(&mut self, arc: &Arc) {
        append_arc(&mut self.bp, arc);
        self.cur = arc_end(arc);
    }

    fn close(&mut self) {
        if self.open {
            self.bp.close_path();
            self.cur = self.start;
        }
        self.open = false;
    }

    /// End the path (`N`): it is mapped to the shape and kept; the next one fills and strokes
    /// unless told otherwise.
    fn finish(&mut self) {
        if !self.bp.elements().is_empty() {
            let p = self.frame.map_path(&std::mem::take(&mut self.bp));
            let (w, h) = self.frame.size;
            let fill = if self.fill { FillMode::Norm } else { FillMode::None };
            self.out.push(CustomPath { w, h, d: path_d(&p), fill, stroke: self.stroke });
        }
        (self.fill, self.stroke, self.open) = (true, true, false);
    }
}

/// Split the path into commands and their parameters, evaluate them into sub-paths placed by
/// `frame`.
fn eval_path(src: &str, ev: &mut Eval, frame: &Frame) -> Vec<CustomPath> {
    let mut pen = Pen::new(frame);
    let toks: Vec<&str> = src.split(|c: char| c.is_whitespace() || c == ',').filter(|t| !t.is_empty()).take(200_000).collect();
    let mut i = 0;
    let mut cmd = 'M';
    let mut quadrant_x = true;
    while let Some(&t) = toks.get(i) {
        let first = t.chars().next().unwrap_or(' ');
        // A command letter (single upper-case letter that isn't a keyword).
        if t.len() == 1 && first.is_ascii_uppercase() && arity(first).is_some() {
            cmd = first;
            i += 1;
            match cmd {
                'Z' => pen.close(),
                'N' => pen.finish(),
                'F' => pen.fill = false,
                'S' => pen.stroke = false,
                'X' => quadrant_x = true,
                'Y' => quadrant_x = false,
                _ => {}
            }
            continue;
        }
        let Some(n) = arity(cmd).filter(|n| *n > 0) else {
            i += 1;
            continue;
        };
        let Some(args) = toks.get(i..i + n) else { break };
        let v: Vec<f64> = args.iter().map(|t| ev.param(t)).collect();
        i += n;
        let g = |k: usize| v.get(k).copied().unwrap_or(0.0);
        let pt = |k: usize| Point::new(g(k), g(k + 1));
        match cmd {
            'M' => {
                pen.to(pt(0), true);
                // Further pairs after a moveto are linetos.
                cmd = 'L';
            }
            'L' => pen.to(pt(0), false),
            'C' => {
                pen.ensure_open();
                pen.bp.curve_to(pt(0), pt(2), pt(4));
                pen.cur = pt(4);
            }
            'Q' => {
                pen.ensure_open();
                pen.bp.quad_to(pt(0), pt(2));
                pen.cur = pt(2);
            }
            'X' | 'Y' => {
                // A quarter ellipse to the point, starting along x (or y), the axes alternating.
                let (p0, p1) = (pen.cur, pt(0));
                pen.ensure_open();
                const K: f64 = 0.552_284_749_8;
                let (c1, c2) = if quadrant_x {
                    (Point::new(p0.x + K * (p1.x - p0.x), p0.y), Point::new(p1.x, p1.y - K * (p1.y - p0.y)))
                } else {
                    (Point::new(p0.x, p0.y + K * (p1.y - p0.y)), Point::new(p1.x - K * (p1.x - p0.x), p1.y))
                };
                pen.bp.curve_to(c1, c2, p1);
                pen.cur = p1;
                quadrant_x = !quadrant_x;
            }
            'T' | 'U' => {
                // Centre, radii, start and end angle (degrees, counter-clockwise on the page).
                let (c, r) = (pt(0), Vec2::new(g(2).abs(), g(3).abs()));
                let (a0, a1) = (g(4), g(5));
                let mut sweep = a1 - a0;
                if sweep <= 0.0 {
                    sweep += 360.0;
                }
                if (a1 - a0).abs() >= 360.0 {
                    sweep = 360.0;
                }
                let arc = Arc::new(c, r, -a0.to_radians(), -sweep.to_radians(), 0.0);
                pen.to(Point::new(c.x + r.x * a0.to_radians().cos(), c.y - r.y * a0.to_radians().sin()), cmd == 'U');
                pen.arc(&arc);
            }
            'A' | 'B' | 'W' | 'V' => {
                // The ellipse in the box p1–p2, from the ray to p3 to the ray to p4.
                let (p1, p2, p3, p4) = (pt(0), pt(2), pt(4), pt(6));
                let c = p1.midpoint(p2);
                let r = Vec2::new(((p2.x - p1.x) / 2.0).abs(), ((p2.y - p1.y) / 2.0).abs());
                if r.x <= 0.0 || r.y <= 0.0 {
                    continue;
                }
                let ang = |p: Point| ((p.y - c.y) / r.y).atan2((p.x - c.x) / r.x);
                let (s, e) = (ang(p3), ang(p4));
                let mut sweep = e - s;
                if matches!(cmd, 'W' | 'V') {
                    if sweep <= 0.0 {
                        sweep += std::f64::consts::TAU;
                    }
                } else if sweep >= 0.0 {
                    sweep -= std::f64::consts::TAU;
                }
                pen.to(Point::new(c.x + r.x * s.cos(), c.y + r.y * s.sin()), matches!(cmd, 'B' | 'V'));
                pen.arc(&Arc::new(c, r, s, sweep, 0.0));
            }
            'G' => {
                // OOXML arcTo: radii, start angle and sweep in degrees, clockwise.
                let r = Vec2::new(g(0).abs(), g(1).abs());
                let (st, sw) = (g(2).to_radians(), g(3).to_radians());
                // The ellipse angle of a visual angle.
                let param = |a: f64| (a.sin() * r.x).atan2(a.cos() * r.y);
                let (s, e) = (param(st), param(st + sw));
                let mut sweep = e - s;
                if sw > 0.0 && sweep < 0.0 {
                    sweep += std::f64::consts::TAU;
                } else if sw < 0.0 && sweep > 0.0 {
                    sweep -= std::f64::consts::TAU;
                }
                if sw.abs() >= std::f64::consts::TAU - 1e-9 {
                    sweep = sw.signum() * std::f64::consts::TAU;
                }
                let c = Point::new(pen.cur.x - r.x * s.cos(), pen.cur.y - r.y * s.sin());
                pen.ensure_open();
                pen.arc(&Arc::new(c, r, s, sweep, 0.0));
            }
            _ => {}
        }
    }
    pen.finish();
    pen.out
}

/// An arc as cubic curves, to a tolerance relative to its size: the number of curves stays a
/// handful per turn at any radius (a fixed tolerance makes a huge radius take millions).
fn append_arc(bp: &mut BezPath, arc: &Arc) {
    let r = arc.radii.x.abs().max(arc.radii.y.abs());
    if !r.is_finite() || r <= 0.0 || !arc.sweep_angle.is_finite() {
        bp.line_to(arc_end(arc));
        return;
    }
    // At most a few turns: more draw the same outline again.
    let sweep = arc.sweep_angle.clamp(-4.0 * std::f64::consts::TAU, 4.0 * std::f64::consts::TAU);
    let arc = Arc { sweep_angle: sweep, ..*arc };
    arc.to_cubic_beziers((r * 1e-4).max(1e-9), |p1, p2, p| bp.curve_to(p1, p2, p));
}

fn arc_end(arc: &Arc) -> Point {
    let a = arc.start_angle + arc.sweep_angle;
    Point::new(arc.center.x + arc.radii.x * a.cos(), arc.center.y + arc.radii.y * a.sin())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A whole document's formula budget, for one shape.
    fn budget() -> usize {
        DOCUMENT_WORK
    }

    fn geom(xml: &str) -> El {
        let wrapped = format!(
            r#"<r xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0">{xml}</r>"#
        );
        crate::dom::parse(wrapped.as_bytes()).unwrap().elements().next().unwrap().clone()
    }

    #[test]
    fn transform_rotates_counter_clockwise_then_translates() {
        let m = transform("rotate (0.523598775598299) translate (19.585cm 2.451cm)").unwrap();
        let deckcraft_model::Xfrm { x, y, w, h, rot, .. } = placed_box(m, units::length("5cm").unwrap(), units::length("3cm").unwrap());
        // 30° counter-clockwise is 330° clockwise; the centre lands at about (22.5cm, 2.5cm).
        assert!((rot - 330.0).abs() < 0.01, "{rot}");
        assert!(((x + w / 2.0) - units::length("22.5cm").unwrap()).abs() < 1.0);
        assert!(((y + h / 2.0) - units::length("2.5cm").unwrap()).abs() < 1.0);
        assert!(transform("rotate(x)").is_none());
        assert!(transform("").is_none());
    }

    #[test]
    fn equations_modifiers_and_path_commands() {
        let g = geom(
            r#"<draw:enhanced-geometry svg:viewBox="0 0 21600 21600" draw:modifiers="10800" draw:enhanced-path="M ?f0 0 L 21600 21600 0 21600 Z N" draw:text-areas="?f1 10800 ?f2 18000"><draw:equation draw:name="f0" draw:formula="$0 "/><draw:equation draw:name="f1" draw:formula="$0 /2"/><draw:equation draw:name="f2" draw:formula="?f1 +10800"/></draw:enhanced-geometry>"#,
        );
        let e = enhanced(&g, 100.0, 100.0, &mut budget()).unwrap();
        assert_eq!(e.paths.len(), 1);
        assert_eq!(e.paths[0].d, "M 10800 0 L 21600 21600 L 0 21600 Z");
        let ta = e.text_area.unwrap();
        assert!((ta[0] - 0.25).abs() < 1e-9 && (ta[2] - 0.75).abs() < 1e-9);
    }

    #[test]
    fn formulas_with_functions_and_self_reference_terminate() {
        let g = geom(
            r#"<draw:enhanced-geometry svg:viewBox="0 0 100 100" draw:enhanced-path="M ?a ?b L ?c 0 N"><draw:equation draw:name="a" draw:formula="if(-1, 5, max(10, 20)) + sqrt(16)*cos(0)"/><draw:equation draw:name="b" draw:formula="?b + 1"/><draw:equation draw:name="c" draw:formula="width/2 + 3/0"/></draw:enhanced-geometry>"#,
        );
        let e = enhanced(&g, 10.0, 10.0, &mut budget()).unwrap();
        assert!(e.paths[0].d.starts_with("M 24 "), "{}", e.paths[0].d);
        assert!(e.paths[0].d.contains("L 50 0"));
    }

    #[test]
    fn arcs_and_nofill_segments() {
        let g = geom(
            r#"<draw:enhanced-geometry svg:viewBox="0 0 21600 21600" draw:enhanced-path="U 10800 10800 10800 10800 0 360 Z N F M 0 0 L 1 1 N"/>"#,
        );
        let e = enhanced(&g, 10.0, 10.0, &mut budget()).unwrap();
        assert_eq!(e.paths.len(), 2);
        assert!(e.paths[0].d.contains('C'));
        assert_eq!(e.paths[1].fill, FillMode::None);
    }

    /// Evaluate `formula` as equation `x` of a geometry with the given view box, shape size (pt),
    /// stretch points and modifiers.
    fn eval_one(formula: &str, vb: [f64; 4], size: (f64, f64), stretch: [Option<f64>; 2], modifiers: &[f64]) -> f64 {
        let formulas = HashMap::from([("x", tokenize(formula))]);
        Eval::new(vb, size, stretch, modifiers, &formulas).equation("x")
    }

    /// `draw:equation`s `f{i}` = `?f{i-1} + 1` (`f0` = 1), listed in the order given.
    fn chain(names: impl Iterator<Item = usize>) -> String {
        names
            .map(|i| {
                let f = if i == 0 { "1".to_string() } else { format!("?f{} + 1", i - 1) };
                format!(r#"<draw:equation draw:name="f{i}" draw:formula="{f}"/>"#)
            })
            .collect()
    }

    #[test]
    fn formula_identifiers_follow_odf() {
        let vb = [10.0, 20.0, 100.0, 50.0];
        // 72 × 36 pt is 2540 × 1270 (1/100 mm).
        let v = |f: &str| eval_one(f, vb, (72.0, 36.0), [Some(30.0), Some(-4.5)], &[7.0, 8.0]);
        let near = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert!(near(v("left"), 10.0) && near(v("top"), 20.0));
        assert!(near(v("right"), 110.0) && near(v("bottom"), 70.0));
        assert!(near(v("width"), 100.0) && near(v("height"), 50.0));
        assert!(near(v("logwidth"), 2540.0), "{}", v("logwidth"));
        assert!(near(v("logheight"), 1270.0), "{}", v("logheight"));
        assert!(near(v("xstretch"), 30.0) && near(v("ystretch"), -4.5));
        assert!(near(v("hasstroke"), 1.0) && near(v("hasfill"), 1.0));
        assert!(near(v("pi"), std::f64::consts::PI));
        assert!(near(v("$1 - $0 + $9"), 1.0));
        assert!(near(v("LogWidth / width"), 25.4));
        assert!(near(v("nosuchname + 2"), 2.0));
        // No stretch point reads as 0; a degenerate shape has no logical size.
        assert_eq!(eval_one("xstretch + logwidth", vb, (0.0, f64::NAN), [None, None], &[]), 0.0);
        // The same table serves path parameters.
        let g = geom(
            r#"<draw:enhanced-geometry svg:viewBox="0 0 2540 2540" draw:path-stretchpoint-x="700" draw:enhanced-path="M logwidth xstretch L right bottom N"/>"#,
        );
        let e = enhanced(&g, 72.0, 72.0, &mut budget()).unwrap();
        assert_eq!(e.paths[0].d, "M 2540 700 L 2540 2540");
    }

    #[test]
    fn a_cut_off_value_is_never_cached() {
        // A chain longer than the shared depth allows, evaluated from its top.
        let src: Vec<(String, Vec<Tok>)> =
            (0..400).map(|i| (format!("f{i}"), tokenize(&if i == 0 { "1".to_string() } else { format!("?f{} + 1", i - 1) }))).collect();
        let formulas: HashMap<&str, Vec<Tok>> = src.iter().map(|(n, t)| (n.as_str(), t.clone())).collect();
        let mut ev = Eval::new([0.0, 0.0, 1.0, 1.0], (1.0, 1.0), [None, None], &[], &formulas);
        let top = ev.equation("f399");
        assert!(top < 400.0, "the cut-off fired: {top}");
        // Nothing that saw the cut-off was kept, and the depth is unwound.
        assert!(!ev.slots.contains_key("f399") && !ev.slots.contains_key("f300"));
        assert!(ev.slots.values().all(|s| matches!(s, Slot::Done(_))));
        assert_eq!(ev.depth, 0);
        // Bottom up, every link is complete and the top comes out right.
        for i in 0..400 {
            ev.equation(&format!("f{i}"));
        }
        assert_eq!(ev.equation("f399"), 400.0);

        // Listed top first: a link reachable within the budget is right even after the top's
        // evaluation cut off below it.
        let g = geom(&format!(
            r#"<draw:enhanced-geometry svg:viewBox="0 0 1000 1000" draw:enhanced-path="M ?f399 ?f100 N">{}</draw:enhanced-geometry>"#,
            chain((0..400).rev())
        ));
        let e = enhanced(&g, 10.0, 10.0, &mut budget()).unwrap();
        assert!(e.paths[0].d.ends_with(" 101"), "{}", e.paths[0].d);
        // Listed bottom first (the usual order), any length works.
        let g = geom(&format!(
            r#"<draw:enhanced-geometry svg:viewBox="0 0 1000 1000" draw:enhanced-path="M ?f2000 0 N">{}</draw:enhanced-geometry>"#,
            chain(0..=2000)
        ));
        assert_eq!(enhanced(&g, 10.0, 10.0, &mut budget()).unwrap().paths[0].d, "M 2001 0");
    }

    #[test]
    fn cycles_read_as_zero_and_terminate() {
        let src = [("a", "?b + 1"), ("b", "?a + 1"), ("c", "?a * 10"), ("s", "?s + 1")];
        let formulas: HashMap<&str, Vec<Tok>> = src.iter().map(|(n, f)| (*n, tokenize(f))).collect();
        let mut ev = Eval::new([0.0, 0.0, 1.0, 1.0], (1.0, 1.0), [None, None], &[], &formulas);
        // a → b → a (in progress, 0): b = 1, a = 2. a closes the cycle and is kept; b, which
        // read the unfinished a, is not.
        assert_eq!(ev.equation("a"), 2.0);
        assert!(matches!(ev.slots.get("a"), Some(Slot::Done(v)) if *v == 2.0));
        assert!(!ev.slots.contains_key("b"));
        assert_eq!(ev.equation("c"), 20.0);
        assert_eq!(ev.equation("s"), 1.0);
        assert_eq!(ev.depth, 0);
        assert_eq!(ev.low, COMPLETE);
    }

    /// Huge or non-finite arc radii draw in a bounded number of curves.
    #[test]
    fn huge_arcs_stay_small() {
        let mut bp = BezPath::new();
        bp.move_to((0.0, 0.0));
        for r in [1e100, 1e300, f64::MAX] {
            let arc = Arc { center: Point::ORIGIN, radii: kurbo::Vec2::new(r, r), start_angle: 0.0, sweep_angle: 1e9, x_rotation: 0.0 };
            append_arc(&mut bp, &arc);
        }
        assert!(bp.elements().len() < 200, "{}", bp.elements().len());
    }

    #[test]
    fn hostile_formulas_stay_within_the_shared_budget() {
        // Every link reads the next one twice and the list is top first: without a shared budget
        // this is 2^4096 evaluations.
        let eqs: String = (0..4096)
            .map(|i| {
                let f = if i == 4095 { "1".to_string() } else { format!("?f{0} + ?f{0}", i + 1) };
                format!(r#"<draw:equation draw:name="f{i}" draw:formula="{f}"/>"#)
            })
            .collect();
        let deep = format!("{}1{}", "(".repeat(4000), ")".repeat(4000));
        let g = geom(&format!(
            r#"<draw:enhanced-geometry svg:viewBox="0 0 10 10" draw:enhanced-path="M ?f0 ?d L ?f2 ?f4000 N" draw:text-areas="?f0 ?d ?f1 ?f3">{eqs}<draw:equation draw:name="d" draw:formula="{deep}"/></draw:enhanced-geometry>"#
        ));
        let started = std::time::Instant::now();
        let e = enhanced(&g, 10.0, 10.0, &mut budget()).unwrap();
        assert!(started.elapsed().as_secs() < 20);
        assert!(e.paths[0].d.split(' ').filter_map(|t| t.parse::<f64>().ok()).all(f64::is_finite));
    }

    #[test]
    fn stretch_points_map_paths_and_text_area_alike() {
        // A 100×100 view box in a 200×100 pt box, stretched at x = 50.
        let g = geom(
            r#"<draw:enhanced-geometry svg:viewBox="0 0 100 100" draw:path-stretchpoint-x="50" draw:enhanced-path="M 0 0 L 40 0 60 0 100 0 100 100 0 100 Z N" draw:text-areas="10 10 90 90"/>"#,
        );
        let e = enhanced(&g, 200.0, 100.0, &mut budget()).unwrap();
        assert_eq!(e.paths[0].d, "M 0 0 L 40 0 L 160 0 L 200 0 L 200 100 L 0 100 Z");
        assert_eq!((e.paths[0].w, e.paths[0].h), (200.0, 100.0));
        // The text area moves with the outline: 10 → 10 pt and 90 → 190 pt of 200.
        let ta = e.text_area.unwrap();
        let want = [0.05, 0.1, 0.95, 0.9];
        assert!(ta.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-9), "{ta:?}");

        // Taller than the view box: stretched vertically, in a view box that does not start at 0.
        let g = geom(
            r#"<draw:enhanced-geometry svg:viewBox="10 10 100 100" draw:path-stretchpoint-x="50" draw:path-stretchpoint-y="60" draw:enhanced-path="M 10 10 L 110 110 N" draw:text-areas="10 ?t 110 110"><draw:equation draw:name="t" draw:formula="ystretch + 10"/></draw:enhanced-geometry>"#,
        );
        let e = enhanced(&g, 50.0, 100.0, &mut budget()).unwrap();
        assert_eq!(e.paths[0].d, "M 0 0 L 50 100");
        let ta = e.text_area.unwrap();
        // y = 70 is past the stretch point: (70 − 10) × 0.5 + 50 = 80 of 100.
        assert!((ta[1] - 0.8).abs() < 1e-9 && (ta[3] - 1.0).abs() < 1e-9, "{ta:?}");

        // Same proportions: nothing to stretch, the path stays in view-box units.
        let g = geom(
            r#"<draw:enhanced-geometry svg:viewBox="0 0 100 100" draw:path-stretchpoint-x="50" draw:enhanced-path="M 0 0 L 100 100 N" draw:text-areas="10 10 90 90"/>"#,
        );
        let e = enhanced(&g, 30.0, 30.0, &mut budget()).unwrap();
        assert_eq!((e.paths[0].d.as_str(), e.paths[0].w), ("M 0 0 L 100 100", 100.0));
        assert!((e.text_area.unwrap()[2] - 0.9).abs() < 1e-9);
    }

    #[test]
    fn numbers_take_exponents_with_signs() {
        let v = |f: &str| eval_one(f, [0.0, 0.0, 1.0, 1.0], (1.0, 1.0), [None, None], &[]);
        assert!((v("1e-3 + 2.5E+2") - 250.001).abs() < 1e-12);
        assert_eq!(v("3E2*2"), 600.0);
        assert_eq!(v("-.5e1"), -5.0);
        assert_eq!(v("5. + .5"), 5.5);
        // An `e` with no digits after it is not an exponent.
        assert_eq!(v("2e+width"), 2.0);
        assert_eq!(v("1e999"), 0.0);
        assert_eq!(scan_number("2E5x"), 3);
        assert_eq!(scan_number("1e+"), 1);
        assert_eq!(scan_number("."), 0);
        assert_eq!(parse_number("+1.5e-1"), Some(0.15));
        for bad in ["nan", "inf", "-infinity", "1e", "1.2.3", "", "+", "0x10"] {
            assert_eq!(parse_number(bad), None, "{bad}");
        }
        let g = geom(
            r#"<draw:enhanced-geometry svg:viewBox="0 0 1000 1000" draw:modifiers="2.5E+2" draw:path-stretchpoint-x="1e2" draw:enhanced-path="M 1e2 $0 L xstretch 5E-1 N"/>"#,
        );
        let e = enhanced(&g, 10.0, 10.0, &mut budget()).unwrap();
        assert_eq!(e.modifiers, vec![250.0]);
        assert_eq!(e.paths[0].d, "M 100 250 L 100 0.5");
    }
}
