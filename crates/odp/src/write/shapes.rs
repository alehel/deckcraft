//! Writing shapes, their graphic styles, master pages and presentation styles.

use std::fmt::Write as _;

use deckcraft_color::Rgba;
use deckcraft_geom::preset::FillMode;
use deckcraft_model::resolve::{self, Ctx};
use deckcraft_model::style::{Dash, Effects, Fill, GradientShape, Line, LineCap, LineJoin, PictureFill, PictureMode};
use deckcraft_model::table::Table;
use deckcraft_model::text::{Action, Anchor, AutoFit, BodyProps, LevelStyle, ListStyle};
use deckcraft_model::{Geom, Layout, PhType, Shape, ShapeKind, Xfrm};
use kurbo::{Affine, BezPath, PathEl, Point};

use super::text;
use super::{Auto, PageCtx, Writer, rad};
use crate::dom::esc;
use crate::units::{cm, hex, pct, trim};
use crate::vocab;

const MAX_DEPTH: usize = 64;

/// Does `s` write an element (and so can carry an id)? Shapes with nothing to show, and on a
/// slide the footer placeholders (the master page draws those), write none.
pub fn writes_element(w: &Writer, s: &Shape, on_slide: bool) -> bool {
    if on_slide && s.ph_type().is_some_and(PhType::is_footer_kind) {
        return false;
    }
    let has = |m: &deckcraft_model::MediaId| w.p.media(*m).is_some();
    match &s.kind {
        ShapeKind::Picture { fill } => has(&fill.media),
        ShapeKind::Media(m) => has(&m.media),
        ShapeKind::Opaque { preview, .. } => preview.as_ref().is_some_and(has),
        ShapeKind::Model3d { poster, .. } => poster.as_ref().is_some_and(has),
        _ => true,
    }
}

/// One shape (and its children).
pub fn shape(w: &mut Writer, auto: &mut Auto, ctx: &Ctx, pg: &PageCtx, s: &Shape, depth: usize) -> String {
    if depth > MAX_DEPTH || !writes_element(w, s, !pg.is_master) {
        return String::new();
    }
    let x = resolve::xfrm(ctx, s);
    let mut common = String::new();
    if !s.name.is_empty() {
        let _ = write!(common, r#" draw:name="{}""#, esc(&s.name));
    }
    if s.hidden {
        common.push_str(r#" draw:display="none""#);
    }
    if let Some(id) = pg.id(s.id, None) {
        let _ = write!(common, r#" xml:id="{id}" draw:id="{id}""#);
    }
    let desc = {
        let mut d = String::new();
        if !s.title.is_empty() {
            let _ = write!(d, "<svg:title>{}</svg:title>", esc(&s.title));
        }
        if !s.descr.is_empty() {
            let _ = write!(d, "<svg:desc>{}</svg:desc>", esc(&s.descr));
        }
        d
    };
    let events = s.click.as_ref().map(|h| event_listener(w, &h.action)).unwrap_or_default();
    match &s.kind {
        ShapeKind::Group { children, child } => {
            // An ODF group has no space of its own: children are written at their page positions,
            // through the group's whole transform (rotation and flips included).
            let m = deckcraft_geom::group_child_affine(&x, child.rect());
            // A child filled with the group's fill (`grpFill`) gets it here: ODF groups have none.
            let group_fill = resolve::fill(ctx, s).0;
            let mut inner = String::new();
            for c in children {
                let mut placed = c.clone();
                placed.xfrm = c.xfrm.map(|cx| place_in(m, cx));
                if matches!(placed.fill, Some(Fill::Group)) {
                    placed.fill.clone_from(&group_fill);
                }
                inner.push_str(&shape(w, auto, ctx, pg, &placed, depth + 1));
            }
            format!("<draw:g{common}>{inner}{desc}{events}</draw:g>")
        }
        ShapeKind::Picture { fill } => {
            let Some(href) = w.media_path(fill.media) else { return String::new() };
            let (line, ph_line) = resolve::line(ctx, s);
            let mut g = line_attrs(w, ctx, &line, ph_line);
            g.push_str(r#" draw:fill="none""#);
            g.push_str(&picture_attrs(w, fill));
            if x.flip_h || x.flip_v {
                let m = match (x.flip_h, x.flip_v) {
                    (true, true) => "horizontal vertical",
                    (true, false) => "horizontal",
                    _ => "vertical",
                };
                let _ = write!(g, r#" style:mirror="{m}""#);
            }
            let (effects, ph_fx) = resolve::effects(ctx, s);
            g.push_str(&shadow_attrs(ctx, effects.as_ref(), ph_fx));
            let st = auto.style("graphic", "gr", Some("standard"), &format!("<style:graphic-properties{g}/>"));
            let mime = w.p.media(fill.media).map(|m| m.content_type.clone()).unwrap_or_default();
            let image = format!(r#"<draw:image {} draw:mime-type="{}"><text:p/></draw:image>"#, embed(&href), esc(&mime));
            frame(&st, &common, &x, &image, &desc, &events)
        }
        ShapeKind::Table(t) => {
            let table = table(w, auto, ctx, pg, t);
            frame(&no_paint(auto), &common, &x, &table, &desc, &events)
        }
        ShapeKind::Media(m) => {
            let Some(href) = w.media_path(m.media) else { return String::new() };
            let mime = w.p.media(m.media).map(|i| i.content_type.clone()).unwrap_or_default();
            let poster = m.poster.and_then(|p| w.media_path(p)).map(|p| format!("<draw:image {}/>", embed(&p))).unwrap_or_default();
            let plugin = format!(
                r#"<draw:plugin {} draw:mime-type="{}"><draw:param draw:name="Loop" draw:value="{}"/><draw:param draw:name="Mute" draw:value="false"/><draw:param draw:name="VolumeDB" draw:value="0"/><draw:param draw:name="Zoom" draw:value="fit"/></draw:plugin>{poster}"#,
                embed(&href),
                esc(&mime),
                m.loop_play
            );
            frame(&no_paint(auto), &common, &x, &plugin, &desc, &events)
        }
        ShapeKind::Opaque { preview: Some(p), .. } | ShapeKind::Model3d { poster: Some(p), .. } => {
            let pic = Shape {
                kind: ShapeKind::Picture { fill: PictureFill { media: *p, ..Default::default() } },
                xfrm: Some(x),
                line: Some(Line::none()),
                ..s.clone()
            };
            shape(w, auto, ctx, pg, &pic, depth + 1)
        }
        ShapeKind::Opaque { .. } | ShapeKind::Model3d { .. } => String::new(),
        ShapeKind::Chart(c) => {
            let object = super::chart::object(w, ctx, &x, c);
            frame(&no_paint(auto), &common, &x, &object, &desc, &events)
        }
        ShapeKind::Ink { strokes } => {
            // The strokes in a group that carries the ink's name, id, visibility and actions.
            let mut out = String::new();
            for st in strokes.iter().take(10_000) {
                if st.points.len() < 2 {
                    continue;
                }
                let mut bp = BezPath::new();
                for (i, (px, py, _)) in st.points.iter().enumerate() {
                    if i == 0 {
                        bp.move_to((*px, *py));
                    } else {
                        bp.line_to((*px, *py));
                    }
                }
                let mut l = Line::solid(deckcraft_model::style::ColorRef::rgb(st.color), st.width.max(0.25));
                l.cap = Some(LineCap::Round);
                l.join = Some(LineJoin::Round);
                let mut g = line_attrs(w, ctx, &l, None);
                if st.highlighter {
                    g.push_str(r#" svg:stroke-opacity="50%""#);
                }
                g.push_str(r#" draw:fill="none""#);
                let style = auto.style("graphic", "gr", Some("standard"), &format!("<style:graphic-properties{g}/>"));
                out.push_str(&path_element(&bp, &style, ""));
            }
            format!("<draw:g{common}>{out}{desc}{events}</draw:g>")
        }
        ShapeKind::Connector { .. } | ShapeKind::Shape => geometric(w, auto, ctx, pg, s, &x, &common, &desc, &events),
    }
}

/// An autoshape, text box, line or connector.
#[allow(clippy::too_many_arguments)]
fn geometric(w: &mut Writer, auto: &mut Auto, ctx: &Ctx, pg: &PageCtx, s: &Shape, x: &Xfrm, common: &str, desc: &str, events: &str) -> String {
    let (fill, ph_fill) = match resolve::fill(ctx, s) {
        // Filled with the page's background: that fill, here.
        (Some(Fill::Background), _) => (Some(pg.background.0.clone()), pg.background.1),
        f => f,
    };
    let (line, ph_line) = resolve::line(ctx, s);
    let (effects, ph_fx) = resolve::effects(ctx, s);
    let body = resolve::body(ctx, s);
    let fill = fill.unwrap_or(Fill::None);
    let mut g = fill_attrs(w, ctx, &fill, ph_fill);
    g.push_str(&line_attrs(w, ctx, &line, ph_line));
    g.push_str(&shadow_attrs(ctx, effects.as_ref(), ph_fx));
    g.push_str(&body_attrs(&body));
    // Text columns.
    let columns = match body.columns.filter(|n| *n > 1) {
        Some(n) => format!(r#"<style:columns fo:column-count="{n}" fo:column-gap="{}"/>"#, cm(body.col_spacing.unwrap_or(0.0))),
        None => String::new(),
    };
    let ph = s.ph.as_ref().map(|p| p.kind);
    let class = ph.and_then(class_of);
    let outline = matches!(ph, Some(PhType::Body | PhType::Obj));
    let text = text::body(w, auto, ctx, pg, s, outline && class.is_some());
    // Straight lines and connectors are written by their end points; bent and curved connectors
    // by their route.
    let straight = matches!(s.geom.preset_name(), Some("line" | "straightConnector1"));
    // Where a connector is glued (our sites run top, left, bottom, right; LibreOffice's glue
    // points top, right, bottom, left).
    let glue = |start: &Option<(deckcraft_model::ShapeId, u32)>, end: &Option<(deckcraft_model::ShapeId, u32)>| {
        let mut g = String::new();
        for (k, e) in [("start", start), ("end", end)] {
            if let Some((id, site)) = e
                && let Some(target) = pg.id(*id, None)
            {
                let pt = crate::GLUE_POINTS.iter().position(|s| s == site).unwrap_or(0);
                let _ = write!(g, r#" draw:{k}-shape="{target}" draw:{k}-glue-point="{pt}""#);
            }
        }
        g
    };
    // Placeholders keep their class so LibreOffice treats them as title/outline objects.
    let (style_attr, class_attr) = match class {
        Some(c) => {
            let parent = format!("{}-{}", pg.master, parent_style_of(c));
            let st = auto.style("presentation", "pr", Some(&parent), &format!("<style:graphic-properties{g}>{columns}</style:graphic-properties>"));
            (format!(r#" presentation:style-name="{st}""#), format!(r#" presentation:class="{c}""#))
        }
        None => {
            let st = auto.style("graphic", "gr", Some("standard"), &format!("<style:graphic-properties{g}>{columns}</style:graphic-properties>"));
            (format!(r#" draw:style-name="{st}""#), String::new())
        }
    };
    if straight {
        // Lines and straight connectors: their end points.
        let (mut x1, mut y1, mut x2, mut y2) = (x.x, x.y, x.x + x.w, x.y + x.h);
        if x.flip_h {
            std::mem::swap(&mut x1, &mut x2);
        }
        if x.flip_v {
            std::mem::swap(&mut y1, &mut y2);
        }
        if x.rot != 0.0 {
            let c = Point::new(x.x + x.w / 2.0, x.y + x.h / 2.0);
            let r = Affine::rotate_about(x.rot.to_radians(), c);
            let a = r * Point::new(x1, y1);
            let b = r * Point::new(x2, y2);
            (x1, y1, x2, y2) = (a.x, a.y, b.x, b.y);
        }
        let (el, glue) = match &s.kind {
            ShapeKind::Connector { start, end } => ("draw:connector", format!(r#" draw:type="line"{}"#, glue(start, end))),
            _ => ("draw:line", String::new()),
        };
        return format!(
            r#"<{el}{style_attr}{common}{glue} svg:x1="{}" svg:y1="{}" svg:x2="{}" svg:y2="{}">{text}{desc}{events}</{el}>"#,
            cm(x1),
            cm(y1),
            cm(x2),
            cm(y2)
        );
    }
    if let ShapeKind::Connector { start, end } = &s.kind {
        // A bent or curved connector: its route on the page (1/100 mm, no view box), from its
        // first point to its last.
        let mut route = BezPath::new();
        for sp in shape_geometry(s, x.w.max(1e-3), x.h.max(1e-3)).paths {
            route.extend(sp.path);
        }
        route.apply_affine(Affine::scale(2540.0 / 72.0) * x.affine());
        let ends = route.elements().first().zip(route.elements().last()).and_then(|(a, b)| Some((a.end_point()?, b.end_point()?)));
        if let Some((a, b)) = ends {
            let kind = if s.geom.preset_name().is_some_and(|n| n.starts_with("curved")) { "curve" } else { "standard" };
            let pt = |v: f64| cm(v * 72.0 / 2540.0);
            return format!(
                r#"<draw:connector{style_attr}{common} draw:type="{kind}"{} svg:x1="{}" svg:y1="{}" svg:x2="{}" svg:y2="{}" svg:d="{}">{text}{desc}{events}</draw:connector>"#,
                glue(start, end),
                pt(a.x),
                pt(a.y),
                pt(b.x),
                pt(b.y),
                crate::geometry::path_d(&route)
            );
        }
    }
    let is_frame = (s.text_box || class.is_some()) && matches!(&s.geom, Geom::Preset { name, .. } if name == "rect");
    let pos = position(x);
    if is_frame {
        return format!(r#"<draw:frame{style_attr}{class_attr}{common}{pos}><draw:text-box>{text}</draw:text-box>{desc}{events}</draw:frame>"#);
    }
    let geo = enhanced_geometry(s, x);
    format!(r#"<draw:custom-shape{style_attr}{class_attr}{common}{pos}>{text}{geo}{desc}{events}</draw:custom-shape>"#)
}

fn class_of(k: PhType) -> Option<&'static str> {
    Some(match k {
        PhType::Title | PhType::CtrTitle => "title",
        PhType::SubTitle => "subtitle",
        PhType::Body | PhType::Obj => "outline",
        PhType::Date => "date-time",
        PhType::Footer => "footer",
        PhType::SlideNum => "page-number",
        PhType::Header => "header",
        _ => return None,
    })
}

fn parent_style_of(class: &str) -> &'static str {
    match class {
        "title" => "title",
        "subtitle" => "subtitle",
        "outline" => "outline1",
        _ => "backgroundobjects",
    }
}

/// `svg:x/y/width/height`, or `draw:transform` for a rotated shape.
fn position(x: &Xfrm) -> String {
    let (w, h) = (x.w.max(0.0), x.h.max(0.0));
    if x.rot.rem_euclid(360.0).abs() < 1e-6 {
        return format!(r#" svg:x="{}" svg:y="{}" svg:width="{}" svg:height="{}""#, cm(x.x), cm(x.y), cm(w), cm(h));
    }
    // Rotate about the origin (counter-clockwise positive), then move so the centre stays put.
    let a = -x.rot.to_radians();
    let r = Affine::new([a.cos(), -a.sin(), a.sin(), a.cos(), 0.0, 0.0]);
    let c = Point::new(x.x + w / 2.0, x.y + h / 2.0);
    let rc = r * Point::new(w / 2.0, h / 2.0);
    let t = c - rc.to_vec2();
    format!(r#" svg:width="{}" svg:height="{}" draw:transform="rotate ({}) translate ({} {})""#, cm(w), cm(h), rad(-x.rot), cm(t.x), cm(t.y))
}

/// A child's box in its group's space, placed on the page by the group's transform `m`: its
/// rotation adds the group's, and a mirroring group flips it.
fn place_in(m: Affine, child: Xfrm) -> Xfrm {
    let unflipped = Xfrm { flip_h: false, flip_v: false, ..child };
    let (x, y, w, h, rot, mirrored) = crate::geometry::placed_box(m * unflipped.affine(), child.w, child.h);
    Xfrm { x, y, w, h, rot, flip_h: child.flip_h, flip_v: child.flip_v ^ mirrored }
}

/// `draw:enhanced-geometry` drawing the shape's outline in a view box of its size (1/100 mm).
fn enhanced_geometry(s: &Shape, x: &Xfrm) -> String {
    let (w, h) = (x.w.max(1e-3), x.h.max(1e-3));
    let geo = shape_geometry(s, w, h);
    let vw = (w * 2540.0 / 72.0).round().max(1.0);
    let vh = (h * 2540.0 / 72.0).round().max(1.0);
    let scale = Affine::scale_non_uniform(vw / w, vh / h);
    let mut path = String::new();
    for sp in &geo.paths {
        let mut p = sp.path.clone();
        p.apply_affine(scale);
        let mut first = true;
        for el in p.elements() {
            let f = |p: Point| format!("{} {}", trim(p.x, 0), trim(p.y, 0));
            match el {
                PathEl::MoveTo(a) => {
                    if !first {
                        path.push(' ');
                    }
                    let _ = write!(path, "M {}", f(*a));
                }
                PathEl::LineTo(a) => {
                    let _ = write!(path, " L {}", f(*a));
                }
                PathEl::QuadTo(a, b) => {
                    let _ = write!(path, " Q {} {}", f(*a), f(*b));
                }
                PathEl::CurveTo(a, b, c) => {
                    let _ = write!(path, " C {} {} {}", f(*a), f(*b), f(*c));
                }
                PathEl::ClosePath => path.push_str(" Z"),
            }
            first = false;
        }
        if sp.fill == FillMode::None {
            path.push_str(" F");
        }
        if !sp.stroke {
            path.push_str(" S");
        }
        path.push_str(" N ");
    }
    let tr = geo.text_rect;
    let ta = format!("{} {} {} {}", trim(tr.x0 * vw / w, 0), trim(tr.y0 * vh / h, 0), trim(tr.x1 * vw / w, 0), trim(tr.y1 * vh / h, 0));
    let (kind, modifiers) = match &s.geom {
        Geom::Preset { name, adj } => (format!("ooxml-{name}"), adj.iter().map(|a| trim(*a, 3)).collect::<Vec<_>>().join(" ")),
        Geom::Custom { .. } => ("non-primitive".to_string(), String::new()),
    };
    let mods = if modifiers.is_empty() { String::new() } else { format!(r#" draw:modifiers="{modifiers}""#) };
    format!(
        r#"<draw:enhanced-geometry svg:viewBox="0 0 {} {}" draw:text-areas="{ta}" draw:type="{}"{mods} draw:mirror-horizontal="{}" draw:mirror-vertical="{}" draw:enhanced-path="{}"/>"#,
        trim(vw, 0),
        trim(vh, 0),
        esc(&kind),
        x.flip_h,
        x.flip_v,
        path.trim()
    )
}

/// The shape's geometry (preset or custom) at its size, without the renderer.
fn shape_geometry(s: &Shape, w: f64, h: f64) -> deckcraft_geom::preset::Geometry {
    use deckcraft_geom::preset::{Geometry, SubPath, build};
    match &s.geom {
        Geom::Preset { name, adj } => build(name, w, h, adj).or_else(|| build("rect", w, h, &[])).unwrap_or(Geometry {
            paths: vec![],
            text_rect: kurbo::Rect::new(0.0, 0.0, w, h),
            handles: vec![],
            sites: vec![],
        }),
        Geom::Custom { paths } => {
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

fn path_element(bp: &BezPath, style: &str, inner: &str) -> String {
    let bb = kurbo::Shape::bounding_box(bp);
    if !bb.is_finite() {
        return String::new();
    }
    let mut p = bp.clone();
    // svg:d in 1/100 mm in a view box of the path's size.
    let s = 2540.0 / 72.0;
    p.apply_affine(Affine::scale(s) * Affine::translate((-bb.x0, -bb.y0)));
    format!(
        r#"<draw:path draw:style-name="{style}" svg:x="{}" svg:y="{}" svg:width="{}" svg:height="{}" svg:viewBox="0 0 {} {}" svg:d="{}">{inner}</draw:path>"#,
        cm(bb.x0),
        cm(bb.y0),
        cm(bb.width().max(0.01)),
        cm(bb.height().max(0.01)),
        trim((bb.width() * s).max(1.0), 0),
        trim((bb.height() * s).max(1.0), 0),
        esc(&p.to_svg())
    )
}

// ---------------------------------------------------------------------------------------------
// Graphic properties

fn color_alpha(ctx: &Ctx, c: &deckcraft_model::style::ColorRef, ph: Option<Rgba>) -> (Rgba, f64) {
    (ctx.color(c, ph), c.alpha())
}

/// Fill attributes for `style:graphic-properties` or `style:drawing-page-properties`.
pub fn fill_attrs(w: &mut Writer, ctx: &Ctx, f: &Fill, ph: Option<Rgba>) -> String {
    match f {
        Fill::None | Fill::Group | Fill::Background => r#" draw:fill="none""#.into(),
        Fill::Solid { color } => {
            let (c, a) = color_alpha(ctx, color, ph);
            let mut s = format!(r#" draw:fill="solid" draw:fill-color="{}""#, hex(c));
            if a < 1.0 {
                let _ = write!(s, r#" draw:opacity="{}""#, pct(a));
            }
            s
        }
        Fill::Gradient(g) => {
            let mut stops: Vec<(f64, Rgba, f64)> = g.stops.iter().map(|s| (s.pos, ctx.color(&s.color, ph), s.color.alpha())).collect();
            stops.sort_by(|a, b| a.0.total_cmp(&b.0));
            if stops.is_empty() {
                return r#" draw:fill="none""#.into();
            }
            let (style, angle, cx, cy, reverse) = match &g.shape {
                GradientShape::Linear { angle, .. } => ("linear", (90.0 - angle).rem_euclid(360.0), 0.5, 0.5, false),
                GradientShape::Path { path, focus } => {
                    let cx = ((focus[0] + 1.0 - focus[2]) / 2.0).clamp(0.0, 1.0);
                    let cy = ((focus[1] + 1.0 - focus[3]) / 2.0).clamp(0.0, 1.0);
                    (if path == "circle" { "radial" } else { "rectangular" }, 0.0, cx, cy, true)
                }
            };
            // Radial ODF gradients run from the outside in: reverse DrawingML's centre-first order.
            if reverse {
                stops = stops.into_iter().rev().map(|(p, c, a)| (1.0 - p, c, a)).collect();
            }
            let first = stops.first().copied().unwrap_or((0.0, Rgba::BLACK, 1.0));
            let last = stops.last().copied().unwrap_or(first);
            let mut children = String::new();
            if stops.len() > 2 || first.0 > 0.0 || last.0 < 1.0 {
                for (p, c, _) in &stops {
                    let _ = write!(
                        children,
                        r#"<loext:gradient-stop svg:offset="{}" loext:color-type="rgb" loext:color-value="{}"/>"#,
                        trim(*p, 4),
                        hex(*c)
                    );
                }
            }
            let attrs = format!(
                r#"draw:style="{style}" draw:cx="{}" draw:cy="{}" draw:start-color="{}" draw:end-color="{}" draw:start-intensity="100%" draw:end-intensity="100%" draw:angle="{}deg" draw:border="0%""#,
                pct(cx),
                pct(cy),
                hex(first.1),
                hex(last.1),
                trim(angle, 1)
            );
            let name = w.named.def("draw:gradient", "Gradient", &attrs, &children);
            let mut s = format!(r#" draw:fill="gradient" draw:fill-gradient-name="{name}""#);
            if first.2 < 1.0 || last.2 < 1.0 {
                let oattrs = format!(
                    r#"draw:style="{style}" draw:cx="{}" draw:cy="{}" draw:start="{}" draw:end="{}" draw:angle="{}deg" draw:border="0%""#,
                    pct(cx),
                    pct(cy),
                    pct(first.2),
                    pct(last.2),
                    trim(angle, 1)
                );
                let oname = w.named.def("draw:opacity", "Transparency", &oattrs, "");
                let _ = write!(s, r#" draw:opacity-name="{oname}""#);
            }
            s
        }
        Fill::Picture(pf) => {
            // A hatch read from a file: the hatch again.
            if let Some(h) = w.p.media(pf.media).and_then(|m| crate::hatch::Hatch::from_media_name(&m.name)) {
                let name = w.named.def(
                    "draw:hatch",
                    "Hatch",
                    &format!(
                        r#"draw:style="{}" draw:color="{}" draw:distance="{}" draw:rotation="{}""#,
                        h.style.as_str(),
                        hex(h.color),
                        cm(h.distance),
                        trim(h.rotation * 10.0, 0)
                    ),
                    "",
                );
                let mut s = format!(r#" draw:fill="hatch" draw:fill-hatch-name="{name}" draw:fill-hatch-solid="{}""#, h.background.is_some());
                if let Some(bg) = h.background {
                    let _ = write!(s, r#" draw:fill-color="{}""#, hex(bg));
                }
                return s;
            }
            let Some(href) = w.media_path(pf.media) else { return r#" draw:fill="none""#.into() };
            let name = w.named.def("draw:fill-image", "Bitmap", &embed(&href), "");
            let repeat = match pf.mode {
                PictureMode::Stretch { .. } => "stretch",
                PictureMode::Tile { .. } => "repeat",
            };
            let mut s = format!(r#" draw:fill="bitmap" draw:fill-image-name="{name}" style:repeat="{repeat}""#);
            if let Some(a) = pf.alpha.filter(|a| *a < 1.0) {
                let _ = write!(s, r#" draw:opacity="{}""#, pct(a.clamp(0.0, 1.0)));
            }
            s
        }
        Fill::Pattern(pat) => {
            let fg = ctx.color(&pat.fg, ph);
            let (bg, bga) = color_alpha(ctx, &pat.bg, ph);
            let (style, rot, dist) = hatch_of(&pat.preset);
            let name = w.named.def(
                "draw:hatch",
                "Hatch",
                &format!(r#"draw:style="{style}" draw:color="{}" draw:distance="{}" draw:rotation="{}""#, hex(fg), cm(dist), rot * 10),
                "",
            );
            let solid = bga > 0.0;
            let mut s = format!(r#" draw:fill="hatch" draw:fill-hatch-name="{name}" draw:fill-hatch-solid="{solid}""#);
            if solid {
                let _ = write!(s, r#" draw:fill-color="{}""#, hex(bg));
            }
            s
        }
    }
}

/// A hatch close to a DrawingML pattern: style, rotation (degrees) and line distance (points).
fn hatch_of(preset: &str) -> (&'static str, i32, f64) {
    let dense = preset.starts_with("dk") || preset.starts_with("nar") || preset.starts_with("sm") || preset.starts_with("pct");
    let d = if dense { 2.0 } else { 4.0 };
    let p = preset.to_ascii_lowercase();
    if p.contains("grid") || p.contains("cross") && !p.contains("diag") || p.contains("plaid") {
        ("double", 0, d)
    } else if p.contains("check") || p.contains("diagcross") || p.contains("dmnd") || p.contains("trellis") {
        ("double", 45, d)
    } else if p.contains("updiag") || p.contains("wdupdiag") {
        ("single", 45, d)
    } else if p.contains("dndiag") {
        ("single", 135, d)
    } else if p.contains("horz") {
        ("single", 0, d)
    } else if p.contains("vert") {
        ("single", 90, d)
    } else {
        ("single", 45, d)
    }
}

pub fn line_attrs(w: &mut Writer, ctx: &Ctx, l: &Line, ph: Option<Rgba>) -> String {
    let fill = l.fill.clone().unwrap_or(Fill::None);
    let color = match &fill {
        Fill::Solid { color } => Some(color_alpha(ctx, color, ph)),
        Fill::Gradient(g) => g.stops.first().map(|s| color_alpha(ctx, &s.color, ph)),
        Fill::Pattern(p) => Some(color_alpha(ctx, &p.fg, ph)),
        _ => None,
    };
    let Some((c, a)) = color else { return r#" draw:stroke="none""#.into() };
    let width = l.width.unwrap_or(0.75).max(0.0);
    let mut s = String::new();
    match &l.dash {
        Some(d) if !matches!(d, Dash::Preset { name } if name == "solid") => {
            let pat = d.pattern();
            let mut dots = vec![];
            for ch in pat.chunks(2) {
                if let [dl, gap] = ch {
                    dots.push((*dl, *gap));
                }
            }
            let gap = dots.first().map(|d| d.1).unwrap_or(3.0);
            let (d1, d2) = (dots.first().map(|d| d.0).unwrap_or(1.0), dots.get(1).map(|d| d.0));
            let n1 = dots.iter().take_while(|d| (d.0 - d1).abs() < 1e-9).count().max(1);
            let rest = dots.iter().skip(n1).count();
            let mut attrs = format!(r#"draw:style="rect" draw:dots1="{n1}" draw:dots1-length="{}""#, pct(d1));
            if let Some(d2) = d2.filter(|_| rest > 0) {
                let _ = write!(attrs, r#" draw:dots2="{rest}" draw:dots2-length="{}""#, pct(d2));
            }
            let _ = write!(attrs, r#" draw:distance="{}""#, pct(gap));
            let name = w.named.def("draw:stroke-dash", "Dash", &attrs, "");
            let _ = write!(s, r#" draw:stroke="dash" draw:stroke-dash="{name}""#);
        }
        _ => s.push_str(r#" draw:stroke="solid""#),
    }
    let _ = write!(s, r#" svg:stroke-width="{}" svg:stroke-color="{}""#, cm(width), hex(c));
    if a < 1.0 {
        let _ = write!(s, r#" svg:stroke-opacity="{}""#, pct(a));
    }
    if let Some(j) = l.join {
        let _ = write!(s, r#" draw:stroke-linejoin="{}""#, vocab::write(vocab::LINE_JOINS, &j).unwrap_or("round"));
    }
    if let Some(cap) = l.cap {
        let _ = write!(s, r#" svg:stroke-linecap="{}""#, vocab::write(vocab::LINE_CAPS, &cap).unwrap_or("butt"));
    }
    for (end, key) in [(&l.head, "start"), (&l.tail, "end")] {
        let Some(e) = end else { continue };
        if e.kind == "none" {
            continue;
        }
        let (d, vb, name) = match e.kind.as_str() {
            "oval" => ("M 10 0 C 15.5 0 20 4.5 20 10 C 20 15.5 15.5 20 10 20 C 4.5 20 0 15.5 0 10 C 0 4.5 4.5 0 10 0 Z", "0 0 20 20", "Circle"),
            "diamond" => ("M 10 0 L 20 10 L 10 20 L 0 10 Z", "0 0 20 20", "Square_20_45"),
            "arrow" => ("M 10 0 L 20 26 L 16 28 L 10 12 L 4 28 L 0 26 Z", "0 0 20 28", "Line_20_Arrow"),
            "stealth" => ("M 10 0 L 20 30 L 10 22 L 0 30 Z", "0 0 20 30", "Concave_20_Arrow"),
            _ => ("M 10 0 L 20 30 L 0 30 Z", "0 0 20 30", "Arrow"),
        };
        let mname = w.named.def("draw:marker", name, &format!(r#"svg:viewBox="{vb}" svg:d="{d}""#), "");
        let mul = match e.w.as_str() {
            "sm" => 2.0,
            "lg" => 5.0,
            _ => 3.0,
        };
        let _ =
            write!(s, r#" draw:marker-{key}="{mname}" draw:marker-{key}-width="{}" draw:marker-{key}-center="false""#, cm((width.max(0.75)) * mul));
    }
    s
}

/// A `draw:frame` around `inner` (a picture, table, media, chart): its style, the shape's common
/// attributes, its position (rotation included), alt text and click action.
fn frame(style: &str, common: &str, x: &Xfrm, inner: &str, desc: &str, events: &str) -> String {
    format!(r#"<draw:frame draw:style-name="{style}"{common}{}>{inner}{desc}{events}</draw:frame>"#, position(x))
}

/// The graphic style of a frame that paints nothing itself.
pub(crate) fn no_paint(auto: &mut Auto) -> String {
    auto.style("graphic", "gr", Some("standard"), r#"<style:graphic-properties draw:fill="none" draw:stroke="none"/>"#)
}

/// The attributes of an embedded package part (`xlink:href` …).
pub(crate) fn embed(href: &str) -> String {
    format!(r#"xlink:href="{}" xlink:type="simple" xlink:show="embed" xlink:actuate="onLoad""#, esc(href))
}

fn shadow_attrs(ctx: &Ctx, e: Option<&Effects>, ph: Option<Rgba>) -> String {
    let Some(sh) = e.and_then(|e| e.outer_shadow.as_ref()) else { return r#" draw:shadow="hidden""#.into() };
    let (c, a) = color_alpha(ctx, &sh.color, ph);
    let dx = sh.dist * sh.dir.to_radians().cos();
    let dy = sh.dist * sh.dir.to_radians().sin();
    let mut s =
        format!(r#" draw:shadow="visible" draw:shadow-offset-x="{}" draw:shadow-offset-y="{}" draw:shadow-color="{}""#, cm(dx), cm(dy), hex(c));
    if a < 1.0 {
        let _ = write!(s, r#" draw:shadow-opacity="{}""#, pct(a));
    }
    if sh.blur > 0.0 {
        let _ = write!(s, r#" loext:shadow-blur="{}""#, cm(sh.blur));
    }
    s
}

/// A text body's insets as `fo:padding-*` (the model's defaults where unset).
fn padding_attrs(b: &BodyProps) -> String {
    format!(
        r#" fo:padding-left="{}" fo:padding-top="{}" fo:padding-right="{}" fo:padding-bottom="{}""#,
        cm(b.inset_l.unwrap_or(7.2)),
        cm(b.inset_t.unwrap_or(3.6)),
        cm(b.inset_r.unwrap_or(7.2)),
        cm(b.inset_b.unwrap_or(3.6))
    )
}

/// A text body's vertical anchor as `draw:textarea-vertical-align`.
fn anchor_attr(b: &BodyProps) -> String {
    format!(r#" draw:textarea-vertical-align="{}""#, vocab::write(vocab::ANCHORS, &b.anchor.unwrap_or(Anchor::Top)).unwrap_or("top"))
}

fn body_attrs(b: &BodyProps) -> String {
    let mut s = padding_attrs(b);
    s.push_str(&anchor_attr(b));
    if b.anchor_ctr == Some(true) {
        s.push_str(r#" draw:textarea-horizontal-align="center""#);
    } else {
        s.push_str(r#" draw:textarea-horizontal-align="justify""#);
    }
    let _ = write!(s, r#" fo:wrap-option="{}""#, if b.wrap == Some(false) { "no-wrap" } else { "wrap" });
    match b.autofit {
        Some(AutoFit::Shape) => s.push_str(r#" draw:auto-grow-height="true""#),
        Some(AutoFit::Shrink { .. }) => s.push_str(r#" draw:auto-grow-height="false" draw:fit-to-size="false" style:shrink-to-fit="true""#),
        _ => s.push_str(r#" draw:auto-grow-height="false""#),
    }
    if b.wrap == Some(false) && matches!(b.autofit, Some(AutoFit::Shape)) {
        s.push_str(r#" draw:auto-grow-width="true""#);
    }
    if let Some(m) = b.vert.as_ref().and_then(|v| vocab::write(vocab::WRITING_MODES, v)) {
        let _ = write!(s, r#" style:writing-mode="{m}""#);
    }
    s
}

fn picture_attrs(w: &Writer, pf: &PictureFill) -> String {
    let mut s = String::new();
    if pf.crop.iter().any(|c| *c != 0.0)
        && let Some((iw, ih)) = w.p.media(pf.media).and_then(|m| deckcraft_render::image_natural_size(&m.data))
    {
        let (iw, ih) = (iw * 0.75, ih * 0.75);
        let [l, t, r, b] = pf.crop;
        let _ = write!(s, r#" fo:clip="rect({}, {}, {}, {})""#, cm(t * ih), cm(r * iw), cm(b * ih), cm(l * iw));
    }
    if let Some(a) = pf.alpha.filter(|a| *a < 1.0) {
        let _ = write!(s, r#" draw:image-opacity="{}""#, pct(a.clamp(0.0, 1.0)));
    }
    let adj = &pf.adjust;
    if adj.brightness != 0.0 {
        let _ = write!(s, r#" draw:luminance="{}""#, pct(adj.brightness.clamp(-1.0, 1.0)));
    }
    if adj.contrast != 0.0 {
        let _ = write!(s, r#" draw:contrast="{}""#, pct(adj.contrast.clamp(-1.0, 1.0)));
    }
    if adj.grayscale {
        s.push_str(r#" draw:color-mode="greyscale""#);
    } else if adj.bilevel.is_some() {
        s.push_str(r#" draw:color-mode="mono""#);
    }
    s
}

fn event_listener(w: &Writer, a: &Action) -> String {
    let (action, href) = match a {
        Action::NextSlide => ("next-page", None),
        Action::PreviousSlide => ("previous-page", None),
        Action::FirstSlide => ("first-page", None),
        Action::LastSlide => ("last-page", None),
        Action::EndShow => ("stop", None),
        Action::Slide { slide } => ("show", text::slide_href(w, *slide)),
        Action::Url { url } if deckcraft_model::text::safe_url(url) => ("show", Some(url.clone())),
        _ => return String::new(),
    };
    let href = href.map(|h| format!(r#" xlink:href="{}" xlink:type="simple""#, esc(&h))).unwrap_or_default();
    if action == "show" && href.is_empty() {
        return String::new();
    }
    format!(
        r#"<office:event-listeners><presentation:event-listener script:event-name="dom:click" presentation:action="{action}"{href}/></office:event-listeners>"#
    )
}

// ---------------------------------------------------------------------------------------------
// Tables

fn table(w: &mut Writer, auto: &mut Auto, ctx: &Ctx, pg: &PageCtx, t: &Table) -> String {
    let mut out = String::from("<table:table>");
    for cw in &t.cols {
        let st = auto.style("table-column", "co", None, &format!(r#"<style:table-column-properties style:column-width="{}"/>"#, cm(*cw)));
        let _ = write!(out, r#"<table:table-column table:style-name="{st}"/>"#);
    }
    for (ri, row) in t.rows.iter().enumerate() {
        let rs = auto.style("table-row", "ro", None, &format!(r#"<style:table-row-properties style:row-height="{}"/>"#, cm(row.height)));
        let _ = write!(out, r#"<table:table-row table:style-name="{rs}">"#);
        for (ci, cell) in row.cells.iter().enumerate() {
            if cell.is_covered() {
                out.push_str("<table:covered-table-cell/>");
                continue;
            }
            // The table style's look of this cell, under the cell's own formatting.
            let look = deckcraft_render::table_cell_look(ctx, t, ri, ci);
            let fill = cell
                .fill
                .clone()
                .map(|f| (f, None))
                .or_else(|| look.paint.clone())
                .or_else(|| look.fill.map(|c| (Fill::solid(deckcraft_model::style::ColorRef::rgb(c)), None)));
            let mut g = match &fill {
                Some((f, ph)) => fill_attrs(w, ctx, f, *ph),
                None => r#" draw:fill="none""#.to_string(),
            };
            let mut cp = String::new();
            // The model's and the table look's sides both run left, right, top, bottom.
            for (i, side) in ["left", "right", "top", "bottom"].iter().enumerate() {
                let own = cell.borders.get(i).cloned().flatten();
                let b = match own {
                    Some(l) => border(ctx, Some(&l)),
                    None => match look.sides.get(i).copied().flatten().or(look.border) {
                        Some((c, wd)) if c.a > 0 && wd > 0.0 => format!("{} solid {}", cm(wd), hex(c)),
                        _ => "none".into(),
                    },
                };
                let _ = write!(cp, r#" fo:border-{side}="{b}""#);
            }
            let body = deckcraft_render::table_cell_body(ctx, t, ri, ci, cell);
            let b = &body.body;
            cp.push_str(&padding_attrs(b));
            g.push_str(&anchor_attr(b));
            let cs = auto.style("table-cell", "ce", None, &format!("<style:graphic-properties{g}/><style:table-cell-properties{cp}/>"));
            let shape = deckcraft_model::Shape::table_cell(body.clone());
            let text = text::body(w, auto, ctx, pg, &shape, false);
            let span = if cell.grid_span > 1 { format!(r#" table:number-columns-spanned="{}""#, cell.grid_span) } else { String::new() };
            let rspan = if cell.row_span > 1 { format!(r#" table:number-rows-spanned="{}""#, cell.row_span) } else { String::new() };
            let _ = write!(out, r#"<table:table-cell table:style-name="{cs}"{span}{rspan}>{text}</table:table-cell>"#);
        }
        out.push_str("</table:table-row>");
    }
    out.push_str("</table:table>");
    out
}

fn border(ctx: &Ctx, l: Option<&Line>) -> String {
    let Some(l) = l else { return "none".into() };
    let Some(Fill::Solid { color }) = &l.fill else { return "none".into() };
    let style = match &l.dash {
        Some(Dash::Preset { name }) if name.contains("ot") => "dotted",
        Some(Dash::Preset { name }) if name != "solid" => "dashed",
        _ => "solid",
    };
    format!("{} {style} {}", cm(l.width.unwrap_or(0.75)), hex(ctx.color(color, None)))
}

// ---------------------------------------------------------------------------------------------
// Master pages and presentation styles

/// The presentation styles of one master page: title, outline levels, subtitle, notes,
/// background and background objects.
pub fn presentation_styles(w: &mut Writer, ctx: &Ctx, name: &str) -> String {
    let mut out = String::new();
    let m = ctx.master;
    let title_ph = m.shapes.iter().find(|s| s.ph_type().is_some_and(PhType::is_title));
    let body_ph = m.shapes.iter().find(|s| s.ph_type() == Some(PhType::Body));
    let level = |ls: &ListStyle, l: u8| ls.level(l).cloned().unwrap_or_default();
    // A presentation style: graphic properties (with a list style), paragraph and text properties.
    let style = |w: &mut Writer, sname: &str, parent: Option<&str>, g: &str, lv: &LevelStyle, props: &str, list: &str| {
        let tp = text::text_attrs(w, Some(ctx), &lv.run);
        let parent = parent.map(|p| format!(r#" style:parent-style-name="{p}""#)).unwrap_or_default();
        format!(
            r#"<style:style style:name="{sname}" style:family="presentation"{parent}><style:graphic-properties{g}>{list}</style:graphic-properties><style:paragraph-properties{props}/><style:text-properties{tp}/></style:style>"#
        )
    };
    let plain = |w: &mut Writer, sname: &str, g: &str, lv: &LevelStyle| {
        let (props, _) = text::level_style(w, ctx, 1, lv, false);
        style(w, sname, None, g, lv, &props, "")
    };
    let body_of = |s: Option<&Shape>| s.map(|s| body_attrs(&resolve::body(ctx, s))).unwrap_or_default();
    let tg = format!(r#" draw:stroke="none" draw:fill="none"{}"#, body_of(title_ph));
    out.push_str(&plain(w, &format!("{name}-title"), &tg, &level(&m.title_style, 0)));
    // The outline levels, each a style deriving from the one above; the first holds the list
    // style with every level's bullet and indents.
    let outline: Vec<(LevelStyle, String, Option<String>)> = (0..9u8)
        .map(|l| {
            let lv = level(&m.body_style, l);
            let (props, list) = text::level_style(w, ctx, usize::from(l) + 1, &lv, true);
            (lv, props, list)
        })
        .collect();
    let levels: String = outline.iter().filter_map(|o| o.2.as_deref()).collect();
    let list = format!(r#"<text:list-style style:name="{name}-outline1">{levels}</text:list-style>"#);
    let og = format!(r#" draw:stroke="none" draw:fill="none"{}"#, body_of(body_ph));
    for (l, (lv, props, _)) in outline.iter().enumerate() {
        let (sname, parent) = (format!("{name}-outline{}", l + 1), format!("{name}-outline{l}"));
        let xml = if l == 0 { style(w, &sname, None, &og, lv, props, &list) } else { style(w, &sname, Some(&parent), "", lv, props, "") };
        out.push_str(&xml);
    }
    let subtitle = r#" draw:stroke="none" draw:fill="none" draw:textarea-vertical-align="middle""#;
    out.push_str(&plain(w, &format!("{name}-subtitle"), subtitle, &level(&m.body_style, 0)));
    out.push_str(&plain(w, &format!("{name}-notes"), r#" draw:stroke="none" draw:fill="none""#, &level(&m.other_style, 0)));
    // The master background, also in its background style (LibreOffice reads it from there too).
    let (bg, ph) = resolve::background(ctx, None);
    let bgx = fill_attrs(w, ctx, &bg, ph);
    let _ = write!(
        out,
        r#"<style:style style:name="{name}-background" style:family="presentation"><style:graphic-properties draw:stroke="none"{bgx}/></style:style>"#
    );
    let _ = write!(
        out,
        r#"<style:style style:name="{name}-backgroundobjects" style:family="presentation"><style:graphic-properties draw:shadow="hidden"/></style:style>"#
    );
    out
}

/// A master page: background, the master's and layout's graphics, and the placeholders.
pub fn master_page(w: &mut Writer, auto: &mut Auto, ctx: &Ctx, name: &str, layout: Option<&Layout>) -> String {
    let mut pg = PageCtx::new(name, format!("{name}-"), true, resolve::background(ctx, None));
    // Connectors on the master and the layout glue to shapes there.
    pg.refer(w, &ctx.master.shapes, &[]);
    if let Some(l) = layout {
        pg.refer(w, &l.shapes, &[]);
    }
    let (bg, ph) = resolve::background(ctx, None);
    let bgx = fill_attrs(w, ctx, &bg, ph);
    let dp = auto.style("drawing-page", "dp", None, &format!(r#"<style:drawing-page-properties{bgx} draw:background-size="border"/>"#));
    let mut shapes_xml = String::new();
    let show_master = layout.is_none_or(|l| l.show_master_shapes);
    let master_ctx = Ctx::for_master(ctx.pres, ctx.master);
    if show_master {
        for s in ctx.master.shapes.iter().filter(|s| s.ph.is_none()) {
            shapes_xml.push_str(&shape(w, auto, &master_ctx, &pg, s, 0));
        }
    }
    if let Some(l) = layout {
        for s in l.shapes.iter().filter(|s| s.ph.is_none()) {
            shapes_xml.push_str(&shape(w, auto, ctx, &pg, s, 0));
        }
    }
    // Placeholders: the layout's where it has them, else the master's.
    let phs: Vec<&Shape> = match layout {
        Some(l) => l.shapes.iter().filter(|s| s.ph.is_some()).collect(),
        None => ctx.master.shapes.iter().filter(|s| s.ph.is_some()).collect(),
    };
    let mut seen = std::collections::HashSet::new();
    for s in phs {
        let Some(kind) = s.ph_type() else { continue };
        let Some(class) = class_of(kind) else { continue };
        if !seen.insert(class) {
            continue;
        }
        let x = resolve::xfrm(ctx, s);
        let pos = position(&x);
        let st = format!("{name}-{}", parent_style_of(class));
        if kind.is_footer_kind() {
            // Footer fields: their text and formatting as the master draws them.
            let field = match kind {
                PhType::Date => "<presentation:date-time/>",
                PhType::Footer => "<presentation:footer/>",
                PhType::Header => "<presentation:header/>",
                _ => "<text:page-number>&lt;number&gt;</text:page-number>",
            };
            let para = s.text.as_ref().and_then(|t| t.paragraphs.first()).cloned().unwrap_or_default();
            let pp = resolve::para(ctx, s, &para);
            let rp = resolve::run(ctx, s, &para, para.runs.first().map(|r| &r.props).unwrap_or(&para.end_props));
            let tp = text::text_attrs(w, Some(ctx), &rp);
            let align = crate::text::odf_align(pp.align.unwrap_or(deckcraft_model::text::Align::Left));
            let ps =
                auto.style("paragraph", "P", None, &format!(r#"<style:paragraph-properties fo:text-align="{align}"/><style:text-properties{tp}/>"#));
            let body = body_attrs(&resolve::body(ctx, s));
            let gs = auto.style(
                "presentation",
                "pr",
                Some(&format!("{name}-backgroundobjects")),
                &format!(r#"<style:graphic-properties draw:stroke="none" draw:fill="none"{body}/>"#),
            );
            let _ = write!(
                shapes_xml,
                r#"<draw:frame presentation:style-name="{gs}"{pos} presentation:class="{class}"><draw:text-box><text:p text:style-name="{ps}">{field}</text:p></draw:text-box></draw:frame>"#
            );
        } else {
            let _ = write!(
                shapes_xml,
                r#"<draw:frame presentation:style-name="{st}"{pos} presentation:class="{class}" presentation:placeholder="true"><draw:text-box/></draw:frame>"#
            );
        }
    }
    format!(
        r#"<style:master-page style:name="{name}" style:page-layout-name="PM1" draw:style-name="{dp}">{shapes_xml}<presentation:notes style:page-layout-name="PM2"/></style:master-page>"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A child of a group turned a quarter about its centre turns with it: the group's left half
    /// becomes its top half.
    #[test]
    fn group_rotation_carries_children() {
        let c = kurbo::Vec2::new(100.0, 50.0);
        let m = Affine::translate(c) * Affine::rotate(std::f64::consts::FRAC_PI_2) * Affine::translate(-c);
        let x = place_in(m, Xfrm::new(0.0, 0.0, 100.0, 100.0));
        for (got, want) in [(x.x, 50.0), (x.y, -50.0), (x.w, 100.0), (x.h, 100.0), (x.rot, 90.0)] {
            assert!((got - want).abs() < 1e-6, "{x:?}");
        }
        // A mirroring group flips its child instead.
        let flipped = place_in(Affine::FLIP_X, Xfrm::new(10.0, 0.0, 20.0, 10.0));
        assert!(flipped.flip_v != flipped.flip_h || (flipped.rot - 180.0).abs() < 1e-6, "{flipped:?}");
    }
}
