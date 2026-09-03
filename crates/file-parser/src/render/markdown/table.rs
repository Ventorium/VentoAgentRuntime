//! Table rendering over the canonical grid. Span-less tables render as GFM
//! pipes; tables carrying any merge render as an HTML `<table>` (GFM has no
//! span syntax, covered positions become `colspan`/`rowspan` attributes).

use crate::model::{Block, Cell, CellSlot, Table};
use crate::render::markdown::Ctx;
use crate::render::markdown::escape::InlineContext;
use crate::render::markdown::inline::{push_code_span, push_math_span, render_inlines};

struct RenderedCell {
    text: String,
    covered_span: bool,
}

/// True when any grid position is swallowed by a span — such tables cannot
/// be represented in GFM and fall back to an HTML `<table>`.
fn has_spans(table: &Table) -> bool {
    table
        .grid
        .iter()
        .flatten()
        .any(|slot| matches!(slot, CellSlot::Covered { .. }))
}

pub(crate) fn render_table(table: &Table, rc: &Ctx) -> Option<String> {
    // Interior empty rows render as blank rows — they carry the source's
    // row coordinates; trailing blank rows are popped below.
    if table.grid.is_empty() {
        return None;
    }
    if has_spans(table) {
        return render_table_html(table, rc);
    }
    let width = table.grid.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut rendered: Vec<Vec<RenderedCell>> = table
        .grid
        .iter()
        .map(|row| {
            let mut cells: Vec<RenderedCell> = row
                .iter()
                .map(|slot| match slot {
                    CellSlot::Origin(cell) => RenderedCell {
                        text: render_cell(cell, rc, InlineContext::TableCell),
                        covered_span: false,
                    },
                    CellSlot::Covered { .. } => RenderedCell {
                        text: String::new(),
                        covered_span: true,
                    },
                })
                .collect();
            cells.resize_with(width, || RenderedCell {
                text: String::new(),
                covered_span: false,
            });
            cells
        })
        .collect();
    while rendered.len() > 1
        && rendered
            .last()
            .is_some_and(|r| r.iter().all(|c| c.text.is_empty() && !c.covered_span))
    {
        rendered.pop();
    }
    let width = rendered
        .iter()
        .map(|r| {
            r.iter()
                .rposition(|c| !c.text.is_empty() || c.covered_span)
                .map_or(0, |i| i + 1)
        })
        .max()
        .unwrap_or(0);
    if width == 0 {
        return None;
    }
    for row in &mut rendered {
        row.truncate(width);
    }

    let mut out = String::new();
    // GFM tables always carry a delimiter row, so a table with no header row
    // of its own renders an empty one above the data.
    let header: Vec<String> = if table.header_rows >= 1 && !rendered.is_empty() {
        rendered.remove(0).into_iter().map(|c| c.text).collect()
    } else {
        vec![String::new(); width]
    };
    out.push_str(&format_row(header.iter().map(String::as_str)));
    out.push('\n');
    out.push_str(&format_row(std::iter::repeat_n("---", width)));
    for row in &rendered {
        out.push('\n');
        out.push_str(&format_row(row.iter().map(|c| c.text.as_str())));
    }
    Some(out)
}

fn format_row<'a>(cells: impl IntoIterator<Item = &'a str>) -> String {
    let mut s = String::from("|");
    for cell in cells {
        s.push(' ');
        s.push_str(cell);
        s.push_str(" |");
    }
    s
}

/// HTML fallback for tables carrying merges. Covered positions are skipped
/// (the origin's `colspan`/`rowspan` accounts for them); header rows render
/// as `<th>`. Cell text is the same flattened markdown the GFM path builds,
/// HTML-escaped except the `<br>` paragraph separators, which stay real
/// tags. Rows that hold only covered positions are structural (a rowspan
/// reaching into them) and are kept; `GridBuilder::finish` has already
/// trimmed trailing all-empty rows, so nothing blank survives here.
fn render_table_html(table: &Table, rc: &Ctx) -> Option<String> {
    let mut out = String::from("<table>");
    let mut any_content = false;
    for (r, row) in table.grid.iter().enumerate() {
        let tag = if r < table.header_rows { "th" } else { "td" };
        out.push_str("\n<tr>");
        for slot in row {
            let CellSlot::Origin(cell) = slot else {
                continue;
            };
            let text = escape_html_cell(&render_cell(cell, rc, InlineContext::HtmlCell));
            if !text.is_empty() {
                any_content = true;
            }
            out.push('<');
            out.push_str(tag);
            if cell.col_span > 1 {
                out.push_str(&format!(" colspan=\"{}\"", cell.col_span));
            }
            if cell.row_span > 1 {
                out.push_str(&format!(" rowspan=\"{}\"", cell.row_span));
            }
            out.push('>');
            out.push_str(&text);
            out.push_str("</");
            out.push_str(tag);
            out.push('>');
        }
        out.push_str("</tr>");
    }
    if !any_content {
        return None;
    }
    out.push_str("\n</table>");
    Some(out)
}

/// Escape a flattened cell's text for HTML, keeping the `<br>` separators
/// (rendered by `render_cell` between blocks) as real tags.
fn escape_html_cell(text: &str) -> String {
    text.split("<br>")
        .map(|part| {
            part.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
        })
        .collect::<Vec<_>>()
        .join("<br>")
}

/// Flatten arbitrary block content into a single table-cell line.
pub(crate) fn render_cell(cell: &Cell, rc: &Ctx, ctx: InlineContext) -> String {
    let mut parts: Vec<String> = Vec::new();
    for block in &cell.blocks {
        cell_block_text(block, rc, ctx, &mut parts);
    }
    // A cell's edge whitespace is padding the table's own padding would
    // swallow anyway, and spreadsheets carry it by the thousand.
    parts
        .join("<br>")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("<br>")
}

fn cell_block_text(block: &Block, rc: &Ctx, ctx: InlineContext, parts: &mut Vec<String>) {
    match block {
        Block::Heading { content, .. } => {
            let t = render_inlines(content, ctx, rc);
            if !t.trim().is_empty() {
                parts.push(format!("**{}**", t.trim()));
            }
        }
        Block::Paragraph(inlines) => {
            // Edge whitespace is preserved here and protected in
            // `render_cell`; sources that retain cell padding (spreadsheets,
            // CSV) keep it in the final output.
            let t = render_inlines(inlines, ctx, rc);
            if !t.trim().is_empty() {
                parts.push(t);
            }
        }
        Block::List(list) => {
            for (i, item) in list.items.iter().enumerate() {
                let mut inner: Vec<String> = Vec::new();
                for b in &item.blocks {
                    cell_block_text(b, rc, ctx, &mut inner);
                }
                let marker = match (&item.marker_label, list.marker) {
                    (Some(label), _) => {
                        format!(
                            "{} ",
                            crate::render::markdown::escape_marker_label(label, ctx)
                        )
                    }
                    (None, crate::model::MarkerKind::Bullet) => "• ".to_string(),
                    (None, kind) => {
                        format!("{} ", kind.label(list.start.saturating_add(i as u64)))
                    }
                };
                if !inner.is_empty() {
                    parts.push(format!("{marker}{}", inner.join(" ")));
                }
            }
        }
        Block::Table(table) => {
            // Keep empty cells in the join so values stay in their source
            // column positions.
            for row in &table.grid {
                let cells: Vec<String> = row
                    .iter()
                    .map(|slot| match slot {
                        CellSlot::Origin(cell) => render_cell(cell, rc, ctx),
                        CellSlot::Covered { .. } => String::new(),
                    })
                    .collect();
                if cells.iter().any(|c| !c.is_empty()) {
                    parts.push(cells.join(" / "));
                }
            }
        }
        Block::BlockQuote(blocks) => {
            for b in blocks {
                cell_block_text(b, rc, ctx, parts);
            }
        }
        Block::CodeBlock { text, .. } => {
            let t = text.trim();
            if !t.is_empty() {
                let mut s = String::new();
                push_code_span(t, ctx, &mut s);
                parts.push(s);
            }
        }
        Block::Math(tex) => {
            if !tex.trim().is_empty() {
                let mut s = String::new();
                push_math_span(tex, ctx, &mut s);
                parts.push(s);
            }
        }
        Block::Rule => {}
    }
}
