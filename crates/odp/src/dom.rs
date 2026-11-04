//! The XML DOM: the PPTX crate's bounded reader, with every element and attribute name rewritten
//! to the conventional OpenDocument prefix of its namespace (`draw:`, `fo:`, `style:` …), so a file
//! that binds other prefixes reads the same. Names are then matched exactly.

use std::collections::HashMap;

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

/// Parse a part and canonicalise its names.
pub fn parse(bytes: &[u8]) -> Result<El, OdpError> {
    let doc = deckcraft_pptx::parse_xml(bytes).map_err(|e| OdpError::Xml(e.to_string()))?;
    let mut root = doc.root;
    canonicalise(&mut root, &HashMap::new(), 0);
    Ok(root)
}

fn canonical_prefix(uri: &str) -> Option<&'static str> {
    NAMESPACES.iter().find(|(u, _)| *u == uri).map(|(_, p)| *p)
}

/// Rewrite prefixes in scope (declared on this element or an ancestor) to the canonical ones.
fn canonicalise(el: &mut El, outer: &HashMap<String, String>, depth: usize) {
    if depth > crate::read::MAX_DEPTH {
        return;
    }
    let mut map = outer.clone();
    let mut local_decls = false;
    for (k, v) in &el.attrs {
        if let Some(p) = k.strip_prefix("xmlns:")
            && let Some(c) = canonical_prefix(v)
        {
            map.insert(p.to_string(), c.to_string());
            local_decls = true;
        }
    }
    let map = if local_decls { map } else { outer.clone() };
    let fix = |name: &mut String| {
        if let Some((p, l)) = name.split_once(':')
            && p != "xmlns"
            && let Some(c) = map.get(p)
            && c != p
        {
            *name = format!("{c}:{l}");
        }
    };
    fix(&mut el.name);
    for (k, _) in &mut el.attrs {
        fix(k);
    }
    for c in &mut el.children {
        if let deckcraft_pptx::XmlNode::El(e) = c {
            canonicalise(e, &map, depth + 1);
        }
    }
}

/// Exact-name attribute lookup.
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
