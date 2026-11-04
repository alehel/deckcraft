//! Tables: rows, repeated cells and spans read within bounds, cell styles, sizes.

use deckcraft_color::Rgba;
use deckcraft_model::Xfrm;
use deckcraft_model::style::{ColorRef, Dash, Line};
use deckcraft_model::table::{Cell, Row, Table};
use deckcraft_model::text::{Paragraph, TextBody};

use super::{Ctx, Reader, anchor, fill_rgb};
use crate::dom::{El, attr, kids, repeat};
use crate::text::{self, TextCtx};
use crate::units;

/// Most rows, and most columns, read from one table.
const MAX_TABLE_SIDE: usize = 1000;
/// Most cells read into one table.
const MAX_TABLE_CELLS: usize = 65_536;
/// Most cells all of a document's tables hold together.
pub(super) const MAX_DOCUMENT_CELLS: usize = 262_144;
/// Most text (bytes) a table's repeated cells and rows may copy.
const MAX_TABLE_TEXT: usize = 4 << 20;

impl Reader<'_> {
    pub(super) fn table(&mut self, t: &El, ctx: &Ctx, x: Xfrm) -> Table {
        let mut table = Table { style: String::new(), ..Default::default() };
        let flag = |k: &str| attr(t, k) == Some("true");
        table.first_row = flag("table:use-first-row-styles");
        table.last_row = flag("table:use-last-row-styles");
        table.first_col = flag("table:use-first-column-styles");
        table.last_col = flag("table:use-last-column-styles");
        table.band_row = flag("table:use-banding-rows-styles");
        table.band_col = flag("table:use-banding-columns-styles");
        for c in t.elements().filter(|c| c.name == "table:table-column" || c.name == "table:table-columns").take(1000) {
            let cols: Vec<&El> = if c.name == "table:table-columns" { kids(c, "table:table-column").collect() } else { vec![c] };
            for c in cols {
                let rep = repeat(c, "table:number-columns-repeated").min(MAX_TABLE_SIDE);
                let w = attr(c, "table:style-name")
                    .map(|n| self.store.resolve(ctx.scope, "table-column", Some(n)))
                    .and_then(|s| s.column.get("style:column-width").and_then(|v| units::length(v)))
                    .filter(|w| *w > 0.0);
                let n = rep.min(MAX_TABLE_SIDE - table.cols.len());
                table.cols.extend(std::iter::repeat_n(w.unwrap_or(0.0), n));
            }
        }
        let rows: Vec<&El> = t
            .elements()
            .flat_map(|e| {
                if e.name == "table:table-row" {
                    vec![e]
                } else if e.name == "table:table-rows" || e.name == "table:table-header-rows" {
                    kids(e, "table:table-row").collect()
                } else {
                    vec![]
                }
            })
            .take(MAX_TABLE_SIDE)
            .collect();
        // The table's width before any cell is read, so every row is budgeted at the width it
        // will be padded to.
        let is_cell = |c: &&El| c.name == "table:table-cell" || c.name == "table:covered-table-cell";
        let row_width = |r: &El| r.elements().filter(is_cell).map(|c| repeat(c, "table:number-columns-repeated")).fold(0usize, usize::saturating_add);
        let width = rows.iter().map(|r| row_width(r)).max().unwrap_or(0).max(table.cols.len()).clamp(1, MAX_TABLE_SIDE);
        let mut text_budget = MAX_TABLE_TEXT;
        for row in rows {
            if table.rows.len() >= MAX_TABLE_SIDE {
                break;
            }
            let rs = attr(row, "table:style-name").map(|n| self.store.resolve(ctx.scope, "table-row", Some(n)));
            let h = rs
                .as_ref()
                .and_then(|s| s.row.get("style:row-height").or_else(|| s.row.get("style:min-row-height")).and_then(|v| units::length(v)))
                .unwrap_or(0.0);
            // An optimal-height row is as tall as its text: give it a small height to grow from.
            let optimal = rs.as_ref().is_some_and(|s| s.row.get("style:use-optimal-row-height").map(String::as_str) == Some("true"));
            let h = if optimal && h <= 0.0 { 12.0 } else { h };
            let default_cell = attr(row, "table:default-cell-style-name");
            let mut cells: Vec<Cell> = vec![];
            for c in row.elements().filter(is_cell) {
                let n = repeat(c, "table:number-columns-repeated").min(width - cells.len().min(width));
                if n == 0 {
                    break;
                }
                // A repeated cell is read once; its copies keep their text while the budget lasts.
                let cell = self.cell(c, ctx, default_cell);
                let size = cell.text.text().len();
                for k in 0..n {
                    if k == 0 || size <= text_budget {
                        text_budget = text_budget.saturating_sub(if k == 0 { 0 } else { size });
                        cells.push(cell.clone());
                    } else {
                        cells.push(Cell { text: Cell::new("").text, ..cell.clone() });
                    }
                }
            }
            // Repeated rows cost the full width, and their text the budget.
            // A table's own budget, and what the document has left.
            let budget = MAX_TABLE_CELLS.min(self.cells_left.saturating_add(table.rows.len() * width));
            let room = budget.saturating_sub(table.rows.len() * width).checked_div(width).unwrap_or(0);
            let row_text: usize = cells.iter().map(|c| c.text.text().len()).sum();
            let text_room = text_budget.checked_div(row_text).map_or(usize::MAX, |r| r + 1);
            let n = repeat(row, "table:number-rows-repeated").min(MAX_TABLE_SIDE - table.rows.len()).min(room).min(text_room);
            text_budget = text_budget.saturating_sub(row_text.saturating_mul(n.saturating_sub(1)));
            if n == 0 {
                break;
            }
            table.rows.extend(std::iter::repeat_n(Row { height: h, cells }, n));
        }
        fit_table(&mut table, x);
        let read: usize = table.rows.iter().map(|r| r.cells.len()).sum();
        self.cells_left = self.cells_left.saturating_sub(read);
        merge_spans(&mut table);
        table
    }

    fn cell(&mut self, c: &El, ctx: &Ctx, default_style: Option<&str>) -> Cell {
        let mut cell = Cell::new("");
        if c.name == "table:covered-table-cell" {
            return cell;
        }
        cell.grid_span = attr(c, "table:number-columns-spanned").and_then(|v| v.parse().ok()).unwrap_or(1).clamp(1, 1000);
        cell.row_span = attr(c, "table:number-rows-spanned").and_then(|v| v.parse().ok()).unwrap_or(1).clamp(1, 1000);
        // A cell's style is a graphic style (the row's default, `standard` in LibreOffice) with
        // its own cell style over it.
        let mut st = self.store.resolve(
            ctx.scope,
            "graphic",
            Some(default_style.filter(|n| self.store.exists(ctx.scope, "graphic", n)).unwrap_or("standard")),
        );
        for name in [default_style, attr(c, "table:style-name")].into_iter().flatten() {
            if self.store.exists(ctx.scope, "table-cell", name) {
                st.merge(&self.store.chain(ctx.scope, "table-cell", name));
            }
        }
        let mut g = st.graphic.clone();
        match st.cell.get("fo:background-color").map(String::as_str) {
            Some("transparent") => {
                g.insert("draw:fill".into(), "none".into());
            }
            Some(bg) => {
                g.insert("draw:fill".into(), "solid".into());
                g.insert("draw:fill-color".into(), bg.to_string());
            }
            None => {}
        }
        // Unset, a cell is filled (the built-in default), as the slide shows it.
        cell.fill = Some(self.fill(&g));
        // An unset border is none (not the table style's). The model's order: left, right, top,
        // bottom.
        // LibreOffice keeps a cell's borders in its paragraph properties, and its padding in its
        // graphic properties.
        let prop = |p: &'_ crate::styles::Props, k: &str, all: &str| p.get(k).or_else(|| p.get(all)).cloned();
        let border = |k: &str| {
            Some(prop(&st.cell, k, "fo:border").or_else(|| prop(&st.paragraph, k, "fo:border")).map_or_else(Line::none, |b| border_line(&b)))
        };
        cell.borders = [border("fo:border-left"), border("fo:border-right"), border("fo:border-top"), border("fo:border-bottom")];
        let pad = |k: &str| prop(&st.cell, k, "fo:padding").or_else(|| prop(&st.graphic, k, "fo:padding")).and_then(|v| units::length(&v));
        if let (Some(l), Some(t), Some(r), Some(b)) =
            (pad("fo:padding-left"), pad("fo:padding-top"), pad("fo:padding-right"), pad("fo:padding-bottom"))
        {
            cell.margins = Some([l, r, t, b]);
        }
        cell.anchor = st.cell.get("style:vertical-align").or_else(|| st.graphic.get("draw:textarea-vertical-align")).and_then(|v| anchor(v));
        let mut base = st.clone();
        base.list = None;
        let store = std::rc::Rc::clone(&self.store);
        let behind = cell.fill.as_ref().and_then(fill_rgb).unwrap_or(ctx.bg);
        let tcx = TextCtx {
            store: &store,
            scope: ctx.scope,
            levels: vec![base],
            shape_para: None,
            auto_color: text::auto_text_color(behind),
            inherit: false,
        };
        let mut paragraphs = text::paragraphs(&tcx, c);
        if paragraphs.is_empty() {
            paragraphs.push(Paragraph::default());
        }
        cell.text = TextBody { paragraphs, ..Default::default() };
        cell
    }
}

/// Every row padded to the table's width; columns and rows without a size share what's left of
/// the frame.
fn fit_table(table: &mut Table, x: Xfrm) {
    let ncols = table.rows.iter().map(|r| r.cells.len()).max().unwrap_or(0).max(table.cols.len()).max(1);
    table.cols.resize(ncols, 0.0);
    let known: f64 = table.cols.iter().sum();
    let unknown = table.cols.iter().filter(|w| **w <= 0.0).count();
    if unknown > 0 {
        let each = ((x.w - known).max(0.0) / unknown as f64).max(x.w / ncols as f64 * 0.25).max(10.0);
        for w in &mut table.cols {
            if *w <= 0.0 {
                *w = each;
            }
        }
    }
    let nrows = table.rows.len().max(1);
    let known: f64 = table.rows.iter().map(|r| r.height).sum();
    let unknown = table.rows.iter().filter(|r| r.height <= 0.0).count();
    if unknown > 0 {
        let each = ((x.h - known).max(0.0) / unknown as f64).max(x.h / nrows as f64 * 0.25).max(12.0);
        for r in &mut table.rows {
            if r.height <= 0.0 {
                r.height = each;
            }
        }
    }
    for r in &mut table.rows {
        while r.cells.len() < ncols {
            r.cells.push(Cell::new(""));
        }
    }
}

/// Spans: each anchor cell spans no further than the table, and the cells it covers are merged
/// into it.
fn merge_spans(table: &mut Table) {
    let (ncols, nrows) = (table.cols.len(), table.rows.len());
    for (ri, r) in table.rows.iter_mut().enumerate() {
        for (ci, c) in r.cells.iter_mut().enumerate() {
            c.grid_span = c.grid_span.min((ncols - ci) as u32).max(1);
            c.row_span = c.row_span.min((nrows - ri) as u32).max(1);
        }
    }
    // In reading order, so a cell an earlier span covers spans nothing itself. Overlapping
    // spans share a budget of visits.
    let mut budget = 4 * MAX_TABLE_CELLS;
    for ri in 0..nrows {
        for ci in 0..ncols {
            let Some(c) = table.cell_mut(ri, ci) else { continue };
            if c.h_merge || c.v_merge {
                (c.grid_span, c.row_span) = (1, 1);
                continue;
            }
            let (gs, rs) = (c.grid_span as usize, c.row_span as usize);
            if gs * rs > budget {
                (c.grid_span, c.row_span) = (1, 1);
                continue;
            }
            budget -= gs * rs;
            for dr in 0..rs {
                for dc in 0..gs {
                    if let Some(c) = table.cell_mut(ri + dr, ci + dc).filter(|_| (dr, dc) != (0, 0))
                        && !(c.h_merge || c.v_merge)
                    {
                        (c.h_merge, c.v_merge) = (dc > 0, dr > 0);
                    }
                }
            }
        }
    }
}

/// A border (`0.03cm solid #000000`, `none`).
fn border_line(b: &str) -> Line {
    if b.trim() == "none" || b.trim().is_empty() {
        return Line::none();
    }
    let mut width = None;
    let mut color = Rgba::BLACK;
    let mut style = "solid";
    for t in b.split_whitespace() {
        if let Some(c) = units::color(t) {
            color = c;
        } else if let Some(w) = units::length(t) {
            width = Some(w);
        } else {
            style = match t {
                "none" | "hidden" => "none",
                "dashed" => "dash",
                "dotted" => "dot",
                _ => style,
            };
        }
    }
    if style == "none" {
        return Line::none();
    }
    let mut l = Line::solid(ColorRef::rgb(color), width.unwrap_or(0.75));
    if style != "solid" {
        l.dash = Some(Dash::Preset { name: style.into() });
    }
    l
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borders_parse() {
        let l = border_line("0.03cm solid #ff0000");
        assert!((l.width.unwrap() - 0.85).abs() < 0.01);
        assert_eq!(border_line("none"), Line::none());
    }
}
