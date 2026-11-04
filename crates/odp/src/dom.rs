//! The XML DOM: the PPTX crate's bounded reader, with every element and attribute name rewritten
//! to the conventional OpenDocument prefix of its namespace (`draw:`, `fo:`, `style:` …), so a file
//! that binds other prefixes reads the same. Names are then matched exactly.

pub use deckcraft_pptx::XmlEl as El;

use crate::OdpError;

/// Namespace URIs and the prefixes this crate matches them by.
const NAMESPACES: &[(&str, &str)] = &[
    ("urn:oasis:names:tc:opendocument:xmlns:office:1.0", "office"),
    ("urn:oasis:names:tc:opendocument:xmlns:style:1.0", "style"),
    ("urn:oasis:names:tc:opendocument:xmlns:text:1.0", "text"),
    ("urn:oasis:names:tc:opendocument:xmlns:table:1.0", "table"),
    ("urn:oasis:names:tc:opendocument:xmlns:drawing:1.0", "draw"),
    ("urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0", "fo"),
    ("http://www.w3.org/1999/xlink", "xlink"),
    ("http://purl.org/dc/elements/1.1/", "dc"),
    ("urn:oasis:names:tc:opendocument:xmlns:meta:1.0", "meta"),
    ("urn:oasis:names:tc:opendocument:xmlns:datastyle:1.0", "number"),
    ("urn:oasis:names:tc:opendocument:xmlns:presentation:1.0", "presentation"),
    ("urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0", "svg"),
    ("urn:oasis:names:tc:opendocument:xmlns:chart:1.0", "chart"),
    ("urn:oasis:names:tc:opendocument:xmlns:dr3d:1.0", "dr3d"),
    ("urn:oasis:names:tc:opendocument:xmlns:animation:1.0", "anim"),
    ("urn:oasis:names:tc:opendocument:xmlns:smil-compatible:1.0", "smil"),
    ("urn:oasis:names:tc:opendocument:xmlns:manifest:1.0", "manifest"),
    ("urn:oasis:names:tc:opendocument:xmlns:of:1.2", "of"),
    ("urn:oasis:names:tc:opendocument:xmlns:script:1.0", "script"),
    ("urn:oasis:names:tc:opendocument:xmlns:form:1.0", "form"),
    ("http://www.w3.org/1998/Math/MathML", "math"),
    ("urn:org:documentfoundation:names:experimental:office:xmlns:loext:1.0", "loext"),
    ("http://openoffice.org/2009/office", "officeooo"),
    ("http://openoffice.org/2010/draw", "drawooo"),
    ("http://openoffice.org/2004/office", "ooo"),
];

/// The prefix that names bound to a namespace this crate does not know move to, when the file
/// writes them with one of the canonical prefixes (`xmlns:draw="urn:example"`). No reader
/// matches it.
const FOREIGN: &str = "foreign-ns";

/// Parse a part and canonicalise its names.
pub fn parse(bytes: &[u8]) -> Result<El, OdpError> {
    let doc = deckcraft_pptx::parse_xml(bytes).map_err(|e| OdpError::Xml(e.to_string()))?;
    let mut root = doc.root;
    canonicalise(&mut root, &mut Scope::new(), 0);
    Ok(root)
}

fn canonical_prefix(uri: &str) -> Option<&'static str> {
    NAMESPACES.iter().find(|(u, _)| *u == uri).map(|(_, p)| *p)
}

fn is_canonical(prefix: &str) -> bool {
    NAMESPACES.iter().any(|(_, p)| *p == prefix)
}

/// A prefix binding in scope: the prefix as the file wrote it (`""` for the default namespace) and
/// the prefix its names are rewritten to (`None`: left as written).
type Binding = Option<&'static str>;

/// The bindings in scope: each prefix's, innermost last, so a lookup is one map access however
/// many prefixes a document declares.
type Scope = std::collections::HashMap<String, Vec<Binding>>;

/// What a namespace declaration binds its prefix to.
fn target(prefix: &str, uri: &str) -> Option<&'static str> {
    match canonical_prefix(uri) {
        Some(c) => Some(c),
        // An unknown namespace under a canonical prefix: its names are not ODF names.
        None if is_canonical(prefix) => Some(FOREIGN),
        // An unknown namespace under its own prefix matches nothing anyway; an unknown default
        // namespace leaves unprefixed names unprefixed.
        None => None,
    }
}

/// A name's new spelling under `scope` (innermost binding last), if it changes. Element names take
/// the default namespace; unprefixed attributes have none. `xmlns` and `xml` are never rewritten.
fn renamed(name: &str, scope: &Scope, element: bool) -> Option<String> {
    let (p, l) = match name.split_once(':') {
        Some(("xmlns" | "xml", _)) => return None,
        Some(pl) => pl,
        None if element => ("", name),
        None => return None,
    };
    let c = (*scope.get(p)?.last()?)?;
    (c != p).then(|| format!("{c}:{l}"))
}

/// Rewrite prefixes in scope (declared on this element or an ancestor) to the canonical ones.
///
/// An element pushes its own declarations onto their prefixes' stacks and pops them when done,
/// so no map is copied per element.
fn canonicalise(el: &mut El, scope: &mut Scope, depth: usize) {
    if depth > crate::read::MAX_DEPTH {
        return;
    }
    let mut declared: Vec<String> = vec![];
    for (k, v) in &el.attrs {
        let prefix = if k == "xmlns" {
            ""
        } else if let Some(p) = k.strip_prefix("xmlns:") {
            p
        } else {
            continue;
        };
        scope.entry(prefix.to_string()).or_default().push(target(prefix, v));
        declared.push(prefix.to_string());
    }
    if let Some(n) = renamed(&el.name, scope, true) {
        el.name = n;
    }
    for (k, _) in &mut el.attrs {
        if let Some(n) = renamed(k, scope, false) {
            *k = n;
        }
    }
    for c in &mut el.children {
        if let deckcraft_pptx::XmlNode::El(e) = c {
            canonicalise(e, scope, depth + 1);
        }
    }
    for p in declared {
        if let Some(stack) = scope.get_mut(&p) {
            stack.pop();
        }
    }
}

/// Exact-name attribute lookup.
/// A `*-repeated` count (at least 1).
pub fn repeat(e: &El, k: &str) -> usize {
    attr(e, k).and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).max(1)
}

pub fn attr<'a>(el: &'a El, name: &str) -> Option<&'a str> {
    el.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

/// Child elements with this exact name.
pub fn kids<'a>(el: &'a El, name: &'a str) -> impl Iterator<Item = &'a El> + 'a {
    el.elements().filter(move |e| e.name == name)
}

pub fn kid<'a>(el: &'a El, name: &str) -> Option<&'a El> {
    el.elements().find(|e| e.name == name)
}

/// Depth-first search for the first descendant with this name (bounded).
pub fn find<'a>(el: &'a El, name: &str) -> Option<&'a El> {
    fn rec<'a>(el: &'a El, name: &str, depth: usize) -> Option<&'a El> {
        if depth > crate::read::MAX_DEPTH {
            return None;
        }
        for e in el.elements() {
            if e.name == name {
                return Some(e);
            }
            if let Some(f) = rec(e, name, depth + 1) {
                return Some(f);
            }
        }
        None
    }
    rec(el, name, 0)
}

/// XML text escaping for writing.
pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            // Characters XML 1.0 forbids.
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            '\u{FFFE}' | '\u{FFFF}' => {}
            c => o.push(c),
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    const DRAW: &str = "urn:oasis:names:tc:opendocument:xmlns:drawing:1.0";
    const SVG: &str = "urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0";

    fn names(el: &El, out: &mut Vec<String>) {
        out.push(el.name.clone());
        out.extend(el.attrs.iter().map(|(k, _)| k.clone()));
        for e in el.elements() {
            names(e, out);
        }
    }

    #[test]
    fn other_prefixes_read_as_canonical() {
        let x = format!(r#"<r xmlns:d="{DRAW}" xmlns:s="{SVG}"><d:frame s:x="1cm" d:name="a"><d:image/></d:frame></r>"#);
        let r = parse(x.as_bytes()).unwrap();
        let f = kid(&r, "draw:frame").unwrap();
        assert_eq!(attr(f, "svg:x"), Some("1cm"));
        assert_eq!(attr(f, "draw:name"), Some("a"));
        assert!(kid(f, "draw:image").is_some());
    }

    /// Regression: a canonical prefix bound to another namespace kept its name, so its elements
    /// and attributes were read as ODF `draw:` ones.
    #[test]
    fn a_canonical_prefix_bound_elsewhere_is_not_odf() {
        let x =
            format!(r#"<r xmlns:draw="urn:example:not-odf" xmlns:d="{DRAW}"><draw:frame draw:name="fake"/><d:frame d:name="real" draw:x="1"/></r>"#);
        let r = parse(x.as_bytes()).unwrap();
        let frames: Vec<&El> = kids(&r, "draw:frame").collect();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames.first().and_then(|f| attr(f, "draw:name")), Some("real"));
        assert_eq!(frames.first().and_then(|f| attr(f, "draw:x")), None);
        let mut all = Vec::new();
        names(&r, &mut all);
        assert!(all.iter().any(|n| n == "foreign-ns:frame"), "{all:?}");
        // The declarations themselves keep their names.
        assert_eq!(attr(&r, "xmlns:draw"), Some("urn:example:not-odf"));
    }

    #[test]
    fn bindings_are_scoped() {
        // An inner element re-binds `draw` elsewhere; the sibling after it sees the outer binding.
        let x = format!(
            r#"<r xmlns:draw="{DRAW}"><a xmlns:draw="urn:example"><draw:frame/></a><draw:frame/><b xmlns:x="{DRAW}"><x:page/></b><x:page/></r>"#
        );
        let r = parse(x.as_bytes()).unwrap();
        assert!(kid(kid(&r, "a").unwrap(), "draw:frame").is_none());
        assert_eq!(kids(&r, "draw:frame").count(), 1);
        assert!(kid(kid(&r, "b").unwrap(), "draw:page").is_some());
        // Undeclared outside `b`: left as written.
        assert!(kid(&r, "x:page").is_some());
    }

    #[test]
    fn default_namespace_names_elements_not_attributes() {
        let x = format!(r#"<r><frame xmlns="{DRAW}" name="n"><image/></frame><frame/></r>"#);
        let r = parse(x.as_bytes()).unwrap();
        let f = kid(&r, "draw:frame").unwrap();
        assert_eq!(attr(f, "name"), Some("n"));
        assert!(kid(f, "draw:image").is_some());
        assert!(kid(&r, "frame").is_some());
        // `xml:` is never rewritten.
        let r = parse(br#"<r xml:lang="en"/>"#).unwrap();
        assert_eq!(attr(&r, "xml:lang"), Some("en"));
    }
}
