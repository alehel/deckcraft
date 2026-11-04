//! Charts: an embedded chart document (`Object N/content.xml`, `office:chart`) read into the
//! model's chart, and written back as one.

use deckcraft_color::ColorTransform;
use deckcraft_model::chart::{Chart, ChartType, Series};
use deckcraft_model::style::{ColorRef, Fill, Line};

use crate::dom::{El, attr, kid, kids};
use crate::styles::{Scope, Store};
use crate::units;

/// Most rows and columns read from a chart's data table.
const MAX_CELLS: usize = 4096;

/// The chart's local data: cells as text and number, row by row.
#[derive(Default)]
struct Grid {
    rows: Vec<Vec<(String, Option<f64>)>>,
}

impl Grid {
    fn read(t: &El) -> Grid {
        let mut g = Grid::default();
        let mut rows: Vec<&El> = vec![];
        collect_rows(t, &mut rows, 0);
        for r in rows {
            let rep = attr(r, "table:number-rows-repeated").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).clamp(1, 64);
            let mut cells = vec![];
            for c in r.elements().filter(|c| c.name == "table:table-cell" || c.name == "table:covered-table-cell") {
                let crep = attr(c, "table:number-columns-repeated").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).clamp(1, 64);
                let text = kids(c, "text:p").map(|p| p.text()).collect::<Vec<_>>().join("\n");
                let num = if attr(c, "office:value-type") == Some("float") || attr(c, "office:value-type") == Some("percentage") {
                    attr(c, "office:value").and_then(|v| v.parse::<f64>().ok()).filter(|v| v.is_finite())
                } else {
                    None
                };
                for _ in 0..crep {
                    if cells.len() < MAX_CELLS {
                        cells.push((text.clone(), num));
                    }
                }
            }
            for _ in 0..rep {
                if g.rows.len() < MAX_CELLS {
                    g.rows.push(cells.clone());
                }
            }
        }
        g
    }

    fn get(&self, r: usize, c: usize) -> Option<&(String, Option<f64>)> {
        self.rows.get(r).and_then(|row| row.get(c))
    }

    /// Cells of a range (`local-table.$B$2:.$B$5`, several ranges separated by spaces), in order.
    fn range(&self, addr: &str) -> Vec<(String, Option<f64>)> {
        let mut out = vec![];
        for part in addr.split_whitespace().take(64) {
            let (a, b) = part.split_once(':').unwrap_or((part, part));
            let (Some((c0, r0)), Some((c1, r1))) = (cell_ref(a), cell_ref(b)) else { continue };
            for r in r0.min(r1)..=r0.max(r1).min(r0.min(r1) + MAX_CELLS) {
                for c in c0.min(c1)..=c0.max(c1).min(c0.min(c1) + MAX_CELLS) {
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

/// Read the chart document of `draw:object` at `dir` (`Object 1`).
pub fn read(content: &El) -> Option<Chart> {
    let mut store = Store::default();
    store.add_document(content, Scope::Content);
    let chart = kid(content, "office:body").and_then(|b| kid(b, "office:chart")).and_then(|c| kid(c, "chart:chart"))?;
    let style = |el: &El| attr(el, "chart:style-name").map(|n| store.resolve(Scope::Content, "chart", Some(n))).unwrap_or_default();
    let plot = kid(chart, "chart:plot-area");
    let plot_style = plot.map(style).unwrap_or_default();
    let cp = |k: &str| plot_style.chart_prop(k);
    let class = attr(chart, "chart:class").unwrap_or("chart:bar").trim_start_matches("chart:");
    let vertical = cp("chart:vertical") == Some("true");
    let stacked = cp("chart:stacked") == Some("true");
    let percent = cp("chart:percentage") == Some("true");
    // LibreOffice's defaults where the file is silent: bars as wide as their gaps, no overlap.
    let mut c = Chart { title: None, legend: None, gap_width: 1.0, overlap: 0.0, ..Default::default() };
    c.kind = chart_kind(class, vertical, stacked, percent);
    c.title = kid(chart, "chart:title").map(|t| kids(t, "text:p").map(|p| p.text()).collect::<Vec<_>>().join(" ")).filter(|t| !t.trim().is_empty());
    if let Some(t) = kid(chart, "chart:title") {
        c.title_size = style(t).text.get("fo:font-size").and_then(|v| units::length(v));
    }
    if let Some(l) = kid(chart, "chart:legend") {
        c.legend = Some(
            match attr(l, "chart:legend-position").unwrap_or("end") {
                "start" => "l",
                "top" => "t",
                "bottom" => "b",
                "top-end" => "tr",
                _ => "r",
            }
            .into(),
        );
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
    let header_rows = 1usize;
    read_series(&mut c, plot, &grid, &store, class, &plot_style);
    if c.categories.is_empty() && !matches!(c.kind, ChartType::Scatter | ChartType::Bubble) {
        c.categories = grid.rows.iter().skip(header_rows).map(|r| r.first().map(|(t, _)| t.clone()).unwrap_or_default()).collect();
        let n = c.series.iter().map(|s| s.values.len()).max().unwrap_or(0);
        c.categories.truncate(n);
    }
    if matches!(c.kind, ChartType::Pie | ChartType::Doughnut) {
        c.vary_colors = true;
    }
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
    let header_rows = 1usize;
    let style = |el: &El| attr(el, "chart:style-name").map(|n| store.resolve(Scope::Content, "chart", Some(n))).unwrap_or_default();
    let series_els: Vec<&El> = kids(plot, "chart:series").take(256).collect();
    for (i, s) in series_els.iter().enumerate() {
        let ss = style(s);
        let values = attr(s, "chart:values-cell-range-address").map(|a| grid.range(a)).unwrap_or_else(|| {
            // No address: the (i+1)th column below the header row.
            grid.rows.iter().skip(header_rows).map(|r| r.get(i + 1).cloned().unwrap_or_default()).collect()
        });
        let name = attr(s, "chart:label-cell-address")
            .and_then(|a| grid.range(a).into_iter().next())
            .map(|(t, _)| t)
            .unwrap_or_else(|| format!("Series {}", i + 1));
        let mut series = Series { name, values: values.iter().map(|(_, n)| *n).collect(), ..Default::default() };
        // Scatter and bubble charts: the first domain is X, a second (bubble) is the size.
        let domains: Vec<&El> = kids(s, "chart:domain").collect();
        if let Some(d) = domains.first().and_then(|d| attr(d, "table:cell-range-address")) {
            let xs: Vec<Option<f64>> = grid.range(d).iter().map(|(_, n)| *n).collect();
            if c.kind == ChartType::Bubble {
                series.sizes = series.values.clone();
                series.values = domains
                    .get(1)
                    .and_then(|d| attr(d, "table:cell-range-address"))
                    .map(|a| grid.range(a).iter().map(|(_, n)| *n).collect())
                    .unwrap_or_default();
                series.x_values = xs;
            } else {
                series.x_values = xs;
            }
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
        if ss.chart_prop("chart:data-label-number").is_some_and(|v| v != "none") {
            series.data_labels = Some(true);
            c.data_labels = true;
        }
        // Per-point overrides (pie slices).
        let mut idx = 0usize;
        for p in kids(s, "chart:data-point").take(MAX_CELLS) {
            let rep = attr(p, "chart:repeated").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).clamp(1, MAX_CELLS);
            if let Some(f) =
                attr(p, "chart:style-name").map(|n| store.resolve(Scope::Content, "chart", Some(n))).and_then(|st| simple_fill(&st.graphic))
            {
                for k in 0..rep {
                    if series.fill.as_ref() != Some(&f) {
                        series.point_fills.push((idx + k, f.clone()));
                    }
                }
            }
            idx += rep;
        }
        if let Some(cl) = attr(s, "chart:class").map(|v| v.trim_start_matches("chart:"))
            && cl != class
        {
            series.kind = Some(match cl {
                "line" => ChartType::Line,
                "area" => ChartType::Area,
                _ => ChartType::Column,
            });
        }
        if attr(s, "chart:attached-axis") == Some("secondary-y") {
            series.secondary_axis = true;
        }
        c.series.push(series);
    }
}

/// The model chart type of an ODF chart class and its plot-area flags.
fn chart_kind(class: &str, vertical: bool, stacked: bool, percent: bool) -> ChartType {
    match (class, vertical, stacked, percent) {
        ("bar", true, _, true) => ChartType::PercentBar,
        ("bar", true, true, _) => ChartType::StackedBar,
        ("bar", true, ..) => ChartType::Bar,
        ("bar", _, _, true) => ChartType::PercentColumn,
        ("bar", _, true, _) => ChartType::StackedColumn,
        ("line", _, true, _) => ChartType::StackedLine,
        ("line", ..) => ChartType::Line,
        ("area", _, s, p) if s || p => ChartType::StackedArea,
        ("area", ..) => ChartType::Area,
        ("circle", ..) => ChartType::Pie,
        ("ring", ..) => ChartType::Doughnut,
        ("scatter", ..) => ChartType::Scatter,
        ("bubble", ..) => ChartType::Bubble,
        ("radar", ..) => ChartType::Radar,
        ("filled-radar", ..) => ChartType::FilledRadar,
        ("stock", ..) => ChartType::Stock,
        _ => ChartType::Column,
    }
}

fn simple_fill(g: &crate::styles::Props) -> Option<Fill> {
    match g.get("draw:fill").map(String::as_str) {
        Some("none") => Some(Fill::None),
        Some("solid") | None => {
            let col = g.get("draw:fill-color").and_then(|v| units::color(v))?;
            let mut cr = ColorRef::rgb(col);
            if let Some(a) = g.get("draw:opacity").and_then(|v| units::percent(v)).filter(|a| *a < 1.0) {
                cr.mods.push(ColorTransform::Alpha((a * 100_000.0) as i32));
            }
            Some(Fill::solid(cr))
        }
        Some("gradient") => g.get("draw:fill-color").and_then(|v| units::color(v)).map(|c| Fill::solid(ColorRef::rgb(c))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_references() {
        assert_eq!(cell_ref("local-table.$C$2"), Some((2, 1)));
        assert_eq!(cell_ref(".A1"), Some((0, 0)));
        assert_eq!(cell_ref("'Sheet 1'.$AB$10"), Some((27, 9)));
        assert_eq!(cell_ref("bogus"), None);
        assert_eq!(cell_ref(".A0"), None);
    }
}
