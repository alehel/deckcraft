//! Text: `text:p`, `text:h`, `text:list` with spans, spaces, tabs, line breaks, links and fields,
//! resolved through paragraph, text and list styles into model paragraphs with explicit formatting.

use deckcraft_color::Rgba;
use deckcraft_model::style::{ColorRef, Fill, Line, Shadow};
use deckcraft_model::text::{Action, Align, Bullet, Caps, Hyperlink, ParaProps, Paragraph, Run, RunKind, RunProps, Spacing, Strike, TabStop};

use crate::dom::{El, attr, kid, kids};
use crate::styles::{Props, Scope, Store, Style, merge_text, unquote};
use crate::units;

/// Most paragraphs read from one text body.
const MAX_PARAGRAPHS: usize = 20_000;
/// Deepest list nesting read (ODF has ten levels; deeper lists keep the last).
const MAX_LIST_DEPTH: usize = 10;

/// What a text body is read against.
pub struct TextCtx<'a> {
    pub store: &'a Store,
    pub scope: Scope,
    /// The shape's resolved style at each list depth, the last one serving deeper levels: one
    /// style for most shapes; an outline placeholder's levels each take the master's outline style
    /// ([`Store::outline_levels`]).
    pub levels: Vec<Style>,
    /// Paragraph style named by the shape's `draw:text-style-name`.
    pub shape_para: Option<Style>,
    /// The colour of automatic text: black, or white over a dark fill or background.
    pub auto_color: Rgba,
    /// Leave what the styles don't set to the master (title and outline placeholders); otherwise
    /// paragraphs carry LibreOffice's defaults for it.
    pub inherit: bool,
}

struct ListCtx<'a> {
    style: Option<&'a El>,
    depth: usize,
    /// Inside a `text:list-item` (bulleted) rather than a `text:list-header`.
    bulleted: bool,
    /// Inside a list at all (items and headers take its indents).
    in_list: bool,
    start: Option<u32>,
}

/// Read the paragraphs of a text container (`draw:text-box`, a shape, a table cell).
pub fn paragraphs(cx: &TextCtx, container: &El) -> Vec<Paragraph> {
    let mut out = vec![];
    let shape_list = cx.levels.first().and_then(|s| s.list.as_ref());
    walk(cx, container, &ListCtx { style: shape_list, depth: 0, bulleted: false, in_list: false, start: None }, &mut out, 0);
    out
}

fn walk(cx: &TextCtx, el: &El, list: &ListCtx, out: &mut Vec<Paragraph>, guard: usize) {
    if guard > 64 {
        return;
    }
    for e in el.elements() {
        if out.len() >= MAX_PARAGRAPHS {
            return;
        }
        match e.name.as_str() {
            "text:p" | "text:h" => out.push(paragraph(cx, e, list)),
            "text:list" => {
                let own = attr(e, "text:style-name").and_then(|n| cx.store.list_style(cx.scope, n));
                let style = own.or(list.style);
                // The outermost list is level 0; each nested list one deeper.
                let depth = if guard_is_list(el) { (list.depth + 1).min(MAX_LIST_DEPTH - 1) } else { list.depth };
                for item in e.elements() {
                    let bulleted = item.name == "text:list-item";
                    if !bulleted && item.name != "text:list-header" {
                        continue;
                    }
                    let style = attr(item, "text:style-override").and_then(|n| cx.store.list_style(cx.scope, n)).or(style);
                    let start = attr(item, "text:start-value").and_then(|v| v.parse::<u32>().ok());
                    let inner = ListCtx { style, depth, bulleted, in_list: true, start };
                    walk(cx, item, &inner, out, guard + 1);
                }
            }
            // Text inside other wrappers (sections, soft page breaks) still counts.
            "text:section" | "text:soft-page-break" => walk(cx, e, list, out, guard + 1),
            _ => {}
        }
    }
}

/// The `xml:id` (or `text:id`) of each paragraph [`paragraphs`] reads from `container`, in order.
pub fn paragraph_ids(container: &El) -> Vec<Option<String>> {
    fn rec(el: &El, out: &mut Vec<Option<String>>, depth: usize) {
        if depth > 64 || out.len() >= MAX_PARAGRAPHS {
            return;
        }
        for e in el.elements() {
            match e.name.as_str() {
                "text:p" | "text:h" => out.push(attr(e, "xml:id").or_else(|| attr(e, "text:id")).map(str::to_string)),
                "text:list" | "text:list-item" | "text:list-header" | "text:section" | "text:soft-page-break" => rec(e, out, depth + 1),
                _ => {}
            }
        }
    }
    let mut out = vec![];
    rec(container, &mut out, 0);
    out
}

/// A `text:list` nested directly in a list item sits one level deeper than its parent list.
fn guard_is_list(parent: &El) -> bool {
    parent.name == "text:list-item" || parent.name == "text:list-header"
}

/// The level style of a list for level `depth` (0-based): the closest `text:level` at or below.
fn level_style(list: &El, depth: usize) -> Option<&El> {
    let want = depth + 1;
    let mut best: Option<(usize, &El)> = None;
    for l in list.elements() {
        if !l.name.starts_with("text:list-level-style") {
            continue;
        }
        let lv = attr(l, "text:level").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1);
        if lv == want {
            return Some(l);
        }
        if lv < want && best.is_none_or(|(b, _)| lv > b) {
            best = Some((lv, l));
        }
    }
    best.map(|(_, l)| l)
}

/// The merged style of a paragraph in this body at `depth`: the shape's style at that level, the
/// shape's text style, the paragraph's own.
fn para_style(cx: &TextCtx, depth: usize, p_style: Option<&str>) -> Style {
    let mut s = cx.levels.get(depth).or(cx.levels.last()).cloned().unwrap_or_default();
    if let Some(sp) = &cx.shape_para {
        s.merge(sp);
    }
    if let Some(n) = p_style {
        s.merge(&cx.store.chain(cx.scope, "paragraph", n));
    }
    s
}

/// The paragraph and text formatting a style gives a paragraph at this list position.
fn props_at<'a>(cx: &TextCtx<'a>, style: &Style, list: &ListCtx<'a>) -> (ParaProps, RunProps) {
    let mut props = para_props(&style.paragraph, style.tabs.as_ref());
    let run = run_props(cx.store, &style.text, cx.auto_color);
    // The list's level, else the one the paragraph style names (`style:list-style-name`).
    let at_depth = |l: &'a El| level_style(l, list.depth);
    let named = || style.list_style_name.as_deref().and_then(|n| cx.store.list_style(cx.scope, n)).and_then(at_depth);
    let level_el = if list.in_list { list.style.and_then(at_depth).or_else(named) } else { None };
    apply_list_level(cx.store, &mut props, level_el, list, run.size);
    (props, run)
}

/// One level of a text style: the formatting a paragraph at `depth` (in a list, or not) gets from
/// `style` alone.
pub fn level_props(store: &Store, scope: Scope, style: &Style, depth: usize, in_list: bool) -> (ParaProps, RunProps) {
    let cx = TextCtx { store, scope, levels: vec![style.clone()], shape_para: None, auto_color: Rgba::BLACK, inherit: false };
    let list = ListCtx { style: style.list.as_ref(), depth: depth.min(MAX_LIST_DEPTH - 1), bulleted: in_list, in_list, start: None };
    props_at(&cx, style, &list)
}

fn paragraph(cx: &TextCtx, p: &El, list: &ListCtx) -> Paragraph {
    let style = para_style(cx, list.depth, attr(p, "text:style-name"));
    let (mut props, base_run) = props_at(cx, &style, list);
    if !cx.inherit {
        complete(&mut props);
    } else if !list.in_list {
        // Out of a list the master level's list indents don't apply.
        props.margin_left.get_or_insert(0.0);
        props.indent.get_or_insert(0.0);
    }
    let mut para = Paragraph { level: list.depth.min(8) as u8, props, runs: vec![], def_props: RunProps::default(), end_props: base_run.clone() };
    let mut st = RunState { space_pending: false, at_start: true };
    inline(cx, p, &style.text, None, &mut para.runs, &mut st, 0);
    // Collapsed white space at the end was never written (a pending space waits for text that
    // didn't come); spaces left are `text:s`, which are meant.
    para.runs.retain(|r| r.kind != RunKind::Text || !r.text.is_empty());
    para
}

struct RunState {
    /// A collapsed white-space run is waiting to be written as one space.
    space_pending: bool,
    at_start: bool,
}

fn push_text(runs: &mut Vec<Run>, props: &RunProps, text: &str, st: &mut RunState, link: Option<&Hyperlink>) {
    let mut s = String::new();
    for c in text.chars() {
        if matches!(c, ' ' | '\t' | '\n' | '\r') {
            st.space_pending = true;
        } else {
            if st.space_pending && !st.at_start {
                // A collapsed space between runs stays with the text before it.
                match runs.last_mut() {
                    Some(last) if s.is_empty() && last.kind == RunKind::Text => last.text.push(' '),
                    _ => s.push(' '),
                }
            }
            st.space_pending = false;
            st.at_start = false;
            s.push(c);
        }
    }
    push_literal(runs, props, &s, link);
}

/// Text that is not white-space collapsed (`text:s`, `text:tab`).
fn push_literal(runs: &mut Vec<Run>, props: &RunProps, s: &str, link: Option<&Hyperlink>) {
    if s.is_empty() {
        return;
    }
    let mut props = props.clone();
    if let Some(l) = link {
        props.link = Some(l.clone());
    }
    if let Some(last) = runs.last_mut()
        && last.kind == RunKind::Text
        && last.props == props
    {
        last.text.push_str(s);
        return;
    }
    runs.push(Run { text: s.to_string(), props, kind: RunKind::Text });
}

fn flush_space(runs: &mut Vec<Run>, props: &RunProps, st: &mut RunState, link: Option<&Hyperlink>) {
    if st.space_pending && !st.at_start {
        push_literal(runs, props, " ", link);
    }
    st.space_pending = false;
}

fn inline(cx: &TextCtx, el: &El, text_props: &Props, link: Option<&Hyperlink>, runs: &mut Vec<Run>, st: &mut RunState, depth: usize) {
    if depth > 32 {
        return;
    }
    let props = run_props(cx.store, text_props, cx.auto_color);
    for node in &el.children {
        match node {
            deckcraft_pptx::XmlNode::Text(t) => push_text(runs, &props, t, st, link),
            deckcraft_pptx::XmlNode::El(e) => match e.name.as_str() {
                "text:span" => {
                    let mut tp = text_props.clone();
                    if let Some(n) = attr(e, "text:style-name") {
                        let s = cx.store.chain(cx.scope, "text", n);
                        merge_text(&mut tp, &s.text);
                    }
                    inline(cx, e, &tp, link, runs, st, depth + 1);
                }
                "text:a" => {
                    let href = attr(e, "xlink:href").unwrap_or("");
                    let l = hyperlink(href);
                    inline(cx, e, text_props, l.as_ref().or(link), runs, st, depth + 1);
                }
                "text:s" => {
                    flush_space(runs, &props, st, link);
                    // Each asks for up to a thousand; the document has a budget for them all.
                    let used = cx.store.spaces_used.get();
                    let want = attr(e, "text:c").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).clamp(1, 1000);
                    let n = want.min(crate::styles::MAX_EXPANDED_SPACES.saturating_sub(used)).max(1);
                    cx.store.spaces_used.set(used.saturating_add(n));
                    push_literal(runs, &props, &" ".repeat(n), link);
                    st.at_start = false;
                }
                "text:tab" => {
                    flush_space(runs, &props, st, link);
                    push_literal(runs, &props, "\t", link);
                    st.at_start = false;
                }
                "text:line-break" => {
                    st.space_pending = false;
                    runs.push(Run { text: String::new(), props: props.clone(), kind: RunKind::Break });
                    st.at_start = true;
                }
                "text:page-number" | "text:page-count" | "text:date" | "text:time" | "presentation:date-time" => {
                    flush_space(runs, &props, st, link);
                    let field = match e.name.as_str() {
                        "text:page-number" => "slidenum",
                        "text:page-count" => "slidecount",
                        _ => "datetime1",
                    };
                    let shown = e.text();
                    let shown = if shown.is_empty() || shown.starts_with('<') {
                        if field == "slidenum" { "‹#›".to_string() } else { String::new() }
                    } else {
                        shown
                    };
                    let mut p = props.clone();
                    if let Some(l) = link {
                        p.link = Some(l.clone());
                    }
                    runs.push(Run { text: shown, props: p, kind: RunKind::Field { field: field.into() } });
                    st.at_start = false;
                }
                // Change tracking, bookmarks, annotations, notes: their text isn't shown.
                "text:bookmark"
                | "text:bookmark-start"
                | "text:bookmark-end"
                | "office:annotation"
                | "office:annotation-end"
                | "text:note"
                | "text:soft-page-break"
                | "text:reference-mark" => {}
                // Anything else (fields we don't model, meta wrappers): keep its text.
                _ => inline(cx, e, text_props, link, runs, st, depth + 1),
            },
        }
    }
}

/// `xlink:href` of a `text:a` or `draw:a`/`presentation:event-listener`.
pub fn hyperlink(href: &str) -> Option<Hyperlink> {
    let href = href.trim();
    if href.is_empty() {
        return None;
    }
    let action = if let Some(name) = href.strip_prefix('#') {
        // Resolved to a slide id by the reader once every page is known.
        Action::Url { url: format!("#{name}") }
    } else if deckcraft_model::text::safe_url(href) {
        Action::Url { url: href.to_string() }
    } else {
        return None;
    };
    Some(Hyperlink { action, tooltip: String::new(), highlight_click: false, sound: None, stop_sound: false, zoom_return: None })
}

/// Automatic text colour over `behind`: white on a dark colour, else black (LibreOffice's rule).
pub fn auto_text_color(behind: Rgba) -> Rgba {
    let lum = 0.299 * f64::from(behind.r) + 0.587 * f64::from(behind.g) + 0.114 * f64::from(behind.b);
    if lum <= 156.0 { Rgba::WHITE } else { Rgba::BLACK }
}

/// LibreOffice's defaults for the paragraph formatting a style leaves unset.
pub fn complete(p: &mut ParaProps) {
    p.align.get_or_insert(Align::Left);
    p.margin_left.get_or_insert(0.0);
    p.indent.get_or_insert(0.0);
    p.margin_right.get_or_insert(0.0);
    p.space_before.get_or_insert(Spacing::Pts(0.0));
    p.space_after.get_or_insert(Spacing::Pts(0.0));
    p.line_spacing.get_or_insert(Spacing::Pct(1.0));
    p.bullet.get_or_insert(Bullet::None);
}

/// Our alignment of an `fo:text-align` value.
pub fn align_of(v: &str) -> Option<Align> {
    match v {
        "start" | "left" => Some(Align::Left),
        "end" | "right" => Some(Align::Right),
        "center" => Some(Align::Center),
        "justify" => Some(Align::Justify),
        _ => None,
    }
}

/// The `fo:text-align` value of our alignment (ODF has one justification).
pub fn odf_align(a: Align) -> &'static str {
    match a {
        Align::Left => "start",
        Align::Center => "center",
        Align::Right => "end",
        Align::Justify | Align::JustLow | Align::Distributed | Align::ThaiDistributed => "justify",
    }
}

/// Paragraph properties from merged `style:paragraph-properties`.
pub fn para_props(p: &Props, tabs: Option<&El>) -> ParaProps {
    let len = |k: &str| p.get(k).and_then(|v| units::length(v));
    let mut pp = ParaProps {
        align: p.get("fo:text-align").and_then(|a| align_of(a)),
        margin_left: len("fo:margin-left"),
        margin_right: len("fo:margin-right"),
        indent: len("fo:text-indent"),
        space_before: len("fo:margin-top").map(Spacing::Pts),
        space_after: len("fo:margin-bottom").map(Spacing::Pts),
        ..Default::default()
    };
    if let Some(lh) = p.get("fo:line-height") {
        pp.line_spacing = units::percent(lh).map(|f| Spacing::Pct(f.clamp(0.0, 100.0))).or_else(|| units::length(lh).map(Spacing::Pts));
    } else if let Some(v) = len("style:line-height-at-least") {
        pp.line_spacing = Some(Spacing::Pts(v));
    }
    if p.get("style:writing-mode").is_some_and(|m| m.starts_with("rl")) {
        pp.rtl = Some(true);
    }
    pp.default_tab = len("style:tab-stop-distance");
    if let Some(t) = tabs {
        let stops: Vec<TabStop> = kids(t, "style:tab-stop")
            .take(64)
            .filter_map(|s| {
                let pos = attr(s, "style:position").and_then(units::length)?;
                let align = match attr(s, "style:type").unwrap_or("left") {
                    "right" => "r",
                    "center" => "ctr",
                    "char" => "dec",
                    _ => "l",
                };
                Some(TabStop { pos, align: align.into() })
            })
            .collect();
        if !stops.is_empty() {
            pp.tabs = Some(stops);
        }
    }
    pp
}

/// Bullets and the hanging indent of a list level.
fn apply_list_level(store: &Store, pp: &mut ParaProps, level: Option<&El>, list: &ListCtx, text_size: Option<f64>) {
    let Some(l) = level else {
        pp.bullet = Some(Bullet::None);
        return;
    };
    let props = kid(l, "style:list-level-properties");
    let lp = |k: &str| props.and_then(|e| attr(e, k)).and_then(units::length);
    // The list level's indents, added to the paragraph's own (LibreOffice writes the paragraph's
    // as zero and the indent in the list). ODF 1.2 "label-alignment" mode puts them on a nested
    // element.
    let (lm, li) = match props.and_then(|e| kid(e, "style:list-level-label-alignment")) {
        Some(a) => {
            (attr(a, "fo:margin-left").and_then(units::length).unwrap_or(0.0), attr(a, "fo:text-indent").and_then(units::length).unwrap_or(0.0))
        }
        None => {
            let label = lp("text:min-label-width").unwrap_or(0.0);
            (lp("text:space-before").unwrap_or(0.0) + label, -label)
        }
    };
    pp.margin_left = Some(pp.margin_left.unwrap_or(0.0) + lm);
    pp.indent = Some(pp.indent.unwrap_or(0.0) + li);
    if !list.bulleted {
        // A list header: indented, without its bullet.
        pp.bullet = Some(Bullet::None);
        return;
    }
    let tp = kid(l, "style:text-properties");
    let tattr = |k: &str| tp.and_then(|e| attr(e, k));
    if let Some(f) = tattr("style:font-name").map(|n| store.font_family(n)).or_else(|| tattr("fo:font-family").map(unquote)) {
        // OpenSymbol is LibreOffice's bullet font; its bullets are plain Unicode characters any
        // text font draws, so follow the text font.
        if f.eq_ignore_ascii_case("OpenSymbol") || f.eq_ignore_ascii_case("StarSymbol") {
            pp.bullet_font_follow_text = Some(true);
        } else {
            pp.bullet_font = Some(f);
        }
    }
    let rel = tattr("fo:font-size").and_then(units::percent).or_else(|| attr(l, "text:bullet-relative-size").and_then(units::percent));
    if let Some(r) = rel {
        pp.bullet_size = Some(r.clamp(0.25, 4.0));
    } else if let (Some(abs), Some(ts)) = (tattr("fo:font-size").and_then(units::length), text_size)
        && ts > 0.0
    {
        pp.bullet_size = Some((abs / ts).clamp(0.25, 4.0));
    }
    if tattr("style:use-window-font-color") != Some("true")
        && let Some(c) = tattr("fo:color").and_then(units::color)
    {
        pp.bullet_color = Some(ColorRef::rgb(c));
    } else {
        pp.bullet_color_follow_text = Some(true);
    }
    pp.bullet = Some(match l.name.as_str() {
        "text:list-level-style-number" => {
            let fmt = attr(l, "style:num-format").unwrap_or("1");
            if fmt.is_empty() {
                Bullet::None
            } else {
                let start = list.start.or_else(|| attr(l, "text:start-value").and_then(|v| v.parse().ok())).unwrap_or(1).clamp(1, 32767);
                Bullet::AutoNum {
                    scheme: num_scheme(fmt, attr(l, "style:num-prefix").unwrap_or(""), attr(l, "style:num-suffix").unwrap_or("")),
                    start_at: start,
                }
            }
        }
        "text:list-level-style-bullet" => {
            let c = attr(l, "text:bullet-char").unwrap_or("•");
            let c: String = c.chars().take(1).collect();
            if c.is_empty() { Bullet::None } else { Bullet::Char { char: c } }
        }
        // Picture bullets: the image the reader loaded for them.
        _ => match attr(l, "xlink:href").and_then(|h| store.image_bullets.get(h)) {
            Some(m) => Bullet::Picture { media: *m },
            None => Bullet::Char { char: "•".into() },
        },
    });
}

/// An ODF number format with prefix/suffix as the closest DrawingML auto-number scheme.
pub fn num_scheme(fmt: &str, prefix: &str, suffix: &str) -> String {
    let core = match fmt {
        "a" => "alphaLc",
        "A" => "alphaUc",
        "i" => "romanLc",
        "I" => "romanUc",
        _ => "arabic",
    };
    let tail = match (prefix.trim(), suffix.trim()) {
        ("(", ")") => "ParenBoth",
        (_, ")") => "ParenR",
        (_, ".") => "Period",
        (_, "") => "Plain",
        _ => "Period",
    };
    format!("{core}{tail}")
}

/// Run properties from merged `style:text-properties`.
pub fn run_props(store: &Store, t: &Props, auto: Rgba) -> RunProps {
    let g = |k: &str| t.get(k).map(String::as_str);
    let font = g("style:font-name").map(|n| store.font_family(n)).or_else(|| g("fo:font-family").map(unquote)).filter(|f| !f.is_empty());
    let mut rp = RunProps {
        font,
        font_ea: g("style:font-name-asian").map(|n| store.font_family(n)).or_else(|| g("style:font-family-asian").map(unquote)),
        font_cs: g("style:font-name-complex").map(|n| store.font_family(n)).or_else(|| g("style:font-family-complex").map(unquote)),
        size: g("fo:font-size").and_then(units::length).map(|v| v.clamp(1.0, 4000.0)),
        bold: g("fo:font-weight").map(|w| w == "bold" || w.parse::<u32>().is_ok_and(|n| n >= 600)),
        italic: g("fo:font-style").map(|s| s == "italic" || s == "oblique"),
        ..Default::default()
    };
    if let Some(u) = g("style:text-underline-style") {
        rp.underline = Some(if u == "none" {
            "none".into()
        } else {
            let double = g("style:text-underline-type") == Some("double");
            let heavy = g("style:text-underline-width") == Some("bold");
            match (u, double, heavy) {
                (_, true, _) => "dbl",
                ("dotted", _, true) => "dottedHeavy",
                ("dotted", _, _) => "dotted",
                ("dash", _, true) => "dashHeavy",
                ("dash", _, _) => "dash",
                ("long-dash", _, _) => "dashLong",
                ("dot-dash", _, _) => "dotDash",
                ("dot-dot-dash", _, _) => "dotDotDash",
                ("wave", _, _) => "wavy",
                (_, _, true) => "heavy",
                _ => "sng",
            }
            .into()
        });
        if let Some(c) = g("style:text-underline-color").and_then(units::color) {
            rp.underline_color = Some(ColorRef::rgb(c));
        }
    }
    if let Some(s) = g("style:text-line-through-style") {
        rp.strike = Some(if s == "none" {
            Strike::None
        } else if g("style:text-line-through-type") == Some("double") {
            Strike::Double
        } else {
            Strike::Single
        });
    }
    if let Some(c) = g("fo:color").and_then(units::color) {
        // A zero opacity is LibreOffice's "unset".
        let op = g("loext:opacity").and_then(units::percent).filter(|o| *o > 0.0);
        rp.fill = Some(Fill::solid(crate::styles::color_ref(c, op)));
    } else if g("style:use-window-font-color") == Some("true") {
        rp.fill = Some(Fill::solid(ColorRef::rgb(auto)));
    }
    if let Some(pos) = g("style:text-position") {
        let mut it = pos.split_whitespace();
        let off = match it.next() {
            Some("super") => Some(0.33),
            Some("sub") => Some(-0.33),
            Some(v) => units::percent(v),
            None => None,
        };
        if let Some(o) = off.filter(|o| *o != 0.0) {
            rp.baseline = Some(o.clamp(-10.0, 10.0));
            let scale = it.next().and_then(units::percent).unwrap_or(0.58);
            if let Some(sz) = rp.size {
                rp.size = Some((sz * scale).clamp(1.0, 4000.0));
            }
        }
    }
    if g("fo:font-variant") == Some("small-caps") {
        rp.caps = Some(Caps::Small);
    } else if g("fo:text-transform") == Some("uppercase") {
        rp.caps = Some(Caps::All);
    } else if g("fo:font-variant") == Some("normal") || g("fo:text-transform") == Some("none") {
        rp.caps = Some(Caps::None);
    }
    if let Some(sp) = g("fo:letter-spacing").and_then(units::length) {
        rp.spacing = Some(sp.clamp(-4000.0, 4000.0));
    }
    if let Some(bg) = g("fo:background-color").and_then(units::color) {
        rp.highlight = Some(ColorRef::rgb(bg));
    }
    if g("fo:text-shadow").is_some_and(|s| s != "none") {
        rp.shadow = Some(Shadow {
            color: ColorRef::rgb(Rgba::rgb(0x80, 0x80, 0x80)),
            blur: 0.0,
            dist: 1.5,
            dir: 45.0,
            inner: false,
            sx: 1.0,
            sy: 1.0,
            kx: 0.0,
            ky: 0.0,
            align: String::new(),
            rotate_with_shape: true,
        });
    }
    if g("style:text-outline") == Some("true") {
        rp.outline = Some(Line::solid(ColorRef::rgb(Rgba::BLACK), 0.75));
        rp.fill = Some(Fill::None);
    }
    if let Some(lang) = g("fo:language").filter(|l| *l != "zxx" && *l != "none") {
        let part = |k: &str| g(k).filter(|v| !v.is_empty() && *v != "none").map(|v| format!("-{v}")).unwrap_or_default();
        rp.lang = Some(format!("{lang}{}{}", part("fo:script"), part("fo:country")));
    }
    if g("style:letter-kerning") == Some("true") {
        rp.kern = Some(0.0);
    }
    rp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_schemes() {
        assert_eq!(num_scheme("1", "", "."), "arabicPeriod");
        assert_eq!(num_scheme("a", "(", ")"), "alphaLcParenBoth");
        assert_eq!(num_scheme("I", "", ")"), "romanUcParenR");
        assert_eq!(num_scheme("i", "", ""), "romanLcPlain");
    }

    #[test]
    fn hyperlinks_keep_only_safe_targets() {
        assert!(hyperlink("https://example.org").is_some());
        assert!(hyperlink("javascript:alert(1)").is_none());
        assert!(hyperlink("#page2").is_some());
    }
}
