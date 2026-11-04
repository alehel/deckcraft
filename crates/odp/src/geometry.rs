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

/// Box (x, y, w, h), rotation clockwise in degrees and a vertical flip, for a shape of size
/// `w`×`h` placed by `m` (a mirrored transform is a rotation plus a flip).
pub fn placed_box(m: Affine, w: f64, h: f64) -> (f64, f64, f64, f64, f64, bool) {
    let c = m * Point::new(w / 2.0, h / 2.0);
    let [a, b, c2, d, _, _] = m.as_coeffs();
    let sx = a.hypot(b);
    let det = a * d - b * c2;
    let sy = if sx > 0.0 { det.abs() / sx } else { c2.hypot(d) };
    let (w2, h2) = (w * if sx > 0.0 { sx } else { 1.0 }, h * if sy > 0.0 { sy } else { 1.0 });
    let rot = b.atan2(a).to_degrees();
    let rot = if rot.is_finite() { rot.rem_euclid(360.0) } else { 0.0 };
    let rot = if (rot - 360.0).abs() < 1e-9 { 0.0 } else { rot };
    (c.x - w2 / 2.0, c.y - h2 / 2.0, w2, h2, rot, det < 0.0)
}

/// Serialise a path in the model's syntax.
pub fn path_d(p: &BezPath) -> String {
    let f = |v: f64| units::trim(v, 3);
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

/// SVG path data in a view box → one model path in view-box units (origin moved to 0,0).
pub fn svg_path(d: &str, vb: [f64; 4], closed_fill: bool) -> Option<CustomPath> {
    if d.len() > 4 * 1024 * 1024 {
        return None;
    }
    let mut p = BezPath::from_svg(d).ok()?;
    p.apply_affine(Affine::translate((-vb[0], -vb[1])));
    Some(CustomPath {
        w: vb[2].abs().max(1e-6),
        h: vb[3].abs().max(1e-6),
        d: path_d(&p),
        fill: if closed_fill { FillMode::Norm } else { FillMode::None },
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
const MAX_EVAL_DEPTH: usize = 64;

struct Eval<'a> {
    vb: [f64; 4],
    modifiers: &'a [f64],
    formulas: HashMap<&'a str, &'a str>,
    cache: HashMap<String, f64>,
}

impl Eval<'_> {
    fn keyword(&self, k: &str) -> Option<f64> {
        let [x, y, w, h] = self.vb;
        Some(match k {
            "left" => x,
            "top" => y,
            "right" => x + w,
            "bottom" => y + h,
            "width" | "logwidth" => w,
            "height" | "logheight" => h,
            "xstretch" | "ystretch" => 0.0,
            "hasstroke" | "hasfill" => 1.0,
            "pi" => std::f64::consts::PI,
            _ => return None,
        })
    }

    /// A parameter of the path or a handle: number, `?name`, `$n` or keyword.
    fn param(&mut self, t: &str, depth: usize) -> f64 {
        let t = t.trim();
        if let Some(name) = t.strip_prefix('?') {
            return self.equation(name, depth);
        }
        if let Some(n) = t.strip_prefix('$') {
            return n.parse::<usize>().ok().and_then(|i| self.modifiers.get(i).copied()).unwrap_or(0.0);
        }
        if let Some(v) = self.keyword(t) {
            return v;
        }
        t.parse::<f64>().ok().filter(|v| v.is_finite()).unwrap_or(0.0)
    }

    fn equation(&mut self, name: &str, depth: usize) -> f64 {
        if let Some(v) = self.cache.get(name) {
            return *v;
        }
        if depth > MAX_EVAL_DEPTH {
            return 0.0;
        }
        let Some(f) = self.formulas.get(name).copied() else { return 0.0 };
        let toks = tokenize(f);
        let mut p = Parser { toks: &toks, i: 0 };
        let v = p.expr(self, depth + 1, 0);
        let v = if v.is_finite() { v } else { 0.0 };
        self.cache.insert(name.to_string(), v);
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

fn tokenize(s: &str) -> Vec<Tok> {
    let mut out = vec![];
    let cs: Vec<char> = s.chars().take(4096).collect();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() || c == '.' {
            let st = i;
            while i < cs.len() && (cs[i].is_ascii_digit() || cs[i] == '.' || cs[i] == 'e' && i + 1 < cs.len() && cs[i + 1].is_ascii_digit()) {
                i += 1;
            }
            out.push(Tok::Num(cs[st..i].iter().collect::<String>().parse().unwrap_or(0.0)));
        } else if c == '?' || c == '$' {
            let st = i;
            i += 1;
            while i < cs.len() && (cs[i].is_alphanumeric() || cs[i] == '_') {
                i += 1;
            }
            out.push(Tok::Ref(cs[st..i].iter().collect()));
        } else if c.is_alphabetic() {
            let st = i;
            while i < cs.len() && (cs[i].is_alphanumeric() || cs[i] == '_') {
                i += 1;
            }
            out.push(Tok::Ident(cs[st..i].iter().collect::<String>().to_ascii_lowercase()));
        } else {
            out.push(Tok::Op(c));
            i += 1;
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
    fn expr(&mut self, ev: &mut Eval, depth: usize, nest: usize) -> f64 {
        let mut v = self.term(ev, depth, nest);
        while let Some(Tok::Op(c @ ('+' | '-'))) = self.peek().cloned() {
            self.i += 1;
            let r = self.term(ev, depth, nest);
            v = if c == '+' { v + r } else { v - r };
        }
        v
    }
    fn term(&mut self, ev: &mut Eval, depth: usize, nest: usize) -> f64 {
        let mut v = self.unary(ev, depth, nest);
        while let Some(Tok::Op(c @ ('*' | '/'))) = self.peek().cloned() {
            self.i += 1;
            let r = self.unary(ev, depth, nest);
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
    fn unary(&mut self, ev: &mut Eval, depth: usize, nest: usize) -> f64 {
        if nest > MAX_EVAL_DEPTH {
            return 0.0;
        }
        match self.peek().cloned() {
            Some(Tok::Op('-')) => {
                self.i += 1;
                -self.unary(ev, depth, nest + 1)
            }
            Some(Tok::Op('+')) => {
                self.i += 1;
                self.unary(ev, depth, nest + 1)
            }
            _ => self.primary(ev, depth, nest),
        }
    }
    fn args(&mut self, ev: &mut Eval, depth: usize, nest: usize) -> Vec<f64> {
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
            out.push(self.expr(ev, depth, nest + 1));
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
    fn primary(&mut self, ev: &mut Eval, depth: usize, nest: usize) -> f64 {
        let Some(t) = self.peek().cloned() else { return 0.0 };
        self.i += 1;
        match t {
            Tok::Num(v) => v,
            Tok::Ref(r) => ev.param(&r, depth),
            Tok::Op('(') => {
                let v = self.expr(ev, depth, nest + 1);
                if self.peek() == Some(&Tok::Op(')')) {
                    self.i += 1;
                }
                v
            }
            Tok::Ident(name) => {
                if self.peek() == Some(&Tok::Op('(')) {
                    let a = self.args(ev, depth, nest);
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
                    ev.keyword(&name).unwrap_or(0.0)
                }
            }
            Tok::Op(_) => 0.0,
        }
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

/// Read a `draw:enhanced-geometry` for a shape of `w`×`h` points.
pub fn enhanced(g: &El, w: f64, h: f64) -> Option<Enhanced> {
    let kind = attr(g, "draw:type").unwrap_or("non-primitive").to_string();
    let vb = view_box(attr(g, "svg:viewBox")).filter(|v| v[2] > 0.0 && v[3] > 0.0).unwrap_or([0.0, 0.0, 21600.0, 21600.0]);
    let modifiers: Vec<f64> = attr(g, "draw:modifiers")
        .unwrap_or("")
        .split_whitespace()
        .take(64)
        .map(|t| t.parse::<f64>().ok().filter(|v| v.is_finite()).unwrap_or(0.0))
        .collect();
    let mut formulas = HashMap::new();
    for e in kids(g, "draw:equation").take(MAX_EQUATIONS) {
        if let (Some(n), Some(f)) = (attr(e, "draw:name"), attr(e, "draw:formula")) {
            formulas.insert(n, f);
        }
    }
    let mut ev = Eval { vb, modifiers: &modifiers, formulas, cache: HashMap::new() };
    let _ = (w, h);
    let path = attr(g, "draw:enhanced-path").unwrap_or("");
    let paths = eval_path(path, &mut ev);
    let text_area = attr(g, "draw:text-areas").and_then(|t| {
        let toks: Vec<&str> = t.split_whitespace().take(4).collect();
        if toks.len() < 4 {
            return None;
        }
        let v: Vec<f64> = toks.iter().map(|t| ev.param(t, 0)).collect();
        let fx = |x: f64| (x - vb[0]) / vb[2];
        let fy = |y: f64| (y - vb[1]) / vb[3];
        Some([fx(v[0].min(v[2])), fy(v[1].min(v[3])), fx(v[0].max(v[2])), fy(v[1].max(v[3]))])
    });
    Some(Enhanced {
        kind,
        modifiers,
        paths,
        text_area,
        mirror_h: attr(g, "draw:mirror-horizontal") == Some("true"),
        mirror_v: attr(g, "draw:mirror-vertical") == Some("true"),
    })
}

/// Split the path into commands and their parameters, evaluate them into sub-paths.
fn eval_path(src: &str, ev: &mut Eval) -> Vec<CustomPath> {
    let [vx, vy, vw, vh] = ev.vb;
    let mut out: Vec<CustomPath> = vec![];
    let mut bp = BezPath::new();
    let (mut fill, mut stroke) = (true, true);
    let mut cur = Point::ZERO;
    let mut start = Point::ZERO;
    let mut open = false;
    let toks: Vec<&str> = src.split(|c: char| c.is_whitespace() || c == ',').filter(|t| !t.is_empty()).take(200_000).collect();
    let mut i = 0;
    let mut cmd = 'M';
    let mut quadrant_x = true;
    let flush = |bp: &mut BezPath, fill: &mut bool, stroke: &mut bool, out: &mut Vec<CustomPath>| {
        if !bp.elements().is_empty() {
            let mut p = std::mem::take(bp);
            p.apply_affine(Affine::translate((-vx, -vy)));
            out.push(CustomPath { w: vw, h: vh, d: path_d(&p), fill: if *fill { FillMode::Norm } else { FillMode::None }, stroke: *stroke });
        }
        *fill = true;
        *stroke = true;
    };
    while i < toks.len() {
        let t = toks[i];
        let first = t.chars().next().unwrap_or(' ');
        // A command letter (single upper-case letter that isn't a keyword).
        if t.len() == 1 && first.is_ascii_uppercase() && arity(first).is_some() {
            cmd = first;
            i += 1;
            match cmd {
                'Z' => {
                    if open {
                        bp.close_path();
                        cur = start;
                    }
                    open = false;
                    continue;
                }
                'N' => {
                    flush(&mut bp, &mut fill, &mut stroke, &mut out);
                    open = false;
                    continue;
                }
                'F' => {
                    fill = false;
                    continue;
                }
                'S' => {
                    stroke = false;
                    continue;
                }
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
        if i + n > toks.len() {
            break;
        }
        let v: Vec<f64> = toks[i..i + n].iter().map(|t| ev.param(t, 0)).collect();
        i += n;
        let pt = |k: usize| Point::new(v[k], v[k + 1]);
        match cmd {
            'M' => {
                bp.move_to(pt(0));
                cur = pt(0);
                start = cur;
                open = true;
                // Further pairs after a moveto are linetos.
                cmd = 'L';
            }
            'L' => {
                line_or_move(&mut bp, &mut open, &mut start, pt(0));
                cur = pt(0);
            }
            'C' => {
                if !open {
                    bp.move_to(cur);
                    start = cur;
                    open = true;
                }
                bp.curve_to(pt(0), pt(2), pt(4));
                cur = pt(4);
            }
            'Q' => {
                if !open {
                    bp.move_to(cur);
                    start = cur;
                    open = true;
                }
                bp.quad_to(pt(0), pt(2));
                cur = pt(2);
            }
            'X' | 'Y' => {
                let p1 = pt(0);
                if !open {
                    bp.move_to(cur);
                    start = cur;
                    open = true;
                }
                const K: f64 = 0.552_284_749_8;
                let (c1, c2) = if quadrant_x {
                    (Point::new(cur.x + K * (p1.x - cur.x), cur.y), Point::new(p1.x, p1.y - K * (p1.y - cur.y)))
                } else {
                    (Point::new(cur.x, cur.y + K * (p1.y - cur.y)), Point::new(p1.x - K * (p1.x - cur.x), p1.y))
                };
                bp.curve_to(c1, c2, p1);
                cur = p1;
                quadrant_x = !quadrant_x;
            }
            'T' | 'U' => {
                // Centre, radii, start and end angle (degrees, counter-clockwise on the page).
                let (c, r) = (pt(0), Vec2::new(v[2].abs(), v[3].abs()));
                let (a0, a1) = (v[4], v[5]);
                let mut sweep = a1 - a0;
                if sweep <= 0.0 {
                    sweep += 360.0;
                }
                if (a1 - a0).abs() >= 360.0 {
                    sweep = 360.0;
                }
                let arc = Arc::new(c, r, -a0.to_radians(), -sweep.to_radians(), 0.0);
                let s = Point::new(c.x + r.x * a0.to_radians().cos(), c.y - r.y * a0.to_radians().sin());
                if cmd == 'U' || !open {
                    bp.move_to(s);
                    start = s;
                    open = true;
                } else {
                    bp.line_to(s);
                }
                append_arc(&mut bp, &arc);
                cur = arc_end(&arc);
            }
            'A' | 'B' | 'W' | 'V' => {
                let (p1, p2, p3, p4) = (pt(0), pt(2), pt(4), pt(6));
                let c = p1.midpoint(p2);
                let r = Vec2::new(((p2.x - p1.x) / 2.0).abs(), ((p2.y - p1.y) / 2.0).abs());
                if r.x <= 0.0 || r.y <= 0.0 {
                    continue;
                }
                let ang = |p: Point| ((p.y - c.y) / r.y).atan2((p.x - c.x) / r.x);
                let (s, e) = (ang(p3), ang(p4));
                let clockwise = matches!(cmd, 'W' | 'V');
                let mut sweep = e - s;
                if clockwise {
                    if sweep <= 0.0 {
                        sweep += std::f64::consts::TAU;
                    }
                } else if sweep >= 0.0 {
                    sweep -= std::f64::consts::TAU;
                }
                let arc = Arc::new(c, r, s, sweep, 0.0);
                let sp = Point::new(c.x + r.x * s.cos(), c.y + r.y * s.sin());
                if matches!(cmd, 'B' | 'V') || !open {
                    bp.move_to(sp);
                    start = sp;
                    open = true;
                } else {
                    bp.line_to(sp);
                }
                append_arc(&mut bp, &arc);
                cur = arc_end(&arc);
            }
            'G' => {
                // OOXML arcTo: radii, start angle and sweep in degrees, clockwise.
                let r = Vec2::new(v[0].abs(), v[1].abs());
                let (st, sw) = (v[2].to_radians(), v[3].to_radians());
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
                let c = Point::new(cur.x - r.x * s.cos(), cur.y - r.y * s.sin());
                if !open {
                    bp.move_to(cur);
                    start = cur;
                    open = true;
                }
                let arc = Arc::new(c, r, s, sweep, 0.0);
                append_arc(&mut bp, &arc);
                cur = arc_end(&arc);
            }
            _ => {}
        }
    }
    flush(&mut bp, &mut fill, &mut stroke, &mut out);
    out
}

fn line_or_move(bp: &mut BezPath, open: &mut bool, start: &mut Point, p: Point) {
    if *open {
        bp.line_to(p);
    } else {
        bp.move_to(p);
        *start = p;
        *open = true;
    }
}

fn append_arc(bp: &mut BezPath, arc: &Arc) {
    arc.to_cubic_beziers(0.1, |p1, p2, p| bp.curve_to(p1, p2, p));
}

fn arc_end(arc: &Arc) -> Point {
    let a = arc.start_angle + arc.sweep_angle;
    Point::new(arc.center.x + arc.radii.x * a.cos(), arc.center.y + arc.radii.y * a.sin())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geom(xml: &str) -> El {
        let wrapped = format!(
            r#"<r xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0">{xml}</r>"#
        );
        crate::dom::parse(wrapped.as_bytes()).unwrap().elements().next().unwrap().clone()
    }

    #[test]
    fn transform_rotates_counter_clockwise_then_translates() {
        let m = transform("rotate (0.523598775598299) translate (19.585cm 2.451cm)").unwrap();
        let (x, y, w, h, rot, _) = placed_box(m, units::length("5cm").unwrap(), units::length("3cm").unwrap());
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
        let e = enhanced(&g, 100.0, 100.0).unwrap();
        assert_eq!(e.paths.len(), 1);
        assert_eq!(e.paths[0].d, "M 10800 0 L 21600 21600 L 0 21600 Z");
        let ta = e.text_area.unwrap();
        assert!((ta[0] - 0.25).abs() < 1e-9 && (ta[2] - 0.75).abs() < 1e-9);
    }

    #[test]
    fn formulas_with_functions_and_self_reference_terminate() {
        let g = geom(
            r#"<draw:enhanced-geometry svg:viewBox="0 0 100 100" draw:enhanced-path="M ?a ?b L ?c 0 N"><draw:equation draw:name="a" draw:formula="if(-1, 5, max(10, 20)) + sqrt(16)*cos(0)"/><draw:equation draw:name="b" draw:formula="?b + 1"/><draw:equation draw:name="c" draw:formula="logwidth/2 + 3/0"/></draw:enhanced-geometry>"#,
        );
        let e = enhanced(&g, 10.0, 10.0).unwrap();
        assert!(e.paths[0].d.starts_with("M 24 "), "{}", e.paths[0].d);
        assert!(e.paths[0].d.contains("L 50 0"));
    }

    #[test]
    fn arcs_and_nofill_segments() {
        let g = geom(
            r#"<draw:enhanced-geometry svg:viewBox="0 0 21600 21600" draw:enhanced-path="U 10800 10800 10800 10800 0 360 Z N F M 0 0 L 1 1 N"/>"#,
        );
        let e = enhanced(&g, 10.0, 10.0).unwrap();
        assert_eq!(e.paths.len(), 2);
        assert!(e.paths[0].d.contains('C'));
        assert_eq!(e.paths[1].fill, FillMode::None);
    }
}
