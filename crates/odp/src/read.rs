//! Reading a presentation: meta data, master pages, pages, shapes, notes.

use std::collections::HashMap;
use std::sync::Arc;

use deckcraft_color::{ColorTransform, Rgba, SchemeSlot};
use deckcraft_geom::preset::FillMode;
use deckcraft_model::style::{
    ColorRef, Dash, Effects, Fill, Gradient, GradientShape, GradientStop, Line, LineCap, LineEnd, LineJoin, PatternFill, PictureAdjust, PictureFill,
    PictureMode, Shadow,
};
use deckcraft_model::table::{Cell, Row, Table};
use deckcraft_model::text::{Action, Align, Anchor, AutoFit, BodyProps, LevelStyle, ListStyle, Paragraph, TextBody, TextDir};
use deckcraft_model::{
    Background, CustomPath, Geom, LayoutType, MediaId, PhType, Placeholder, Presentation, Shape, ShapeId, ShapeKind, Size, Slide, SlideId, Xfrm,
    defaults,
};

use crate::OdpError;
use crate::dom::{El, attr, kid, kids};
use crate::geometry;
use crate::package::Package;
use crate::styles::{Props, Scope, Store, Style};
use crate::text::{self, TextCtx};
use crate::units;

/// Deepest element nesting followed (groups in groups …).
pub const MAX_DEPTH: usize = 128;
/// Most shapes read from one file.
const MAX_SHAPES: usize = 250_000;
/// Most rows, and most columns, read from one table.
const MAX_TABLE_SIDE: usize = 1000;
/// Most cells read into one table.
const MAX_TABLE_CELLS: usize = 65_536;
/// Most text (bytes) a table's repeated cells and rows may copy.
const MAX_TABLE_TEXT: usize = 4 << 20;

/// A `*-repeated` count (at least 1).
fn repeat(e: &El, k: &str) -> usize {
    attr(e, k).and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).max(1)
}

/// Most pages read.
const MAX_PAGES: usize = 10_000;

pub fn import(bytes: &[u8]) -> Result<Presentation, OdpError> {
    let pkg = Package::open(bytes)?;
    // A flat document is one tree holding content, styles and meta: parsed once and borrowed for
    // all three rather than copied.
    let (flat_root, content_doc, styles_doc, meta_doc);
    let (content, styles, meta): (&El, &El, Option<&El>) = match &pkg.flat {
        Some(flat) => {
            flat_root = crate::dom::parse(flat)?;
            (&flat_root, &flat_root, Some(&flat_root))
        }
        None => {
            content_doc = pkg.xml("content.xml").ok_or_else(|| OdpError::NotOdp("no content.xml".into()))?;
            styles_doc = pkg.xml("styles.xml").unwrap_or_default();
            meta_doc = pkg.xml("meta.xml");
            (&content_doc, &styles_doc, meta_doc.as_ref())
        }
    };
    let flat = pkg.flat.is_some();
    let mut store = Store::default();
    if !flat {
        store.add_document(styles, Scope::Styles);
    }
    store.add_document(content, Scope::Content);
    let body = kid(content, "office:body")
        .and_then(|b| kid(b, "office:presentation"))
        .ok_or_else(|| OdpError::NotOdp("no office:presentation body".into()))?;
    let master_scope = if flat { Scope::Content } else { Scope::Styles };
    let master_styles = kid(styles, "office:master-styles");
    let masters: Vec<&El> = master_styles.map(|m| kids(m, "style:master-page").collect()).unwrap_or_default();

    // Slide size: the first master's page layout.
    let size = masters
        .first()
        .and_then(|m| attr(m, "style:page-layout-name"))
        .and_then(|n| store.page_layouts.get(n))
        .and_then(|p| Some(Size::new(units::length(p.get("fo:page-width")?)?, units::length(p.get("fo:page-height")?)?)))
        .filter(|s| s.width >= 1.0 && s.height >= 1.0 && s.width < 1.0e5 && s.height < 1.0e5)
        .unwrap_or(Size::new(28.0 * 72.0 / 2.54, 15.75 * 72.0 / 2.54));

    let mut p = defaults::blank_presentation(size, Default::default(), false);
    // Picture bullets and embedded fonts: loaded into the presentation before the styles are
    // shared with the text reader.
    let hrefs: Vec<String> = store
        .all_lists()
        .iter()
        .flat_map(|l| kids(l, "text:list-level-style-image"))
        .filter_map(|i| attr(i, "xlink:href"))
        .map(str::to_string)
        .collect();
    for h in hrefs.into_iter().take(256) {
        if let Some(data) = pkg.get(&h) {
            let name = h.rsplit('/').next().unwrap_or(&h).to_string();
            if let Some(id) = p.add_media(&name, mime_for(&name), data.to_vec()) {
                store.image_bullets.insert(h, id);
            }
        }
    }
    embedded_fonts(&pkg, &store, &mut p);
    let mut r = Reader {
        pkg: &pkg,
        store: std::rc::Rc::new(store),
        p,
        shapes: 0,
        master_index: HashMap::new(),
        page_names: HashMap::new(),
        package_media: HashMap::new(),
        targets: HashMap::new(),
        glue: vec![],
    };
    // The blank deck's master stands in only when the file has none.
    let fallback = std::mem::take(&mut r.p.masters);
    if let Some(m) = meta.as_ref().and_then(|m| kid(m, "office:meta")) {
        r.meta(m);
    }
    for m in masters.iter().take(256) {
        r.master(m, master_scope);
    }
    if r.p.masters.is_empty() {
        r.p.masters = fallback;
    }
    let decls = Decls::read(body);
    for page in kids(body, "draw:page").take(MAX_PAGES) {
        if let Some(s) = r.page(page, &decls) {
            r.p.slides.push(Arc::new(s));
        }
    }
    if let Some(settings) = kid(body, "presentation:settings") {
        r.settings(settings);
    }
    r.resolve_links();
    r.p.fix_next_id();
    Ok(r.p)
}

/// Footer, header and date-time texts a page can use (`presentation:footer-decl` …).
#[derive(Default)]
struct Decls {
    footers: HashMap<String, String>,
    headers: HashMap<String, String>,
    dates: HashMap<String, (bool, String)>,
}

impl Decls {
    fn read(body: &El) -> Self {
        let mut d = Decls::default();
        for e in body.elements() {
            let name = attr(e, "presentation:name").unwrap_or_default().to_string();
            match e.name.as_str() {
                "presentation:footer-decl" => {
                    d.footers.insert(name, e.text());
                }
                "presentation:header-decl" => {
                    d.headers.insert(name, e.text());
                }
                "presentation:date-time-decl" => {
                    d.dates.insert(name, (attr(e, "presentation:source") == Some("current-date"), e.text()));
                }
                _ => {}
            }
        }
        d
    }
}

struct Reader<'a> {
    pkg: &'a Package,
    store: std::rc::Rc<Store>,
    p: Presentation,
    shapes: usize,
    /// Master page name → index in `p.masters`, with its footer placeholders.
    master_index: HashMap<String, usize>,
    /// Page names (`draw:name`) → slide ids, for `#name` links.
    page_names: HashMap<String, SlideId>,
    /// Package parts already added as media, by reference: each part is copied once.
    package_media: HashMap<String, MediaId>,
    /// Element ids on the page being read (shapes and paragraphs), for its animations.
    targets: crate::anim::Targets,
    /// Connectors on the page being read and the (element id, glue point) of each end.
    glue: Vec<(ShapeId, Option<(String, u32)>, Option<(String, u32)>)>,
}

/// Where a shape is being read: which file's automatic styles apply, and the master it sits on.
#[derive(Clone)]
struct Ctx {
    scope: Scope,
    master: String,
    /// The background colour behind shapes, for automatic text colour.
    bg: Rgba,
    /// The page has a subtitle: its title is a title slide's centred title.
    title_slide: bool,
}

impl Reader<'_> {
    fn id(&mut self) -> ShapeId {
        ShapeId(self.p.alloc_id().unwrap_or(u32::MAX))
    }

    fn meta(&mut self, m: &El) {
        let t = |n: &str| kid(m, n).map(|e| e.text()).unwrap_or_default();
        let props = &mut self.p.props;
        props.title = t("dc:title");
        props.subject = t("dc:subject");
        props.comments = t("dc:description");
        props.author = {
            let c = t("meta:initial-creator");
            if c.is_empty() { t("dc:creator") } else { c }
        };
        props.last_modified_by = t("dc:creator");
        props.created = t("meta:creation-date");
        props.modified = t("dc:date");
        props.keywords = kids(m, "meta:keyword").map(|k| k.text()).collect::<Vec<_>>().join(", ");
        props.revision = t("meta:editing-cycles").trim().parse().unwrap_or(0);
    }

    fn settings(&mut self, s: &El) {
        if attr(s, "presentation:endless") == Some("true") {
            self.p.show.loop_until_esc = true;
        }
        if attr(s, "presentation:show-type") == Some("window") || attr(s, "presentation:full-screen") == Some("false") {
            self.p.show.show_type = "browse".into();
        }
        if let Some(name) = attr(s, "presentation:show") {
            self.p.show.custom_show = Some(name.to_string());
        }
        // Custom shows name their pages.
        for cs in kids(s, "presentation:show").take(256) {
            let name = attr(cs, "presentation:name").unwrap_or_default().to_string();
            let pages = attr(cs, "presentation:pages").unwrap_or("");
            let slides = pages.split(',').take(self.page_names.len().max(1) * 4).filter_map(|n| self.page_names.get(n.trim()).copied()).collect();
            self.p.custom_shows.push(deckcraft_model::CustomShow { name, slides });
        }
    }

    // -----------------------------------------------------------------------------------------
    // Masters

    fn master(&mut self, m: &El, scope: Scope) {
        let name = attr(m, "style:name").unwrap_or("Default").to_string();
        let mut theme = deckcraft_model::Theme::default();
        if let Some(t) = kid(m, "loext:theme") {
            theme.name = attr(t, "loext:name").unwrap_or("Office").to_string();
            if let Some(cs) = kid(t, "loext:theme-colors") {
                for c in kids(cs, "loext:color") {
                    let slot = match attr(c, "loext:name").unwrap_or("") {
                        "dark1" => SchemeSlot::Dk1,
                        "light1" => SchemeSlot::Lt1,
                        "dark2" => SchemeSlot::Dk2,
                        "light2" => SchemeSlot::Lt2,
                        "accent1" => SchemeSlot::Accent1,
                        "accent2" => SchemeSlot::Accent2,
                        "accent3" => SchemeSlot::Accent3,
                        "accent4" => SchemeSlot::Accent4,
                        "accent5" => SchemeSlot::Accent5,
                        "accent6" => SchemeSlot::Accent6,
                        "hyperlink" => SchemeSlot::Hlink,
                        "followed-hyperlink" => SchemeSlot::FolHlink,
                        _ => continue,
                    };
                    if let Some(v) = attr(c, "loext:color").and_then(units::color) {
                        theme.colors.set(slot, v);
                    }
                }
            }
        }
        let title_style = self.store.resolve(scope, "presentation", Some(&format!("{name}-title")));
        let outline_style = self.store.resolve(scope, "presentation", Some(&format!("{name}-outline1")));
        let title_rp = text::run_props(&self.store, &title_style.text, Rgba::BLACK);
        let body_rp = text::run_props(&self.store, &outline_style.text, Rgba::BLACK);
        if let Some(f) = &title_rp.font {
            theme.fonts.major.latin.clone_from(f);
        }
        if let Some(f) = &body_rp.font {
            theme.fonts.minor.latin.clone_from(f);
        }
        let first = self.p.next_id.max(1);
        let (mut master, next) = defaults::build_master(self.p.slide_size, theme, first);
        self.p.next_id = next.max(self.p.next_id);
        master.name = attr(m, "style:display-name").unwrap_or(&name).to_string();
        let page_style = attr(m, "draw:style-name").map(|n| self.store.resolve(scope, "drawing-page", Some(n))).unwrap_or_default();
        // The page style's fill, else the master's background presentation style (older files).
        // Its colour may come from the defaults below it, but only a fill it sets itself counts.
        let bg_name = format!("{name}-background");
        let sets_fill = self.store.chain(scope, "presentation", &bg_name).graphic.contains_key("draw:fill");
        let bg_style = if sets_fill { self.store.resolve(scope, "presentation", Some(&bg_name)) } else { Default::default() };
        master.background = Some(
            self.background(&page_style.page)
                .or_else(|| self.background(&bg_style.graphic))
                .unwrap_or(Background::Fill { fill: Fill::solid(ColorRef::rgb(Rgba::WHITE)) }),
        );
        // Text styles: the title level, and the nine outline levels.
        master.title_style = ListStyle::default();
        master.title_style.set(0, self.level_style(&title_style, scope, 0, false));
        master.body_style = ListStyle::default();
        for lv in 0..9u8 {
            let st = self.store.resolve(scope, "presentation", Some(&format!("{name}-outline{}", lv + 1)));
            let mut s = st.clone();
            if s.list.is_none() {
                s.list.clone_from(&outline_style.list);
            }
            master.body_style.set(lv, self.level_style(&s, scope, lv, true));
        }
        let bg = master.background.as_ref().and_then(background_rgb).unwrap_or(Rgba::WHITE);
        let ctx = Ctx { scope, master: name.clone(), bg, title_slide: false };
        let mut decorations = vec![];
        for e in m.elements() {
            let class = attr(e, "presentation:class");
            let kind = class.and_then(ph_kind);
            if let Some(kind) = kind {
                // A master placeholder: move the default one to where this master has it.
                if let Some(x) = self.xfrm(e)
                    && let Some(target) = master
                        .shapes
                        .iter_mut()
                        .find(|s| s.ph_type() == Some(kind) || (kind == PhType::SubTitle && s.ph_type() == Some(PhType::Body)))
                {
                    target.xfrm = Some(x);
                    if kind.is_footer_kind()
                        && let Some(s) = self.shape(e, &ctx, 0)
                    {
                        target.text = s.text;
                        target.fill = s.fill;
                        target.line = s.line;
                    }
                }
                continue;
            }
            if e.name == "presentation:notes" || e.name == "loext:theme" || e.name == "office:forms" {
                continue;
            }
            if let Some(s) = self.shape(e, &ctx, 0) {
                decorations.push(s);
            }
        }
        // Decorations are drawn behind the placeholders.
        let mut shapes = decorations;
        shapes.append(&mut master.shapes);
        master.shapes = shapes;
        // Layout placeholders take the master's title and body boxes.
        let title = master.shapes.iter().find(|s| s.ph_type() == Some(PhType::Title)).and_then(|s| s.xfrm);
        let body = master.shapes.iter().find(|s| s.ph_type() == Some(PhType::Body)).and_then(|s| s.xfrm);
        for l in &mut master.layouts {
            if !matches!(l.kind, LayoutType::TitleAndContent | LayoutType::TitleOnly) {
                continue;
            }
            for s in &mut l.shapes {
                match s.ph_type() {
                    Some(PhType::Title) if title.is_some() => s.xfrm = title,
                    Some(PhType::Body | PhType::Obj) if body.is_some() => s.xfrm = body,
                    _ => {}
                }
            }
        }
        self.master_index.insert(name, self.p.masters.len());
        self.p.masters.push(Arc::new(master));
    }

    /// One level of a master text style from a resolved presentation style.
    fn level_style(&self, s: &Style, scope: Scope, level: u8, in_list: bool) -> LevelStyle {
        let (mut para, run) = text::level_props(&self.store, scope, s, usize::from(level), in_list);
        text::complete(&mut para);
        LevelStyle { para, run }
    }

    // -----------------------------------------------------------------------------------------
    // Pages

    fn page(&mut self, page: &El, decls: &Decls) -> Option<Slide> {
        let id = SlideId(self.p.alloc_id()?);
        let master_name = attr(page, "draw:master-page-name").unwrap_or("").to_string();
        let mi = self.master_index.get(&master_name).copied().unwrap_or(0);
        let master_name = if self.master_index.contains_key(&master_name) {
            master_name
        } else {
            self.master_index.iter().find(|(_, i)| **i == 0).map(|(n, _)| n.clone()).unwrap_or_default()
        };
        let page_style = attr(page, "draw:style-name").map(|n| self.store.resolve(Scope::Content, "drawing-page", Some(n))).unwrap_or_default();
        let pp = &page_style.page;
        let master_bg = self.p.masters.get(mi).and_then(|m| m.background.as_ref()).and_then(background_rgb);
        let page_bg = self.background(pp);
        let bg = page_bg.as_ref().and_then(background_rgb).or(master_bg).unwrap_or(Rgba::WHITE);
        let title_slide = page.elements().any(|e| attr(e, "presentation:class") == Some("subtitle"));
        let ctx = Ctx { scope: Scope::Content, master: master_name, bg, title_slide };
        let mut slide = Slide { id, name: attr(page, "draw:name").unwrap_or("").to_string(), ..Default::default() };
        if let Some(n) = attr(page, "draw:name") {
            self.page_names.entry(n.to_string()).or_insert(id);
        }
        slide.background = page_bg;
        if pp.get("presentation:background-visible").map(String::as_str) == Some("false") {
            slide.background = Some(Background::Fill { fill: Fill::solid(ColorRef::rgb(Rgba::WHITE)) });
        }
        if pp.get("presentation:background-objects-visible").map(String::as_str) == Some("false") {
            slide.show_master_shapes = false;
        }
        if pp.get("presentation:visibility").map(String::as_str) == Some("hidden") {
            slide.hidden = true;
        }
        slide.transition = transition(pp);
        self.targets.clear();
        self.glue.clear();
        let mut shapes = vec![];
        let mut timing = None;
        for e in page.elements() {
            match e.name.as_str() {
                "presentation:notes" => {
                    let note_ctx = ctx.clone();
                    if let Some(f) = e.elements().find(|f| attr(f, "presentation:class") == Some("notes"))
                        && let Some(s) = self.shape(f, &note_ctx, 0)
                        && let Some(t) = s.text
                    {
                        slide.notes = TextBody { paragraphs: t.paragraphs, ..Default::default() };
                    }
                }
                "anim:par" if attr(e, "presentation:node-type") == Some("timing-root") => timing = Some(e),
                "officeooo:annotation" | "office:annotation" => {
                    if slide.comments.len() < 10_000 {
                        slide.comments.push(comment(e));
                    }
                }
                "anim:par" | "anim:seq" | "office:forms" => {}
                _ => {
                    if let Some(s) = self.shape(e, &ctx, 0) {
                        shapes.push(s);
                    }
                }
            }
        }
        // Glued connector ends, now that every shape on the page has its id.
        for (cid, start, end) in std::mem::take(&mut self.glue) {
            let resolve = |g: &Option<(String, u32)>| {
                let (id, pt) = g.as_ref()?;
                let (sid, _) = self.targets.get(id)?;
                // LibreOffice numbers a shape's glue points top, right, bottom, left; ours run top,
                // left, bottom, right.
                Some((*sid, crate::GLUE_POINTS.get(*pt as usize).copied().unwrap_or(0)))
            };
            let (a, b) = (resolve(&start), resolve(&end));
            if let Some(c) = deckcraft_model::find_shape_mut(&mut shapes, cid)
                && let ShapeKind::Connector { start, end } = &mut c.kind
            {
                *start = a;
                *end = b;
            }
        }
        if let Some(t) = timing {
            let page_id = attr(page, "xml:id").or_else(|| attr(page, "draw:id"));
            let tm = crate::anim::read(t, page_id, &self.targets);
            slide.animations = tm.animations;
            if let Some(mut tr) = tm.transition {
                // The page style keeps how the slide advances.
                if let Some(old) = &slide.transition {
                    tr.advance_after_ms = old.advance_after_ms;
                }
                slide.transition = Some(tr);
            }
        }
        // Footer fields the page shows, from its master's placeholders.
        self.page_footers(mi, pp, page, decls, &mut shapes);
        // The layout from the placeholders the page has.
        let has = |k: PhType| shapes.iter().any(|s: &Shape| s.ph_type() == Some(k));
        let bodies = shapes
            .iter()
            .filter(|s| matches!(s.ph_type(), Some(PhType::Body | PhType::Obj | PhType::Chart | PhType::Table | PhType::Picture)))
            .count();
        let kind = if ctx.title_slide || has(PhType::CtrTitle) {
            LayoutType::Title
        } else if has(PhType::Title) && bodies >= 2 {
            LayoutType::TwoContent
        } else if has(PhType::Title) && bodies == 1 {
            LayoutType::TitleAndContent
        } else if has(PhType::Title) {
            LayoutType::TitleOnly
        } else {
            LayoutType::Blank
        };
        let master = self.p.masters.get(mi)?;
        slide.layout = master.layouts.iter().find(|l| l.kind == kind).or(master.layouts.first()).map(|l| l.id).unwrap_or_default();
        slide.shapes = shapes;
        Some(slide)
    }

    fn page_footers(&mut self, mi: usize, pp: &Props, page: &El, decls: &Decls, shapes: &mut Vec<Shape>) {
        let Some(master) = self.p.masters.get(mi).cloned() else { return };
        let on = |k: &str| pp.get(k).map(String::as_str) == Some("true");
        let mut add = |kind: PhType, text: Option<String>, this: &mut Self| {
            let Some(src) = master.shapes.iter().find(|s| s.ph_type() == Some(kind)) else { return };
            let mut s = src.clone();
            s.id = this.id();
            // The declared text replaces the field, in the placeholder's first paragraph.
            if let (Some(t), Some(body)) = (text, s.text.as_mut()) {
                body.paragraphs.truncate(1);
                if let Some(p) = body.paragraphs.first_mut() {
                    let props = p.runs.first().map(|r| r.props.clone()).unwrap_or_else(|| p.end_props.clone());
                    p.runs = vec![deckcraft_model::text::Run::with(t, props)];
                }
            }
            shapes.push(s);
        };
        if on("presentation:display-page-number") {
            add(PhType::SlideNum, None, self);
            self.p.header_footer.slide_number = true;
        }
        if on("presentation:display-footer")
            && let Some(t) = attr(page, "presentation:use-footer-name").and_then(|n| decls.footers.get(n))
            && !t.is_empty()
        {
            add(PhType::Footer, Some(t.clone()), self);
            self.p.header_footer.footer = true;
            self.p.header_footer.footer_text.clone_from(t);
        }
        if on("presentation:display-date-time")
            && let Some((current, t)) = attr(page, "presentation:use-date-time-name").and_then(|n| decls.dates.get(n))
        {
            self.p.header_footer.date = true;
            if *current {
                add(PhType::Date, None, self);
            } else if !t.is_empty() {
                add(PhType::Date, Some(t.clone()), self);
                self.p.header_footer.date_text.clone_from(t);
            }
        }
    }

    /// `#page-name` links become slide links.
    fn resolve_links(&mut self) {
        let names = self.page_names.clone();
        let fix = |l: &mut Option<deckcraft_model::text::Hyperlink>| {
            let Some(h) = l else { return };
            if let Action::Url { url } = &h.action
                && let Some(name) = url.strip_prefix('#')
            {
                match names.get(name) {
                    Some(id) => h.action = Action::Slide { slide: *id },
                    None => *l = None,
                }
            }
        };
        // Every holder of a link: shapes (and their groups and tables), notes, masters, layouts.
        for s in &mut self.p.slides {
            let s = Arc::make_mut(s);
            for sh in &mut s.shapes {
                fix_shape(sh, &fix, 0);
            }
            fix_body(&mut s.notes, &fix);
        }
        for m in &mut self.p.masters {
            let m = Arc::make_mut(m);
            for sh in &mut m.shapes {
                fix_shape(sh, &fix, 0);
            }
            for l in &mut m.layouts {
                for sh in &mut l.shapes {
                    fix_shape(sh, &fix, 0);
                }
            }
        }
        type Fix<'a> = &'a dyn Fn(&mut Option<deckcraft_model::text::Hyperlink>);
        fn fix_body(t: &mut TextBody, fix: Fix) {
            for p in &mut t.paragraphs {
                for r in &mut p.runs {
                    fix(&mut r.props.link);
                    fix(&mut r.props.hover);
                }
            }
        }
        fn fix_shape(s: &mut Shape, fix: Fix, depth: usize) {
            if depth > MAX_DEPTH {
                return;
            }
            fix(&mut s.click);
            fix(&mut s.hover);
            if let Some(t) = s.text.as_mut() {
                fix_body(t, fix);
            }
            if let ShapeKind::Table(t) = &mut s.kind {
                for c in t.rows.iter_mut().flat_map(|r| r.cells.iter_mut()) {
                    fix_body(&mut c.text, fix);
                }
            }
            if let Some(ch) = s.children_mut() {
                for c in ch {
                    fix_shape(c, fix, depth + 1);
                }
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // Shapes

    fn xfrm(&self, e: &El) -> Option<Xfrm> {
        let len = |k: &str| attr(e, k).and_then(units::length);
        let w = len("svg:width").unwrap_or(0.0).max(0.0);
        let h = len("svg:height").unwrap_or(0.0).max(0.0);
        if let Some(m) = attr(e, "draw:transform").and_then(geometry::transform) {
            let (x, y, w, h, rot, flip) = geometry::placed_box(m, w, h);
            return Some(Xfrm { x, y, w, h, rot, flip_h: false, flip_v: flip });
        }
        Some(Xfrm::new(len("svg:x").unwrap_or(0.0), len("svg:y").unwrap_or(0.0), w, h))
    }

    /// The resolved style of a shape, and whether it is a presentation style.
    fn shape_style(&self, e: &El, ctx: &Ctx) -> (Style, bool) {
        match attr(e, "presentation:style-name") {
            Some(n) => (self.store.resolve(ctx.scope, "presentation", Some(n)), true),
            None => (self.store.resolve(ctx.scope, "graphic", attr(e, "draw:style-name")), false),
        }
    }

    fn shape(&mut self, e: &El, ctx: &Ctx, depth: usize) -> Option<Shape> {
        if depth > MAX_DEPTH || self.shapes >= MAX_SHAPES {
            return None;
        }
        self.shapes += 1;
        let (style, presentation) = self.shape_style(e, ctx);
        let g = &style.graphic;
        let mut s = Shape {
            id: self.id(),
            name: attr(e, "draw:name").unwrap_or("").to_string(),
            xfrm: self.xfrm(e),
            descr: kid(e, "svg:desc").map(|d| d.text()).unwrap_or_default(),
            title: kid(e, "svg:title").map(|d| d.text()).unwrap_or_default(),
            decorative: g.get("loext:decorative").map(String::as_str) == Some("true"),
            hidden: attr(e, "draw:display") == Some("none") || attr(e, "presentation:display") == Some("none"),
            ..Default::default()
        };
        let class = attr(e, "presentation:class");
        if let Some(kind) = class.and_then(ph_kind).map(|k| if k == PhType::Title && ctx.title_slide { PhType::CtrTitle } else { k }) {
            let idx = match kind {
                PhType::Title | PhType::CtrTitle => 0,
                PhType::Date => 10,
                PhType::Footer => 11,
                PhType::SlideNum => 12,
                _ => 1,
            };
            s.ph = Some(Placeholder { kind, idx, ..Default::default() });
        }
        if let Some(id) = attr(e, "xml:id").or_else(|| attr(e, "draw:id")) {
            self.targets.insert(id.to_string(), (s.id, None));
        }
        s.fill = Some(self.fill(g));
        s.line = Some(self.line(g));
        s.effects = self.effects(g);
        let shape_para = attr(e, "draw:text-style-name").map(|n| self.store.chain(ctx.scope, "paragraph", n));
        let outline = class == Some("outline");
        let store = std::rc::Rc::clone(&self.store);
        let behind = s.fill.as_ref().and_then(fill_rgb).unwrap_or(ctx.bg);
        let levels =
            if outline { store.outline_levels(ctx.scope, &ctx.master, attr(e, "presentation:style-name"), &style) } else { vec![style.clone()] };
        // Title and outline placeholders inherit what their styles leave unset from the master's
        // title and outline levels, which are read from the same styles.
        let inherit = matches!(class, Some("title" | "outline"));
        let tcx = TextCtx { store: &store, scope: ctx.scope, levels, shape_para, auto_color: text::auto_text_color(behind), inherit };
        let name = e.name.as_str();
        match name {
            "draw:frame" => self.frame(e, &mut s, &tcx, g, ctx)?,
            "draw:rect" | "draw:caption" => {
                s.geom = Geom::preset("rect");
                if let Some(r) = attr(e, "draw:corner-radius").and_then(units::length).filter(|r| *r > 0.0) {
                    let x = s.xfrm.unwrap_or_default();
                    let ss = x.w.min(x.h).max(1e-6);
                    s.geom = Geom::Preset { name: "roundRect".into(), adj: vec![(r / ss * 100_000.0).clamp(0.0, 50_000.0)] };
                }
                s.text = Some(self.text_body(&tcx, e, g));
            }
            "draw:ellipse" | "draw:circle" => {
                s.geom = ellipse_geom(e);
                s.text = Some(self.text_body(&tcx, e, g));
            }
            "draw:line" | "draw:measure" => {
                s.xfrm = Some(line_xfrm(e));
                s.geom = Geom::preset("line");
                s.fill = Some(Fill::None);
                s.text = Some(self.text_body(&tcx, e, g));
            }
            "draw:polyline" | "draw:polygon" | "draw:path" | "draw:connector" => {
                let x = s.xfrm.unwrap_or_default();
                let vb = geometry::view_box(attr(e, "svg:viewBox")).unwrap_or([0.0, 0.0, x.w.max(1e-6), x.h.max(1e-6)]);
                let path = match name {
                    "draw:polyline" => attr(e, "draw:points").and_then(|p| geometry::points_path(p, vb, false)),
                    "draw:polygon" => attr(e, "draw:points").and_then(|p| geometry::points_path(p, vb, true)),
                    _ => attr(e, "svg:d").and_then(|d| geometry::svg_path(d, vb, name == "draw:path")),
                };
                let straight = attr(e, "draw:type") == Some("line");
                if name == "draw:connector" && (path.is_none() || straight) {
                    // A straight connector: its end points, and the shapes it is glued to.
                    s.xfrm = Some(line_xfrm(e));
                    s.kind = ShapeKind::Connector { start: None, end: None };
                    let glue = |k: &str, g: &str| attr(e, k).map(|id| (id.to_string(), attr(e, g).and_then(|v| v.parse::<u32>().ok()).unwrap_or(0)));
                    self.glue.push((s.id, glue("draw:start-shape", "draw:start-glue-point"), glue("draw:end-shape", "draw:end-glue-point")));
                    s.geom = Geom::preset("straightConnector1");
                    s.fill = Some(Fill::None);
                } else {
                    let path = path?;
                    if name == "draw:polyline" || name == "draw:connector" {
                        s.fill = Some(Fill::None);
                    }
                    if name == "draw:connector" {
                        // A connector's svg:d is in page coordinates (no view box of its own).
                        if attr(e, "svg:viewBox").is_none() {
                            return self.connector_from_page_path(s, e);
                        }
                    }
                    s.geom = Geom::Custom { paths: vec![path] };
                }
                s.text = Some(self.text_body(&tcx, e, g));
            }
            "draw:custom-shape" => self.custom_shape(e, &mut s, &tcx, g),
            "draw:g" => {
                let mut children = vec![];
                for c in e.elements() {
                    if let Some(cs) = self.shape(c, ctx, depth + 1) {
                        children.push(cs);
                    }
                }
                if children.is_empty() {
                    return None;
                }
                let bb = bbox(&children);
                s.xfrm = Some(bb);
                s.kind = ShapeKind::Group { children, child: bb };
                s.fill = None;
                s.line = None;
                s.effects = None;
            }
            "draw:a" => {
                // A link around a shape: the shape, clickable.
                let inner = e.elements().next()?;
                let mut inner_shape = self.shape(inner, ctx, depth + 1)?;
                inner_shape.click = text::hyperlink(attr(e, "xlink:href").unwrap_or(""));
                return Some(inner_shape);
            }
            _ => return None,
        }
        if let Some(click) = self.event_listener(e) {
            s.click = Some(click);
        }
        // Paragraph ids, for effects that animate one paragraph.
        let container = kid(e, "draw:text-box").unwrap_or(e);
        for (i, id) in text::paragraph_ids(container).into_iter().enumerate() {
            if let Some(id) = id {
                self.targets.insert(id, (s.id, Some(i as u32)));
            }
        }
        // Empty text bodies on non-placeholders aren't kept.
        if s.ph.is_none() && s.text.as_ref().is_some_and(|t| t.paragraphs.iter().all(Paragraph::is_empty)) && !matches!(s.kind, ShapeKind::Shape) {
            s.text = None;
        }
        if presentation && s.ph.is_some() && attr(e, "presentation:placeholder") == Some("true") {
            // An empty placeholder showing its prompt.
            if let Some(t) = s.text.as_mut() {
                t.paragraphs.retain(|p| !p.is_empty());
            }
        }
        Some(s)
    }

    /// A `draw:frame`: a text box, table, picture or object. `None`: nothing we show.
    fn frame(&mut self, e: &El, s: &mut Shape, tcx: &TextCtx, g: &Props, ctx: &Ctx) -> Option<()> {
        let is_object = kid(e, "draw:object").is_some() || kid(e, "draw:object-ole").is_some();
        if let Some(tb) = kid(e, "draw:text-box") {
            s.text = Some(self.text_body(tcx, tb, g));
            if s.ph.is_none() {
                s.text_box = true;
            }
        } else if let Some(t) = kid(e, "table:table") {
            let x = s.xfrm.unwrap_or_default();
            s.kind = ShapeKind::Table(self.table(t, ctx, x));
            s.fill = None;
            s.line = None;
        } else if let Some(chart) = kid(e, "draw:object").and_then(|o| self.object_chart(o)) {
            s.kind = ShapeKind::Chart(Box::new(chart));
            s.ph = None;
            s.fill = None;
            s.line = None;
        } else if let Some(pic) = self.frame_image(e) {
            let adjust = picture_adjust(g);
            let alpha = g.get("draw:image-opacity").and_then(|v| units::percent(v)).filter(|a| *a < 1.0);
            s.kind = if is_object {
                ShapeKind::Opaque { xml: String::new(), preview: Some(pic), label: "OLE object".into(), parts: vec![] }
            } else {
                ShapeKind::Picture {
                    fill: PictureFill {
                        media: pic,
                        crop: self.crop(g, pic),
                        mode: PictureMode::default(),
                        alpha,
                        adjust,
                        upright: false,
                        artistic: None,
                    },
                }
            };
            s.ph = None;
        } else if let Some(clip) = kid(e, "draw:plugin").and_then(|pl| self.media_clip(e, pl)) {
            s.kind = ShapeKind::Media(clip);
            s.ph = None;
        } else if is_object || kid(e, "draw:plugin").is_some() {
            s.kind = ShapeKind::Opaque { xml: String::new(), preview: None, label: "Embedded object".into(), parts: vec![] };
        } else if s.ph.is_none() {
            return None;
        } else {
            s.text = Some(self.text_body(tcx, e, g));
        }
        Some(())
    }

    /// Audio or video (`draw:plugin`), its poster picture and play settings.
    fn media_clip(&mut self, frame: &El, pl: &El) -> Option<deckcraft_model::MediaClip> {
        let href = attr(pl, "xlink:href")?;
        let mime = attr(pl, "draw:mime-type").map(str::to_string).unwrap_or_else(|| media_mime(href).to_string());
        let name = href.rsplit('/').next().unwrap_or(href).to_string();
        let media = match self.package_media(href, &mime) {
            Some(id) => id,
            None => {
                // A linked file: kept as a link.
                let id = self.p.add_media(&name, &mime, vec![])?;
                if let Some(m) = self.p.media.iter_mut().find(|m| m.id == id) {
                    m.link = Some(href.to_string());
                }
                id
            }
        };
        let param = |n: &str| kids(pl, "draw:param").find(|p| attr(p, "draw:name") == Some(n)).and_then(|p| attr(p, "draw:value"));
        let video = mime.starts_with("video/")
            || !mime.starts_with("audio/") && !matches!(name.rsplit('.').next(), Some("mp3" | "wav" | "ogg" | "m4a" | "flac" | "wma" | "aac"));
        let poster = self.frame_image(frame);
        let db = param("VolumeDB").and_then(|v| v.parse::<f64>().ok()).filter(|v| v.is_finite()).unwrap_or(0.0);
        Some(deckcraft_model::MediaClip {
            media,
            video,
            poster,
            volume: if param("Mute") == Some("true") { 0.0 } else { 10f64.powf(db / 20.0).clamp(0.0, 1.0) },
            loop_play: param("Loop") == Some("true"),
            ..Default::default()
        })
    }

    /// The chart an embedded object holds (`Object 1/content.xml`), if it is a chart.
    fn object_chart(&self, o: &El) -> Option<deckcraft_model::chart::Chart> {
        let href = attr(o, "xlink:href")?.trim_start_matches("./").trim_end_matches('/');
        if href.is_empty() {
            // An inline chart document (flat files), read in place.
            return crate::chart::read(kid(o, "office:document")?);
        }
        crate::chart::read(&self.pkg.xml(&format!("{href}/content.xml"))?)
    }

    /// `fo:clip="rect(top, right, bottom, left)"`, lengths at the picture's own size, as the
    /// crop fractions (left, top, right, bottom).
    fn crop(&self, g: &Props, media: MediaId) -> [f64; 4] {
        let Some(clip) = g.get("fo:clip").and_then(|c| c.trim().strip_prefix("rect(")).and_then(|c| c.strip_suffix(')')) else { return [0.0; 4] };
        let v: Vec<f64> = clip.split([',', ' ']).filter(|t| !t.is_empty()).map(|t| units::length(t).unwrap_or(0.0)).collect();
        let [top, right, bottom, left] = v.as_slice() else { return [0.0; 4] };
        let Some((w, h)) = self.p.media(media).and_then(|m| deckcraft_render::image_natural_size(&m.data)) else { return [0.0; 4] };
        // Natural size is in 96-dpi pixels; points are three quarters of those.
        let (w, h) = (w * 0.75, h * 0.75);
        if w <= 0.0 || h <= 0.0 {
            return [0.0; 4];
        }
        [left / w, top / h, right / w, bottom / h].map(|f| if f.is_finite() { f.clamp(-1.0, 0.99) } else { 0.0 })
    }

    /// A `draw:custom-shape`: its enhanced geometry as a preset or a custom path.
    fn custom_shape(&mut self, e: &El, s: &mut Shape, tcx: &TextCtx, g: &Props) {
        let x = s.xfrm.unwrap_or_default();
        let Some(eg) = kid(e, "draw:enhanced-geometry").and_then(|eg| geometry::enhanced(eg, x.w, x.h)) else {
            s.text = Some(self.text_body(tcx, e, g));
            return;
        };
        s.geom = custom_geom(&eg);
        if let Some(x) = s.xfrm.as_mut() {
            x.flip_h ^= eg.mirror_h;
            x.flip_v ^= eg.mirror_v;
        }
        s.text = Some(self.text_body(tcx, e, g));
        if let (Some(ta), Some(t), Geom::Custom { .. }) = (eg.text_area, s.text.as_mut(), &s.geom) {
            // Our custom geometry puts text in the whole box: inset it to the text area.
            let b = &mut t.body;
            b.inset_l = Some(b.inset_l.unwrap_or(0.0) + ta[0] * x.w);
            b.inset_t = Some(b.inset_t.unwrap_or(0.0) + ta[1] * x.h);
            b.inset_r = Some(b.inset_r.unwrap_or(0.0) + (1.0 - ta[2]) * x.w);
            b.inset_b = Some(b.inset_b.unwrap_or(0.0) + (1.0 - ta[3]) * x.h);
        }
    }

    /// A connector whose `svg:d` is in page coordinates: a custom path in its own bounding box.
    fn connector_from_page_path(&mut self, mut s: Shape, e: &El) -> Option<Shape> {
        let d = attr(e, "svg:d")?;
        // svg:d without a view box is in 1/100 mm.
        let mut p = kurbo::BezPath::from_svg(d).ok()?;
        p.apply_affine(kurbo::Affine::scale(72.0 / 2540.0));
        let bb = kurbo::Shape::bounding_box(&p);
        if !bb.is_finite() {
            return None;
        }
        p.apply_affine(kurbo::Affine::translate((-bb.x0, -bb.y0)));
        s.xfrm = Some(Xfrm::new(bb.x0, bb.y0, bb.width(), bb.height()));
        s.geom = Geom::Custom {
            paths: vec![CustomPath {
                w: bb.width().max(1e-6),
                h: bb.height().max(1e-6),
                d: geometry::path_d(&p),
                fill: FillMode::None,
                stroke: true,
            }],
        };
        s.fill = Some(Fill::None);
        s.text = None;
        Some(s)
    }

    /// `office:event-listeners`: a click that goes to a page, a URL or a show position.
    fn event_listener(&self, e: &El) -> Option<deckcraft_model::text::Hyperlink> {
        let ls = kid(e, "office:event-listeners")?;
        let l = ls.elements().find(|l| attr(l, "script:event-name").is_some_and(|n| n.ends_with("click")))?;
        let action = match attr(l, "presentation:action").unwrap_or("") {
            "next-page" => Action::NextSlide,
            "previous-page" => Action::PreviousSlide,
            "first-page" => Action::FirstSlide,
            "last-page" => Action::LastSlide,
            "stop" => Action::EndShow,
            "show" => return text::hyperlink(attr(l, "xlink:href").unwrap_or("")),
            _ => return None,
        };
        Some(deckcraft_model::text::Hyperlink {
            action,
            tooltip: String::new(),
            highlight_click: false,
            sound: None,
            stop_sound: false,
            zoom_return: None,
        })
    }

    fn text_body(&self, cx: &TextCtx, container: &El, g: &Props) -> TextBody {
        let mut paragraphs = text::paragraphs(cx, container);
        let custom_shape = container.name == "draw:custom-shape";
        let body = body_props(g, custom_shape);
        // A text block pushed to the right of the frame (centred is `anchor_ctr`): we have no
        // such block placement, so lines that start at the left follow it.
        if g.get("draw:textarea-horizontal-align").map(String::as_str) == Some("right") {
            for p in &mut paragraphs {
                if p.props.align.unwrap_or(Align::Left) == Align::Left && p.props.bullet == Some(deckcraft_model::text::Bullet::None) {
                    p.props.align = Some(Align::Right);
                }
            }
        }
        TextBody { body, list_style: ListStyle::default(), paragraphs }
    }

    /// The image of a `draw:frame` (the first `draw:image` we can read), as media.
    fn frame_image(&mut self, frame: &El) -> Option<MediaId> {
        let mut images: Vec<&El> = kids(frame, "draw:image").collect();
        // An object's replacement image.
        if images.is_empty() {
            return None;
        }
        // Prefer raster images the renderer reads directly over SVG and metafiles.
        images.sort_by_key(|i| {
            let h = attr(i, "xlink:href").unwrap_or("").to_ascii_lowercase();
            if h.ends_with(".png") || h.ends_with(".jpg") || h.ends_with(".jpeg") || h.ends_with(".gif") { 0 } else { 1 }
        });
        for img in images {
            if let Some(m) = self.image_media(img) {
                return Some(m);
            }
        }
        None
    }

    /// A package part as media, added once however many shapes refer to it.
    fn package_media(&mut self, href: &str, mime: &str) -> Option<MediaId> {
        if let Some(id) = self.package_media.get(href) {
            return Some(*id);
        }
        let data = self.pkg.get(href)?;
        let name = href.rsplit('/').next().unwrap_or(href);
        let id = self.p.add_media(name, mime, data.to_vec())?;
        self.package_media.insert(href.to_string(), id);
        Some(id)
    }

    fn image_media(&mut self, img: &El) -> Option<MediaId> {
        if let Some(href) = attr(img, "xlink:href").filter(|h| !h.is_empty() && !h.contains("://")) {
            let mime = attr(img, "draw:mime-type").map(str::to_string).unwrap_or_else(|| mime_for(href).to_string());
            return self.package_media(href, &mime);
        }
        let b64 = kid(img, "office:binary-data")?.text();
        let data = base64(&b64)?;
        let mime = sniff_image(&data);
        self.p.add_media(&format!("image.{}", mime.1), mime.0, data)
    }

    fn table(&mut self, t: &El, ctx: &Ctx, x: Xfrm) -> Table {
        let mut table = Table { first_row: false, band_row: false, style: String::new(), ..Default::default() };
        let flag = |k: &str| attr(t, k) == Some("true");
        table.first_row = flag("table:use-first-row-styles");
        table.last_row = flag("table:use-last-row-styles");
        table.first_col = flag("table:use-first-column-styles");
        table.last_col = flag("table:use-last-column-styles");
        table.band_row = flag("table:use-banding-rows-styles");
        table.band_col = flag("table:use-banding-columns-styles");
        for c in t.elements().filter(|c| c.name == "table:table-column" || c.name == "table:table-columns").take(1000) {
            let cols: Vec<&El> = if c.name == "table:table-columns" { kids(c, "table:table-column").collect() } else { vec![c] };
            for c in cols {
                let rep = repeat(c, "table:number-columns-repeated").min(MAX_TABLE_SIDE);
                let w = attr(c, "table:style-name")
                    .map(|n| self.store.resolve(ctx.scope, "table-column", Some(n)))
                    .and_then(|s| s.column.get("style:column-width").and_then(|v| units::length(v)))
                    .filter(|w| *w > 0.0);
                let n = rep.min(MAX_TABLE_SIDE - table.cols.len());
                table.cols.extend(std::iter::repeat_n(w.unwrap_or(0.0), n));
            }
        }
        let rows: Vec<&El> = t
            .elements()
            .flat_map(|e| {
                if e.name == "table:table-row" {
                    vec![e]
                } else if e.name == "table:table-rows" || e.name == "table:table-header-rows" {
                    kids(e, "table:table-row").collect()
                } else {
                    vec![]
                }
            })
            .take(MAX_TABLE_SIDE)
            .collect();
        // The table's width before any cell is read, so every row is budgeted at the width it
        // will be padded to.
        let is_cell = |c: &&El| c.name == "table:table-cell" || c.name == "table:covered-table-cell";
        let row_width = |r: &El| r.elements().filter(is_cell).map(|c| repeat(c, "table:number-columns-repeated")).fold(0usize, usize::saturating_add);
        let width = rows.iter().map(|r| row_width(r)).max().unwrap_or(0).max(table.cols.len()).clamp(1, MAX_TABLE_SIDE);
        let mut text_budget = MAX_TABLE_TEXT;
        for row in rows {
            if table.rows.len() >= MAX_TABLE_SIDE {
                break;
            }
            let rs = attr(row, "table:style-name").map(|n| self.store.resolve(ctx.scope, "table-row", Some(n)));
            let h = rs
                .as_ref()
                .and_then(|s| s.row.get("style:row-height").or_else(|| s.row.get("style:min-row-height")).and_then(|v| units::length(v)))
                .unwrap_or(0.0);
            // An optimal-height row is as tall as its text: give it a small height to grow from.
            let optimal = rs.as_ref().is_some_and(|s| s.row.get("style:use-optimal-row-height").map(String::as_str) == Some("true"));
            let h = if optimal && h <= 0.0 { 12.0 } else { h };
            let default_cell = attr(row, "table:default-cell-style-name");
            let mut cells: Vec<Cell> = vec![];
            for c in row.elements().filter(is_cell) {
                let n = repeat(c, "table:number-columns-repeated").min(width - cells.len().min(width));
                if n == 0 {
                    break;
                }
                // A repeated cell is read once; its copies keep their text while the budget lasts.
                let cell = self.cell(c, ctx, default_cell);
                let size = cell.text.text().len();
                for k in 0..n {
                    if k == 0 || size <= text_budget {
                        text_budget = text_budget.saturating_sub(if k == 0 { 0 } else { size });
                        cells.push(cell.clone());
                    } else {
                        cells.push(Cell { text: Cell::new("").text, ..cell.clone() });
                    }
                }
            }
            // Repeated rows cost the full width, and their text the budget.
            let room = MAX_TABLE_CELLS.saturating_sub(table.rows.len() * width).checked_div(width).unwrap_or(0);
            let row_text: usize = cells.iter().map(|c| c.text.text().len()).sum();
            let text_room = text_budget.checked_div(row_text).map_or(usize::MAX, |r| r + 1);
            let n = repeat(row, "table:number-rows-repeated").min(MAX_TABLE_SIDE - table.rows.len()).min(room).min(text_room);
            text_budget = text_budget.saturating_sub(row_text.saturating_mul(n.saturating_sub(1)));
            if n == 0 {
                break;
            }
            table.rows.extend(std::iter::repeat_n(Row { height: h, cells }, n));
        }
        fit_table(&mut table, x);
        merge_spans(&mut table);
        table
    }

    fn cell(&mut self, c: &El, ctx: &Ctx, default_style: Option<&str>) -> Cell {
        let mut cell = Cell::new("");
        if c.name == "table:covered-table-cell" {
            return cell;
        }
        cell.grid_span = attr(c, "table:number-columns-spanned").and_then(|v| v.parse().ok()).unwrap_or(1).clamp(1, 1000);
        cell.row_span = attr(c, "table:number-rows-spanned").and_then(|v| v.parse().ok()).unwrap_or(1).clamp(1, 1000);
        // A cell's style is a graphic style (the row's default, `standard` in LibreOffice) with
        // its own cell style over it.
        let mut st = self.store.resolve(
            ctx.scope,
            "graphic",
            Some(default_style.filter(|n| self.store.exists(ctx.scope, "graphic", n)).unwrap_or("standard")),
        );
        for name in [default_style, attr(c, "table:style-name")].into_iter().flatten() {
            if self.store.exists(ctx.scope, "table-cell", name) {
                st.merge(&self.store.chain(ctx.scope, "table-cell", name));
            }
        }
        let mut g = st.graphic.clone();
        match st.cell.get("fo:background-color").map(String::as_str) {
            Some("transparent") => {
                g.insert("draw:fill".into(), "none".into());
            }
            Some(bg) => {
                g.insert("draw:fill".into(), "solid".into());
                g.insert("draw:fill-color".into(), bg.to_string());
            }
            None => {}
        }
        // Unset, a cell is filled (the built-in default), as the slide shows it.
        cell.fill = Some(self.fill(&g));
        let border = |k: &str| st.cell.get(k).or_else(|| st.cell.get("fo:border")).and_then(|b| border_line(b));
        // An unset border is none (not the table style's).
        // The model's order: left, right, top, bottom.
        cell.borders = [border("fo:border-left"), border("fo:border-right"), border("fo:border-top"), border("fo:border-bottom")]
            .map(|b| b.or_else(|| Some(Line::none())));
        let pad = |k: &str| st.cell.get(k).or_else(|| st.cell.get("fo:padding")).and_then(|v| units::length(v));
        if let (Some(l), Some(t), Some(r), Some(b)) =
            (pad("fo:padding-left"), pad("fo:padding-top"), pad("fo:padding-right"), pad("fo:padding-bottom"))
        {
            cell.margins = Some([l, r, t, b]);
        }
        cell.anchor = st.cell.get("style:vertical-align").or_else(|| st.graphic.get("draw:textarea-vertical-align")).and_then(|v| anchor(v));
        let mut base = st.clone();
        base.list = None;
        let store = std::rc::Rc::clone(&self.store);
        let behind = cell.fill.as_ref().and_then(fill_rgb).unwrap_or(ctx.bg);
        let tcx = TextCtx {
            store: &store,
            scope: ctx.scope,
            levels: vec![base],
            shape_para: None,
            auto_color: text::auto_text_color(behind),
            inherit: false,
        };
        let mut paragraphs = text::paragraphs(&tcx, c);
        if paragraphs.is_empty() {
            paragraphs.push(Paragraph::default());
        }
        cell.text = TextBody { paragraphs, ..Default::default() };
        cell
    }

    // -----------------------------------------------------------------------------------------
    // Fills, lines, effects

    fn background(&mut self, page: &Props) -> Option<Background> {
        let fill = page.get("draw:fill")?;
        if fill == "none" {
            return None;
        }
        // A page style names no colour of its own: the graphic default's applies.
        let mut props = self.store.default_of("graphic").graphic;
        props.extend(page.iter().map(|(k, v)| (k.clone(), v.clone())));
        Some(Background::Fill { fill: self.fill(&props) })
    }

    fn fill(&mut self, g: &Props) -> Fill {
        // An opacity gradient wins over a flat opacity.
        let og = g.get("draw:opacity-name").and_then(|n| self.store.opacities.get(n)).cloned();
        let alpha = match &og {
            Some(og) => {
                let a = attr(og, "draw:start").and_then(units::percent).unwrap_or(1.0);
                let b = attr(og, "draw:end").and_then(units::percent).unwrap_or(1.0);
                Some(((a + b) / 2.0).clamp(0.0, 1.0))
            }
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
            "gradient" => g
                .get("draw:fill-gradient-name")
                .and_then(|n| self.store.gradients.get(n))
                .map(|gr| Fill::Gradient(gradient(gr, &with_alpha, None)))
                .unwrap_or_else(|| Fill::solid(with_alpha(color))),
            "hatch" => {
                let h = g.get("draw:fill-hatch-name").and_then(|n| self.store.hatches.get(n)).cloned();
                let hc = h.as_ref().and_then(|h| attr(h, "draw:color")).and_then(units::color).unwrap_or(Rgba::BLACK);
                let solid = g.get("draw:fill-hatch-solid").map(String::as_str) == Some("true");
                // Lines at the hatch's angle and spacing, as a tile picture.
                let hatch = crate::hatch::Hatch::new(
                    h.as_ref()
                        .and_then(|h| attr(h, "draw:style"))
                        .and_then(crate::hatch::HatchStyle::parse)
                        .unwrap_or(crate::hatch::HatchStyle::Single),
                    hc,
                    h.as_ref().and_then(|h| attr(h, "draw:distance")).and_then(units::length).unwrap_or(5.0),
                    h.as_ref().and_then(|h| attr(h, "draw:rotation")).and_then(|a| units::angle(a, true)).unwrap_or(0.0),
                    solid.then_some(color),
                );
                let tile = hatch.tile().and_then(|tile| {
                    let px = f64::from(tile.px);
                    let id = self.p.add_media(&hatch.media_name(), "image/png", tile.png)?;
                    Some((id, tile.edge_pt / (0.75 * px)))
                });
                match tile {
                    Some((media, s)) => Fill::Picture(PictureFill {
                        media,
                        mode: PictureMode::Tile { tx: 0.0, ty: 0.0, sx: s, sy: s, flip: "none".into(), align: "tl".into() },
                        alpha: alpha.filter(|a| *a < 1.0),
                        ..Default::default()
                    }),
                    None => {
                        let bg = if solid { with_alpha(color) } else { ColorRef::rgb(Rgba::WHITE).with(ColorTransform::Alpha(0)) };
                        Fill::Pattern(PatternFill { preset: hatch_preset(h.as_ref()), fg: with_alpha(hc), bg })
                    }
                }
            }
            "bitmap" => {
                let media = g.get("draw:fill-image-name").and_then(|n| self.store.fill_images.get(n).cloned()).and_then(|img| self.image_media(&img));
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

    fn line(&self, g: &Props) -> Line {
        let stroke = g.get("draw:stroke").map(String::as_str).unwrap_or("none");
        if stroke == "none" {
            return Line::none();
        }
        let color = crate::styles::color_prop(g, "svg:stroke-color", "svg:stroke-opacity").unwrap_or(ColorRef::rgb(Rgba::BLACK));
        // A zero width is a hairline.
        let width = g.get("svg:stroke-width").and_then(|v| units::length(v)).filter(|w| *w > 0.0).unwrap_or(0.5).min(1000.0);
        let mut l = Line::solid(color, width);
        l.join = g.get("draw:stroke-linejoin").and_then(|j| match j.as_str() {
            "miter" => Some(LineJoin::Miter),
            "bevel" => Some(LineJoin::Bevel),
            "round" => Some(LineJoin::Round),
            _ => None,
        });
        l.cap = g.get("svg:stroke-linecap").and_then(|c| match c.as_str() {
            "round" => Some(LineCap::Round),
            "square" => Some(LineCap::Square),
            "butt" => Some(LineCap::Flat),
            _ => None,
        });
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
            let kind = marker_kind(n, m)?;
            let mw = w.and_then(|v| units::length(v)).unwrap_or(0.3 * 72.0 / 2.54);
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

    fn effects(&self, g: &Props) -> Option<Effects> {
        if g.get("draw:shadow").map(String::as_str) != Some("visible") {
            return None;
        }
        let len = |k: &str| g.get(k).and_then(|v| units::length(v));
        let (dx, dy) = (len("draw:shadow-offset-x").unwrap_or(0.0), len("draw:shadow-offset-y").unwrap_or(0.0));
        let color = crate::styles::color_prop(g, "draw:shadow-color", "draw:shadow-opacity").unwrap_or(ColorRef::rgb(Rgba::rgb(0x80, 0x80, 0x80)));
        Some(Effects {
            outer_shadow: Some(Shadow {
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
            }),
            ..Default::default()
        })
    }
}

/// Fonts embedded in the package (`svg:font-face-uri` in the font face declarations).
fn embedded_fonts(pkg: &Package, store: &Store, p: &mut Presentation) {
    for f in &store.font_faces {
        let family = attr(f, "svg:font-family").map(crate::styles::unquote).or_else(|| attr(f, "style:name").map(str::to_string)).unwrap_or_default();
        if family.is_empty() {
            continue;
        }
        let bold = attr(f, "svg:font-weight").is_some_and(|w| w == "bold" || w.parse::<u32>().is_ok_and(|n| n >= 600));
        let italic = attr(f, "svg:font-style").is_some_and(|s| s == "italic" || s == "oblique");
        let Some(uri) = crate::dom::find(f, "svg:font-face-uri").and_then(|u| attr(u, "xlink:href")) else { continue };
        let Some(data) = pkg.get(uri) else { continue };
        if deckcraft_fonts::embedded_sfnt(data).is_none() {
            continue;
        }
        let name = uri.rsplit('/').next().unwrap_or(uri).to_string();
        let Some(id) = p.add_media(&name, "application/x-font-ttf", data.to_vec()) else { continue };
        let i = match p.embedded_fonts.iter().position(|e| e.typeface == family) {
            Some(i) => i,
            None => {
                p.embedded_fonts.push(deckcraft_model::EmbeddedFont { typeface: family.clone(), ..Default::default() });
                p.embedded_fonts.len() - 1
            }
        };
        if let Some(ef) = p.embedded_fonts.get_mut(i) {
            let slot = match (bold, italic) {
                (true, true) => &mut ef.bold_italic,
                (true, false) => &mut ef.bold,
                (false, true) => &mut ef.italic,
                (false, false) => &mut ef.regular,
            };
            slot.get_or_insert(id);
        }
    }
}

/// A comment (`officeooo:annotation`): author, initials, date, text and where it is pinned.
fn comment(e: &El) -> deckcraft_model::Comment {
    let t = |n: &str| kid(e, n).map(|c| c.text()).unwrap_or_default();
    let initials = {
        let i = t("meta:creator-initials");
        if i.is_empty() { t("loext:sender-initials") } else { i }
    };
    deckcraft_model::Comment {
        author: t("dc:creator"),
        initials,
        date: t("dc:date"),
        text: kids(e, "text:p").map(|p| p.text()).collect::<Vec<_>>().join("\n"),
        x: attr(e, "svg:x").and_then(units::length).unwrap_or(0.0),
        y: attr(e, "svg:y").and_then(units::length).unwrap_or(0.0),
        resolved: attr(e, "loext:resolved") == Some("true"),
        replies: vec![],
        shape: None,
    }
}

/// The colour a fill mostly shows (for automatic text colour): `None` for no fill or a picture.
fn fill_rgb(f: &Fill) -> Option<Rgba> {
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

fn background_rgb(b: &Background) -> Option<Rgba> {
    match b {
        Background::Fill { fill } => fill_rgb(fill),
        Background::Ref { .. } => None,
    }
}

/// The box of a line from its end points (rotated by `draw:transform`, if any).
fn line_xfrm(e: &El) -> Xfrm {
    let len = |k: &str| attr(e, k).and_then(units::length).unwrap_or(0.0);
    let (mut a, mut b) = (kurbo::Point::new(len("svg:x1"), len("svg:y1")), kurbo::Point::new(len("svg:x2"), len("svg:y2")));
    if let Some(m) = attr(e, "draw:transform").and_then(geometry::transform) {
        a = m * a;
        b = m * b;
    }
    let mut x = Xfrm::new(a.x.min(b.x), a.y.min(b.y), (b.x - a.x).abs(), (b.y - a.y).abs());
    x.flip_h = b.x < a.x;
    x.flip_v = b.y < a.y;
    x
}

/// Every row padded to the table's width; columns and rows without a size share what's left of
/// the frame.
fn fit_table(table: &mut Table, x: Xfrm) {
    let ncols = table.rows.iter().map(|r| r.cells.len()).max().unwrap_or(0).max(table.cols.len()).max(1);
    // Columns without a width share what's left of the frame.
    table.cols.resize(ncols, 0.0);
    let known: f64 = table.cols.iter().sum();
    let unknown = table.cols.iter().filter(|w| **w <= 0.0).count();
    if unknown > 0 {
        let each = ((x.w - known).max(0.0) / unknown as f64).max(x.w / ncols as f64 * 0.25).max(10.0);
        for w in &mut table.cols {
            if *w <= 0.0 {
                *w = each;
            }
        }
    }
    let nrows = table.rows.len().max(1);
    let known: f64 = table.rows.iter().map(|r| r.height).sum();
    let unknown = table.rows.iter().filter(|r| r.height <= 0.0).count();
    if unknown > 0 {
        let each = ((x.h - known).max(0.0) / unknown as f64).max(x.h / nrows as f64 * 0.25).max(12.0);
        for r in &mut table.rows {
            if r.height <= 0.0 {
                r.height = each;
            }
        }
    }
    for r in &mut table.rows {
        while r.cells.len() < ncols {
            r.cells.push(Cell::new(""));
        }
    }
}

/// Spans: each anchor cell spans no further than the table, and the cells it covers are merged
/// into it.
fn merge_spans(table: &mut Table) {
    let ncols = table.cols.len();
    // Spans: the anchor cell spans (no further than the table), covered cells are merged into it.
    let nrows = table.rows.len();
    for (ri, r) in table.rows.iter_mut().enumerate() {
        for (ci, c) in r.cells.iter_mut().enumerate() {
            c.grid_span = c.grid_span.min((ncols - ci) as u32).max(1);
            c.row_span = c.row_span.min((nrows - ri) as u32).max(1);
        }
    }
    // In reading order, so a cell an earlier span covers spans nothing itself. Overlapping
    // spans share a budget of visits.
    let mut budget = 4 * MAX_TABLE_CELLS;
    for ri in 0..nrows {
        for ci in 0..ncols {
            let Some(c) = table.cell_mut(ri, ci) else { continue };
            if c.h_merge || c.v_merge {
                (c.grid_span, c.row_span) = (1, 1);
                continue;
            }
            let (gs, rs) = (c.grid_span as usize, c.row_span as usize);
            if gs * rs > budget {
                (c.grid_span, c.row_span) = (1, 1);
                continue;
            }
            budget -= gs * rs;
            for dr in 0..rs {
                for dc in 0..gs {
                    if let Some(c) = table.cell_mut(ri + dr, ci + dc).filter(|_| (dr, dc) != (0, 0))
                        && !(c.h_merge || c.v_merge)
                    {
                        (c.h_merge, c.v_merge) = (dc > 0, dr > 0);
                    }
                }
            }
        }
    }
}

fn ph_kind(class: &str) -> Option<PhType> {
    Some(match class {
        "title" => PhType::Title,
        "subtitle" => PhType::SubTitle,
        "outline" | "text" | "notes" => PhType::Body,
        "graphic" => PhType::Picture,
        "object" => PhType::Obj,
        "chart" => PhType::Chart,
        "table" => PhType::Table,
        "orgchart" => PhType::Diagram,
        "page" => PhType::SlideImage,
        "header" => PhType::Header,
        "footer" => PhType::Footer,
        "date-time" => PhType::Date,
        "page-number" => PhType::SlideNum,
        _ => return None,
    })
}

fn anchor(v: &str) -> Option<Anchor> {
    Some(match v {
        "top" => Anchor::Top,
        "middle" => Anchor::Middle,
        "bottom" => Anchor::Bottom,
        "justify" => Anchor::Justified,
        _ => return None,
    })
}

fn body_props(g: &Props, custom_shape: bool) -> BodyProps {
    let len = |k: &str| g.get(k).and_then(|v| units::length(v));
    let pad = |k: &str| len(k).or_else(|| len("fo:padding"));
    let mut b = BodyProps {
        inset_l: Some(pad("fo:padding-left").unwrap_or(0.0)),
        inset_t: Some(pad("fo:padding-top").unwrap_or(0.0)),
        inset_r: Some(pad("fo:padding-right").unwrap_or(0.0)),
        inset_b: Some(pad("fo:padding-bottom").unwrap_or(0.0)),
        anchor: Some(g.get("draw:textarea-vertical-align").and_then(|v| anchor(v)).unwrap_or(Anchor::Top)),
        // The text block centred in the frame.
        anchor_ctr: Some(g.get("draw:textarea-horizontal-align").map(String::as_str) == Some("center")),
        wrap: Some(if custom_shape {
            g.get("fo:wrap-option").map(String::as_str) != Some("no-wrap")
        } else {
            // Text frames wrap unless they grow to fit their text's width.
            g.get("draw:auto-grow-width").map(String::as_str) != Some("true")
        }),
        autofit: Some(AutoFit::None),
        ..Default::default()
    };
    if g.get("draw:auto-grow-height").map(String::as_str) == Some("true") {
        b.autofit = Some(AutoFit::Shape);
    }
    if let Some(n) = g.get("fo:column-count").and_then(|v| v.parse::<u32>().ok()).filter(|n| *n > 1) {
        b.columns = Some(n.min(16));
        b.col_spacing = g.get("fo:column-gap").and_then(|v| units::length(v));
    }
    if g.get("style:shrink-to-fit").map(String::as_str) == Some("true") || g.get("draw:fit-to-size").is_some_and(|v| v == "shrink-to-fit") {
        b.autofit = Some(AutoFit::Shrink { font_scale: None, line_reduction: 0.0 });
    }
    match g.get("style:writing-mode").map(String::as_str) {
        Some("tb-rl" | "tb") => b.vert = Some(TextDir::EaVertical),
        Some("tb-lr") => b.vert = Some(TextDir::Vertical270),
        _ => {}
    }
    b
}

fn ellipse_geom(e: &El) -> Geom {
    let kind = attr(e, "draw:kind").unwrap_or("full");
    if kind == "full" {
        return Geom::preset("ellipse");
    }
    // Section (pie), cut (chord) and arc: angles counter-clockwise from 3 o'clock, in degrees.
    let a0 = attr(e, "draw:start-angle").and_then(|v| units::angle(v, false)).unwrap_or(0.0);
    let a1 = attr(e, "draw:end-angle").and_then(|v| units::angle(v, false)).unwrap_or(360.0);
    // Presets measure clockwise from 3 o'clock in 1/60000°.
    let to = |a: f64| ((-a).rem_euclid(360.0) * 60_000.0).round();
    let (st, en) = (to(a1), to(a0));
    match kind {
        "section" => Geom::Preset { name: "pie".into(), adj: vec![st, en] },
        "cut" => Geom::Preset { name: "chord".into(), adj: vec![st, en] },
        _ => Geom::Preset { name: "arc".into(), adj: vec![st, en] },
    }
}

fn custom_geom(eg: &geometry::Enhanced) -> Geom {
    let k = eg.kind.as_str();
    if let Some(name) = k.strip_prefix("ooxml-")
        && deckcraft_geom::preset::info(name).is_some()
    {
        return Geom::Preset { name: name.to_string(), adj: eg.modifiers.clone() };
    }
    match k {
        "rectangle" | "mso-spt1" => return Geom::preset("rect"),
        "ellipse" | "mso-spt3" => return Geom::preset("ellipse"),
        _ => {}
    }
    if eg.paths.is_empty() {
        // No path draws nothing (the text, if any, still shows).
        return Geom::Custom { paths: vec![] };
    }
    Geom::Custom { paths: eg.paths.clone() }
}

fn bbox(children: &[Shape]) -> Xfrm {
    let mut r: Option<kurbo::Rect> = None;
    for c in children {
        let Some(x) = c.xfrm else { continue };
        let rect = kurbo::Rect::new(x.x, x.y, x.x + x.w, x.y + x.h);
        r = Some(r.map_or(rect, |a| a.union(rect)));
    }
    let r = r.unwrap_or_default();
    Xfrm::new(r.x0, r.y0, r.width(), r.height())
}

/// A gradient fill from a `draw:gradient`, or (with `opacity_of`) a single colour faded by a
/// `draw:opacity` gradient.
fn gradient(gr: &El, with_alpha: &dyn Fn(Rgba) -> ColorRef, opacity_of: Option<Rgba>) -> Gradient {
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
fn srgb_steps(stops: &[(f64, Rgba)]) -> Vec<(f64, Rgba)> {
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

/// The closest DrawingML pattern to a hatch.
fn hatch_preset(h: Option<&El>) -> String {
    let Some(h) = h else { return "ltDnDiag".into() };
    let style = attr(h, "draw:style").unwrap_or("single");
    let rot = attr(h, "draw:rotation").and_then(|a| units::angle(a, true)).unwrap_or(0.0).rem_euclid(180.0);
    let dist = attr(h, "draw:distance").and_then(units::length).unwrap_or(5.0);
    let dense = dist < 4.0;
    let near = |a: f64| (rot - a).abs() < 22.5 || (rot - a - 180.0).abs() < 22.5;
    let p = match style {
        "double" | "triple" => {
            if near(45.0) || near(135.0) {
                if dense { "smCheck" } else { "openDmnd" }
            } else if dense {
                "smGrid"
            } else {
                "lgGrid"
            }
        }
        _ => {
            if near(0.0) {
                if dense { "narHorz" } else { "ltHorz" }
            } else if near(90.0) {
                if dense { "narVert" } else { "ltVert" }
            } else if near(45.0) {
                if dense { "dkUpDiag" } else { "ltUpDiag" }
            } else if dense {
                "dkDnDiag"
            } else {
                "ltDnDiag"
            }
        }
    };
    p.into()
}

fn dash(d: &El, width: f64) -> Dash {
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

/// A marker's DrawingML end type: by name, else by the shape of its path.
fn marker_kind(name: &str, m: Option<&El>) -> Option<&'static str> {
    let n = name.to_ascii_lowercase();
    let d = m.and_then(|m| attr(m, "svg:d")).unwrap_or("");
    Some(if n.contains("circle") || n.contains("oval") || d.contains('c') || d.contains('C') || d.contains('a') || d.contains('A') {
        "oval"
    } else if n.contains("square") || n.contains("diamond") {
        "diamond"
    } else if n.contains("line") && n.contains("arrow") {
        "arrow"
    } else if n.contains("stealth") || n.contains("concave") {
        "stealth"
    } else {
        "triangle"
    })
}

fn border_line(b: &str) -> Option<Line> {
    // `0.03cm solid #000000`
    if b.trim() == "none" || b.trim().is_empty() {
        return Some(Line::none());
    }
    let mut width = None;
    let mut color = Rgba::BLACK;
    let mut style = "solid";
    for t in b.split_whitespace() {
        if let Some(c) = units::color(t) {
            color = c;
        } else if let Some(w) = units::length(t) {
            width = Some(w);
        } else {
            style = match t {
                "none" | "hidden" => "none",
                "dashed" => "dash",
                "dotted" => "dot",
                _ => style,
            };
        }
    }
    if style == "none" {
        return Some(Line::none());
    }
    let mut l = Line::solid(ColorRef::rgb(color), width.unwrap_or(0.75));
    if style != "solid" {
        l.dash = Some(Dash::Preset { name: style.into() });
    }
    Some(l)
}

fn picture_adjust(g: &Props) -> PictureAdjust {
    let pct = |k: &str| g.get(k).and_then(|v| units::percent(v)).unwrap_or(0.0).clamp(-1.0, 1.0);
    PictureAdjust {
        brightness: pct("draw:luminance"),
        contrast: pct("draw:contrast"),
        grayscale: g.get("draw:color-mode").map(String::as_str) == Some("greyscale"),
        bilevel: (g.get("draw:color-mode").map(String::as_str) == Some("mono")).then_some(0.5),
        ..Default::default()
    }
}

/// The transition a page style describes: SMIL attributes, or the ODF 1.0 `transition-style`.
fn transition(pp: &Props) -> Option<deckcraft_model::Transition> {
    let get = |k: &str| pp.get(k).map(String::as_str);
    let (kind, option) = match (get("smil:type"), get("presentation:transition-style")) {
        (Some(ty), _) => crate::anim::transition_of(ty, get("smil:subtype").unwrap_or(""), get("smil:direction") == Some("reverse")),
        (None, Some(s)) if s.contains("fade") => ("fade", ""),
        (None, Some(s)) if s.starts_with("move") => ("cover", ""),
        (None, Some(s)) if s != "none" => ("fade", ""),
        _ if pp.contains_key("presentation:duration") => ("none", ""),
        _ => return None,
    };
    let duration = get("smil:dur")
        .and_then(units::clock)
        .or_else(|| match get("presentation:transition-speed")? {
            "slow" => Some(1000),
            "fast" => Some(300),
            _ => Some(500),
        })
        .unwrap_or(700);
    let auto = get("presentation:transition-type") == Some("automatic");
    let after = if auto { get("presentation:duration").and_then(duration_iso) } else { None };
    if kind == "none" && after.is_none() {
        return None;
    }
    Some(deckcraft_model::Transition {
        kind: kind.into(),
        option: option.into(),
        duration_ms: duration.min(60_000),
        advance_after_ms: after,
        ..Default::default()
    })
}

/// An ISO 8601 duration (`PT00H00M05S`, `PT5.5S`) in ms.
fn duration_iso(s: &str) -> Option<u32> {
    let t = s.trim().strip_prefix("PT").or_else(|| s.trim().strip_prefix("P0DT"))?;
    let mut total = 0.0;
    let mut num = String::new();
    for c in t.chars() {
        match c {
            'H' => total += num.parse::<f64>().ok()? * 3_600_000.0,
            'M' => total += num.parse::<f64>().ok()? * 60_000.0,
            'S' => total += num.parse::<f64>().ok()? * 1000.0,
            _ => {
                num.push(c);
                continue;
            }
        }
        num.clear();
    }
    (total.is_finite() && total >= 0.0).then(|| total.min(f64::from(u32::MAX)) as u32)
}

fn media_mime(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "wmv" => "video/x-ms-wmv",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" | "oga" => "audio/ogg",
        "m4a" => "audio/mp4",
        "flac" => "audio/flac",
        _ => "application/octet-stream",
    }
}

pub fn mime_for(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "wmf" => "image/x-wmf",
        "emf" => "image/x-emf",
        "svm" => "image/x-svm",
        _ => "application/octet-stream",
    }
}

fn sniff_image(d: &[u8]) -> (&'static str, &'static str) {
    if d.starts_with(b"\x89PNG") {
        ("image/png", "png")
    } else if d.starts_with(&[0xFF, 0xD8]) {
        ("image/jpeg", "jpg")
    } else if d.starts_with(b"GIF8") {
        ("image/gif", "gif")
    } else if d.starts_with(b"<svg") || d.starts_with(b"<?xml") {
        ("image/svg+xml", "svg")
    } else {
        ("application/octet-stream", "bin")
    }
}

/// Standard base64 (white space ignored), bounded.
fn base64(s: &str) -> Option<Vec<u8>> {
    if s.len() > 512 * 1024 * 1024 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            c if c.is_ascii_whitespace() => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clocks_and_durations() {
        assert_eq!(units::clock("1.5s"), Some(1500));
        assert_eq!(units::clock("250ms"), Some(250));
        assert_eq!(units::clock("00:00:02"), Some(2000));
        assert_eq!(units::clock("0.5min"), Some(30_000));
        assert_eq!(units::clock("0.01h"), Some(36_000));
        assert_eq!(duration_iso("PT00H00M05S"), Some(5000));
        assert_eq!(duration_iso("PT1.5S"), Some(1500));
        assert_eq!(duration_iso("bogus"), None);
    }

    #[test]
    fn base64_decodes() {
        assert_eq!(base64("aGVs\nbG8="), Some(b"hello".to_vec()));
        assert_eq!(base64("*"), None);
    }

    #[test]
    fn borders_parse() {
        let l = border_line("0.03cm solid #ff0000").unwrap();
        assert!((l.width.unwrap() - 0.85).abs() < 0.01);
        assert_eq!(border_line("none"), Some(Line::none()));
    }
}
