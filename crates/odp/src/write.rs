//! Writing a package: `mimetype` (stored, first), `META-INF/manifest.xml`, `content.xml`,
//! `styles.xml`, `meta.xml`, `settings.xml` and the media.
//!
//! The model inherits formatting the way PowerPoint does (placeholder → layout → master →
//! theme); OpenDocument has its own inheritance. Rather than map one onto the other, every shape is
//! written with its effective formatting resolved, in automatic styles, so it looks the same in any
//! reader. OpenDocument has no layouts: each layout a slide uses becomes a master page holding
//! the master's and the layout's graphics.

mod chart;
mod shapes;
mod text;

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write as _;

use deckcraft_model::resolve::{self, Ctx};
use deckcraft_model::{LayoutId, PhType, Presentation, ShapeKind, Slide};

use crate::OdpError;
use crate::dom::esc;
use crate::units::{cm, trim};

pub(crate) const NS: &str = concat!(
    r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" "#,
    r#"xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" "#,
    r#"xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" "#,
    r#"xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" "#,
    r#"xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" "#,
    r#"xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" "#,
    r#"xmlns:xlink="http://www.w3.org/1999/xlink" "#,
    r#"xmlns:dc="http://purl.org/dc/elements/1.1/" "#,
    r#"xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0" "#,
    r#"xmlns:number="urn:oasis:names:tc:opendocument:xmlns:datastyle:1.0" "#,
    r#"xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" "#,
    r#"xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" "#,
    r#"xmlns:chart="urn:oasis:names:tc:opendocument:xmlns:chart:1.0" "#,
    r#"xmlns:dr3d="urn:oasis:names:tc:opendocument:xmlns:dr3d:1.0" "#,
    r#"xmlns:smil="urn:oasis:names:tc:opendocument:xmlns:smil-compatible:1.0" "#,
    r#"xmlns:anim="urn:oasis:names:tc:opendocument:xmlns:animation:1.0" "#,
    r#"xmlns:script="urn:oasis:names:tc:opendocument:xmlns:script:1.0" "#,
    r#"xmlns:dom="http://www.w3.org/2001/xml-events" "#,
    r#"xmlns:loext="urn:org:documentfoundation:names:experimental:office:xmlns:loext:1.0" "#,
    r#"xmlns:officeooo="http://openoffice.org/2009/office" "#,
    r#"office:version="1.3""#
);

/// Automatic styles of one document, deduplicated by content.
#[derive(Default)]
pub(crate) struct Auto {
    prefix: &'static str,
    map: HashMap<(String, String), String>,
    pub out: String,
    n: HashMap<&'static str, usize>,
}

impl Auto {
    fn new(prefix: &'static str) -> Self {
        Auto { prefix, ..Default::default() }
    }

    /// The name of the style `key` describes: the one written before, or a new one numbered by
    /// `tag` whose XML `xml` writes.
    fn intern(&mut self, key: (String, String), tag: &'static str, xml: impl FnOnce(&str) -> String) -> String {
        if let Some(n) = self.map.get(&key) {
            return n.clone();
        }
        let c = self.n.entry(tag).or_insert(0);
        *c += 1;
        let name = format!("{}{tag}{c}", self.prefix);
        self.out.push_str(&xml(&name));
        self.map.insert(key, name.clone());
        name
    }

    /// A `style:style` of `family` with these property elements; `tag` names it (`gr`, `P`…).
    pub fn style(&mut self, family: &str, tag: &'static str, parent: Option<&str>, body: &str) -> String {
        let key = (format!("{family}|{}", parent.unwrap_or("")), body.to_string());
        let parent = parent.map(|p| format!(r#" style:parent-style-name="{}""#, esc(p))).unwrap_or_default();
        self.intern(key, tag, |name| format!(r#"<style:style style:name="{name}" style:family="{family}"{parent}>{body}</style:style>"#))
    }

    /// A `text:list-style` with these level elements.
    pub fn list(&mut self, levels: &str) -> String {
        self.intern(("list".to_string(), levels.to_string()), "L", |name| {
            format!(r#"<text:list-style style:name="{name}">{levels}</text:list-style>"#)
        })
    }
}

/// Named drawing definitions (gradients, hatches, markers, dashes, fill images) in `styles.xml`.
#[derive(Default)]
pub(crate) struct Named {
    map: HashMap<String, String>,
    pub out: String,
    n: usize,
}

impl Named {
    /// `el` is the element name, `body` its attributes after `draw:name` (and any children).
    pub fn def(&mut self, el: &str, prefix: &str, attrs: &str, children: &str) -> String {
        let key = format!("{el}|{attrs}|{children}");
        if let Some(n) = self.map.get(&key) {
            return n.clone();
        }
        self.n += 1;
        let name = format!("{prefix}_{}", self.n);
        if children.is_empty() {
            let _ = write!(self.out, r#"<{el} draw:name="{name}" {attrs}/>"#);
        } else {
            let _ = write!(self.out, r#"<{el} draw:name="{name}" {attrs}>{children}</{el}>"#);
        }
        self.map.insert(key, name.clone());
        name
    }
}

/// Everything the writer collects while walking the presentation.
pub(crate) struct Writer<'p> {
    pub p: &'p Presentation,
    pub named: Named,
    /// Media written to the package: media id → path in the package.
    pub media: HashMap<u32, String>,
    pub files: Vec<(String, String, std::sync::Arc<Vec<u8>>)>,
    pub fonts: std::collections::BTreeSet<String>,
    /// Slide id → page name, for links.
    pub page_names: HashMap<u32, String>,
    /// Embedded objects written (charts), and the package directories they live in.
    pub objects: usize,
    pub dirs: Vec<(String, String)>,
}

impl Writer<'_> {
    /// The package path of a media item, adding it on first use.
    pub fn media_path(&mut self, id: deckcraft_model::MediaId) -> Option<String> {
        if let Some(p) = self.media.get(&id.0) {
            return Some(p.clone());
        }
        let m = self.p.media(id)?;
        let ext =
            m.name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).filter(|e| e.len() <= 5 && e.chars().all(|c| c.is_ascii_alphanumeric()));
        let ext = ext.unwrap_or_else(|| crate::vocab::ext_for(&m.content_type).to_string());
        let dir = if m.content_type.starts_with("audio/") || m.content_type.starts_with("video/") { "Media" } else { "Pictures" };
        let path = format!("{dir}/m{}.{ext}", id.0);
        self.files.push((path.clone(), m.content_type.clone(), std::sync::Arc::clone(&m.data)));
        self.media.insert(id.0, path.clone());
        Some(path)
    }
}

/// The page being written: the master page whose presentation styles its placeholders take, and
/// the element ids it gives shapes and paragraphs others refer to (animations, connector ends).
pub(crate) struct PageCtx {
    /// The master page's name: the prefix of its presentation styles (`Name-title` …).
    pub master: String,
    /// The page's id prefix (`pg3`), unique in the document.
    pub tag: String,
    /// A master page: its date fields are the slides' date-time field, which each slide shows
    /// or hides.
    pub is_master: bool,
    /// The page's background, for shapes filled with it (`useBgFill`).
    pub background: (deckcraft_model::style::Fill, Option<deckcraft_color::Rgba>),
    ids: std::collections::HashSet<(u32, Option<u32>)>,
    /// The glue point each connector end attaches to: (shape, connection site) → glue point.
    glue: HashMap<(u32, u32), u32>,
}

impl PageCtx {
    pub fn new(master: &str, tag: String, is_master: bool, background: (deckcraft_model::style::Fill, Option<deckcraft_color::Rgba>)) -> Self {
        PageCtx { master: master.to_string(), tag, is_master, background, ids: Default::default(), glue: HashMap::new() }
    }

    /// The glue point a connector end at `site` of `shape` attaches to.
    pub fn glue_point(&self, shape: deckcraft_model::ShapeId, site: u32) -> u32 {
        self.glue.get(&(shape.0, site)).copied().unwrap_or_else(|| crate::geometry::glue_point_of_site(None, site))
    }

    /// The `xml:id` of a shape (or one of its paragraphs), when something refers to it and it is
    /// written.
    pub fn id(&self, shape: deckcraft_model::ShapeId, para: Option<u32>) -> Option<String> {
        self.ids.contains(&(shape.0, para)).then(|| match para {
            Some(p) => format!("{}s{}p{p}", self.tag, shape.0),
            None => format!("{}s{}", self.tag, shape.0),
        })
    }

    /// Give ids to what the page's animations and connectors refer to, if it is written.
    pub fn refer(&mut self, w: &Writer, shapes: &[deckcraft_model::Shape], animations: &[deckcraft_model::Animation]) {
        let mut wanted: Vec<(deckcraft_model::ShapeId, Option<u32>)> =
            animations.iter().flat_map(|a| [Some((a.shape, a.paragraph)), a.trigger.map(|t| (t, None))]).flatten().collect();
        // Every shape by id, once (not a search of the page per reference).
        let mut ends = vec![];
        let mut by_id: HashMap<deckcraft_model::ShapeId, &deckcraft_model::Shape> = HashMap::new();
        deckcraft_model::walk(shapes, &mut |sh, _| {
            by_id.insert(sh.id, sh);
            if let ShapeKind::Connector { start, end } = &sh.kind {
                ends.extend([start, end].into_iter().flatten().copied());
            }
        });
        for (id, site) in ends {
            wanted.push((id, None));
            self.glue.insert((id.0, site), crate::geometry::glue_point_of_site(by_id.get(&id).copied(), site));
        }
        for (id, para) in wanted {
            let Some(sh) = by_id.get(&id).copied() else { continue };
            let para_ok = para.is_none_or(|i| sh.text.as_ref().is_some_and(|t| (i as usize) < t.paragraphs.len()));
            if shapes::writes_element(w, sh, !self.is_master) && para_ok {
                self.ids.insert((id.0, para));
            }
        }
    }
}

/// A master page to write: a master and one of its layouts.
struct Page {
    name: String,
    /// The name people see (the style name is escaped).
    display: String,
    master: usize,
    layout: Option<LayoutId>,
}

pub fn export(p: &Presentation, mime: &str) -> Result<Vec<u8>, OdpError> {
    // Every page needs a master page: a deck without masters gets the default one.
    let with_master;
    let p = if p.masters.is_empty() {
        let mut q = p.clone();
        let (m, next) = deckcraft_model::defaults::build_master(q.slide_size, Default::default(), q.next_id.max(1));
        q.next_id = next.max(q.next_id);
        q.masters.push(std::sync::Arc::new(m));
        with_master = q;
        &with_master
    } else {
        p
    };
    let mut w = Writer {
        p,
        named: Named::default(),
        media: HashMap::new(),
        files: vec![],
        fonts: Default::default(),
        page_names: HashMap::new(),
        objects: 0,
        dirs: vec![],
    };
    w.page_names = page_names(p);
    // Master pages: one per (master, layout) the slides use; a deck without slides keeps each
    // master's first layout.
    let mut pages: Vec<Page> = vec![];
    let mut page_of: HashMap<(usize, Option<LayoutId>), String> = HashMap::new();
    let mut want = |mi: usize, layout: Option<LayoutId>, pages: &mut Vec<Page>| -> String {
        if let Some(n) = page_of.get(&(mi, layout)) {
            return n.clone();
        }
        let Some(m) = p.masters.get(mi) else { return String::new() };
        let lname = layout.and_then(|l| m.layout(l)).map(|l| l.name.clone()).unwrap_or_default();
        let base = if lname.is_empty() { m.name.clone() } else { format!("{} {}", m.name, lname) };
        let shown = if base.trim().is_empty() { "Default".to_string() } else { base };
        let base = style_name(&shown);
        let (mut name, mut display) = (base.clone(), shown.clone());
        let mut k = 1;
        while pages.iter().any(|pg| pg.name == name) {
            k += 1;
            name = format!("{base}_{k}");
            display = format!("{shown} {k}");
        }
        pages.push(Page { name: name.clone(), display, master: mi, layout });
        page_of.insert((mi, layout), name.clone());
        name
    };
    let mut slide_pages = vec![];
    for s in &p.slides {
        // The master and layout the slide is drawn with, as the model resolves them.
        let (mi, layout) = match p.master_for(s) {
            Some((m, l)) => (p.masters.iter().position(|x| std::ptr::eq(x.as_ref(), m)).unwrap_or(0), l.map(|l| l.id)),
            None => (0, None),
        };
        slide_pages.push(want(mi, layout, &mut pages));
    }
    if pages.is_empty() {
        let layout = p.masters.first().and_then(|m| m.layouts.first()).map(|l| l.id);
        want(0, layout, &mut pages);
    }

    // styles.xml: presentation styles and master pages.
    let mut sauto = Auto::new("M");
    let mut common = String::new();
    let mut masters = String::new();
    for pg in &pages {
        let Some(m) = p.masters.get(pg.master) else { continue };
        let layout = pg.layout.and_then(|l| m.layout(l));
        let ctx = match layout {
            Some(l) => Ctx::for_layout(p, m, l),
            None => Ctx::for_master(p, m),
        };
        common.push_str(&shapes::presentation_styles(&mut w, &ctx, &pg.name));
        masters.push_str(&shapes::master_page(&mut w, &mut sauto, &ctx, &pg.name, &pg.display, layout));
    }

    // content.xml
    let mut cauto = Auto::new("");
    let mut body = String::new();
    for (i, s) in p.slides.iter().enumerate() {
        let master_name = slide_pages.get(i).cloned().unwrap_or_default();
        body.push_str(&slide(&mut w, &mut cauto, s, i, &master_name));
    }
    let mut decls = String::new();
    let hf = &p.header_footer;
    if hf.footer && !hf.footer_text.is_empty() {
        let _ = write!(decls, r#"<presentation:footer-decl presentation:name="ftr1">{}</presentation:footer-decl>"#, esc(&hf.footer_text));
    }
    if hf.date {
        if hf.date_text.is_empty() {
            let _ = write!(
                decls,
                r#"<presentation:date-time-decl presentation:name="dtd1" presentation:source="current-date" style:data-style-name="{}"/>"#,
                date_style(&hf.date_format)
            );
        } else {
            let _ = write!(
                decls,
                r#"<presentation:date-time-decl presentation:name="dtd1" presentation:source="fixed">{}</presentation:date-time-decl>"#,
                esc(&hf.date_text)
            );
        }
    }
    let settings = show_settings(p, &w.page_names);
    let embedded = embedded_fonts(&mut w);
    let fonts = font_decls(&w.fonts, &embedded);
    let content = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><office:document-content {NS}>{fonts}<office:automatic-styles>{}</office:automatic-styles><office:body><office:presentation>{decls}{body}{settings}</office:presentation></office:body></office:document-content>"#,
        cauto.out
    );
    let page_layouts = format!(
        r#"<style:page-layout style:name="PM1"><style:page-layout-properties fo:margin-top="0cm" fo:margin-bottom="0cm" fo:margin-left="0cm" fo:margin-right="0cm" fo:page-width="{}" fo:page-height="{}" style:print-orientation="{}"/></style:page-layout><style:page-layout style:name="PM2"><style:page-layout-properties fo:margin-top="0cm" fo:margin-bottom="0cm" fo:margin-left="0cm" fo:margin-right="0cm" fo:page-width="{}" fo:page-height="{}" style:print-orientation="portrait"/></style:page-layout>"#,
        cm(p.slide_size.width),
        cm(p.slide_size.height),
        if p.slide_size.width >= p.slide_size.height { "landscape" } else { "portrait" },
        cm(p.notes_size.width.max(1.0)),
        cm(p.notes_size.height.max(1.0)),
    );
    let styles = format!(
        r##"<?xml version="1.0" encoding="UTF-8"?><office:document-styles {NS}>{fonts}<office:styles>{}{}<style:default-style style:family="graphic"><style:graphic-properties svg:stroke-color="#3465a4" draw:fill-color="#729fcf" fo:wrap-option="no-wrap"/><style:paragraph-properties style:writing-mode="lr-tb"/><style:text-properties fo:font-size="18pt" fo:language="en" fo:country="US"/></style:default-style><style:style style:name="standard" style:family="graphic"><style:graphic-properties draw:stroke="solid" svg:stroke-width="0cm" svg:stroke-color="#3465a4" draw:fill="solid" draw:fill-color="#729fcf" fo:padding-top="0.125cm" fo:padding-bottom="0.125cm" fo:padding-left="0.25cm" fo:padding-right="0.25cm" draw:shadow="hidden"/></style:style>{common}</office:styles><office:automatic-styles>{page_layouts}{}</office:automatic-styles><office:master-styles>{masters}</office:master-styles></office:document-styles>"##,
        w.named.out,
        date_styles(),
        sauto.out
    );
    let meta = meta_xml(p);
    let settings_xml = r#"<?xml version="1.0" encoding="UTF-8"?><office:document-settings xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:config="urn:oasis:names:tc:opendocument:xmlns:config:1.0" office:version="1.3"><office:settings/></office:document-settings>"#;

    let mut manifest = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0" manifest:version="1.3"><manifest:file-entry manifest:full-path="/" manifest:version="1.3" manifest:media-type="{mime}"/><manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/><manifest:file-entry manifest:full-path="styles.xml" manifest:media-type="text/xml"/><manifest:file-entry manifest:full-path="meta.xml" manifest:media-type="text/xml"/><manifest:file-entry manifest:full-path="settings.xml" manifest:media-type="text/xml"/>"#
    );
    for (path, ct) in w.dirs.iter().map(|(p, c)| (p, c)).chain(w.files.iter().map(|(p, c, _)| (p, c))) {
        let _ = write!(manifest, r#"<manifest:file-entry manifest:full-path="{}" manifest:media-type="{}"/>"#, esc(path), esc(ct));
    }
    manifest.push_str("</manifest:manifest>");

    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let deflated = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let err = |e: &dyn std::fmt::Display| OdpError::Write(e.to_string());
    z.start_file("mimetype", stored).map_err(|e| err(&e))?;
    z.write_all(mime.as_bytes()).map_err(|e| err(&e))?;
    for (name, data) in [
        ("content.xml", content.as_bytes()),
        ("styles.xml", styles.as_bytes()),
        ("meta.xml", meta.as_bytes()),
        ("settings.xml", settings_xml.as_bytes()),
    ] {
        z.start_file(name, deflated).map_err(|e| err(&e))?;
        z.write_all(data).map_err(|e| err(&e))?;
    }
    for (path, ct, data) in &w.files {
        // Compressed formats are stored as they are.
        let opts = if ct.starts_with("image/png") || ct.starts_with("image/jpeg") || ct.starts_with("video/") || ct.starts_with("audio/") {
            stored
        } else {
            deflated
        };
        let opts = if data.len() >= u32::MAX as usize / 2 { opts.large_file(true) } else { opts };
        z.start_file(path.as_str(), opts).map_err(|e| err(&e))?;
        z.write_all(data).map_err(|e| err(&e))?;
    }
    z.start_file("META-INF/manifest.xml", deflated).map_err(|e| err(&e))?;
    z.write_all(manifest.as_bytes()).map_err(|e| err(&e))?;
    Ok(z.finish().map_err(|e| err(&e))?.into_inner())
}

/// Each slide's page name: its own name, unique and without commas (custom shows list pages
/// separated by commas), else `pageN`.
fn page_names(p: &Presentation) -> HashMap<u32, String> {
    let mut out = HashMap::new();
    let own = |s: &Slide| s.name.replace(',', " ").trim().to_string();
    // Named slides first, so an unnamed one keeps its plain `pageN` (which a reader takes for no
    // name) unless a slide is really called that.
    let order = p.slides.iter().enumerate().filter(|(_, s)| !own(s).is_empty()).chain(p.slides.iter().enumerate().filter(|(_, s)| own(s).is_empty()));
    let mut used = std::collections::HashSet::new();
    for (i, s) in order {
        let own = own(s);
        let base = if own.is_empty() { format!("page{}", i + 1) } else { own };
        // Taken (by an earlier slide, or by another's numbered name): number it until it isn't.
        let mut name = base.clone();
        let mut k = i + 1;
        while !used.insert(name.clone()) {
            name = format!("{base} ({k})");
            k += 1;
        }
        out.insert(s.id.0, name);
    }
    out
}

/// PowerPoint's date and time formats (`datetime1` … `datetime13`) as number styles
/// `datetime1` … `datetime13` (their own names, which LibreOffice's `D1`… don't collide with).
const DATE_FORMATS: [&str; 13] = [
    // 11/4/2026
    "M/D/Y",
    // Wednesday, November 4, 2026
    "W, N D, Y",
    // 4 November 2026
    "D N Y",
    // November 4, 2026
    "N D, Y",
    // 4-Nov-26
    "D-b-y",
    // November 26
    "N y",
    // Nov-26
    "b-y",
    // 11/4/2026 9:30 AM
    "M/D/Y h:m a",
    // 11/4/2026 9:30:45 AM
    "M/D/Y h:m:s a",
    // 9:30
    "h:m",
    // 9:30:45
    "h:m:s",
    // 9:30 AM
    "h:m a",
    // 9:30:45 AM
    "h:m:s a",
];

/// The number style of a date format (`datetime4`; anything else is the first).
pub(crate) fn date_style(format: &str) -> String {
    let n = format.strip_prefix("datetime").and_then(|n| n.parse::<usize>().ok()).filter(|n| (1..=DATE_FORMATS.len()).contains(n));
    format!("datetime{}", n.unwrap_or(1))
}

/// The `number:date-style` of every date format.
fn date_styles() -> String {
    let mut out = String::new();
    for (i, f) in DATE_FORMATS.iter().enumerate() {
        let _ = write!(out, r#"<number:date-style style:name="datetime{}">"#, i + 1);
        for c in f.chars() {
            out.push_str(match c {
                'M' => "<number:month/>",
                'D' => "<number:day/>",
                'Y' => r#"<number:year number:style="long"/>"#,
                'y' => "<number:year/>",
                'N' => r#"<number:month number:textual="true" number:style="long"/>"#,
                'b' => r#"<number:month number:textual="true"/>"#,
                'W' => r#"<number:day-of-week number:style="long"/>"#,
                'h' => "<number:hours/>",
                'm' => r#"<number:minutes number:style="long"/>"#,
                's' => r#"<number:seconds number:style="long"/>"#,
                'a' => "<number:am-pm/>",
                _ => "",
            });
            if !"MDYyNbWhmsa".contains(c) {
                let _ = write!(out, "<number:text>{}</number:text>", esc(&c.to_string()));
            }
        }
        out.push_str("</number:date-style>");
    }
    out
}

/// A font family as `svg:font-family` and `fo:font-family` take it: quoted, escaped.
pub(crate) fn quoted_family(f: &str) -> String {
    esc(&format!("'{}'", f.replace('\'', "")))
}

/// A style name: letters, digits, `-` and `_`, other characters as `_` (spaces as `_20_`, as
/// LibreOffice does), never starting with a digit or `-`.
pub(crate) fn style_name(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars().take(64) {
        if c.is_ascii_alphanumeric() || c == '-' {
            o.push(c);
        } else if c == ' ' {
            o.push_str("_20_");
        } else {
            o.push('_');
        }
    }
    if o.chars().next().is_some_and(|c| c.is_ascii_digit() || c == '-') {
        o.insert(0, '_');
    }
    o
}

fn font_decls(fonts: &std::collections::BTreeSet<String>, embedded: &[(String, &'static str, &'static str, String)]) -> String {
    let mut s = String::from("<office:font-face-decls>");
    for f in fonts {
        let faces: Vec<_> = embedded.iter().filter(|e| e.0 == *f).collect();
        if faces.is_empty() {
            let _ = write!(s, r#"<style:font-face style:name="{}" svg:font-family="{}"/>"#, esc(f), quoted_family(f));
            continue;
        }
        // An embedded font: one face per file, pointing at it.
        for (k, (_, weight, style, path)) in faces.iter().enumerate() {
            let name = if k == 0 { f.clone() } else { format!("{f}{k}") };
            let _ = write!(
                s,
                r#"<style:font-face style:name="{}" svg:font-family="{}" svg:font-weight="{weight}" svg:font-style="{style}"><svg:font-face-src><svg:font-face-uri xlink:href="{}" xlink:type="simple"><svg:font-face-format svg:string="truetype"/></svg:font-face-uri></svg:font-face-src></style:font-face>"#,
                esc(&name),
                quoted_family(f),
                esc(path)
            );
        }
    }
    s.push_str("</office:font-face-decls>");
    s
}

/// The embedded font files to write: (family, weight, style, package path).
fn embedded_fonts(w: &mut Writer) -> Vec<(String, &'static str, &'static str, String)> {
    let mut out = vec![];
    for (fi, ef) in w.p.embedded_fonts.iter().enumerate().take(64) {
        for (slot, id) in ef.faces().into_iter().enumerate() {
            let Some(id) = id else { continue };
            let Some(m) = w.p.media(id) else { continue };
            // An obfuscated or EOT font is written as the plain font inside it.
            let Some(sfnt) = deckcraft_fonts::embedded_sfnt(&m.data) else { continue };
            let path = format!("Fonts/f{fi}-{slot}.ttf");
            w.files.push((path.clone(), "application/x-font-ttf".into(), std::sync::Arc::new(sfnt.to_vec())));
            w.fonts.insert(ef.typeface.clone());
            let (weight, style) = match slot {
                1 => ("bold", "normal"),
                2 => ("normal", "italic"),
                3 => ("bold", "italic"),
                _ => ("normal", "normal"),
            };
            out.push((ef.typeface.clone(), weight, style, path));
        }
    }
    out
}

fn meta_xml(p: &Presentation) -> String {
    let pr = &p.props;
    let mut m = String::new();
    let mut el = |n: &str, v: &str| {
        if !v.is_empty() {
            let _ = write!(m, "<{n}>{}</{n}>", esc(v));
        }
    };
    el("dc:title", &pr.title);
    el("dc:subject", &pr.subject);
    el("dc:description", &pr.comments);
    el("meta:initial-creator", &pr.author);
    el("dc:creator", if pr.last_modified_by.is_empty() { &pr.author } else { &pr.last_modified_by });
    el("meta:creation-date", &pr.created);
    el("dc:date", &pr.modified);
    for k in pr.keywords.split(',').map(str::trim).filter(|k| !k.is_empty()) {
        let _ = write!(m, "<meta:keyword>{}</meta:keyword>", esc(k));
    }
    if pr.revision > 0 {
        let _ = write!(m, "<meta:editing-cycles>{}</meta:editing-cycles>", pr.revision);
    }
    // Properties ODF has no element for, as user-defined ones.
    for (name, v) in [("Category", &pr.category), ("Company", &pr.company)] {
        if !v.is_empty() {
            let _ = write!(m, r#"<meta:user-defined meta:name="{name}">{}</meta:user-defined>"#, esc(v));
        }
    }
    let _ = write!(m, "<meta:generator>DeckCraft/{}</meta:generator>", env!("CARGO_PKG_VERSION"));
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><office:document-meta xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0" office:version="1.3"><office:meta>{m}</office:meta></office:document-meta>"#
    )
}

fn show_settings(p: &Presentation, names: &HashMap<u32, String>) -> String {
    let mut attrs = String::new();
    if p.show.loop_until_esc {
        attrs.push_str(r#" presentation:endless="true""#);
    }
    if p.show.show_type == "browsed" {
        attrs.push_str(r#" presentation:full-screen="false""#);
    }
    // A range starts at its first slide (ODF has no end).
    if let Some(name) = p.show.range.and_then(|(from, _)| p.slides.get((from as usize).saturating_sub(1))).and_then(|s| names.get(&s.id.0)) {
        let _ = write!(attrs, r#" presentation:start-page="{}""#, esc(name));
    }
    if !p.show.use_timings {
        attrs.push_str(r#" presentation:force-manual="true""#);
    }
    if p.show.without_animation {
        attrs.push_str(r#" presentation:animations="disabled""#);
    }
    if let Some(cs) = &p.show.custom_show {
        let _ = write!(attrs, r#" presentation:show="{}""#, esc(cs));
    }
    let mut shows = String::new();
    for cs in &p.custom_shows {
        let pages: Vec<&str> = cs.slides.iter().filter_map(|id| names.get(&id.0).map(String::as_str)).collect();
        let _ = write!(shows, r#"<presentation:show presentation:name="{}" presentation:pages="{}"/>"#, esc(&cs.name), esc(&pages.join(",")));
    }
    format!(r#"<presentation:settings presentation:mouse-visible="true"{attrs}>{shows}</presentation:settings>"#)
}

/// One `draw:page`.
fn slide(w: &mut Writer, auto: &mut Auto, s: &Slide, i: usize, master_name: &str) -> String {
    let p = w.p;
    let Some(ctx) = Ctx::for_slide(p, s) else { return String::new() };
    let mut pg = PageCtx::new(master_name, format!("pg{i}"), false, resolve::background(&ctx, Some(s)));
    // By-paragraph builds as one effect per paragraph, which the page's ids must cover.
    let effects = crate::anim::expand_builds(&s.animations, &s.shapes);
    let animations: Vec<deckcraft_model::Animation> = effects.iter().map(|e| e.a.clone()).collect();
    pg.refer(w, &s.shapes, &animations);
    // The page style: background, transition, visibility, footers.
    let mut pp = String::new();
    if s.background.is_some() {
        let (fill, ph) = &pg.background;
        pp.push_str(&shapes::fill_attrs(w, &ctx, fill, *ph));
        pp.push_str(r#" draw:background-size="border""#);
    }
    if s.hidden {
        pp.push_str(r#" presentation:visibility="hidden""#);
    }
    if !s.show_master_shapes {
        pp.push_str(r#" presentation:background-objects-visible="false""#);
    }
    let hf = &p.header_footer;
    let has = |k: PhType| s.shapes.iter().any(|sh| sh.ph_type() == Some(k));
    let _ = write!(
        pp,
        r#" presentation:display-footer="{}" presentation:display-page-number="{}" presentation:display-date-time="{}""#,
        hf.footer && has(PhType::Footer),
        hf.slide_number && has(PhType::SlideNum),
        hf.date && has(PhType::Date)
    );
    if let Some(t) = &s.transition {
        pp.push_str(&crate::anim::transition_props(t));
    }
    let page_style = auto.style("drawing-page", "dp", None, &format!("<style:drawing-page-properties{pp}/>"));
    let mut out = format!(
        r#"<draw:page draw:name="{}" draw:style-name="{page_style}" draw:master-page-name="{}" xml:id="{tag}" draw:id="{tag}""#,
        esc(w.page_names.get(&s.id.0).map(String::as_str).unwrap_or_default()),
        esc(master_name),
        tag = pg.tag,
    );
    if hf.footer && has(PhType::Footer) && !hf.footer_text.is_empty() {
        out.push_str(r#" presentation:use-footer-name="ftr1""#);
    }
    if hf.date && has(PhType::Date) {
        out.push_str(r#" presentation:use-date-time-name="dtd1""#);
    }
    out.push('>');
    for sh in &s.shapes {
        out.push_str(&shapes::shape(w, auto, &ctx, &pg, sh, 0));
    }
    // Comments (replies follow their comment, at its place).
    for c in &s.comments {
        for (k, reply) in std::iter::once(c).chain(c.replies.iter()).enumerate().take(1000) {
            let text: String = reply.text.split('\n').map(|l| format!("<text:p>{}</text:p>", text::text_content(l))).collect();
            let date = if reply.date.is_empty() { String::new() } else { format!("<dc:date>{}</dc:date>", esc(&reply.date)) };
            let initials = if reply.initials.is_empty() {
                String::new()
            } else {
                format!("<meta:creator-initials>{}</meta:creator-initials>", esc(&reply.initials))
            };
            let resolved = if reply.resolved && k == 0 { r#" loext:resolved="true""# } else { "" };
            let _ = write!(
                out,
                r#"<officeooo:annotation svg:x="{}" svg:y="{}" svg:width="0.5cm" svg:height="0.5cm"{resolved}><dc:creator>{}</dc:creator>{date}{initials}{text}</officeooo:annotation>"#,
                cm(c.x),
                cm(c.y),
                esc(&reply.author)
            );
        }
    }
    // Transition and animations.
    let filter = s.transition.as_ref().and_then(crate::anim::transition_filter);
    if filter.is_some() || !s.animations.is_empty() {
        out.push_str(&crate::anim::write(&pg.tag, filter.as_deref(), &effects, &ctx, &|sid, para| pg.id(sid, para)));
    }
    if !s.notes.paragraphs.is_empty() && !s.notes.text().trim().is_empty() {
        let notes = text::plain_paragraphs(w, auto, &ctx, &s.notes);
        let _ = write!(
            out,
            r#"<presentation:notes><draw:page-thumbnail draw:page-number="{}" presentation:class="page" svg:x="1.5cm" svg:y="2cm" svg:width="18cm" svg:height="10cm"/><draw:frame presentation:style-name="{}-notes" presentation:class="notes" svg:x="2cm" svg:y="13.5cm" svg:width="17cm" svg:height="13cm"><draw:text-box>{notes}</draw:text-box></draw:frame></presentation:notes>"#,
            i + 1,
            esc(master_name)
        );
    }
    out.push_str("</draw:page>");
    out
}

/// An angle in radians as written in `draw:transform`.
pub(crate) fn rad(deg: f64) -> String {
    trim(deg.to_radians(), 6)
}
