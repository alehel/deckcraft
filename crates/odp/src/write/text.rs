//! Writing text: paragraphs, lists and spans with resolved formatting in automatic styles.

use std::fmt::Write as _;

use deckcraft_color::Rgba;
use deckcraft_model::resolve::{self, Ctx};
use deckcraft_model::style::Fill;
use deckcraft_model::text::{Action, Align, Bullet, Caps, LevelStyle, ParaProps, Paragraph, Run, RunKind, RunProps, Spacing, Strike, TextBody};
use deckcraft_model::{Shape, SlideId};

use super::{Auto, PageCtx, Writer};
use crate::dom::esc;
use crate::units::{cm, hex, pct, pt, trim};

/// The text of a shape, resolved through its placeholder chain. `outline`: an outline
/// placeholder, whose every paragraph is a list item (its level is its nesting).
pub fn body(w: &mut Writer, auto: &mut Auto, ctx: &Ctx, pg: &PageCtx, shape: &Shape, outline: bool) -> String {
    let Some(t) = &shape.text else { return String::new() };
    let mut out = String::new();
    let mut numbering = Numbering::default();
    for (i, p) in t.paragraphs.iter().enumerate() {
        let pp = resolve::para(ctx, shape, p);
        let starts = numbering.starts_run(p, pp.bullet.as_ref());
        let runs: Vec<(RunProps, &Run)> = p.runs.iter().map(|r| (resolve::run(ctx, shape, p, &r.props), r)).collect();
        let end = resolve::run(ctx, shape, p, &p.end_props);
        // A paragraph an animation targets carries an id.
        let id = pg.id(shape.id, Some(i as u32));
        out.push_str(&paragraph(w, auto, ctx, p.level, &pp, &end, &runs, outline, id, pg.is_master, starts));
    }
    out
}

/// Paragraphs with only their own formatting (notes), their theme colours and fonts resolved
/// through `ctx`.
pub fn plain_paragraphs(w: &mut Writer, auto: &mut Auto, ctx: &Ctx, t: &TextBody) -> String {
    let mut out = String::new();
    let mut numbering = Numbering::default();
    for p in &t.paragraphs {
        let starts = numbering.starts_run(p, p.props.bullet.as_ref());
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
        out.push_str(&paragraph(w, auto, ctx, p.level, &p.props, &end, &runs, false, None, false, starts));
    }
    out
}

/// Where runs of auto-numbers begin, by the renderer's rules: a numbered paragraph whose start
/// differs from its level's current run starts a new one; a paragraph without a number ends the
/// runs at its level and below; numbered or not, it ends the runs below its level. Empty
/// paragraphs change nothing.
#[derive(Default)]
struct Numbering {
    run_start: [Option<u32>; 10],
}

impl Numbering {
    fn starts_run(&mut self, p: &Paragraph, bullet: Option<&Bullet>) -> bool {
        if p.runs.iter().all(|r| r.text.is_empty()) {
            return false;
        }
        let lvl = usize::from(p.level).min(self.run_start.len() - 1);
        let (keep, starts) = match bullet {
            Some(Bullet::AutoNum { start_at, .. }) => {
                let starts = self.run_start.get(lvl).copied().flatten() != Some(*start_at);
                if let Some(slot) = self.run_start.get_mut(lvl) {
                    *slot = Some(*start_at);
                }
                (lvl + 1, starts)
            }
            _ => (lvl, false),
        };
        for slot in self.run_start.iter_mut().skip(keep) {
            *slot = None;
        }
        starts
    }
}

#[allow(clippy::too_many_arguments)]
fn paragraph(
    w: &mut Writer,
    auto: &mut Auto,
    ctx: &Ctx,
    level: u8,
    pp: &ParaProps,
    end: &RunProps,
    runs: &[(RunProps, &Run)],
    outline: bool,
    id: Option<String>,
    on_master: bool,
    starts_run: bool,
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
                    // Times (`datetime10` on) are time fields.
                    let style = super::date_style(&f);
                    let el = if crate::vocab::date_format(&style).is_some_and(crate::vocab::is_time) { "text:time" } else { "text:date" };
                    format!(r#"<{el} style:data-style-name="{style}">{}</{el}>"#, esc(&r.text))
                } else if f == "slidecount" {
                    format!("<text:page-count>{}</text:page-count>", esc(&r.text))
                } else {
                    text_content(&r.text)
                }
            }
            RunKind::Math { .. } => text_content(&r.text),
            RunKind::Text => text_content(&r.text),
        };
        // A link sits inside its span (LibreOffice shows no text for a span inside a link).
        let content = match rp.link.as_ref().and_then(|l| link_href(w, &l.action).map(|h| (h, l))) {
            Some((href, l)) => {
                let title = if l.tooltip.is_empty() { String::new() } else { format!(r#" office:title="{}""#, esc(&l.tooltip)) };
                format!(r#"<text:a xlink:type="simple" xlink:href="{}"{title}>{content}</text:a>"#, esc(&href))
            }
            None => content,
        };
        let _ = write!(inner, r#"<text:span text:style-name="{tstyle}">{content}</text:span>"#);
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
    // Numbering continues from the list before, or (a new run) restarts at the start its level
    // style gives: LibreOffice keeps a run's start there through its own saves, and adds an item's
    // start to it.
    // The restart goes on the list of the paragraph's own level: on an outer one it would
    // restart the levels above too.
    let restarts = starts_run && matches!(bullet, Bullet::AutoNum { .. });
    let mut open = format!(r#"<text:list text:style-name="{lstyle}" text:continue-numbering="{}">"#, !(restarts && lv == 0));
    let mut close = String::new();
    for d in 0..=lv {
        let it = if d == lv { item } else { "text:list-item" };
        if d > 0 {
            open.push_str(if restarts && d == lv { r#"<text:list text:continue-numbering="false">"# } else { "<text:list>" });
        }
        let _ = write!(open, "<{it}>");
        close.insert_str(0, &format!("</{it}>{}", if d > 0 { "</text:list>" } else { "" }));
    }
    format!("{open}{p}{close}</text:list>")
}

fn list_level(w: &mut Writer, ctx: &Ctx, level: usize, pp: &ParaProps, bullet: &Bullet, size: f64, first: &RunProps) -> String {
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
    } else {
        let f = resolve::font_family(ctx, first);
        let _ = write!(tp, r#" fo:font-family="{}""#, super::quoted_family(&f));
    }
    match &pp.bullet_color {
        Some(c) if pp.bullet_color_follow_text != Some(true) => {
            let _ = write!(tp, r#" fo:color="{}""#, hex(ctx.color(c, None)));
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
        Bullet::AutoNum { scheme, start_at } => {
            let (fmt, prefix, suffix) = num_format(scheme);
            format!(
                r#"<text:list-level-style-number text:level="{level}" style:num-format="{fmt}" style:num-prefix="{prefix}" style:num-suffix="{suffix}" text:start-value="{start_at}">{props}<style:text-properties{tp}/></text:list-level-style-number>"#
            )
        }
        Bullet::Picture { .. } => match picture {
            Some(href) => {
                format!(r#"<text:list-level-style-image text:level="{level}" {}>{props}</text:list-level-style-image>"#, super::shapes::embed(&href))
            }
            None => format!(
                r#"<text:list-level-style-bullet text:level="{level}" text:bullet-char="•">{props}<style:text-properties{tp}/></text:list-level-style-bullet>"#
            ),
        },
        Bullet::None => format!(r#"<text:list-level-style-number text:level="{level}" style:num-format="">{props}</text:list-level-style-number>"#),
    }
}

/// A DrawingML auto-number scheme as ODF number format, prefix and suffix
/// ([`NUM_SCHEMES`](crate::vocab::NUM_SCHEMES); others as `1.`).
pub fn num_format(scheme: &str) -> (&'static str, &'static str, &'static str) {
    crate::vocab::NUM_SCHEMES.iter().find(|r| r.0 == scheme).map_or(("1", "", "."), |r| (r.1, r.2, r.3))
}

fn link_href(w: &Writer, a: &Action) -> Option<String> {
    match a {
        Action::Url { url } if deckcraft_model::text::safe_url(url) => Some(url.clone()),
        Action::Slide { slide } => slide_href(w, *slide),
        a => crate::vocab::write(crate::vocab::JUMPS, a).map(str::to_string),
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
        // The first space of a run is literal (unless the run starts the span or ends a line);
        // the rest are `text:s`, which white-space collapsing keeps.
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
                // Spaces before a break would be collapsed away, as at the end.
                flush(&mut out, &mut spaces, true);
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
    let list = in_list.then(|| list_level(w, ctx, level, &lv.para, &lv.para.bullet.clone().unwrap_or(Bullet::None), size, &lv.run));
    (props, list)
}

fn para_attrs(pp: &ParaProps, size: f64) -> String {
    let mut a = String::new();
    let _ = write!(a, r#" fo:text-align="{}""#, crate::text::odf_align(pp.align.unwrap_or(Align::Left)));
    if pp.align == Some(Align::Distributed) {
        a.push_str(r#" fo:text-align-last="justify""#);
    }
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
        let kind = crate::vocab::write(crate::vocab::TAB_TYPES, &t.align.as_str()).unwrap_or("left");
        // A decimal tab aligns on the decimal point.
        let ch = if kind == "char" { r#" style:char=".""# } else { "" };
        let _ = write!(s, r#"<style:tab-stop style:position="{}" style:type="{kind}"{ch}/>"#, cm(t.pos));
    }
    s.push_str("</style:tab-stops>");
    s
}

/// `style:text-properties` attributes of resolved run properties.
pub fn text_attrs(w: &mut Writer, ctx: &Ctx, r: &RunProps) -> String {
    let mut a = String::new();
    let f = resolve::font_family(ctx, r);
    if !f.is_empty() {
        let _ = write!(a, r#" style:font-name="{}""#, esc(&f));
        w.fonts.insert(f);
    }
    let theme_font = |name: &Option<String>| -> Option<String> {
        let n = name.as_deref()?;
        let f = ctx.master.theme.font(n);
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
        let (style, ty, width) = crate::vocab::odf_underline(if u == "words" { "sng" } else { u });
        let _ = write!(a, r#" style:text-underline-style="{style}""#);
        if u == "words" {
            a.push_str(r#" style:text-underline-mode="skip-white-space""#);
        }
        if style != "none" {
            let color = r.underline_color.as_ref().map_or_else(|| "font-color".into(), |c| hex(ctx.color(c, None)));
            let _ = write!(a, r#" style:text-underline-type="{ty}" style:text-underline-width="{width}" style:text-underline-color="{color}""#);
        }
    }
    match r.strike {
        Some(Strike::Single) => a.push_str(r#" style:text-line-through-style="solid" style:text-line-through-type="single""#),
        Some(Strike::Double) => a.push_str(r#" style:text-line-through-style="solid" style:text-line-through-type="double""#),
        Some(Strike::None) => a.push_str(r#" style:text-line-through-style="none""#),
        None => {}
    }
    // Outlined text LibreOffice can show only as hollow letters in the text's colour: unfilled,
    // or filled near-white (WordArt's white-with-outline, which filled would vanish on white).
    let outline_rgb = match r.outline.as_ref().and_then(|o| o.fill.as_ref()) {
        Some(Fill::Solid { color }) => Some(ctx.color(color, None)),
        _ => None,
    };
    let near_white = |c: Rgba| c.r > 230 && c.g > 230 && c.b > 230;
    let hollow = outline_rgb.is_some()
        && match &r.fill {
            Some(Fill::None) => true,
            Some(Fill::Solid { color }) => near_white(ctx.color(color, None)),
            _ => false,
        };
    let color = match &r.fill {
        _ if hollow => outline_rgb.map(|c| (c, 1.0)),
        Some(Fill::Solid { color }) => Some((ctx.color(color, None), color.alpha())),
        Some(Fill::Gradient(g)) => g.stops.first().map(|s| (ctx.color(&s.color, None), s.color.alpha())),
        Some(Fill::None) => Some((Rgba::BLACK, 0.0)),
        _ => Some((resolve::text_color(ctx, r), 1.0)),
    };
    // A colour with its opacity, opaque too: a style's otherwise shows through.
    if let Some((c, alpha)) = color {
        let _ = write!(a, r#" fo:color="{}" loext:opacity="{}""#, hex(c), pct(alpha.clamp(0.0, 1.0)));
    }
    // Values set to "off" are written too, so a style's (or the master's) don't show through.
    match r.baseline {
        // Raised or lowered text is drawn at SUB_SUPER of its size.
        Some(b) if b != 0.0 => {
            let _ = write!(a, r#" style:text-position="{}% {}""#, trim(b * 100.0, 1), pct(deckcraft_render::SUB_SUPER));
        }
        Some(_) => a.push_str(r#" style:text-position="0% 100%""#),
        None => {}
    }
    match r.caps {
        // Both attributes each time, so the paragraph's other one doesn't show through.
        Some(Caps::Small) => a.push_str(r#" fo:font-variant="small-caps" fo:text-transform="none""#),
        Some(Caps::All) => a.push_str(r#" fo:font-variant="normal" fo:text-transform="uppercase""#),
        Some(Caps::None) => a.push_str(r#" fo:font-variant="normal" fo:text-transform="none""#),
        None => {}
    }
    match r.spacing {
        Some(sp) if sp != 0.0 => {
            let _ = write!(a, r#" fo:letter-spacing="{}""#, cm(sp));
        }
        Some(_) => a.push_str(r#" fo:letter-spacing="normal""#),
        None => {}
    }
    if let Some(h) = &r.highlight {
        let _ = write!(a, r#" fo:background-color="{}""#, hex(ctx.color(h, None)));
    }
    if let Some(sh) = &r.shadow {
        let (dx, dy) = (sh.dist * sh.dir.to_radians().cos(), sh.dist * sh.dir.to_radians().sin());
        let _ = write!(a, r#" fo:text-shadow="{} {} {} {}""#, pt(dx), pt(dy), pt(sh.blur.max(0.0)), hex(ctx.color(&sh.color, None)));
    }
    if hollow {
        a.push_str(r#" style:text-outline="true""#);
    }
    if let Some(k) = r.kern {
        let _ = write!(a, r#" style:letter-kerning="{}""#, k > 0.0);
    }
    if let Some(lang) = r.lang.as_deref().filter(|l| !l.is_empty()) {
        a.push_str(&lang_attrs(lang));
    }
    a
}

/// A BCP 47 tag as ODF writes it: language, then a four-letter script and a two-letter or
/// three-digit region; other subtags have no ODF attribute.
fn lang_attrs(lang: &str) -> String {
    let mut a = String::new();
    let mut tags = lang.split(['-', '_']);
    let _ = write!(a, r#" fo:language="{}""#, esc(tags.next().unwrap_or(lang)));
    // The first script and the first region only (each attribute is written once).
    let (mut script, mut country) = (None, None);
    for t in tags {
        match t.len() {
            4 if t.chars().all(|c| c.is_ascii_alphabetic()) => script = script.or(Some(t)),
            2 if t.chars().all(|c| c.is_ascii_alphabetic()) => country = country.or(Some(t)),
            3 if t.chars().all(|c| c.is_ascii_digit()) => country = country.or(Some(t)),
            _ => {}
        }
    }
    for (k, v) in [("fo:script", script), ("fo:country", country)] {
        if let Some(v) = v {
            let _ = write!(a, r#" {k}="{}""#, esc(v));
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
        // Regression: the space before a break was literal, and dropped on reading.
        assert_eq!(text_content("a \nb"), r#"a<text:s text:c="1"/><text:line-break/>b"#);
    }

    #[test]
    fn number_formats_round_trip() {
        for (s, ..) in crate::vocab::NUM_SCHEMES {
            let (f, pre, suf) = num_format(s);
            assert_eq!(crate::text::num_scheme(f, pre, suf), *s);
        }
        // Regression: `arabic2Minus` (Arabic letters) and `hebrew2Minus` were written as "1-".
        assert_eq!(num_format("arabic2Minus"), ("1", "", "."));
        assert_eq!(num_format("hebrew2Minus"), ("1", "", "."));
    }
}
