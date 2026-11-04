//! Charts as embedded chart documents (`Object N/content.xml`), with their data in a local table.

use std::fmt::Write as _;

use deckcraft_color::{Rgba, SchemeSlot};
use deckcraft_model::Xfrm;
use deckcraft_model::chart::{Chart, ChartType};
use deckcraft_model::resolve::Ctx;
use deckcraft_model::style::Fill;

use super::Writer;
use crate::dom::esc;
use crate::units::{cm, hex, pt, trim};

/// Most rows and series written.
const MAX: usize = 4096;

fn col_name(i: usize) -> String {
    let mut n = i + 1;
    let mut s = String::new();
    while n > 0 {
        let r = (n - 1) % 26;
        s.insert(0, char::from(b'A' + r as u8));
        n = (n - 1) / 26;
    }
    s
}

fn range(col: usize, r0: usize, r1: usize) -> String {
    let c = col_name(col);
    format!("local-table.${c}${}:.${c}${}", r0 + 1, r1 + 1)
}

fn fill_color(ctx: &Ctx, f: Option<&Fill>, i: usize) -> Rgba {
    match f {
        Some(Fill::Solid { color }) => ctx.color(color, None),
        Some(Fill::Gradient(g)) => g.stops.first().map(|s| ctx.color(&s.color, None)).unwrap_or(Rgba::BLACK),
        _ => {
            let slots =
                [SchemeSlot::Accent1, SchemeSlot::Accent2, SchemeSlot::Accent3, SchemeSlot::Accent4, SchemeSlot::Accent5, SchemeSlot::Accent6];
            ctx.master.theme.colors.get(slots[i % slots.len()])
        }
    }
}

/// The `draw:object` of a chart frame, with the chart document added to the package.
pub fn object(w: &mut Writer, ctx: &Ctx, x: &Xfrm, c: &Chart) -> String {
    w.objects += 1;
    let dir = format!("Object {}", w.objects);
    let content = chart_document(ctx, c, x);
    let styles = format!(r#"<?xml version="1.0" encoding="UTF-8"?><office:document-styles {}><office:styles/></office:document-styles>"#, super::NS);
    w.files.push((format!("{dir}/content.xml"), "text/xml".into(), std::sync::Arc::new(content.into_bytes())));
    w.files.push((format!("{dir}/styles.xml"), "text/xml".into(), std::sync::Arc::new(styles.into_bytes())));
    w.dirs.push((format!("{dir}/"), "application/vnd.oasis.opendocument.chart".into()));
    format!("<draw:object {}/>", super::shapes::embed(&format!("./{dir}")))
}

fn chart_document(ctx: &Ctx, c: &Chart, x: &Xfrm) -> String {
    let mut styles = String::new();
    let mut n = 0usize;
    let mut style = |body: String| {
        n += 1;
        let name = format!("ch{n}");
        let _ = write!(styles, r#"<style:style style:name="{name}" style:family="chart">{body}</style:style>"#);
        name
    };
    let (class, vertical, stacked, percent) = crate::chart::class_of(c.kind);
    let text_color = c.text_color.as_ref().map(|t| hex(ctx.color(t, None))).unwrap_or_else(|| "#595959".into());
    // The renderer's default sizes, so the chart reads the same in other apps.
    let base = c.text_size.unwrap_or_else(|| (x.h.min(x.w) / 22.0).clamp(7.0, 18.0));
    let text_size = pt(base);
    let chart_fill = match &c.chart_fill {
        Some(f @ Fill::Solid { .. }) => format!(r#" draw:fill="solid" draw:fill-color="{}""#, hex(fill_color(ctx, Some(f), 0))),
        _ => r#" draw:fill="none""#.to_string(),
    };
    let ch_style = style(format!(
        r#"<style:graphic-properties draw:stroke="none"{chart_fill}/><style:text-properties fo:font-size="{text_size}" fo:color="{text_color}"/>"#
    ));
    let mut plot_props = format!(r#" chart:vertical="{vertical}" chart:stacked="{stacked}" chart:percentage="{percent}""#);
    if class == "bar" {
        let _ = write!(
            plot_props,
            r#" chart:gap-width="{}" chart:overlap="{}""#,
            trim(c.gap_width * 100.0, 0),
            trim(if stacked || percent { 100.0 } else { c.overlap * 100.0 }, 0)
        );
    }
    if matches!(c.kind, ChartType::LineMarkers | ChartType::Scatter) {
        plot_props.push_str(r#" chart:symbol-type="automatic""#);
    } else if class == "line" {
        plot_props.push_str(r#" chart:symbol-type="none""#);
    }
    let plot_style = style(format!(r#"<style:chart-properties{plot_props}/>"#));
    let axis_text = format!(r#"<style:text-properties fo:font-size="{text_size}" fo:color="{text_color}"/>"#);
    let yprops = y_props(c);
    let y_style = style(format!(
        r##"<style:chart-properties chart:display-label="{}"{yprops}/><style:graphic-properties svg:stroke-color="#b3b3b3"/>{axis_text}"##,
        !c.y_hidden
    ));
    let x_style = style(format!(
        r##"<style:chart-properties chart:display-label="true" chart:reverse-direction="{}"/><style:graphic-properties svg:stroke-color="#b3b3b3"/>{axis_text}"##,
        c.cat_reversed
    ));
    let grid_style = style(r##"<style:graphic-properties svg:stroke-color="#d9d9d9"/>"##.to_string());
    let wall = match &c.plot_fill {
        Some(f @ Fill::Solid { .. }) => format!(r#" draw:fill="solid" draw:fill-color="{}""#, hex(fill_color(ctx, Some(f), 0))),
        _ => r#" draw:fill="none""#.to_string(),
    };
    let wall_style = style(format!(r#"<style:graphic-properties draw:stroke="none"{wall}/>"#));

    // The data table: categories in column A, a column per series (scatter: X, then Y).
    let series: Vec<_> = c.series.iter().take(MAX).collect();
    let xy = matches!(c.kind, ChartType::Scatter | ChartType::Bubble);
    let rows = series.iter().map(|s| s.values.len().max(s.x_values.len())).max().unwrap_or(0).max(c.categories.len()).min(MAX);
    // Column layout: col 0 categories; per series: [x] [y] [size].
    let mut cols: Vec<(String, Vec<Option<f64>>)> = vec![];
    let mut series_xml = String::new();
    for (i, s) in series.iter().enumerate() {
        let color = hex(fill_color(ctx, s.fill.as_ref(), i));
        let mut g = format!(r#" draw:fill-color="{color}" svg:stroke-color="{color}""#);
        // Scatter series are markers, as DeckCraft draws them.
        if c.kind == ChartType::Scatter {
            g.push_str(r#" draw:stroke="none""#);
        }
        if class == "line" || class == "scatter" || class == "radar" {
            let width = s.line.as_ref().and_then(|l| l.width).unwrap_or(2.25);
            let _ = write!(g, r#" svg:stroke-width="{}""#, cm(width));
            if let Some(l) = &s.line
                && let Some(Fill::Solid { color }) = &l.fill
            {
                let lc = hex(ctx.color(color, None));
                g = format!(r#" draw:fill-color="{lc}" svg:stroke-color="{lc}" svg:stroke-width="{}""#, cm(width));
            }
        }
        let mut chp = String::new();
        if s.data_labels.unwrap_or(c.data_labels) {
            chp.push_str(r#" chart:data-label-number="value""#);
        }
        if s.smooth {
            chp.push_str(r#" chart:interpolation="cubic-spline""#);
        }
        let ss = style(format!(r#"<style:graphic-properties{g}/><style:chart-properties{chp}/>"#));
        // Columns named by domains: a scatter series' X values; a bubble series' Y values, then
        // its X values (its sizes are its values).
        let mut domains = String::new();
        let mut domain = |cols: &mut Vec<(String, Vec<Option<f64>>)>, name: String, vals: Vec<Option<f64>>| {
            cols.push((name, vals));
            let _ = write!(domains, r#"<chart:domain table:cell-range-address="{}"/>"#, range(cols.len(), 1, rows.max(1)));
        };
        if c.kind == ChartType::Bubble {
            domain(&mut cols, format!("{} Y", s.name), s.values.clone());
            domain(&mut cols, format!("{} X", s.name), s.x_values.clone());
            cols.push((s.name.clone(), s.sizes.clone()));
        } else {
            if xy {
                domain(&mut cols, format!("{} X", s.name), s.x_values.clone());
            }
            cols.push((s.name.clone(), s.values.clone()));
        }
        let vcol = cols.len();
        // Points with their own colour: overridden ones, and every slice of a varied pie. The
        // rest are unstyled runs (`chart:repeated`) that keep the series colour.
        // A pie not varying its colours styles every slice in the series' colour (left unstyled,
        // a pie's slices take a colour each).
        let pie = matches!(c.kind, ChartType::Pie | ChartType::Doughnut);
        let varied = c.vary_colors && pie;
        let mut points = String::new();
        let mut plain = 0usize;
        let own_fills: std::collections::HashMap<usize, &Fill> = s.point_fills.iter().map(|(k, f)| (*k, f)).collect();
        for p in 0..s.values.len().min(MAX) {
            let own = own_fills.get(&p).copied();
            if own.is_none() && !pie {
                plain += 1;
                continue;
            }
            if plain > 0 {
                let _ = write!(points, r#"<chart:data-point chart:repeated="{plain}"/>"#);
                plain = 0;
            }
            // Its own colour, the slice's (varied), or the series'.
            let c = match own {
                Some(f) => hex(fill_color(ctx, Some(f), p)),
                None if varied => hex(fill_color(ctx, None, p)),
                None => color.clone(),
            };
            let ps = style(format!(r#"<style:graphic-properties draw:fill-color="{c}"/>"#));
            let _ = write!(points, r#"<chart:data-point chart:style-name="{ps}"/>"#);
        }
        let axis = if s.secondary_axis { r#" chart:attached-axis="secondary-y""# } else { "" };
        let sclass = s.kind.map_or(class, |k| crate::chart::class_of(k).0);
        let _ = write!(
            series_xml,
            r#"<chart:series chart:style-name="{ss}" chart:values-cell-range-address="{}" chart:label-cell-address="local-table.${}$1" chart:class="chart:{sclass}"{axis}>{domains}{points}</chart:series>"#,
            range(vcol, 1, rows.max(1)),
            col_name(vcol)
        );
    }
    let table = data_table(c, &cols, rows);

    let title = c.title.as_ref().filter(|t| !t.trim().is_empty()).map(|t| {
        let st = style(format!(r#"<style:text-properties fo:font-size="{}" fo:color="{text_color}"/>"#, pt(c.title_size.unwrap_or(base * 1.4))));
        format!(r#"<chart:title chart:style-name="{st}"><text:p>{}</text:p></chart:title>"#, esc(t))
    });
    let legend = c.legend.as_deref().map(|l| {
        let pos = crate::chart::legend_position(l);
        format!(r#"<chart:legend chart:legend-position="{pos}" style:legend-expansion="balanced"/>"#)
    });
    let cats = if c.categories.is_empty() || xy {
        String::new()
    } else {
        format!(r#"<chart:categories table:cell-range-address="{}"/>"#, range(0, 1, rows.max(1)))
    };
    let grid = if c.gridlines { format!(r#"<chart:grid chart:style-name="{grid_style}" chart:class="major"/>"#) } else { String::new() };
    let minor = if c.minor_gridlines { format!(r#"<chart:grid chart:style-name="{grid_style}" chart:class="minor"/>"#) } else { String::new() };
    let axes = if matches!(c.kind, ChartType::Pie | ChartType::Doughnut) {
        String::new()
    } else {
        let xt = c.axis_titles.0.as_ref().map(|t| format!("<chart:title><text:p>{}</text:p></chart:title>", esc(t))).unwrap_or_default();
        let yt = c.axis_titles.1.as_ref().map(|t| format!("<chart:title><text:p>{}</text:p></chart:title>", esc(t))).unwrap_or_default();
        format!(
            r#"<chart:axis chart:dimension="x" chart:name="primary-x" chart:style-name="{x_style}">{xt}{cats}</chart:axis><chart:axis chart:dimension="y" chart:name="primary-y" chart:style-name="{y_style}">{yt}{grid}{minor}</chart:axis>"#
        )
    };
    let body = format!(
        r#"<chart:chart svg:width="{}" svg:height="{}" chart:class="chart:{class}" chart:style-name="{ch_style}">{}{}<chart:plot-area chart:style-name="{plot_style}" chart:data-source-has-labels="both">{axes}{series_xml}<chart:wall chart:style-name="{wall_style}"/><chart:floor/></chart:plot-area>{table}</chart:chart>"#,
        cm(x.w.max(1.0)),
        cm(x.h.max(1.0)),
        title.unwrap_or_default(),
        legend.unwrap_or_default(),
    );
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><office:document-content {}><office:automatic-styles>{styles}</office:automatic-styles><office:body><office:chart>{body}</office:chart></office:body></office:document-content>"#,
        super::NS
    )
}

/// The value axis' scale: minimum, maximum, major unit, logarithmic, hidden labels.
fn y_props(c: &Chart) -> String {
    let mut yprops = String::new();
    for (k, v) in [("chart:minimum", c.y_min), ("chart:maximum", c.y_max), ("chart:interval-major", c.y_major)] {
        if let Some(v) = v {
            let _ = write!(yprops, r#" {k}="{}""#, trim(v, 6));
        }
    }
    if c.y_log_base.is_some() {
        yprops.push_str(r#" chart:logarithmic="true""#);
    }
    yprops
}

/// The chart's local data table: categories in column A, then one column per entry of `cols`.
fn data_table(c: &Chart, cols: &[(String, Vec<Option<f64>>)], rows: usize) -> String {
    let mut table = String::from(
        r#"<table:table table:name="local-table"><table:table-header-columns><table:table-column/></table:table-header-columns><table:table-columns>"#,
    );
    let _ = write!(table, r#"<table:table-column table:number-columns-repeated="{}"/>"#, cols.len().max(1));
    table.push_str("</table:table-columns><table:table-header-rows><table:table-row><table:table-cell><text:p/></table:table-cell>");
    for (name, _) in cols {
        let _ = write!(table, r#"<table:table-cell office:value-type="string"><text:p>{}</text:p></table:table-cell>"#, esc(name));
    }
    table.push_str("</table:table-row></table:table-header-rows><table:table-rows>");
    for r in 0..rows {
        let cat = c.categories.get(r).cloned().unwrap_or_default();
        let _ = write!(table, r#"<table:table-row><table:table-cell office:value-type="string"><text:p>{}</text:p></table:table-cell>"#, esc(&cat));
        for (_, vals) in cols {
            match vals.get(r).copied().flatten().filter(|v| v.is_finite()) {
                Some(v) => {
                    let _ =
                        write!(table, r#"<table:table-cell office:value-type="float" office:value="{v}"><text:p>{v}</text:p></table:table-cell>"#);
                }
                None => table.push_str(r#"<table:table-cell office:value-type="float" office:value="NaN"><text:p>NaN</text:p></table:table-cell>"#),
            }
        }
        table.push_str("</table:table-row>");
    }
    table.push_str("</table:table-rows></table:table>");
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_names() {
        assert_eq!(col_name(0), "A");
        assert_eq!(col_name(25), "Z");
        assert_eq!(col_name(26), "AA");
        assert_eq!(range(2, 1, 4), "local-table.$C$2:.$C$5");
    }
}
