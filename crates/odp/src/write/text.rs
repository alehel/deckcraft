//! Writing text: paragraphs, lists and spans with resolved formatting in automatic styles.

use std::fmt::Write as _;

use deckcraft_color::Rgba;
use deckcraft_model::resolve::{self, Ctx};
use deckcraft_model::style::Fill;
use deckcraft_model::text::{Action, Align, Bullet, Caps, LevelStyle, ParaProps, Run, RunKind, RunProps, Spacing, Strike, TextBody};
use deckcraft_model::{Shape, SlideId};

use super::{Auto, PageCtx, Writer};
use crate::dom::esc;
use crate::units::{cm, hex, pct, pt, trim};

/// The text of a shape, resolved through its placeholder chain. `outline`: an outline
/// placeholder, whose every paragraph is a list item (its level is its nesting).
pub fn body(w: &mut Writer, auto: &mut Auto, ctx: &Ctx, pg: &PageCtx, shape: &Shape, outline: bool) -> String {
    let Some(t) = &shape.text else { return String::new() };
    let mut out = String::new();
    for (i, p) in t.paragraphs.iter().enumerate() {
        let pp = resolve::para(ctx, shape, p);
        let runs: Vec<(RunProps, &Run)> = p.runs.iter().map(|r| (resolve::run(ctx, shape, p, &r.props), r)).collect();
        let end = resolve::run(ctx, shape, p, &p.end_props);
        // A paragraph an animation targets carries an id.
        let id = pg.id(shape.id, Some(i as u32));
        out.push_str(&paragraph(w, auto, Some(ctx), p.level, &pp, &end, &runs, outline, id, pg.is_master));
    }
    out
}

/// Paragraphs with only their own formatting (notes, table cells resolved by the caller).
pub fn plain_paragraphs(w: &mut Writer, auto: &mut Auto, t: &TextBody) -> String {
    let mut out = String::new();
    for p in &t.paragraphs {
        let mut end = p.end_props.clone();
        end.inherit(&p.def_props);
        let runs: Vec<(RunProps, &Run)> = p
            .runs
            .iter()
            .map(|r| {
                let mut rp = r.props.clone();
                rp.inherit(&p.def_props);
                (rp, r)
            })
            .collect();
        out.push_str(&paragraph(w, auto, None, p.level, &p.props, &end, &runs, false, None, false));
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn paragraph(
    w: &mut Writer,
    auto: &mut Auto,
    ctx: Option<&Ctx>,
    level: u8,
    pp: &ParaProps,
    end: &RunProps,
    runs: &[(RunProps, &Run)],
    outline: bool,
    id: Option<String>,
    on_master: bool,
) -> String {
    // The paragraph style carries its alignment, spacing and indents, and the text properties of
    // its end (so an empty paragraph has its height).
    let first = runs.first().map(|(r, _)| r).unwrap_or(end);
    let size = first.size.or(end.size).unwrap_or(18.0);
    let bullet = pp.bullet.clone().unwrap_or(Bullet::None);
    let bulleted = !matches!(bullet, Bullet::None);
    let in_list = bulleted || outline || level > 0;
    let ppx = para_attrs_in(pp, size, in_list);
    let tabs = tab_stops(pp);
    let ptext = text_attrs(w, ctx, end);
    let pstyle = auto.style(
        "paragraph",
        "P",
        None,
        &format!(r#"<style:paragraph-properties{ppx}>{tabs}</style:paragraph-properties><style:text-properties{ptext}/>"#),
    );
    let mut inner = String::new();
    for (rp, r) in runs {
        let tp = text_attrs(w, ctx, rp);
        let tstyle = auto.style("text", "T", None, &format!("<style:text-properties{tp}/>"));
        let content = match &r.kind {
            RunKind::Break => "<text:line-break/>".to_string(),
            RunKind::Field { field } => {
                let f = field.to_ascii_lowercase();
                if f == "slidenum" {
                    format!("<text:page-number>{}</text:page-number>", esc(&r.text))
                } else if f.starts_with("datetime") && on_master {
                    "<presentation:date-time/>".to_string()
                } else if f.starts_with("datetime") {
                    format!(r#"<text:date style:data-style-name="D1">{}</text:date>"#, esc(&r.text))
                } else if f == "slidecount" {
                    format!("<text:page-count>{}</text:page-count>", esc(&r.text))
                } else {
                    text_content(&r.text)
                }
            }
            RunKind::Math { .. } => text_content(&r.text),
            RunKind::Text => text_content(&r.text),
        };
        let span = format!(r#"<text:span text:style-name="{tstyle}">{content}</text:span>"#);
        match rp.link.as_ref().and_then(|l| link_href(w, &l.action)) {
            Some(href) => {
                let _ = write!(inner, r#"<text:a xlink:type="simple" xlink:href="{}">{span}</text:a>"#, esc(&href));
            }
            None => inner.push_str(&span),
        }
    }
    let id = id.map(|i| format!(r#" xml:id="{i}" text:id="{i}""#)).unwrap_or_default();
    let p = format!(r#"<text:p{id} text:style-name="{pstyle}">{inner}</text:p>"#);
    if !in_list {
        return p;
    }
    // A list: one level element for this paragraph's level, nested to its depth.
    let lv = usize::from(level.min(9));
    let level_el = list_level(w, ctx, lv + 1, pp, &bullet, size, first);
    let lstyle = auto.list(&level_el);
    let item = if bulleted { "text:list-item" } else { "text:list-header" };
    let start = match &bullet {
        Bullet::AutoNum { start_at, .. } if *start_at != 1 => format!(r#" text:start-value="{start_at}""#),
        _ => String::new(),
    };
    let mut open = format!(r#"<text:list text:style-name="{lstyle}" text:continue-numbering="true">"#);
    let mut close = String::new();
    for d in 0..=lv {
        let it = if d == lv { item } else { "text:list-item" };
        let st = if d == lv { start.as_str() } else { "" };
        if d > 0 {
            open.push_str("<text:list>");
        }
        let _ = write!(open, "<{it}{st}>");
        close.insert_str(0, &format!("</{it}>{}", if d > 0 { "</text:list>" } else { "" }));
    }
    format!("{open}{p}{close}</text:list>")
}

fn list_level(w: &mut Writer, ctx: Option<&Ctx>, level: usize, pp: &ParaProps, bullet: &Bullet, size: f64, first: &RunProps) -> String {
    let margin = pp.margin_left.unwrap_or(0.0);
    let indent = pp.indent.unwrap_or(0.0);
    let rel = pp.bullet_size.or_else(|| pp.bullet_size_points.map(|p| p / size.max(1.0))).unwrap_or(1.0);
    // A picture bullet also has its size here.
    let picture = match bullet {
        Bullet::Picture { media } => w.media_path(*media),
        _ => None,
    };
    let image_size = match picture {
        Some(_) => {
            let edge = cm(size * rel.clamp(0.25, 4.0));
            format!(r#" fo:width="{edge}" fo:height="{edge}""#)
        }
        None => String::new(),
    };
    let props = format!(
        r#"<style:list-level-properties text:space-before="{}" text:min-label-width="{}"{image_size}/>"#,
        cm((margin + indent).max(0.0)),
        cm((-indent).max(0.0))
    );
    let mut tp = format!(r#" fo:font-size="{}""#, pct(rel.clamp(0.25, 4.0)));
    if let Some(f) = pp.bullet_font.as_deref().filter(|_| pp.bullet_font_follow_text != Some(true)) {
        let _ = write!(tp, r#" fo:font-family="{}""#, super::quoted_family(f));
    } else if let Some(c) = ctx {
        let f = resolve::font_family(c, first);
        let _ = write!(tp, r#" fo:font-family="{}""#, super::quoted_family(&f));
    }
    match (&pp.bullet_color, ctx) {
        (Some(c), Some(cx)) if pp.bullet_color_follow_text != Some(true) => {
            let _ = write!(tp, r#" fo:color="{}""#, hex(cx.color(c, None)));
        }
        _ => tp.push_str(r#" style:use-window-font-color="true""#),
    }
    match bullet {
        Bullet::Char { char } => {
            let c: String = char.chars().take(1).collect();
            format!(
                r#"<text:list-level-style-bullet text:level="{level}" text:bullet-char="{}">{props}<style:text-properties{tp}/></text:list-level-style-bullet>"#,
                esc(&c)
            )
        }
        Bullet::AutoNum { scheme, .. } => {
            let (fmt, prefix, suffix) = num_format(scheme);
            format!(
                r#"<text:list-level-style-number text:level="{level}" style:num-format="{fmt}" style:num-prefix="{prefix}" style:num-suffix="{suffix}">{props}<style:text-properties{tp}/></text:list-level-style-number>"#
            )
        }
        Bullet::Picture { .. } => match picture {
            Some(href) => {
                format!(
                    r#"<text:list-level-style-image text:level="{level}" xlink:href="{}" xlink:type="simple" xlink:show="embed" xlink:actuate="onLoad">{props}</text:list-level-style-image>"#,
                    esc(&href)
                )
            }
            None => format!(
                r#"<text:list-level-style-bullet text:level="{level}" text:bullet-char="•">{props}<style:text-properties{tp}/></text:list-level-style-bullet>"#
            ),
        },
        Bullet::None => format!(r#"<text:list-level-style-number text:level="{level}" style:num-format="">{props}</text:list-level-style-number>"#),
    }
}

/// A DrawingML auto-number scheme as ODF number format, prefix and suffix.
pub fn num_format(scheme: &str) -> (&'static str, &'static str, &'static str) {
    let fmt = if scheme.starts_with("alphaLc") {
        "a"
    } else if scheme.starts_with("alphaUc") {
        "A"
    } else if scheme.starts_with("romanLc") {
        "i"
    } else if scheme.starts_with("romanUc") {
        "I"
    } else {
        "1"
    };
    let (pre, suf) = if scheme.ends_with("ParenBoth") {
        ("(", ")")
    } else if scheme.ends_with("ParenR") {
        ("", ")")
    } else if scheme.ends_with("Plain") {
        ("", "")
    } else if scheme.ends_with("Minus") {
        ("", "-")
    } else {
        ("", ".")
    };
    (fmt, pre, suf)
}

fn link_href(w: &Writer, a: &Action) -> Option<String> {
    match a {
        Action::Url { url } if deckcraft_model::text::safe_url(url) => Some(url.clone()),
        Action::Slide { slide } => slide_href(w, *slide),
        _ => None,
    }
}

pub fn slide_href(w: &Writer, id: SlideId) -> Option<String> {
    w.page_names.get(&id.0).map(|n| format!("#{n}"))
}

/// Text with runs of spaces, tabs and new lines as their ODF elements.
pub fn text_content(s: &str) -> String {
    let mut out = String::new();
    let mut spaces = 0usize;
    let mut at_start = true;
    let flush = |out: &mut String, spaces: &mut usize, at_start: bool| {
        if *spaces == 0 {
            return;
        }
        // The first space of a run is literal (unless the run starts the span); the rest are
        // `text:s`, which white-space collapsing keeps.
        if at_start {
            let _ = write!(out, r#"<text:s text:c="{spaces}"/>"#);
        } else {
            out.push(' ');
            if *spaces > 1 {
                let _ = write!(out, r#"<text:s text:c="{}"/>"#, *spaces - 1);
            }
        }
        *spaces = 0;
    };
    for c in s.chars() {
        match c {
            ' ' => spaces += 1,
            '\t' => {
                flush(&mut out, &mut spaces, at_start);
                out.push_str("<text:tab/>");
                at_start = false;
            }
            '\n' | '\u{b}' => {
                flush(&mut out, &mut spaces, at_start);
                out.push_str("<text:line-break/>");
                at_start = true;
            }
            c => {
                flush(&mut out, &mut spaces, at_start);
                out.push_str(&esc(&c.to_string()));
                at_start = false;
            }
        }
    }
    // Trailing spaces would be collapsed away: keep them all as `text:s`.
    if spaces > 0 {
        let _ = write!(out, r#"<text:s text:c="{spaces}"/>"#);
    }
    out
}

/// Paragraph properties for a paragraph in a list, or not: in a list the indents live in the list
/// level and the paragraph's are zero (as LibreOffice writes them: it adds the two).
fn para_attrs_in(pp: &ParaProps, size: f64, in_list: bool) -> String {
    if in_list { para_attrs(&ParaProps { margin_left: Some(0.0), indent: Some(0.0), ..pp.clone() }, size) } else { para_attrs(pp, size) }
}

/// One level of a master text style as a presentation style writes it: its paragraph properties
/// and, for a list level (1-based `level`), its list level style.
pub(crate) fn level_style(w: &mut Writer, ctx: &Ctx, level: usize, lv: &LevelStyle, in_list: bool) -> (String, Option<String>) {
    let size = lv.run.size.unwrap_or(18.0);
    let props = para_attrs_in(&lv.para, size, in_list);
    let list = in_list.then(|| list_level(w, Some(ctx), level, &lv.para, &lv.para.bullet.clone().unwrap_or(Bullet::None), size, &lv.run));
    (props, list)
}

fn para_attrs(pp: &ParaProps, size: f64) -> String {
    let mut a = String::new();
    let _ = write!(a, r#" fo:text-align="{}""#, crate::text::odf_align(pp.align.unwrap_or(Align::Left)));
    let _ = write!(
        a,
        r#" fo:margin-left="{}" fo:margin-right="{}" fo:text-indent="{}""#,
        cm(pp.margin_left.unwrap_or(0.0)),
        cm(pp.margin_right.unwrap_or(0.0)),
        cm(pp.indent.unwrap_or(0.0))
    );
    let space = |s: Option<Spacing>| match s {
        Some(Spacing::Pts(v)) => v,
        // A percentage of the line: of the font size, as LibreOffice reads it.
        Some(Spacing::Pct(f)) => f * size * 1.2,
        None => 0.0,
    };
    let _ = write!(a, r#" fo:margin-top="{}" fo:margin-bottom="{}""#, cm(space(pp.space_before)), cm(space(pp.space_after)));
    match pp.line_spacing {
        Some(Spacing::Pct(f)) => {
            let _ = write!(a, r#" fo:line-height="{}""#, pct(f));
        }
        Some(Spacing::Pts(v)) => {
            let _ = write!(a, r#" fo:line-height="{}""#, pt(v));
        }
        None => {}
    }
    if pp.rtl == Some(true) {
        a.push_str(r#" style:writing-mode="rl-tb""#);
    }
    if let Some(d) = pp.default_tab {
        let _ = write!(a, r#" style:tab-stop-distance="{}""#, cm(d));
    }
    a
}

fn tab_stops(pp: &ParaProps) -> String {
    let Some(tabs) = &pp.tabs else { return String::new() };
    let mut s = String::from("<style:tab-stops>");
    for t in tabs.iter().take(64) {
        let kind = match t.align.as_str() {
            "r" => "right",
            "ctr" => "center",
            "dec" => "char",
            _ => "left",
        };
        let _ = write!(s, r#"<style:tab-stop style:position="{}" style:type="{kind}"/>"#, cm(t.pos));
    }
    s.push_str("</style:tab-stops>");
    s
}

/// `style:text-properties` attributes of resolved run properties.
pub fn text_attrs(w: &mut Writer, ctx: Option<&Ctx>, r: &RunProps) -> String {
    let mut a = String::new();
    let family = match ctx {
        Some(c) => Some(resolve::font_family(c, r)),
        None => r.font.clone().filter(|f| !f.starts_with('+')),
    };
    if let Some(f) = family.filter(|f| !f.is_empty()) {
        let _ = write!(a, r#" style:font-name="{}""#, esc(&f));
        w.fonts.insert(f);
    }
    let theme_font = |name: &Option<String>| -> Option<String> {
        let n = name.as_deref()?;
        let f = match ctx {
            Some(c) => c.master.theme.font(n),
            None => n.to_string(),
        };
        (!f.is_empty() && !f.starts_with('+')).then_some(f)
    };
    if let Some(f) = theme_font(&r.font_ea) {
        let _ = write!(a, r#" style:font-name-asian="{}""#, esc(&f));
        w.fonts.insert(f);
    }
    if let Some(f) = theme_font(&r.font_cs) {
        let _ = write!(a, r#" style:font-name-complex="{}""#, esc(&f));
        w.fonts.insert(f);
    }
    if let Some(sz) = r.size {
        let s = pt(sz);
        let _ = write!(a, r#" fo:font-size="{s}" style:font-size-asian="{s}" style:font-size-complex="{s}""#);
    }
    if let Some(b) = r.bold {
        let v = if b { "bold" } else { "normal" };
        let _ = write!(a, r#" fo:font-weight="{v}" style:font-weight-asian="{v}" style:font-weight-complex="{v}""#);
    }
    if let Some(i) = r.italic {
        let v = if i { "italic" } else { "normal" };
        let _ = write!(a, r#" fo:font-style="{v}" style:font-style-asian="{v}" style:font-style-complex="{v}""#);
    }
    if let Some(u) = r.underline.as_deref() {
        let (style, ty, width) = match u {
            "none" => ("none", "single", "auto"),
            "dbl" => ("solid", "double", "auto"),
            "heavy" => ("solid", "single", "bold"),
            "dotted" | "dottedHeavy" => ("dotted", "single", if u.ends_with("Heavy") { "bold" } else { "auto" }),
            "dash" | "dashHeavy" => ("dash", "single", if u.ends_with("Heavy") { "bold" } else { "auto" }),
            "dashLong" | "dashLongHeavy" => ("long-dash", "single", "auto"),
            "dotDash" | "dotDashHeavy" => ("dot-dash", "single", "auto"),
            "dotDotDash" | "dotDotDashHeavy" => ("dot-dot-dash", "single", "auto"),
            "wavy" | "wavyHeavy" | "wavyDbl" => ("wave", if u == "wavyDbl" { "double" } else { "single" }, "auto"),
            _ => ("solid", "single", "auto"),
        };
        let _ = write!(a, r#" style:text-underline-style="{style}""#);
        if style != "none" {
            let color = match (&r.underline_color, ctx) {
                (Some(c), Some(cx)) => hex(cx.color(c, None)),
                _ => "font-color".into(),
            };
            let _ = write!(a, r#" style:text-underline-type="{ty}" style:text-underline-width="{width}" style:text-underline-color="{color}""#);
        }
    }
    match r.strike {
        Some(Strike::Single) => a.push_str(r#" style:text-line-through-style="solid" style:text-line-through-type="single""#),
        Some(Strike::Double) => a.push_str(r#" style:text-line-through-style="solid" style:text-line-through-type="double""#),
        Some(Strike::None) => a.push_str(r#" style:text-line-through-style="none""#),
        None => {}
    }
    let color = match (&r.fill, ctx) {
        (Some(Fill::Solid { color }), Some(c)) => Some((c.color(color, None), color.alpha())),
        (Some(Fill::Solid { color }), None) => Some((color.resolve(&deckcraft_model::Theme::default().colors, None), color.alpha())),
        (Some(Fill::Gradient(g)), Some(c)) => g.stops.first().map(|s| (c.color(&s.color, None), s.color.alpha())),
        (Some(Fill::None), _) => Some((Rgba::BLACK, 0.0)),
        _ => ctx.map(|c| (resolve::text_color(c, r), 1.0)),
    };
    if let Some((c, alpha)) = color {
        let _ = write!(a, r#" fo:color="{}""#, hex(c));
        if alpha < 1.0 {
            let _ = write!(a, r#" loext:opacity="{}""#, pct(alpha.clamp(0.0, 1.0)));
        }
    }
    if let Some(b) = r.baseline.filter(|b| *b != 0.0) {
        let _ = write!(a, r#" style:text-position="{}% 100%""#, trim(b * 100.0, 1));
    }
    match r.caps {
        Some(Caps::Small) => a.push_str(r#" fo:font-variant="small-caps""#),
        Some(Caps::All) => a.push_str(r#" fo:text-transform="uppercase""#),
        _ => {}
    }
    if let Some(sp) = r.spacing.filter(|s| *s != 0.0) {
        let _ = write!(a, r#" fo:letter-spacing="{}""#, cm(sp));
    }
    if let (Some(h), Some(c)) = (&r.highlight, ctx) {
        let _ = write!(a, r#" fo:background-color="{}""#, hex(c.color(h, None)));
    }
    if r.shadow.is_some() {
        a.push_str(r#" fo:text-shadow="1pt 1pt""#);
    }
    if r.outline.is_some() && matches!(r.fill, Some(Fill::None)) {
        a.push_str(r#" style:text-outline="true""#);
    }
    if let Some(lang) = r.lang.as_deref().filter(|l| !l.is_empty()) {
        // BCP 47: language, then a four-letter script and a two-letter or three-digit region;
        // other subtags have no ODF attribute.
        let mut tags = lang.split(['-', '_']);
        let _ = write!(a, r#" fo:language="{}""#, esc(tags.next().unwrap_or(lang)));
        for t in tags {
            let k = match t.len() {
                4 if t.chars().all(|c| c.is_ascii_alphabetic()) => "fo:script",
                2 if t.chars().all(|c| c.is_ascii_alphabetic()) => "fo:country",
                3 if t.chars().all(|c| c.is_ascii_digit()) => "fo:country",
                _ => continue,
            };
            let _ = write!(a, r#" {k}="{}""#, esc(t));
        }
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spaces_tabs_and_breaks() {
        assert_eq!(text_content("a  b"), r#"a <text:s text:c="1"/>b"#);
        assert_eq!(text_content(" a"), r#"<text:s text:c="1"/>a"#);
        assert_eq!(text_content("a\tb\u{b}c "), r#"a<text:tab/>b<text:line-break/>c<text:s text:c="1"/>"#);
        assert_eq!(text_content("<&>"), "&lt;&amp;&gt;");
    }

    #[test]
    fn number_formats_round_trip() {
        for s in ["arabicPeriod", "alphaLcParenBoth", "romanUcParenR", "alphaUcPlain"] {
            let (f, pre, suf) = num_format(s);
            assert_eq!(crate::text::num_scheme(f, pre, suf), s);
        }
    }
}
