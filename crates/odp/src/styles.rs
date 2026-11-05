//! OpenDocument styles: common and automatic styles from `styles.xml` and `content.xml`, default
//! styles, list styles, and the named drawing definitions (gradients, hatches, fill images,
//! markers, dashes, opacity gradients) they refer to. A style is resolved by walking its parent
//! chain from the family's default style down, a property set nearer the element winning.

use std::collections::{BTreeMap, HashMap};

use crate::dom::{El, attr, kid, kids};
use crate::units;

pub type Props = BTreeMap<String, String>;

/// Parent chains longer than this are treated as broken (a cycle or a hostile file).
const MAX_CHAIN: usize = 32;

/// LibreOffice's built-in graphic defaults, which files leave unwritten: the lowest layer of every
/// graphic and presentation style.
const POOL_GRAPHIC: [(&str, &str); 4] =
    [("draw:fill", "solid"), ("draw:fill-color", "#729fcf"), ("draw:stroke", "solid"), ("svg:stroke-color", "#3465a4")];

/// Outline levels a presentation style can have (`{master}-outline1` … `-outline9`).
pub const OUTLINE_LEVELS: usize = 9;

#[derive(Clone, Debug, Default)]
pub struct Style {
    pub family: String,
    pub parent: Option<String>,
    /// `style:list-style-name` (paragraph styles).
    pub list_style_name: Option<String>,
    pub graphic: Props,
    pub paragraph: Props,
    pub text: Props,
    pub page: Props,
    pub cell: Props,
    pub column: Props,
    pub row: Props,
    pub table: Props,
    pub chart: Props,
    /// A list style embedded in `style:graphic-properties` (presentation and graphic styles).
    pub list: Option<std::rc::Rc<El>>,
    /// `style:tab-stops` of the paragraph properties.
    pub tabs: Option<std::rc::Rc<El>>,
}

impl Style {
    pub fn chart_prop(&self, k: &str) -> Option<&str> {
        self.chart.get(k).map(String::as_str)
    }

    /// Lay `over` on this style: each property it sets wins; relative font sizes apply to the
    /// size they replace.
    pub fn merge(&mut self, over: &Style) {
        let lay = |base: &mut Props, over: &Props| base.extend(over.iter().map(|(k, v)| (k.clone(), v.clone())));
        lay(&mut self.graphic, &over.graphic);
        lay(&mut self.paragraph, &over.paragraph);
        merge_text(&mut self.text, &over.text);
        lay(&mut self.page, &over.page);
        lay(&mut self.cell, &over.cell);
        lay(&mut self.column, &over.column);
        lay(&mut self.row, &over.row);
        lay(&mut self.table, &over.table);
        lay(&mut self.chart, &over.chart);
        if over.list.is_some() {
            self.list.clone_from(&over.list);
        }
        if over.tabs.is_some() {
            self.tabs.clone_from(&over.tabs);
        }
        if over.list_style_name.is_some() {
            self.list_style_name.clone_from(&over.list_style_name);
        }
    }
}

/// Text properties merge, with relative font sizes (`fo:font-size="45%"`) applied to the size they
/// override.
pub fn merge_text(base: &mut Props, over: &Props) {
    for (k, v) in over {
        if k.contains("font-size")
            && let Some(f) = units::percent(v)
            && let Some(under) = base.get(k.as_str())
        {
            // Of an absolute size, a size; of another percentage, their product.
            if let Some(abs) = units::length(under) {
                base.insert(k.clone(), units::pt(abs * f));
                continue;
            }
            if let Some(g) = units::percent(under) {
                base.insert(k.clone(), units::pct(f * g));
                continue;
            }
        }
        base.insert(k.clone(), v.clone());
    }
}

/// Which file an element came from: its automatic styles are looked up there first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Content,
    Styles,
}

/// Styles and list styles of one kind: common, or one file's automatic ones.
#[derive(Debug, Default)]
struct Layer {
    /// By (family, name).
    styles: HashMap<(String, String), Style>,
    lists: HashMap<String, El>,
}

#[derive(Debug, Default)]
pub struct Store {
    /// Common styles (`office:styles`), and each file's automatic styles.
    common: Layer,
    auto_styles: Layer,
    auto_content: Layer,
    defaults: HashMap<String, Style>,
    pub gradients: HashMap<String, El>,
    pub hatches: HashMap<String, El>,
    pub fill_images: HashMap<String, El>,
    pub markers: HashMap<String, El>,
    pub dashes: HashMap<String, El>,
    pub opacities: HashMap<String, El>,
    fonts: HashMap<String, String>,
    /// Font faces with embedded font files (`svg:font-face-uri`).
    pub font_faces: Vec<El>,
    /// Picture bullets: `xlink:href` → the media read from the package.
    pub image_bullets: HashMap<String, deckcraft_model::MediaId>,
    pub page_layouts: HashMap<String, Props>,
    /// The PowerPoint date format (1–13) of each date and time number style that stands for one,
    /// by name; worked out once, as the style is read.
    pub date_formats: HashMap<String, u8>,
    /// Page layouts (`style:presentation-page-layout`): where a page's placeholders go, by name.
    pub presentation_layouts: HashMap<String, El>,
    /// Display names, for writing styles back (`Default-title` etc. are plain already).
    pub common_names: Vec<(String, String)>,
    /// Spaces the document's `text:s` elements have expanded to so far (each one asks for up to
    /// a thousand), against [`MAX_EXPANDED_SPACES`].
    pub spaces_used: std::cell::Cell<usize>,
}

/// Most date and time styles read (a deck has a few).
const MAX_DATA_STYLES: usize = 1024;

/// Most spaces a document's `text:s` elements expand to in all.
pub const MAX_EXPANDED_SPACES: usize = 1 << 22;

fn read_props(el: &El) -> Props {
    el.attrs.iter().filter(|(k, _)| !k.starts_with("xmlns")).map(|(k, v)| (k.clone(), v.clone())).collect()
}

fn read_style(el: &El) -> Style {
    let mut s = Style {
        family: attr(el, "style:family").unwrap_or_default().to_string(),
        parent: attr(el, "style:parent-style-name").map(str::to_string),
        list_style_name: attr(el, "style:list-style-name").map(str::to_string),
        ..Default::default()
    };
    for p in el.elements() {
        match p.name.as_str() {
            "style:graphic-properties" | "loext:graphic-properties" => {
                // A paragraph style's `loext:graphic-properties` describe the shape, not the text;
                // they never override the shape's own graphic style.
                if p.name == "style:graphic-properties" || s.graphic.is_empty() {
                    s.graphic.extend(read_props(p));
                }
                if let Some(l) = kid(p, "text:list-style") {
                    s.list = Some(std::rc::Rc::new(list_levels(l)));
                }
                // Text columns (`style:columns`), kept with the graphic properties.
                if let Some(c) = kid(p, "style:columns") {
                    for k in ["fo:column-count", "fo:column-gap"] {
                        if let Some(v) = attr(c, k) {
                            s.graphic.insert(k.to_string(), v.to_string());
                        }
                    }
                }
            }
            "style:paragraph-properties" => {
                s.paragraph.extend(read_props(p));
                if let Some(t) = kid(p, "style:tab-stops") {
                    s.tabs = Some(std::rc::Rc::new(t.clone()));
                }
            }
            "style:text-properties" => s.text.extend(read_props(p)),
            "style:drawing-page-properties" => s.page.extend(read_props(p)),
            "style:table-cell-properties" => s.cell.extend(read_props(p)),
            "style:table-column-properties" => s.column.extend(read_props(p)),
            "style:table-row-properties" => s.row.extend(read_props(p)),
            "style:table-properties" => s.table.extend(read_props(p)),
            "style:chart-properties" => s.chart.extend(read_props(p)),
            _ => {}
        }
    }
    s
}

impl Store {
    /// Read `office:font-face-decls`, `office:styles`, `office:automatic-styles` of one document.
    pub fn add_document(&mut self, root: &El, scope: Scope) {
        for decls in kids(root, "office:font-face-decls") {
            for f in kids(decls, "style:font-face") {
                if let Some(name) = attr(f, "style:name") {
                    let fam = attr(f, "svg:font-family").map(unquote).unwrap_or_else(|| name.to_string());
                    self.fonts.entry(name.to_string()).or_insert(fam);
                }
                if crate::dom::find(f, "svg:font-face-uri").is_some() && self.font_faces.len() < 256 {
                    self.font_faces.push(f.clone());
                }
            }
        }
        for styles in kids(root, "office:styles") {
            self.add_styles(styles, None);
        }
        for auto in kids(root, "office:automatic-styles") {
            self.add_styles(auto, Some(scope));
        }
    }

    fn add_styles(&mut self, parent: &El, auto: Option<Scope>) {
        for el in parent.elements() {
            let name = attr(el, "style:name").or_else(|| attr(el, "draw:name")).unwrap_or_default().to_string();
            match el.name.as_str() {
                "style:style" => {
                    let s = read_style(el);
                    let key = (s.family.clone(), name.clone());
                    if auto.is_none()
                        && let Some(d) = attr(el, "style:display-name")
                    {
                        self.common_names.push((name, d.to_string()));
                    }
                    self.layer_mut(auto).styles.insert(key, s);
                }
                "style:default-style" => {
                    let s = read_style(el);
                    self.defaults.insert(s.family.clone(), s);
                }
                "text:list-style" => {
                    self.layer_mut(auto).lists.insert(name, list_levels(el));
                }
                "draw:gradient" | "svg:linearGradient" | "svg:radialGradient" => {
                    self.gradients.insert(name, el.clone());
                }
                "draw:hatch" => {
                    self.hatches.insert(name, el.clone());
                }
                "draw:fill-image" => {
                    self.fill_images.insert(name, el.clone());
                }
                "draw:marker" => {
                    self.markers.insert(name, el.clone());
                }
                "draw:stroke-dash" => {
                    self.dashes.insert(name, el.clone());
                }
                "draw:opacity" => {
                    self.opacities.insert(name, el.clone());
                }
                "number:date-style" | "number:time-style" if self.date_formats.len() < MAX_DATA_STYLES => {
                    if let Some(n) = crate::text::date_format_by_parts(el) {
                        self.date_formats.insert(name, n);
                    }
                }
                "style:presentation-page-layout" if self.presentation_layouts.len() < MAX_DATA_STYLES => {
                    self.presentation_layouts.insert(name, el.clone());
                }
                "style:page-layout" => {
                    let props = kid(el, "style:page-layout-properties").map(read_props).unwrap_or_default();
                    self.page_layouts.insert(name, props);
                }
                _ => {}
            }
        }
    }

    pub fn font_family(&self, font_name: &str) -> String {
        self.fonts.get(font_name).cloned().unwrap_or_else(|| font_name.to_string())
    }

    /// Common styles (`None`) or one file's automatic styles.
    fn layer(&self, auto: Option<Scope>) -> &Layer {
        match auto {
            None => &self.common,
            Some(Scope::Styles) => &self.auto_styles,
            Some(Scope::Content) => &self.auto_content,
        }
    }

    fn layer_mut(&mut self, auto: Option<Scope>) -> &mut Layer {
        match auto {
            None => &mut self.common,
            Some(Scope::Styles) => &mut self.auto_styles,
            Some(Scope::Content) => &mut self.auto_content,
        }
    }

    /// A style by name: the element's file's automatic styles first, then the common ones.
    fn lookup(&self, scope: Scope, family: &str, name: &str) -> Option<&Style> {
        let key = (family.to_string(), name.to_string());
        self.layer(Some(scope)).styles.get(&key).or_else(|| self.common.styles.get(&key))
    }

    pub fn exists(&self, scope: Scope, family: &str, name: &str) -> bool {
        self.lookup(scope, family, name).is_some()
    }

    /// The style's own properties merged over its parents', without the family default.
    pub fn chain(&self, scope: Scope, family: &str, name: &str) -> Style {
        self.chain_until(scope, family, name, &|_| false)
    }

    /// The named style merged with its parents up to (not including) the first one `stop`
    /// accepts: what a style adds to a common style it derives from.
    pub fn chain_until(&self, scope: Scope, family: &str, name: &str, stop: &dyn Fn(&str) -> bool) -> Style {
        let mut chain = vec![];
        let mut cur = Some(name).filter(|n| !stop(n)).and_then(|n| self.lookup(scope, family, n));
        while let Some(s) = cur {
            if chain.len() >= MAX_CHAIN {
                break;
            }
            chain.push(s);
            // Parents are common styles (an automatic style can't be a parent).
            cur = s.parent.as_deref().filter(|p| !stop(p)).and_then(|p| self.common.styles.get(&(family.to_string(), p.to_string())));
        }
        let mut out = Style { family: family.to_string(), ..Default::default() };
        for s in chain.iter().rev() {
            out.merge(s);
        }
        out
    }

    /// An outline placeholder's style at each level: the master's `{master}-outline{n}` with what
    /// the shape's own style adds over the outline styles laid on top. A missing level keeps the
    /// one above.
    pub fn outline_levels(&self, scope: Scope, master: &str, shape_style: Option<&str>, resolved: &Style) -> Vec<Style> {
        let prefix = format!("{master}-outline");
        let own = shape_style.map(|n| self.chain_until(scope, "presentation", n, &|p| p.starts_with(&prefix))).unwrap_or_default();
        let mut out: Vec<Style> = vec![];
        for level in 1..=OUTLINE_LEVELS {
            let name = format!("{prefix}{level}");
            let s = if self.exists(scope, "presentation", &name) {
                let mut s = self.resolve(scope, "presentation", Some(&name));
                s.merge(&own);
                // A level without a list of its own uses the levels' list (the bullets of every
                // level live in one list style).
                if s.list.is_none() {
                    s.list = out.last().map_or(&resolved.list, |l| &l.list).clone();
                }
                s
            } else {
                out.last().cloned().unwrap_or_else(|| resolved.clone())
            };
            out.push(s);
        }
        out
    }

    /// The family default style over the built-in defaults (presentation styles fall back to the
    /// graphic default).
    pub fn default_of(&self, family: &str) -> Style {
        let mut out = Style::default();
        if family == "graphic" || family == "presentation" {
            out.graphic.extend(POOL_GRAPHIC.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        }
        if family == "presentation"
            && let Some(g) = self.defaults.get("graphic")
        {
            out.merge(g);
        }
        if let Some(d) = self.defaults.get(family) {
            out.merge(d);
        }
        out
    }

    /// A background's fill properties: only when they set a fill themselves (a page or background
    /// style without one shows what's behind it), over the graphic defaults for what they leave
    /// unset (the colour of a bare `draw:fill="solid"`).
    pub fn background_fill(&self, own: &Props) -> Option<Props> {
        if !own.contains_key("draw:fill") {
            return None;
        }
        let mut props = self.default_of("graphic").graphic;
        props.extend(own.iter().map(|(k, v)| (k.clone(), v.clone())));
        Some(props)
    }

    /// The default style merged with the named style's chain.
    pub fn resolve(&self, scope: Scope, family: &str, name: Option<&str>) -> Style {
        let mut out = self.default_of(family);
        if let Some(n) = name {
            out.merge(&self.chain(scope, family, n));
        }
        out
    }

    /// Every list style, standalone or inside a style, for collecting picture bullets.
    pub fn all_lists(&self) -> Vec<&El> {
        let layers = [&self.common, &self.auto_styles, &self.auto_content];
        let standalone = layers.iter().flat_map(|l| l.lists.values());
        let in_styles = layers.iter().flat_map(|l| l.styles.values()).filter_map(|s| s.list.as_deref());
        standalone.chain(in_styles).collect()
    }

    pub fn list_style(&self, scope: Scope, name: &str) -> Option<&El> {
        self.layer(Some(scope)).lists.get(name).or_else(|| self.common.lists.get(name))
    }
}

/// A list style with at most its ten levels (ODF has ten; a file may list thousands, and
/// each paragraph looks through them).
fn list_levels(l: &El) -> El {
    let mut out = l.clone();
    let mut kept = 0;
    out.children.retain(|c| match c {
        deckcraft_pptx::XmlNode::El(e) if e.name.starts_with("text:list-level-style") => {
            kept += 1;
            kept <= 10
        }
        _ => true,
    });
    out
}

/// A colour at an opacity (0–1; opaque when `None` or 1).
pub fn color_ref(c: deckcraft_color::Rgba, opacity: Option<f64>) -> deckcraft_model::style::ColorRef {
    let mut r = deckcraft_model::style::ColorRef::rgb(c);
    if let Some(a) = opacity.filter(|a| a.is_finite() && *a < 1.0) {
        r.mods.push(deckcraft_color::ColorTransform::Alpha((a.max(0.0) * 100_000.0).round() as i32));
    }
    r
}

/// A colour property at the opacity a percentage property gives it.
pub fn color_prop(p: &Props, color: &str, opacity: &str) -> Option<deckcraft_model::style::ColorRef> {
    let c = p.get(color).and_then(|v| units::color(v))?;
    Some(color_ref(c, p.get(opacity).and_then(|v| units::percent(v))))
}

/// `'Liberation Sans'` → `Liberation Sans`; a family list keeps its first entry.
pub fn unquote(s: &str) -> String {
    let first = s.split(',').next().unwrap_or(s).trim();
    first.trim_matches(|c| c == '\'' || c == '"').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(xml: &str) -> Store {
        let root = crate::dom::parse(xml.as_bytes()).unwrap();
        let mut s = Store::default();
        s.add_document(&root, Scope::Styles);
        s
    }

    const NS: &str = r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" xmlns:s="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0""#;

    #[test]
    fn chains_merge_and_relative_sizes_apply() {
        let s = store(&format!(
            r##"<office:document-styles {NS}><office:font-face-decls><style:font-face style:name="LS" s:font-family="'Liberation Sans'"/></office:font-face-decls><office:styles>
            <style:default-style style:family="graphic"><style:text-properties fo:font-size="18pt" style:font-name="LS"/></style:default-style>
            <style:style style:name="a" style:family="presentation"><style:text-properties fo:font-size="40pt" fo:color="#ff0000"/></style:style>
            <style:style style:name="b" style:family="presentation" style:parent-style-name="a"><style:text-properties fo:font-size="50%"/></style:style>
            <style:style style:name="loop" style:family="presentation" style:parent-style-name="loop"/>
            </office:styles></office:document-styles>"##
        ));
        let r = s.resolve(Scope::Styles, "presentation", Some("b"));
        assert_eq!(r.text.get("fo:font-size").map(String::as_str), Some("20pt"));
        assert_eq!(r.text.get("fo:color").map(String::as_str), Some("#ff0000"));
        assert_eq!(s.font_family("LS"), "Liberation Sans");
        // A percentage of a percentage multiplies.
        let mut base = Props::new();
        base.insert("fo:font-size".into(), "80%".into());
        let mut over = Props::new();
        over.insert("fo:font-size".into(), "50%".into());
        merge_text(&mut base, &over);
        assert_eq!(base.get("fo:font-size").map(String::as_str), Some("40%"));
        // A self-parented style terminates.
        let _ = s.resolve(Scope::Styles, "presentation", Some("loop"));
    }
}
