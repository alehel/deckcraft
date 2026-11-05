//! Reading a presentation: meta data, master pages, pages, shapes, notes.

use std::collections::HashMap;
use std::sync::Arc;

use deckcraft_color::{Rgba, SchemeSlot};
use deckcraft_geom::preset::FillMode;
use deckcraft_model::style::{ColorRef, Fill, PictureFill, PictureMode};
use deckcraft_model::text::{Action, Align, Anchor, AutoFit, BodyProps, LevelStyle, ListStyle, Paragraph, TextBody};
use deckcraft_model::{
    Background, CustomPath, Geom, LayoutId, LayoutType, MediaId, PhType, Placeholder, Presentation, Shape, ShapeId, ShapeKind, Size, Slide, SlideId,
    Xfrm, defaults,
};

use crate::OdpError;
use crate::dom::{El, attr, kid, kids};
use crate::geometry;
use crate::package::Package;
use crate::styles::{Props, Scope, Store, Style};
use crate::text::{self, TextCtx};
use crate::{units, vocab};

/// Deepest element nesting followed (groups in groups …).
pub const MAX_DEPTH: usize = 128;
mod fill;
mod table;

use fill::{background_rgb, fill_rgb, picture_adjust};

/// Most shapes read from one file.
const MAX_SHAPES: usize = 250_000;

/// Most pages read.
const MAX_PAGES: usize = 10_000;

/// Most layouts made from page layouts, and most placeholders they get in all.
const MAX_LAYOUTS_READ: usize = 4096;
const MAX_LAYOUT_BOXES: usize = 65_536;

/// Most comment text read from one file, in bytes; and from one comment paragraph, and the
/// paragraphs of one comment.
const MAX_COMMENT_TEXT: usize = 16 << 20;
const MAX_COMMENT_PARAGRAPH: usize = 1 << 16;
const MAX_COMMENT_PARAGRAPHS: usize = 1024;

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
        .unwrap_or(Size::new(28.0 * units::PT_PER_CM, 15.75 * units::PT_PER_CM));

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
    let mut parts = PartMedia::default();
    for h in hrefs.into_iter().take(256) {
        if let Some(id) = parts.add(&pkg, &mut p, &h, vocab::mime_for(&h)) {
            store.image_bullets.insert(h, id);
        }
    }
    embedded_fonts(&pkg, &store, &mut p, &mut parts);
    let mut r = Reader {
        pkg: &pkg,
        store: std::rc::Rc::new(store),
        p,
        shapes: 0,
        master_index: HashMap::new(),
        layouts_read: HashMap::new(),
        layouts_claimed: std::collections::HashSet::new(),
        our_layouts: HashMap::new(),
        layouts_made: 0,
        layout_boxes: 0,
        page_names: HashMap::new(),
        para_ids: vec![],
        hatch_tiles: HashMap::new(),
        fill_images: HashMap::new(),
        charts: HashMap::new(),
        formula_work: geometry::DOCUMENT_WORK,
        cells_left: table::MAX_DOCUMENT_CELLS,
        text_copies_left: table::MAX_DOCUMENT_COPIED_TEXT,
        plain_left: MAX_COMMENT_TEXT,
        package_media: parts,
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
    r.read_our_layouts();
    let decls = Decls::read(body);
    // The date format, from a current-date declaration in one of our date styles (named after
    // the format).
    if let Some(n) =
        decls.dates.values().filter(|d| d.current).find_map(|d| d.style.as_deref().and_then(|s| text::date_format_of(&r.store, Scope::Content, s)))
    {
        r.p.header_footer.date_format = format!("datetime{n}");
    }
    for (i, page) in kids(body, "draw:page").take(MAX_PAGES).enumerate() {
        if let Some(s) = r.page(page, i, &decls) {
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
    dates: HashMap<String, DateDecl>,
}

/// A `presentation:date-time-decl`.
struct DateDecl {
    /// Shows the current date (else its fixed text).
    current: bool,
    text: String,
    /// Its number style's name.
    style: Option<String>,
}

/// Longest footer, header or date text kept (each page that shows it gets a copy).
const MAX_DECL_TEXT: usize = 1024;

impl Decls {
    fn read(body: &El) -> Self {
        let mut d = Decls::default();
        // A page refers to one of each; thousands are a hostile file's.
        let decls = body.elements().filter(|e| e.name.ends_with("-decl")).take(4096);
        for e in decls {
            let name = attr(e, "presentation:name").unwrap_or_default().to_string();
            let text = || {
                let mut t = text::plain_text(e, MAX_DECL_TEXT * 4);
                if let Some((i, _)) = t.char_indices().nth(MAX_DECL_TEXT) {
                    t.truncate(i);
                }
                t
            };
            match e.name.as_str() {
                "presentation:footer-decl" => {
                    d.footers.insert(name, text());
                }
                "presentation:header-decl" => {
                    d.headers.insert(name, text());
                }
                "presentation:date-time-decl" => {
                    let style = attr(e, "style:data-style-name").map(str::to_string);
                    d.dates.insert(name, DateDecl { current: attr(e, "presentation:source") == Some("current-date"), text: text(), style });
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
    /// The layout each other app's page layout became, per master (index) and name.
    layouts_read: HashMap<(usize, String), LayoutId>,
    /// The defaults other apps' described page layouts took (each is taken once).
    layouts_claimed: std::collections::HashSet<LayoutId>,
    /// The layout each page layout of ours became, by name (which names its master).
    our_layouts: HashMap<String, LayoutId>,
    /// Layouts made from page layouts, and the placeholders they had (against
    /// [`MAX_LAYOUTS_READ`] and [`MAX_LAYOUT_BOXES`]).
    layouts_made: usize,
    layout_boxes: usize,
    /// Page names (`draw:name`) → slide ids, for `#name` links.
    page_names: HashMap<String, SlideId>,
    /// The ids of the paragraphs of the text body read last.
    para_ids: Vec<Option<String>>,
    /// What many shapes may share, worked out once: hatch tiles (by hatch) and fill images and
    /// embedded charts (by name).
    hatch_tiles: HashMap<String, Option<(MediaId, f64)>>,
    fill_images: HashMap<String, Option<MediaId>>,
    charts: HashMap<String, Option<deckcraft_model::chart::Chart>>,
    /// Shape-formula steps the document has left ([`geometry::DOCUMENT_WORK`] in all).
    formula_work: usize,
    /// Table cells the document has left ([`table::MAX_DOCUMENT_CELLS`] in all).
    cells_left: usize,
    /// Text that copies of repeated table cells and rows may still take
    /// ([`table::MAX_DOCUMENT_COPIED_TEXT`] in all), in bytes.
    text_copies_left: usize,
    /// Comment text the document has left, in bytes ([`MAX_COMMENT_TEXT`] in all).
    plain_left: usize,
    package_media: PartMedia,
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
        let user = |n: &str| kids(m, "meta:user-defined").find(|u| attr(u, "meta:name") == Some(n)).map(|u| u.text()).unwrap_or_default();
        props.category = user("Category");
        props.company = user("Company");
    }

    fn settings(&mut self, s: &El) {
        if attr(s, "presentation:endless") == Some("true") {
            self.p.show.loop_until_esc = true;
        }
        if attr(s, "presentation:show-type") == Some("window") || attr(s, "presentation:full-screen") == Some("false") {
            self.p.show.show_type = deckcraft_model::ShowSettings::BROWSED.into();
        }
        if attr(s, "presentation:force-manual") == Some("true") {
            self.p.show.use_timings = false;
        }
        if attr(s, "presentation:animations") == Some("disabled") {
            self.p.show.without_animation = true;
        }
        // The show starts at a page: a range from it to the end.
        let start =
            attr(s, "presentation:start-page").and_then(|n| self.page_names.get(n)).and_then(|id| self.p.slides.iter().position(|sl| sl.id == *id));
        if let (Some(i), Ok(n)) = (start, u32::try_from(self.p.slides.len())) {
            self.p.show.range = Some((i as u32 + 1, n));
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
        master.name = attr(m, "style:display-name").map_or_else(|| unescape_style_name(&name), str::to_string);
        let page_style = attr(m, "draw:style-name").map(|n| self.store.resolve(scope, "drawing-page", Some(n))).unwrap_or_default();
        // The page style's fill, else the master's background presentation style (older files).
        let bg_style = self.store.chain(scope, "presentation", &format!("{name}-background"));
        master.background = Some(
            self.background(&page_style.page)
                .or_else(|| self.background(&bg_style.graphic))
                .unwrap_or(Background::Fill { fill: Fill::solid(ColorRef::rgb(Rgba::WHITE)) }),
        );
        // Text styles: the title level, and the nine outline levels.
        master.title_style = ListStyle::default();
        master.title_style.set(0, self.level_style(&title_style, scope, 0, false));
        master.body_style = ListStyle::default();
        // The same levels an outline placeholder on a slide reads.
        for (lv, s) in self.store.outline_levels(scope, &name, None, &outline_style).iter().enumerate().take(9) {
            let lv = lv as u8;
            master.body_style.set(lv, self.level_style(s, scope, lv, true));
        }
        let bg = master.background.as_ref().and_then(background_rgb).unwrap_or(Rgba::WHITE);
        let ctx = Ctx { scope, master: name.clone(), bg, title_slide: false };
        let mut decorations = vec![];
        for e in m.elements() {
            let class = attr(e, "presentation:class");
            let kind = class.and_then(ph_kind);
            if let Some(kind) = kind {
                // A master placeholder: move the default one to where this master has it.
                let x = shape_xfrm(e);
                if let Some(target) =
                    master.shapes.iter_mut().find(|s| s.ph_type() == Some(kind) || (kind == PhType::SubTitle && s.ph_type() == Some(PhType::Body)))
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

    /// The `index`th page (from 0) of the document.
    fn page(&mut self, page: &El, index: usize, decls: &Decls) -> Option<Slide> {
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
        // The page's own background (white when it hides the master's), and the colour shapes'
        // automatic text is judged against.
        let page_bg = if pp.get("presentation:background-visible").map(String::as_str) == Some("false") {
            Some(Background::Fill { fill: Fill::solid(ColorRef::rgb(Rgba::WHITE)) })
        } else {
            self.background(pp)
        };
        let bg = page_bg.as_ref().and_then(background_rgb).or(master_bg).unwrap_or(Rgba::WHITE);
        // The page's layout: the one its page layout became, else the master's of the kind its
        // placeholders suggest.
        let store = std::rc::Rc::clone(&self.store);
        // Ours were read up front (they name their master); others belong to the page's.
        let read = attr(page, "presentation:presentation-page-layout-name").and_then(|n| match self.our_layouts.get(n) {
            Some(id) => Some(*id),
            None => store.presentation_layout(n).and_then(|el| self.layout_for(mi, n, el)),
        });
        let layout = read.or_else(|| {
            let kind = guess_layout(page.elements().filter_map(|e| attr(e, "presentation:class")).filter_map(ph_kind));
            let layouts = &self.p.masters.get(mi)?.layouts;
            layouts.iter().find(|l| l.kind == kind).or(layouts.first()).map(|l| l.id)
        });
        let kind = layout.and_then(|id| self.p.masters.get(mi)?.layout(id)).map(|l| l.kind);
        let title_slide = kind == Some(LayoutType::Title);
        let ctx = Ctx { scope: Scope::Content, master: master_name, bg, title_slide };
        // `pageN` for the Nth page is the name a writer gives a slide that has none.
        let unnamed = format!("page{}", index + 1);
        let name = attr(page, "draw:name").filter(|n| *n != unnamed).unwrap_or("").to_string();
        let mut slide = Slide { id, name, background: page_bg, ..Default::default() };
        if let Some(n) = attr(page, "draw:name") {
            self.page_names.entry(n.to_string()).or_insert(id);
        }
        if pp.get("presentation:background-objects-visible").map(String::as_str) == Some("false") {
            slide.show_master_shapes = false;
        }
        if pp.get("presentation:visibility").map(String::as_str) == Some("hidden") {
            slide.hidden = true;
        }
        slide.transition = crate::anim::page_transition(pp);
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
                        slide.comments.push(comment(e, &mut self.plain_left));
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
        let glue = std::mem::take(&mut self.glue);
        if !glue.is_empty() {
            // Every shape by id, once (not a search of the page per connector).
            let mut by_id: HashMap<ShapeId, &Shape> = HashMap::new();
            deckcraft_model::walk(&shapes, &mut |s, _| {
                by_id.insert(s.id, s);
            });
            let resolve = |g: &Option<(String, u32)>| {
                let (id, pt) = g.as_ref()?;
                let (sid, _) = self.targets.get(id)?;
                // The target's connection site nearest LibreOffice's glue point.
                Some((*sid, geometry::site_of_glue_point(by_id.get(sid).copied(), *pt)))
            };
            let ends: HashMap<ShapeId, Ends> = glue.iter().map(|(cid, start, end)| (*cid, (resolve(start), resolve(end)))).collect();
            set_connector_ends(&mut shapes, &ends, 0);
        }
        if let Some(t) = timing {
            let page_id = attr(page, "xml:id").or_else(|| attr(page, "draw:id"));
            let tm = crate::anim::read(t, page_id, &self.targets);
            slide.animations = tm.animations;
            // The effect is the timing's; how the slide advances, the page style's.
            if let Some(tr) = tm.transition {
                let advance_after_ms = slide.transition.as_ref().and_then(|t| t.advance_after_ms);
                slide.transition = Some(deckcraft_model::Transition { advance_after_ms, ..tr });
            }
        }
        // Footer fields the page shows, from its master's placeholders.
        self.page_footers(mi, pp, page, decls, &mut shapes);
        slide.layout = layout.unwrap_or_default();
        slide.shapes = shapes;
        Some(slide)
    }

    /// The page layouts of ours (`obj.Office`, see `write::PageLayouts`): each master they name
    /// has them as its layouts, used or not, in file order; its built defaults only lend each
    /// kind its placeholders. Past the budgets ([`MAX_LAYOUTS_READ`], [`MAX_LAYOUT_BOXES`]) a
    /// layout is its kind's default.
    fn read_our_layouts(&mut self) {
        let store = std::rc::Rc::clone(&self.store);
        let mut built: Vec<(usize, Vec<deckcraft_model::Layout>)> = vec![];
        for (name, el) in &store.presentation_layouts {
            let Some(mi) = name.split_once('.').and_then(|(_, page)| self.master_index.get(page).copied()) else { continue };
            let Some(master) = self.p.masters.get(mi).cloned() else { continue };
            let kind = page_layout_kind(el);
            let template = master.layouts.iter().find(|l| l.kind == kind).or_else(|| master.layouts.iter().find(|l| l.kind == LayoutType::Blank));
            let shown = attr(el, "style:display-name").unwrap_or(name);
            let boxes = self.boxes_of(el);
            let Some(layout) = self.new_layout(template, kind, shown, boxes, &master) else { continue };
            self.our_layouts.insert(name.clone(), layout.id);
            match built.iter_mut().find(|(m, _)| *m == mi) {
                Some((_, ls)) => ls.push(layout),
                None => built.push((mi, vec![layout])),
            }
        }
        for (mi, layouts) in built {
            if let Some(m) = self.p.masters.get_mut(mi) {
                Arc::make_mut(m).layouts = layouts;
            }
        }
    }

    /// A page layout's placeholders (the footers aside), within the budget: none past it.
    fn boxes_of(&mut self, el: &El) -> Option<Vec<(PhType, Xfrm)>> {
        if self.layouts_made >= MAX_LAYOUTS_READ || self.layout_boxes >= MAX_LAYOUT_BOXES {
            return None;
        }
        let boxes: Vec<(PhType, Xfrm)> = kids(el, "presentation:placeholder")
            .take(64.min(MAX_LAYOUT_BOXES - self.layout_boxes))
            .filter_map(|p| Some((ph_kind(attr(p, "presentation:object")?)?, shape_xfrm(p))))
            .filter(|(k, _)| !k.is_footer_kind())
            .collect();
        self.layouts_made += 1;
        self.layout_boxes += boxes.len();
        Some(boxes)
    }

    /// A layout of `kind` named `name`: a copy of `template` (with ids of its own) with the
    /// placeholders `boxes` puts where (all of the template's when there are none to say).
    fn new_layout(
        &mut self,
        template: Option<&deckcraft_model::Layout>,
        kind: LayoutType,
        name: &str,
        boxes: Option<Vec<(PhType, Xfrm)>>,
        master: &deckcraft_model::Master,
    ) -> Option<deckcraft_model::Layout> {
        let mut l = template.cloned().unwrap_or_default();
        (l.id, l.kind, l.name) = (LayoutId(self.p.alloc_id()?), kind, name.to_string());
        for s in &mut l.shapes {
            s.id = self.id();
        }
        if let Some(boxes) = boxes {
            self.set_placeholders(&mut l, boxes, master);
        }
        Some(l)
    }

    /// Give layout `l` the placeholders `boxes` puts where, the footers aside: each keeps the one
    /// of its kind it had (in order), new ones the master's look. Its own shapes come first, then
    /// these, then the footers.
    fn set_placeholders(&mut self, l: &mut deckcraft_model::Layout, boxes: Vec<(PhType, Xfrm)>, master: &deckcraft_model::Master) {
        let (mut old, mut footers) = (Vec::new(), Vec::new());
        l.shapes.retain(|s| match s.ph_type() {
            Some(k) if k.is_footer_kind() => {
                footers.push(s.clone());
                false
            }
            Some(_) => {
                old.push(s.clone());
                false
            }
            None => true,
        });
        // Titles are titles, and content placeholders (`outline`) bodies or objects.
        let same = |a: PhType, b: PhType| {
            a == b || a.is_title() && b.is_title() || matches!((a, b), (PhType::Body | PhType::Obj, PhType::Body | PhType::Obj))
        };
        for (i, (k, x)) in boxes.into_iter().enumerate() {
            let mut s = match old.iter().position(|s| s.ph_type().is_some_and(|o| same(o, k))) {
                Some(j) => old.remove(j),
                None => {
                    let mut s = master.shapes.iter().find(|s| s.ph_type().is_some_and(|o| same(o, k))).cloned().unwrap_or_default();
                    s.id = self.id();
                    s.ph = Some(Placeholder { kind: k, idx: u32::try_from(i).unwrap_or(0), ..Default::default() });
                    s
                }
            };
            s.xfrm = Some(x);
            l.shapes.push(s);
        }
        l.shapes.append(&mut footers);
    }

    /// The layout another app's page layout (named `name`) becomes on master `mi`. A described
    /// one (with a name to show) takes the master's default of its kind and name if no other did,
    /// else is a new layout of its kind, with its placeholders where it puts them. A generic one
    /// (LibreOffice's own: arrangements boxed for no page in particular) only picks the master's
    /// layout of its kind, as does any past the budgets.
    fn layout_for(&mut self, mi: usize, name: &str, el: &El) -> Option<LayoutId> {
        if let Some(id) = self.layouts_read.get(&(mi, name.to_string())) {
            return Some(*id);
        }
        let kind = page_layout_kind(el);
        let master = self.p.masters.get(mi).cloned()?;
        let of_kind = master.layouts.iter().find(|l| l.kind == kind).or(master.layouts.first());
        let described = attr(el, "style:display-name").and_then(|shown| Some((shown, self.boxes_of(el)?)));
        let id = match described {
            None => of_kind?.id,
            Some((shown, boxes)) => {
                let claim = master.layouts.iter().position(|l| l.kind == kind && l.name == shown && !self.layouts_claimed.contains(&l.id));
                let id = match claim {
                    Some(at) => {
                        let mut l = master.layouts.get(at)?.clone();
                        self.set_placeholders(&mut l, boxes, &master);
                        let id = l.id;
                        *Arc::make_mut(self.p.masters.get_mut(mi)?).layouts.get_mut(at)? = l;
                        id
                    }
                    None => {
                        let template = of_kind.filter(|l| l.kind == kind).or_else(|| master.layouts.iter().find(|l| l.kind == LayoutType::Blank));
                        let l = self.new_layout(template, kind, shown, Some(boxes), &master)?;
                        let id = l.id;
                        Arc::make_mut(self.p.masters.get_mut(mi)?).layouts.push(l);
                        id
                    }
                };
                self.layouts_claimed.insert(id);
                id
            }
        };
        self.layouts_read.insert((mi, name.to_string()), id);
        Some(id)
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
            && let Some(d) = attr(page, "presentation:use-date-time-name").and_then(|n| decls.dates.get(n))
        {
            self.p.header_footer.date = true;
            if d.current {
                add(PhType::Date, None, self);
                // The master's field (`presentation:date-time`) has no format: the declaration's.
                let format = d.style.as_deref().and_then(|s| text::date_format_of(&self.store, Scope::Content, s));
                if let (Some(n), Some(body)) = (format, shapes.last_mut().filter(|s| s.ph_type() == Some(PhType::Date)).and_then(|s| s.text.as_mut()))
                {
                    for r in body.paragraphs.iter_mut().flat_map(|p| p.runs.iter_mut()) {
                        if let deckcraft_model::text::RunKind::Field { field } = &mut r.kind
                            && field.starts_with("datetime")
                        {
                            *field = format!("datetime{n}");
                        }
                    }
                }
            } else if !d.text.is_empty() {
                add(PhType::Date, Some(d.text.clone()), self);
                self.p.header_footer.date_text.clone_from(&d.text);
            }
        }
    }

    /// `#page-name` links become slide links.
    fn resolve_links(&mut self) {
        let names = std::mem::take(&mut self.page_names);
        let fix = |l: &mut Option<deckcraft_model::text::Hyperlink>| {
            let Some(h) = l else { return };
            if let Action::Url { url } = &h.action
                && let Some(name) = url.strip_prefix('#')
            {
                if let Some(jump) = vocab::read(vocab::JUMPS, url) {
                    h.action = jump;
                    return;
                }
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
        // Ids of a body read for a shape that was then dropped don't belong to this one.
        self.para_ids.clear();
        let (style, presentation) = self.shape_style(e, ctx);
        let g = &style.graphic;
        let mut s = Shape {
            id: self.id(),
            name: attr(e, "draw:name").unwrap_or("").to_string(),
            xfrm: Some(shape_xfrm(e)),
            descr: kid(e, "svg:desc").map(|d| d.text()).unwrap_or_default(),
            title: kid(e, "svg:title").map(|d| d.text()).unwrap_or_default(),
            decorative: g.get("loext:decorative").map(String::as_str) == Some("true"),
            // LibreOffice re-saves `draw:display` as `drawooo:display`.
            hidden: ["draw:display", "presentation:display", "drawooo:display"].iter().any(|k| attr(e, k) == Some("none")),
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
            "draw:connector" => {
                // Glued to the shapes at its ends, whatever its route.
                s.kind = ShapeKind::Connector { start: None, end: None };
                s.fill = Some(Fill::None);
                let glue = |k: &str, g: &str| attr(e, k).map(|id| (id.to_string(), attr(e, g).and_then(|v| v.parse::<u32>().ok()).unwrap_or(0)));
                self.glue.push((s.id, glue("draw:start-shape", "draw:start-glue-point"), glue("draw:end-shape", "draw:end-glue-point")));
                let d = attr(e, "svg:d").filter(|_| attr(e, "draw:type") != Some("line"));
                if d.is_some() {
                    // A routed path: always in page coordinates (a connector has no box of its
                    // own, whatever view box it names).
                    return self.connector_from_page_path(s, e);
                }
                // Straight: its end points.
                s.xfrm = Some(line_xfrm(e));
                s.geom = Geom::preset("straightConnector1");
                s.text = Some(self.text_body(&tcx, e, g));
            }
            "draw:polyline" | "draw:polygon" | "draw:path" => {
                let x = s.xfrm.unwrap_or_default();
                let vb = geometry::view_box(attr(e, "svg:viewBox")).unwrap_or([0.0, 0.0, x.w.max(1e-6), x.h.max(1e-6)]);
                let path = match name {
                    "draw:polyline" => attr(e, "draw:points").and_then(|p| geometry::points_path(p, vb, false)),
                    "draw:polygon" => attr(e, "draw:points").and_then(|p| geometry::points_path(p, vb, true)),
                    _ => attr(e, "svg:d").and_then(|d| geometry::svg_path(d, vb, true)),
                };
                if name == "draw:polyline" {
                    s.fill = Some(Fill::None);
                }
                s.geom = Geom::Custom { paths: vec![path?] };
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
        // Paragraph ids the text body had, for effects that animate one paragraph.
        for (i, id) in std::mem::take(&mut self.para_ids).into_iter().enumerate() {
            if let Some(id) = id {
                self.targets.insert(id, (s.id, Some(i as u32)));
            }
        }
        // Empty text bodies are dropped from pictures, tables and the like; an autoshape keeps
        // its (empty) body to type into, as a placeholder does.
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
        // LibreOffice's generic media type (and none) leaves it to the file's extension.
        let mime = attr(pl, "draw:mime-type").filter(|m| *m != "application/vnd.sun.star.media").unwrap_or_else(|| vocab::mime_for(href)).to_string();
        let name = part_name(href).to_string();
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
        // Video unless the type (or, without one, the file's extension) says audio.
        let video = mime.starts_with("video/") || !mime.starts_with("audio/") && !vocab::mime_for(&name).starts_with("audio/");
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
    fn object_chart(&mut self, o: &El) -> Option<deckcraft_model::chart::Chart> {
        let href = attr(o, "xlink:href")?.trim_start_matches("./").trim_end_matches('/');
        if href.is_empty() {
            // An inline chart document (flat files), read in place.
            return crate::chart::read(kid(o, "office:document")?);
        }
        // A package's chart document is read once, however many frames show it.
        if let Some(c) = self.charts.get(href) {
            return c.clone();
        }
        let chart = self.pkg.xml(&format!("{href}/content.xml")).and_then(|d| crate::chart::read(&d));
        self.charts.insert(href.to_string(), chart.clone());
        chart
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
        let Some(eg) = kid(e, "draw:enhanced-geometry").and_then(|eg| geometry::enhanced(eg, x.w, x.h, &mut self.formula_work)) else {
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
        let d = attr(e, "svg:d").filter(|d| d.len() <= geometry::MAX_PATH_TEXT)?;
        // svg:d without a view box is in 1/100 mm.
        let mut p = kurbo::BezPath::from_svg(d).ok()?;
        p.apply_affine(kurbo::Affine::scale(1.0 / units::HMM_PER_PT));
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
        Some(text::link(action))
    }

    /// A shape's text; the paragraphs' ids wait in `para_ids` for the shape to register them.
    fn text_body(&mut self, cx: &TextCtx, container: &El, g: &Props) -> TextBody {
        let (mut paragraphs, ids): (Vec<Paragraph>, Vec<Option<String>>) = text::paragraphs_with_ids(cx, container).into_iter().unzip();
        self.para_ids = ids;
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
        self.package_media.add(self.pkg, &mut self.p, href, mime)
    }

    fn image_media(&mut self, img: &El) -> Option<MediaId> {
        if let Some(href) = attr(img, "xlink:href").filter(|h| !h.is_empty() && !h.contains("://")) {
            let mime = attr(img, "draw:mime-type").map(str::to_string).unwrap_or_else(|| vocab::mime_for(href).to_string());
            return self.package_media(href, &mime);
        }
        let b64 = kid(img, "office:binary-data")?.text();
        let data = crate::package::base64(&b64)?;
        let mime = crate::package::sniff_image(&data);
        self.package_media.add_bytes(&mut self.p, &format!("image.{}", mime.1), mime.0, data)
    }

    // -----------------------------------------------------------------------------------------
    // Fills, lines, effects
}

/// Package parts already added as media, by reference: each part is copied once, whether it's
/// a picture, a bullet, a clip or a font.
#[derive(Default)]
struct PartMedia {
    by_part: HashMap<String, MediaId>,
    /// Everything added, by size and content hash: an identical picture is stored once without
    /// comparing it to every other one.
    by_content: HashMap<(usize, u64), Vec<MediaId>>,
}

impl PartMedia {
    fn add(&mut self, pkg: &Package, p: &mut Presentation, href: &str, mime: &str) -> Option<MediaId> {
        // By the part's own name, so differently spelled references share it.
        let (key, data) = pkg.part(href)?;
        if let Some(id) = self.by_part.get(key) {
            return Some(*id);
        }
        let id = self.add_bytes(p, part_name(key), mime, data.to_vec())?;
        self.by_part.insert(key.to_string(), id);
        Some(id)
    }

    /// Media from anywhere (a package part, inline data, a drawn tile), once per content.
    fn add_bytes(&mut self, p: &mut Presentation, name: &str, mime: &str, data: Vec<u8>) -> Option<MediaId> {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        data.hash(&mut h);
        let key = (data.len(), h.finish());
        let same = self.by_content.get(&key).and_then(|ids| ids.iter().copied().find(|id| p.media(*id).is_some_and(|m| *m.data == data)));
        if let Some(id) = same {
            return Some(id);
        }
        let id = p.add_new_media(name, mime, data)?;
        self.by_content.entry(key).or_default().push(id);
        Some(id)
    }
}

/// A connector's glued ends: (shape, connection site) at its start and its end.
type Ends = (Option<(ShapeId, u32)>, Option<(ShapeId, u32)>);

/// Set the ends of the connectors among `shapes` (and in their groups) that `ends` names.
fn set_connector_ends(shapes: &mut [Shape], ends: &HashMap<ShapeId, Ends>, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    for s in shapes {
        match &mut s.kind {
            ShapeKind::Connector { start, end } => {
                if let Some((a, b)) = ends.get(&s.id) {
                    (*start, *end) = (*a, *b);
                }
            }
            ShapeKind::Group { children, .. } => set_connector_ends(children, ends, depth + 1),
            _ => {}
        }
    }
}

/// A style name with its `_xx_` escapes (hexadecimal character codes, `_20_` a space) undone.
fn unescape_style_name(name: &str) -> String {
    let mut out = String::new();
    let mut rest = name;
    while let Some(i) = rest.find('_') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let code = after.find('_').filter(|n| (1..=6).contains(n)).and_then(|n| {
            let c = u32::from_str_radix(&after[..n], 16).ok().and_then(char::from_u32)?;
            Some((c, n))
        });
        match code {
            Some((c, n)) => {
                out.push(c);
                rest = &after[n + 1..];
            }
            None => {
                out.push('_');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// A part's file name (`Pictures/a.png` → `a.png`).
fn part_name(href: &str) -> &str {
    href.rsplit('/').next().unwrap_or(href)
}

/// Fonts embedded in the package (`svg:font-face-uri` in the font face declarations).
fn embedded_fonts(pkg: &Package, store: &Store, p: &mut Presentation, parts: &mut PartMedia) {
    for f in &store.font_faces {
        let family = attr(f, "svg:font-family").map(crate::styles::unquote).or_else(|| attr(f, "style:name").map(str::to_string)).unwrap_or_default();
        if family.is_empty() {
            continue;
        }
        let bold = attr(f, "svg:font-weight").is_some_and(|w| w == "bold" || w.parse::<u32>().is_ok_and(|n| n >= 600));
        let italic = attr(f, "svg:font-style").is_some_and(|s| s == "italic" || s == "oblique");
        let Some(uri) = crate::dom::find(f, "svg:font-face-uri").and_then(|u| attr(u, "xlink:href")) else { continue };
        if pkg.get(uri).and_then(deckcraft_fonts::embedded_sfnt).is_none() {
            continue;
        }
        let Some(id) = parts.add(pkg, p, uri, "application/x-font-ttf") else { continue };
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
/// A comment; its text from what is left of the document's `budget` for such texts.
fn comment(e: &El, budget: &mut usize) -> deckcraft_model::Comment {
    let t = |n: &str| kid(e, n).map(|c| c.text()).unwrap_or_default();
    let initials = {
        let i = t("meta:creator-initials");
        if i.is_empty() { t("loext:sender-initials") } else { i }
    };
    deckcraft_model::Comment {
        author: t("dc:creator"),
        initials,
        date: t("dc:date"),
        text: kids(e, "text:p")
            .take(MAX_COMMENT_PARAGRAPHS)
            .map(|p| {
                let t = text::plain_text(p, (*budget).min(MAX_COMMENT_PARAGRAPH));
                *budget = budget.saturating_sub(t.len() + 1);
                t
            })
            .collect::<Vec<_>>()
            .join("\n"),
        x: attr(e, "svg:x").and_then(units::length).unwrap_or(0.0),
        y: attr(e, "svg:y").and_then(units::length).unwrap_or(0.0),
        resolved: attr(e, "loext:resolved") == Some("true"),
        replies: vec![],
        shape: None,
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

/// A page layout's kind: by its name, as we write it (`obj.Office`, `secHead-2.Office`, see
/// `write::PageLayouts`), else guessed from its placeholders.
fn page_layout_kind(el: &El) -> LayoutType {
    let token = attr(el, "style:name").unwrap_or_default().split(['.', '-', '_']).next().unwrap_or_default();
    let kind = LayoutType::from_xml(token);
    if kind.xml() == token {
        kind
    } else {
        guess_layout(kids(el, "presentation:placeholder").filter_map(|p| attr(p, "presentation:object")).filter_map(ph_kind))
    }
}

/// The layout a page's placeholders suggest: a subtitle makes a title slide, then the content
/// placeholders beside a title count.
fn guess_layout(placeholders: impl Iterator<Item = PhType>) -> LayoutType {
    let (mut title, mut subtitle, mut bodies) = (false, false, 0);
    for k in placeholders {
        match k {
            PhType::CtrTitle | PhType::SubTitle => subtitle = true,
            PhType::Title => title = true,
            PhType::Body | PhType::Obj | PhType::Chart | PhType::Table | PhType::Picture => bodies += 1,
            _ => {}
        }
    }
    match (subtitle, title, bodies) {
        (true, ..) => LayoutType::Title,
        (_, true, 2..) => LayoutType::TwoContent,
        (_, true, 1) => LayoutType::TitleAndContent,
        (_, true, 0) => LayoutType::TitleOnly,
        _ => LayoutType::Blank,
    }
}

fn ph_kind(class: &str) -> Option<PhType> {
    vocab::read(vocab::PRESENTATION_CLASSES, class)
}

fn anchor(v: &str) -> Option<Anchor> {
    vocab::read(vocab::ANCHORS, v)
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
    // LibreOffice's own modes (rotated text) first, then ODF's.
    let loext = g.get("loext:writing-mode").and_then(|m| vocab::read(vocab::LOEXT_WRITING_MODES, m));
    if let Some(v) = loext.or_else(|| g.get("style:writing-mode").and_then(|m| vocab::read(vocab::WRITING_MODES, m))) {
        b.vert = Some(v);
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
    // No path draws nothing (the text, if any, still shows).
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

/// A marker's DrawingML end type: by name, else by the shape of its path.
/// A shape's box: `svg:x/y/width/height`, placed by its `draw:transform` when it has one.
fn shape_xfrm(e: &El) -> Xfrm {
    let len = |k: &str| attr(e, k).and_then(units::length);
    let w = len("svg:width").unwrap_or(0.0).max(0.0);
    let h = len("svg:height").unwrap_or(0.0).max(0.0);
    match attr(e, "draw:transform").and_then(geometry::transform) {
        Some(m) => geometry::placed_box(m, w, h),
        None => Xfrm::new(len("svg:x").unwrap_or(0.0), len("svg:y").unwrap_or(0.0), w, h),
    }
}
