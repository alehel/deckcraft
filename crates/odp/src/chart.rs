//! Charts: an embedded chart document (`Object N/content.xml`, `office:chart`) read into the
//! model's chart, and written back as one.

use deckcraft_model::chart::{Chart, ChartType, Series};
use deckcraft_model::style::{ColorRef, Fill, Line};

use crate::dom::{El, attr, kid, kids, repeat};
use crate::styles::{Scope, Store};
use crate::units;

/// Most cells kept from a chart's data table, across all rows.
const MAX_GRID_CELLS: usize = 65_536;
/// Most cells one range address yields, and most points per series.
const MAX_CELLS: usize = 4096;
/// Longest text kept from one cell.
const MAX_CELL_TEXT: usize = 1024;

type Value = (String, Option<f64>);

/// The chart's local data: cells as text and number, row by row.
#[derive(Default)]
struct Grid {
    rows: Vec<Vec<Value>>,
    width: usize,
}

impl Grid {
    /// Read the table, repeats expanded, within [`MAX_GRID_CELLS`] in all.
    fn read(t: &El) -> Grid {
        let mut g = Grid::default();
        let mut budget = MAX_GRID_CELLS;
        let mut rows: Vec<&El> = vec![];
        collect_rows(t, &mut rows, 0);
        for r in rows {
            let mut cells = vec![];
            for c in r.elements().filter(|c| c.name == "table:table-cell" || c.name == "table:covered-table-cell") {
                let mut text = kids(c, "text:p").map(|p| p.text()).collect::<Vec<_>>().join("\n");
                text.truncate(text.char_indices().nth(MAX_CELL_TEXT).map_or(text.len(), |(i, _)| i));
                let num = if matches!(attr(c, "office:value-type"), Some("float" | "percentage")) {
                    attr(c, "office:value").and_then(|v| v.parse::<f64>().ok()).filter(|v| v.is_finite())
                } else {
                    None
                };
                let n = repeat(c, "table:number-columns-repeated").min(budget.saturating_sub(cells.len()));
                cells.extend(std::iter::repeat_n((text, num), n));
            }
            // Every row costs at least one cell, so empty repeated rows run the budget down too.
            let cost = cells.len().max(1);
            let n = repeat(r, "table:number-rows-repeated").min(budget / cost);
            budget -= n * cost;
            g.rows.extend(std::iter::repeat_n(cells.clone(), n));
            g.width = g.width.max(cells.len());
            if budget == 0 {
                break;
            }
        }
        g
    }

    fn get(&self, r: usize, c: usize) -> Option<&Value> {
        self.rows.get(r).and_then(|row| row.get(c))
    }

    /// Cells of a range (`local-table.$B$2:.$B$5`, several ranges separated by spaces), in order,
    /// within the table and at most [`MAX_CELLS`].
    fn range(&self, addr: &str) -> Vec<Value> {
        let mut out = vec![];
        let (last_row, last_col) = (self.rows.len().saturating_sub(1), self.width.saturating_sub(1));
        for part in addr.split_whitespace().take(64) {
            let (a, b) = part.split_once(':').unwrap_or((part, part));
            let (Some((c0, r0)), Some((c1, r1))) = (cell_ref(a), cell_ref(b)) else { continue };
            if self.rows.is_empty() || r0.min(r1) > last_row || c0.min(c1) > last_col {
                continue;
            }
            for r in r0.min(r1)..=r0.max(r1).min(last_row) {
                for c in c0.min(c1)..=c0.max(c1).min(last_col) {
                    if out.len() >= MAX_CELLS {
                        return out;
                    }
                    out.push(self.get(r, c).cloned().unwrap_or_default());
                }
            }
        }
        out
    }
}

fn collect_rows<'a>(el: &'a El, out: &mut Vec<&'a El>, depth: usize) {
    if depth > 8 {
        return;
    }
    for e in el.elements() {
        match e.name.as_str() {
            "table:table-row" => out.push(e),
            "table:table-header-rows" | "table:table-rows" | "table:table-row-group" => collect_rows(e, out, depth + 1),
            _ => {}
        }
    }
}

/// `table.$C$2` / `.C2` / `'Sheet 1'.$C$2` → (column, row), 0-based.
fn cell_ref(s: &str) -> Option<(usize, usize)> {
    let cell = s.rsplit_once('.').map(|(_, c)| c).unwrap_or(s).replace('$', "");
    let letters: String = cell.chars().take_while(char::is_ascii_alphabetic).collect();
    let digits = &cell[letters.len()..];
    if letters.is_empty() || letters.len() > 3 {
        return None;
    }
    let col = letters.to_ascii_uppercase().bytes().fold(0usize, |a, b| a * 26 + usize::from(b - b'A' + 1)).checked_sub(1)?;
    let row = digits.parse::<usize>().ok()?.checked_sub(1)?;
    Some((col, row))
}

/// Rows of the data table above its values (the series names).
const HEADER_ROWS: usize = 1;

/// An element's chart style (`chart:style-name`), resolved.
fn chart_style(store: &Store, el: &El) -> crate::styles::Style {
    attr(el, "chart:style-name").map(|n| store.resolve(Scope::Content, "chart", Some(n))).unwrap_or_default()
}

/// Read the chart document of `draw:object` at `dir` (`Object 1`).
pub fn read(content: &El) -> Option<Chart> {
    let mut store = Store::default();
    store.add_document(content, Scope::Content);
    let chart = kid(content, "office:body").and_then(|b| kid(b, "office:chart")).and_then(|c| kid(c, "chart:chart"))?;
    let style = |el: &El| chart_style(&store, el);
    let plot = kid(chart, "chart:plot-area");
    let plot_style = plot.map(style).unwrap_or_default();
    let cp = |k: &str| plot_style.chart_prop(k);
    let class = attr(chart, "chart:class").unwrap_or("chart:bar").trim_start_matches("chart:");
    let vertical = cp("chart:vertical") == Some("true");
    let stacked = cp("chart:stacked") == Some("true");
    let percent = cp("chart:percentage") == Some("true");
    // LibreOffice's defaults where the file is silent: bars as wide as their gaps, no overlap.
    let mut c = Chart { title: None, legend: None, gap_width: 1.0, overlap: 0.0, ..Default::default() };
    c.kind = kind_of(class, vertical, stacked, percent);
    c.title = kid(chart, "chart:title").map(|t| kids(t, "text:p").map(|p| p.text()).collect::<Vec<_>>().join(" ")).filter(|t| !t.trim().is_empty());
    if let Some(t) = kid(chart, "chart:title") {
        c.title_size = style(t).text.get("fo:font-size").and_then(|v| units::length(v));
    }
    if let Some(l) = kid(chart, "chart:legend") {
        c.legend = Some(legend_of(attr(l, "chart:legend-position").unwrap_or("end")).into());
    }
    let chart_style = style(chart);
    if let Some(f) = simple_fill(&chart_style.graphic) {
        c.chart_fill = Some(f);
    }
    c.text_size = chart_style.text.get("fo:font-size").and_then(|v| units::length(v));
    let grid = kid(chart, "table:table").map(Grid::read).unwrap_or_default();
    let Some(plot) = plot else { return Some(c) };
    if let Some(wall) = kid(plot, "chart:wall")
        && let Some(f) = simple_fill(&style(wall).graphic)
    {
        c.plot_fill = Some(f);
    }
    let axes: Vec<&El> = kids(plot, "chart:axis").collect();
    let y = axes.iter().find(|a| attr(a, "chart:dimension") == Some("y"));
    let x = axes.iter().find(|a| attr(a, "chart:dimension") == Some("x"));
    c.gridlines = y.is_some_and(|y| kids(y, "chart:grid").any(|g| attr(g, "chart:class").unwrap_or("major") == "major"));
    c.minor_gridlines = y.is_some_and(|y| kids(y, "chart:grid").any(|g| attr(g, "chart:class") == Some("minor")));
    if let Some(y) = y {
        let ys = style(y);
        let num = |k: &str| ys.chart_prop(k).and_then(|v| v.parse::<f64>().ok()).filter(|v| v.is_finite());
        c.y_min = num("chart:minimum");
        c.y_max = num("chart:maximum");
        c.y_major = num("chart:interval-major");
        if ys.chart_prop("chart:logarithmic") == Some("true") {
            c.y_log_base = Some(10.0);
        }
        if ys.chart_prop("chart:display-label") == Some("false") {
            c.y_hidden = true;
        }
        if c.text_size.is_none() {
            c.text_size = ys.text.get("fo:font-size").and_then(|v| units::length(v));
        }
        if let Some(col) = ys.text.get("fo:color").and_then(|v| units::color(v)) {
            c.text_color = Some(ColorRef::rgb(col));
        }
        c.axis_titles.1 = kid(y, "chart:title").map(|t| t.text());
    }
    if let Some(x) = x {
        if let Some(cats) = kid(x, "chart:categories").and_then(|c| attr(c, "table:cell-range-address")) {
            c.categories =
                grid.range(cats).into_iter().map(|(t, n)| if t.is_empty() { n.map(|v| v.to_string()).unwrap_or_default() } else { t }).collect();
        }
        if style(x).chart_prop("chart:reverse-direction") == Some("true") {
            c.cat_reversed = true;
        }
        c.axis_titles.0 = kid(x, "chart:title").map(|t| t.text());
    }
    read_series(&mut c, plot, &grid, &store, class, &plot_style);
    if c.categories.is_empty() && !matches!(c.kind, ChartType::Scatter | ChartType::Bubble) {
        c.categories = grid.rows.iter().skip(HEADER_ROWS).map(|r| r.first().map(|(t, _)| t.clone()).unwrap_or_default()).collect();
        let n = c.series.iter().map(|s| s.values.len()).max().unwrap_or(0);
        c.categories.truncate(n);
    }

    // Bar spacing: the plot area's, else the value axis's (LibreOffice's place for it).
    let y_axis = kids(plot, "chart:axis").find(|a| attr(a, "chart:dimension") == Some("y")).map(style);
    let cp = |k: &str| cp(k).or_else(|| y_axis.as_ref().and_then(|s| s.chart_prop(k)));
    if let Some(g) = cp("chart:gap-width").and_then(|v| v.parse::<f64>().ok()) {
        c.gap_width = (g / 100.0).clamp(0.0, 5.0);
    }
    if let Some(o) = cp("chart:overlap").and_then(|v| v.parse::<f64>().ok()) {
        c.overlap = o / 100.0;
    }
    Some(c)
}

/// The plot area's series: values, names, X values and sizes from the data table, and colours.
fn read_series(c: &mut Chart, plot: &El, grid: &Grid, store: &Store, class: &str, plot_style: &crate::styles::Style) {
    let cp = |k: &str| plot_style.chart_prop(k);
    let series_els: Vec<&El> = kids(plot, "chart:series").take(256).collect();
    // Points with their own fill, across the chart's series.
    let mut fills_left = MAX_GRID_CELLS;
    for (i, s) in series_els.iter().enumerate() {
        let ss = chart_style(store, s);
        let values = attr(s, "chart:values-cell-range-address").map(|a| grid.range(a)).unwrap_or_else(|| {
            // No address: the (i+1)th column below the header row.
            grid.rows.iter().skip(HEADER_ROWS).take(MAX_CELLS).map(|r| r.get(i + 1).cloned().unwrap_or_default()).collect()
        });
        let name = attr(s, "chart:label-cell-address")
            .and_then(|a| grid.range(a).into_iter().next())
            .map(|(t, _)| t)
            .unwrap_or_else(|| format!("Series {}", i + 1));
        let mut series = Series { name, values: values.iter().map(|(_, n)| *n).collect(), ..Default::default() };
        // A scatter series' domain is its X values. A bubble series' values are its sizes, and
        // its domains its Y values, then its X values (with none, its values are Y and its
        // bubbles one size, rather than nothing drawn).
        let domain = |k: usize| -> Option<Vec<Option<f64>>> {
            let a = kids(s, "chart:domain").nth(k).and_then(|d| attr(d, "table:cell-range-address"))?;
            Some(grid.range(a).iter().map(|(_, n)| *n).collect())
        };
        match domain(0) {
            Some(ys) if c.kind == ChartType::Bubble => {
                series.sizes = std::mem::replace(&mut series.values, ys);
                series.x_values = domain(1).unwrap_or_default();
            }
            Some(xs) => series.x_values = xs,
            None => {}
        }
        if matches!(c.kind, ChartType::Line | ChartType::StackedLine | ChartType::Scatter | ChartType::Radar) {
            // Lines are drawn in their stroke colour.
            if let Some(col) = ss.graphic.get("svg:stroke-color").and_then(|v| units::color(v)) {
                let w = ss.graphic.get("svg:stroke-width").and_then(|v| units::length(v)).filter(|w| *w > 0.0).unwrap_or(2.0);
                series.line = Some(Line::solid(ColorRef::rgb(col), w));
            }
            let symbol = ss.chart_prop("chart:symbol-type").or(cp("chart:symbol-type"));
            if symbol.is_some_and(|v| v != "none") && c.kind == ChartType::Line {
                c.kind = ChartType::LineMarkers;
            }
            if ss.chart_prop("chart:interpolation").is_some_and(|v| v != "none") {
                series.smooth = true;
            }
        }
        series.fill = simple_fill(&ss.graphic);
        // Labels are a series' own.
        series.data_labels = Some(ss.chart_prop("chart:data-label-number").is_some_and(|v| v != "none"));
        // Points with their own fill (pie slices), for the points the series has.
        let mut idx = 0usize;
        let mut styled = vec![];
        for p in kids(s, "chart:data-point") {
            if idx >= series.values.len() {
                break;
            }
            let rep = repeat(p, "chart:repeated").min(series.values.len() - idx);
            if let Some(f) = simple_fill(&chart_style(store, p).graphic) {
                styled.push((idx, rep, f));
            }
            idx += rep;
        }
        if let Some(cl) = attr(s, "chart:class").map(|v| v.trim_start_matches("chart:"))
            && cl != class
        {
            series.kind = Some(kind_of(cl, false, false, false));
        }
        if attr(s, "chart:attached-axis") == Some("secondary-y") {
            series.secondary_axis = true;
        }
        // A chart that can vary its colours (see [`Chart::varies`]: a pie, or a lone series with every
        // point styled) does when its series has no colour of its own (as we write it). Other
        // files say no more (LibreOffice writes both kinds as styled points): it varies unless
        // most points are in the series' colour (a varied one matches it on one point at most; a
        // single-colour one may recolour a few). A varied chart keeps every styled point's
        // colour, a single-colour one those not in its own.
        let all_styled = styled.iter().map(|(_, rep, _)| rep).sum::<usize>() == series.values.len();
        let can_vary = i == 0 && (c.kind.is_pie() || series_els.len() == 1 && all_styled && c.kind != ChartType::Waterfall);
        let varied = can_vary
            && series.fill.as_ref().is_none_or(|own| {
                let same: usize = styled.iter().filter(|(_, _, f)| f == own).map(|(_, rep, _)| rep).sum();
                same * 2 <= series.values.len().max(2)
            });
        if can_vary {
            c.vary_colors = varied;
        }
        for (at, rep, f) in styled {
            if varied || series.fill.as_ref() != Some(&f) {
                let n = rep.min(fills_left);
                fills_left -= n;
                series.point_fills.extend((at..at + n).map(|k| (k, f.clone())));
            }
        }
        c.series.push(series);
    }
}

/// (model type, ODF `chart:class`, bars run sideways (`chart:vertical`), stacked, percent).
/// Percent charts are written unstacked (`chart:percentage` alone). Types without a row are
/// written as columns.
const KINDS: &[(ChartType, &str, bool, bool, bool)] = &[
    (ChartType::Column, "bar", false, false, false),
    (ChartType::StackedColumn, "bar", false, true, false),
    (ChartType::PercentColumn, "bar", false, false, true),
    (ChartType::Bar, "bar", true, false, false),
    (ChartType::StackedBar, "bar", true, true, false),
    (ChartType::PercentBar, "bar", true, false, true),
    (ChartType::Line, "line", false, false, false),
    (ChartType::LineMarkers, "line", false, false, false),
    (ChartType::StackedLine, "line", false, true, false),
    (ChartType::Area, "area", false, false, false),
    (ChartType::StackedArea, "area", false, true, false),
    (ChartType::Pie, "circle", false, false, false),
    (ChartType::Doughnut, "ring", false, false, false),
    (ChartType::Scatter, "scatter", false, false, false),
    (ChartType::Bubble, "bubble", false, false, false),
    (ChartType::Radar, "radar", false, false, false),
    (ChartType::FilledRadar, "filled-radar", false, false, false),
    (ChartType::Stock, "stock", false, false, false),
];

/// The model chart type of an ODF chart class and its plot-area flags: the exact row, else one
/// stacked the same way (a stacked percent chart is a percent chart), else the class's first.
pub(crate) fn kind_of(class: &str, vertical: bool, stacked: bool, percent: bool) -> ChartType {
    let of_class = || KINDS.iter().filter(|r| r.1 == class);
    of_class()
        .find(|r| r.2 == vertical && r.4 == percent && (percent || r.3 == stacked))
        .or_else(|| of_class().find(|r| r.2 == vertical && r.3 == (stacked || percent)))
        .or_else(|| of_class().next())
        .map_or(ChartType::Column, |r| r.0)
}

/// The ODF class and plot-area flags (vertical, stacked, percent) of a model chart type.
pub(crate) fn class_of(kind: ChartType) -> (&'static str, bool, bool, bool) {
    let r = KINDS.iter().find(|r| r.0 == kind).or(KINDS.first());
    r.map_or(("bar", false, false, false), |r| (r.1, r.2, r.3, r.4))
}

/// Our legend position (`r`, `tr` …) for `chart:legend-position`; unknown ones on the right.
pub(crate) fn legend_of(position: &str) -> &'static str {
    crate::vocab::read(crate::vocab::LEGEND, position).unwrap_or("r")
}

/// The `chart:legend-position` of ours; unknown ones at the end.
pub(crate) fn legend_position(ours: &str) -> &'static str {
    crate::vocab::write(crate::vocab::LEGEND, &ours).unwrap_or("end")
}

fn simple_fill(g: &crate::styles::Props) -> Option<Fill> {
    match g.get("draw:fill").map(String::as_str) {
        Some("none") => Some(Fill::None),
        Some("solid") | None => Some(Fill::solid(crate::styles::color_prop(g, "draw:fill-color", "draw:opacity")?)),
        Some("gradient") => g.get("draw:fill-color").and_then(|v| units::color(v)).map(|c| Fill::solid(ColorRef::rgb(c))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every chart type with an ODF class reads back as itself.
    #[test]
    fn chart_kinds_and_legends_map_both_ways() {
        for (kind, ..) in KINDS {
            let (class, vertical, stacked, percent) = class_of(*kind);
            let back = kind_of(class, vertical, stacked, percent);
            // Line markers are told apart by their symbols, not the class.
            let want = if *kind == ChartType::LineMarkers { ChartType::Line } else { *kind };
            assert_eq!(back, want);
        }
        assert_eq!(kind_of("bar", true, true, true), ChartType::PercentBar);
        assert_eq!(legend_of(legend_position("tr")), "tr");
    }

    /// Regression: a bubble series without domains (no Y or X values) lost its values to the
    /// sizes, and drew nothing.
    #[test]
    fn a_bubble_series_without_domains_keeps_its_values() {
        let x = br#"<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:chart="urn:oasis:names:tc:opendocument:xmlns:chart:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"><office:body><office:chart><chart:chart chart:class="chart:bubble"><chart:plot-area><chart:series chart:values-cell-range-address="local-table.$B$2:.$B$3"/></chart:plot-area><table:table table:name="local-table"><table:table-header-rows><table:table-row><table:table-cell/><table:table-cell office:value-type="string"><text:p>S</text:p></table:table-cell></table:table-row></table:table-header-rows><table:table-rows><table:table-row><table:table-cell office:value-type="string"><text:p>a</text:p></table:table-cell><table:table-cell office:value-type="float" office:value="3"/></table:table-row><table:table-row><table:table-cell office:value-type="string"><text:p>b</text:p></table:table-cell><table:table-cell office:value-type="float" office:value="5"/></table:table-row></table:table-rows></table:table></chart:chart></office:chart></office:body></office:document-content>"#;
        let c = read(&crate::dom::parse(x).unwrap()).unwrap();
        assert_eq!(c.kind, ChartType::Bubble);
        assert_eq!((c.series[0].values.clone(), c.series[0].sizes.len()), (vec![Some(3.0), Some(5.0)], 0));
    }

    #[test]
    fn cell_references() {
        assert_eq!(cell_ref("local-table.$C$2"), Some((2, 1)));
        assert_eq!(cell_ref(".A1"), Some((0, 0)));
        assert_eq!(cell_ref("'Sheet 1'.$AB$10"), Some((27, 9)));
        assert_eq!(cell_ref("bogus"), None);
        assert_eq!(cell_ref(".A0"), None);
    }
}
