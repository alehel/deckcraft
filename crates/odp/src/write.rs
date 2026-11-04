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
use deckcraft_model::{LayoutId, PhType, Presentation, Slide};

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

    /// A `style:style` of `family` with these property elements; `tag` names it (`gr`, `P`…).
    pub fn style(&mut self, family: &str, tag: &'static str, parent: Option<&str>, body: &str) -> String {
        let key = (format!("{family}|{}", parent.unwrap_or("")), body.to_string());
        if let Some(n) = self.map.get(&key) {
            return n.clone();
        }
        let c = self.n.entry(tag).or_insert(0);
        *c += 1;
        let name = format!("{}{tag}{c}", self.prefix);
        let parent = parent.map(|p| format!(r#" style:parent-style-name="{}""#, esc(p))).unwrap_or_default();
        let _ = write!(self.out, r#"<style:style style:name="{name}" style:family="{family}"{parent}>{body}</style:style>"#);
        self.map.insert(key, name.clone());
        name
    }

    /// A `text:list-style` with these level elements.
    pub fn list(&mut self, levels: &str) -> String {
        let key = ("list".to_string(), levels.to_string());
        if let Some(n) = self.map.get(&key) {
            return n.clone();
        }
        let c = self.n.entry("L").or_insert(0);
        *c += 1;
        let name = format!("{}L{c}", self.prefix);
        let _ = write!(self.out, r#"<text:list-style style:name="{name}">{levels}</text:list-style>"#);
        self.map.insert(key, name.clone());
        name
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
    /// The master page the shapes being written sit on (their presentation styles' prefix).
    pub page: String,
    /// Embedded objects written (charts), and the package directories they live in.
    pub objects: usize,
    pub dirs: Vec<(String, String)>,
    /// Shapes and paragraphs the page's animations target: they get `xml:id`s.
    pub anim_targets: std::collections::HashSet<(u32, Option<u32>)>,
    /// Writing a master page: its date fields follow each slide's date setting.
    pub in_master: bool,
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
        let ext = ext.unwrap_or_else(|| ext_for(&m.content_type).to_string());
        let dir = if m.content_type.starts_with("audio/") || m.content_type.starts_with("video/") { "Media" } else { "Pictures" };
        let path = format!("{dir}/m{}.{ext}", id.0);
        self.files.push((path.clone(), m.content_type.clone(), std::sync::Arc::clone(&m.data)));
        self.media.insert(id.0, path.clone());
        Some(path)
    }
}

fn ext_for(ct: &str) -> &'static str {
    match ct {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/svg+xml" => "svg",
        "image/bmp" => "bmp",
        "image/tiff" => "tif",
        "image/webp" => "webp",
        "image/x-emf" => "emf",
        "image/x-wmf" => "wmf",
        "video/mp4" => "mp4",
        "audio/mpeg" => "mp3",
        "audio/wav" => "wav",
        _ => "bin",
    }
}

/// A master page to write: a master and one of its layouts.
struct Page {
    name: String,
    master: usize,
    layout: Option<LayoutId>,
}

pub fn export(p: &Presentation, mime: &str) -> Result<Vec<u8>, OdpError> {
    let mut w = Writer {
        p,
        named: Named::default(),
        media: HashMap::new(),
        files: vec![],
        fonts: Default::default(),
        page_names: HashMap::new(),
        page: String::new(),
        objects: 0,
        dirs: vec![],
        anim_targets: Default::default(),
        in_master: false,
    };
    for (i, s) in p.slides.iter().enumerate() {
        w.page_names.insert(s.id.0, page_name(i, s));
    }
    // Master pages: one per (master, layout) the slides use; a deck without slides keeps each
    // master's first layout.
    let mut pages: Vec<Page> = vec![];
    let mut page_of: HashMap<(usize, Option<LayoutId>), String> = HashMap::new();
    let mut want = |mi: usize, layout: Option<LayoutId>, pages: &mut Vec<Page>| -> String {
        if let Some(n) = page_of.get(&(mi, layout)) {
            return n.clone();
        }
        let m = &p.masters[mi.min(p.masters.len().saturating_sub(1))];
        let lname = layout.and_then(|l| m.layout(l)).map(|l| l.name.clone()).unwrap_or_default();
        let base = if lname.is_empty() { m.name.clone() } else { format!("{} {}", m.name, lname) };
        let base = style_name(if base.trim().is_empty() { "Default" } else { &base });
        let mut name = base.clone();
        let mut k = 1;
        while pages.iter().any(|pg| pg.name == name) {
            k += 1;
            name = format!("{base}_{k}");
        }
        pages.push(Page { name: name.clone(), master: mi, layout });
        page_of.insert((mi, layout), name.clone());
        name
    };
    let mut slide_pages = vec![];
    for s in &p.slides {
        let mi = p.masters.iter().position(|m| m.layout(s.layout).is_some()).unwrap_or(0);
        let layout = p
            .masters
            .get(mi)
            .and_then(|m| m.layout(s.layout))
            .map(|l| l.id)
            .or_else(|| p.masters.get(mi).and_then(|m| m.layouts.first()).map(|l| l.id));
        slide_pages.push(want(mi, layout, &mut pages));
    }
    if pages.is_empty() && !p.masters.is_empty() {
        let layout = p.masters[0].layouts.first().map(|l| l.id);
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
        common.push_str(&shapes::presentation_styles(&mut w, &mut sauto, &ctx, &pg.name));
        masters.push_str(&shapes::master_page(&mut w, &mut sauto, &ctx, &pg.name, layout));
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
            decls
                .push_str(r#"<presentation:date-time-decl presentation:name="dtd1" presentation:source="current-date" style:data-style-name="D1"/>"#);
        } else {
            let _ = write!(
                decls,
                r#"<presentation:date-time-decl presentation:name="dtd1" presentation:source="fixed">{}</presentation:date-time-decl>"#,
                esc(&hf.date_text)
            );
        }
    }
    let settings = show_settings(p);
    let fonts = font_decls(&w.fonts);
    let content = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><office:document-content {NS}>{fonts}<office:automatic-styles>{}<number:date-style style:name="D1" number:automatic-order="true"><number:month number:style="long"/><number:text>/</number:text><number:day number:style="long"/><number:text>/</number:text><number:year/></number:date-style></office:automatic-styles><office:body><office:presentation>{decls}{body}{settings}</office:presentation></office:body></office:document-content>"#,
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
    let fonts = font_decls(&w.fonts);
    let styles = format!(
        r##"<?xml version="1.0" encoding="UTF-8"?><office:document-styles {NS}>{fonts}<office:styles>{}<style:default-style style:family="graphic"><style:graphic-properties svg:stroke-color="#3465a4" draw:fill-color="#729fcf" fo:wrap-option="no-wrap"/><style:paragraph-properties style:writing-mode="lr-tb"/><style:text-properties fo:font-size="18pt" fo:language="en" fo:country="US"/></style:default-style><style:style style:name="standard" style:family="graphic"><style:graphic-properties draw:stroke="solid" svg:stroke-width="0cm" svg:stroke-color="#3465a4" draw:fill="solid" draw:fill-color="#729fcf" fo:padding-top="0.125cm" fo:padding-bottom="0.125cm" fo:padding-left="0.25cm" fo:padding-right="0.25cm" draw:shadow="hidden"/></style:style>{common}</office:styles><office:automatic-styles>{page_layouts}{}</office:automatic-styles><office:master-styles>{masters}</office:master-styles></office:document-styles>"##,
        w.named.out, sauto.out
    );
    let meta = meta_xml(p);
    let settings_xml = r#"<?xml version="1.0" encoding="UTF-8"?><office:document-settings xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:config="urn:oasis:names:tc:opendocument:xmlns:config:1.0" office:version="1.3"><office:settings/></office:document-settings>"#;

    let mut manifest = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0" manifest:version="1.3"><manifest:file-entry manifest:full-path="/" manifest:version="1.3" manifest:media-type="{mime}"/><manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/><manifest:file-entry manifest:full-path="styles.xml" manifest:media-type="text/xml"/><manifest:file-entry manifest:full-path="meta.xml" manifest:media-type="text/xml"/><manifest:file-entry manifest:full-path="settings.xml" manifest:media-type="text/xml"/>"#
    );
    for (path, ct) in &w.dirs {
        let _ = write!(manifest, r#"<manifest:file-entry manifest:full-path="{}" manifest:media-type="{}"/>"#, esc(path), esc(ct));
    }
    for (path, ct, _) in &w.files {
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

fn page_name(i: usize, s: &Slide) -> String {
    let n = s.name.trim();
    if n.is_empty() { format!("page{}", i + 1) } else { n.to_string() }
}

/// A style name: letters, digits and `_` (spaces encoded as `_20_`, as LibreOffice does).
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

fn font_decls(fonts: &std::collections::BTreeSet<String>) -> String {
    let mut s = String::from("<office:font-face-decls>");
    for f in fonts {
        let _ = write!(s, r#"<style:font-face style:name="{}" svg:font-family="'{}'"/>"#, esc(f), esc(&f.replace('\'', "")));
    }
    s.push_str("</office:font-face-decls>");
    s
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
    let _ = write!(m, "<meta:generator>DeckCraft/{}</meta:generator>", env!("CARGO_PKG_VERSION"));
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><office:document-meta xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0" office:version="1.3"><office:meta>{m}</office:meta></office:document-meta>"#
    )
}

fn show_settings(p: &Presentation) -> String {
    let mut attrs = String::new();
    if p.show.loop_until_esc {
        attrs.push_str(r#" presentation:endless="true""#);
    }
    if p.show.show_type == "browse" {
        attrs.push_str(r#" presentation:full-screen="false""#);
    }
    if let Some(cs) = &p.show.custom_show {
        let _ = write!(attrs, r#" presentation:show="{}""#, esc(cs));
    }
    let mut shows = String::new();
    for cs in &p.custom_shows {
        let pages: Vec<String> =
            cs.slides.iter().filter_map(|id| p.slide_index(*id).and_then(|i| p.slides.get(i).map(|s| page_name(i, s)))).collect();
        let _ = write!(shows, r#"<presentation:show presentation:name="{}" presentation:pages="{}"/>"#, esc(&cs.name), esc(&pages.join(",")));
    }
    format!(r#"<presentation:settings presentation:mouse-visible="true"{attrs}>{shows}</presentation:settings>"#)
}

/// One `draw:page`.
fn slide(w: &mut Writer, auto: &mut Auto, s: &Slide, i: usize, master_name: &str) -> String {
    let p = w.p;
    let Some(ctx) = Ctx::for_slide(p, s) else { return String::new() };
    w.page = master_name.to_string();
    w.anim_targets = s.animations.iter().flat_map(|a| [Some((a.shape.0, a.paragraph)), a.trigger.map(|t| (t.0, None))]).flatten().collect();
    // The page style: background, transition, visibility, footers.
    let mut pp = String::new();
    if let Some(bg) = &s.background {
        let _ = bg;
        let (fill, ph) = resolve::background(&ctx, Some(s));
        pp.push_str(&shapes::fill_attrs(w, &ctx, &fill, ph, true));
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
        pp.push_str(&shapes::transition_attrs(t));
    }
    let page_style = auto.style("drawing-page", "dp", None, &format!("<style:drawing-page-properties{pp}/>"));
    let mut out = format!(
        r#"<draw:page draw:name="{}" draw:style-name="{page_style}" draw:master-page-name="{}" xml:id="pg{i}" draw:id="pg{i}""#,
        esc(&page_name(i, s)),
        esc(master_name)
    );
    if hf.footer && has(PhType::Footer) && !hf.footer_text.is_empty() {
        out.push_str(r#" presentation:use-footer-name="ftr1""#);
    }
    if hf.date && has(PhType::Date) {
        out.push_str(r#" presentation:use-date-time-name="dtd1""#);
    }
    out.push('>');
    for sh in &s.shapes {
        // Footer placeholders are drawn by the master page from the declarations above.
        if sh.ph_type().is_some_and(PhType::is_footer_kind) {
            continue;
        }
        out.push_str(&shapes::shape(w, auto, &ctx, sh, 0));
    }
    // Comments (replies follow their comment, at its place).
    for c in &s.comments {
        for (k, cm) in std::iter::once(c).chain(c.replies.iter()).enumerate().take(1000) {
            let text: String = cm.text.split('\n').map(|l| format!("<text:p>{}</text:p>", text::text_content(l))).collect();
            let date = if cm.date.is_empty() { String::new() } else { format!("<dc:date>{}</dc:date>", esc(&cm.date)) };
            let initials =
                if cm.initials.is_empty() { String::new() } else { format!("<meta:creator-initials>{}</meta:creator-initials>", esc(&cm.initials)) };
            let resolved = if cm.resolved && k == 0 { r#" loext:resolved="true""# } else { "" };
            let _ = write!(
                out,
                r#"<officeooo:annotation svg:x="{}" svg:y="{}" svg:width="0.5cm" svg:height="0.5cm"{resolved}><dc:creator>{}</dc:creator>{date}{initials}{text}</officeooo:annotation>"#,
                crate::units::cm(c.x),
                crate::units::cm(c.y),
                esc(&cm.author)
            );
        }
    }
    // Transition and animations.
    let filter = s.transition.as_ref().and_then(crate::anim::transition_filter);
    if filter.is_some() || !s.animations.is_empty() {
        let ids = |sid: deckcraft_model::ShapeId, para: Option<u32>| match para {
            Some(pi) => Some(format!("s{}p{pi}", sid.0)),
            None => Some(format!("s{}", sid.0)),
        };
        out.push_str(&crate::anim::write(&format!("pg{i}"), filter.as_deref(), &s.animations, &ids));
    }
    if !s.notes.paragraphs.is_empty() && !s.notes.text().trim().is_empty() {
        let notes = text::plain_paragraphs(w, auto, &s.notes);
        let _ = write!(
            out,
            r#"<presentation:notes><draw:page-thumbnail draw:page-number="{}" presentation:class="page" svg:x="1.5cm" svg:y="2cm" svg:width="18cm" svg:height="10cm"/><draw:frame presentation:class="notes" svg:x="2cm" svg:y="13.5cm" svg:width="17cm" svg:height="13cm"><draw:text-box>{notes}</draw:text-box></draw:frame></presentation:notes>"#,
            i + 1
        );
    }
    out.push_str("</draw:page>");
    out
}

/// An angle in radians as written in `draw:transform`.
pub(crate) fn rad(deg: f64) -> String {
    trim(deg.to_radians(), 6)
}
