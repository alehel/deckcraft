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
    pub list: Option<El>,
    /// `style:tab-stops` of the paragraph properties.
    pub tabs: Option<El>,
}

impl Style {
    pub fn chart_prop(&self, k: &str) -> Option<&str> {
        self.chart.get(k).map(String::as_str)
    }

    fn merge(&mut self, over: &Style) {
        for (k, v) in &over.graphic {
            self.graphic.insert(k.clone(), v.clone());
        }
        for (k, v) in &over.paragraph {
            self.paragraph.insert(k.clone(), v.clone());
        }
        merge_text(&mut self.text, &over.text);
        for (k, v) in &over.page {
            self.page.insert(k.clone(), v.clone());
        }
        for (k, v) in &over.cell {
            self.cell.insert(k.clone(), v.clone());
        }
        for (k, v) in &over.column {
            self.column.insert(k.clone(), v.clone());
        }
        for (k, v) in &over.row {
            self.row.insert(k.clone(), v.clone());
        }
        for (k, v) in &over.table {
            self.table.insert(k.clone(), v.clone());
        }
        for (k, v) in &over.chart {
            self.chart.insert(k.clone(), v.clone());
        }
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
            && let Some(abs) = base.get(k.as_str()).and_then(|b| units::length(b))
        {
            base.insert(k.clone(), units::pt(abs * f));
            continue;
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

#[derive(Debug, Default)]
pub struct Store {
    common: HashMap<(String, String), Style>,
    auto_styles: HashMap<(String, String), Style>,
    auto_content: HashMap<(String, String), Style>,
    defaults: HashMap<String, Style>,
    lists_common: HashMap<String, El>,
    lists_auto_styles: HashMap<String, El>,
    lists_auto_content: HashMap<String, El>,
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
    /// Display names, for writing styles back (`Default-title` etc. are plain already).
    pub common_names: Vec<(String, String)>,
}

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
                    s.list = Some(l.clone());
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
                    s.tabs = Some(t.clone());
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
                    match auto {
                        None => {
                            if let Some(d) = attr(el, "style:display-name") {
                                self.common_names.push((name, d.to_string()));
                            }
                            self.common.insert(key, s);
                        }
                        Some(Scope::Styles) => {
                            self.auto_styles.insert(key, s);
                        }
                        Some(Scope::Content) => {
                            self.auto_content.insert(key, s);
                        }
                    }
                }
                "style:default-style" => {
                    let s = read_style(el);
                    self.defaults.insert(s.family.clone(), s);
                }
                "text:list-style" => {
                    let map = match auto {
                        None => &mut self.lists_common,
                        Some(Scope::Styles) => &mut self.lists_auto_styles,
                        Some(Scope::Content) => &mut self.lists_auto_content,
                    };
                    map.insert(name, el.clone());
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

    fn lookup(&self, scope: Scope, family: &str, name: &str) -> Option<&Style> {
        let key = (family.to_string(), name.to_string());
        let auto = match scope {
            Scope::Content => self.auto_content.get(&key),
            Scope::Styles => self.auto_styles.get(&key),
        };
        auto.or_else(|| self.common.get(&key))
    }

    /// The style's own properties, without its parents.
    pub fn own(&self, scope: Scope, family: &str, name: &str) -> Option<Style> {
        self.lookup(scope, family, name).cloned()
    }

    pub fn exists(&self, scope: Scope, family: &str, name: &str) -> bool {
        self.lookup(scope, family, name).is_some()
    }

    /// The style's own properties merged over its parents', without the family default.
    pub fn chain(&self, scope: Scope, family: &str, name: &str) -> Style {
        let mut chain = vec![];
        let mut cur = self.lookup(scope, family, name);
        while let Some(s) = cur {
            if chain.len() >= MAX_CHAIN {
                break;
            }
            chain.push(s);
            // Parents are common styles (an automatic style can't be a parent).
            cur = s.parent.as_deref().and_then(|p| self.common.get(&(family.to_string(), p.to_string())));
        }
        let mut out = Style { family: family.to_string(), ..Default::default() };
        for s in chain.iter().rev() {
            out.merge(s);
        }
        out
    }

    /// The family default style (presentation styles fall back to the graphic default).
    pub fn default_of(&self, family: &str) -> Style {
        let mut out = Style::default();
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
        let mut out: Vec<&El> = self.lists_common.values().chain(self.lists_auto_styles.values()).chain(self.lists_auto_content.values()).collect();
        for s in self.common.values().chain(self.auto_styles.values()).chain(self.auto_content.values()) {
            if let Some(l) = &s.list {
                out.push(l);
            }
        }
        out
    }

    pub fn list_style(&self, scope: Scope, name: &str) -> Option<&El> {
        let auto = match scope {
            Scope::Content => self.lists_auto_content.get(name),
            Scope::Styles => self.lists_auto_styles.get(name),
        };
        auto.or_else(|| self.lists_common.get(name))
    }
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
        // A self-parented style terminates.
        let _ = s.resolve(Scope::Styles, "presentation", Some("loop"));
    }
}
