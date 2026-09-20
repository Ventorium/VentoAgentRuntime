//! GitHub-Flavored Markdown serializer for the document model.

mod anchors;
mod escape;
mod inline;
mod table;

#[cfg(test)]
mod tests;

use crate::model::AssetId;
use crate::model::{
    Block, CellSlot, Document, ImageSource, Inline, List, MarkerKind, Note, TableKind,
    inlines_are_empty,
};
use crate::ocr::{OcrOutcome, ocr_annotation};
use anchors::{AnchorMap, resolve_anchors};
use escape::{EscapeOpts, InlineContext, backtick_fence, escape_text};
use inline::render_inlines;
use std::collections::{HashMap, HashSet};

/// Escape a source-derived composite marker label for literal use: control
/// characters collapse to spaces and Markdown syntax is neutralized so a
/// crafted label cannot alter document structure.
pub(crate) fn escape_marker_label(label: &str, ctx: InlineContext) -> String {
    let cleaned: String = label
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let opts = EscapeOpts {
        // List-item content re-opens block syntax after the `- ` marker.
        at_line_start: ctx == InlineContext::Block,
        trailing_active: true,
        ..Default::default()
    };
    escape_text(&cleaned, ctx, opts)
}

/// Footnote id -> rendered number, shared by all render functions.
type NoteNumbers = HashMap<String, usize>;

/// Immutable render context threaded through every render function.
pub(crate) struct Ctx {
    nums: NoteNumbers,
    anchors: AnchorMap,
    /// `图片N` figure numbers for embedded image assets, in first-reference
    /// order; assets referenced nowhere keep the bare `图片` alt.
    asset_numbers: HashMap<AssetId, usize>,
    /// Per-asset OCR outcomes produced by the embedded-image OCR pass; each
    /// one is emitted as an annotation block after the block that first
    /// references the image.
    asset_ocr: HashMap<AssetId, OcrOutcome>,
}

/// Render with per-asset OCR annotation blocks for embedded images.
pub fn document_to_markdown_with_ocr(
    doc: &Document,
    asset_ocr: HashMap<AssetId, OcrOutcome>,
) -> String {
    let rc = Ctx {
        nums: number_notes(doc),
        anchors: resolve_anchors(doc),
        asset_numbers: number_assets(doc),
        asset_ocr,
    };
    let mut annotated: HashSet<AssetId> = HashSet::new();
    let mut parts: Vec<String> = render_blocks_with_annotations(&doc.blocks, &rc, &mut annotated);
    let mut rendered_defs: HashSet<usize> = HashSet::new();
    let mut ordered: Vec<(&Note, usize)> = doc
        .notes
        .iter()
        .filter_map(|n| rc.nums.get(&n.id).map(|&num| (n, num)))
        .collect();
    ordered.sort_by_key(|(_, num)| *num);
    for (note, num) in ordered {
        let body = render_blocks_with_annotations(&note.blocks, &rc, &mut annotated).join("\n\n");
        if body.is_empty() {
            continue;
        }
        // The first non-empty definition for a duplicate id wins.
        if !rendered_defs.insert(num) {
            log::debug!("duplicate note id {:?} dropped from output", note.id);
            continue;
        }
        let mut lines = body.lines();
        let first = lines.next().unwrap_or("");
        let mut s = format!("[^{num}]: {first}");
        for line in lines {
            s.push('\n');
            if !line.is_empty() {
                s.push_str("    ");
                s.push_str(line);
            }
        }
        parts.push(s);
    }
    let mut out = parts.join("\n\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// Render without embedded-image OCR (no provider configured).
pub fn document_to_markdown(doc: &Document) -> String {
    document_to_markdown_with_ocr(doc, HashMap::new())
}

/// Number notes in first-reference order; unreferenced notes follow at the
/// end. The first note wins a duplicated id.
fn number_notes(doc: &Document) -> NoteNumbers {
    let mut valid: HashMap<&str, &Note> = HashMap::new();
    for note in &doc.notes {
        if !note.blocks.iter().all(block_is_blank) {
            valid.entry(note.id.as_str()).or_insert(note);
        }
    }
    let mut order: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    collect_note_refs(&doc.blocks, &valid, &mut order, &mut seen);
    for note in &doc.notes {
        if valid.contains_key(note.id.as_str()) && seen.insert(note.id.clone()) {
            order.push(note.id.clone());
        }
    }
    order
        .into_iter()
        .enumerate()
        .map(|(i, id)| (id, i + 1))
        .collect()
}

fn block_is_blank(block: &Block) -> bool {
    match block {
        Block::Paragraph(inlines) => inlines_are_empty(inlines),
        _ => false,
    }
}

fn collect_note_refs(
    blocks: &[Block],
    valid: &HashMap<&str, &Note>,
    order: &mut Vec<String>,
    seen: &mut HashSet<String>,
) {
    fn walk_inlines(
        inlines: &[Inline],
        valid: &HashMap<&str, &Note>,
        order: &mut Vec<String>,
        seen: &mut HashSet<String>,
    ) {
        for inline in inlines {
            match inline {
                Inline::NoteRef(id) => {
                    if let Some(note) = valid.get(id.as_str())
                        && seen.insert(id.clone())
                    {
                        order.push(id.clone());
                        collect_note_refs(&note.blocks, valid, order, seen);
                    }
                }
                Inline::Link { content, .. } => walk_inlines(content, valid, order, seen),
                _ => {}
            }
        }
    }
    for block in blocks {
        match block {
            Block::Paragraph(i) | Block::Heading { content: i, .. } => {
                walk_inlines(i, valid, order, seen)
            }
            Block::List(list) => {
                for item in &list.items {
                    collect_note_refs(&item.blocks, valid, order, seen);
                }
            }
            Block::Table(t) => {
                for row in &t.grid {
                    for slot in row {
                        if let crate::model::CellSlot::Origin(cell) = slot {
                            collect_note_refs(&cell.blocks, valid, order, seen);
                        }
                    }
                }
            }
            Block::BlockQuote(blocks) => collect_note_refs(blocks, valid, order, seen),
            Block::CodeBlock { .. } | Block::Rule | Block::Math(_) => {}
        }
    }
}

fn render_blocks(blocks: &[Block], rc: &Ctx) -> String {
    let parts: Vec<String> = blocks.iter().filter_map(|b| render_block(b, rc)).collect();
    parts.join("\n\n")
}

/// Render `blocks`, appending each OCR'd image's annotation block right after
/// the block that first references it. `annotated` is shared across the whole
/// document so an image referenced repeatedly is annotated once, next to its
/// first occurrence.
fn render_blocks_with_annotations(
    blocks: &[Block],
    rc: &Ctx,
    annotated: &mut HashSet<AssetId>,
) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    for block in blocks {
        let Some(rendered) = render_block(block, rc) else {
            continue;
        };
        parts.push(rendered);
        for id in block_image_assets(block) {
            if !annotated.insert(id) {
                continue;
            }
            let (Some(outcome), Some(number)) = (rc.asset_ocr.get(&id), rc.asset_numbers.get(&id))
            else {
                continue;
            };
            parts.push(ocr_annotation(*number, outcome));
        }
    }
    parts
}

/// Figure numbers for embedded images, in first-reference order: blocks
/// first, then note bodies, matching render order.
fn number_assets(doc: &Document) -> HashMap<AssetId, usize> {
    let mut ids: Vec<AssetId> = Vec::new();
    for block in &doc.blocks {
        collect_block_assets(block, &mut ids);
    }
    for note in &doc.notes {
        for block in &note.blocks {
            collect_block_assets(block, &mut ids);
        }
    }
    let mut numbers = HashMap::new();
    for id in ids {
        let next = numbers.len() + 1;
        numbers.entry(id).or_insert(next);
    }
    numbers
}

/// Embedded image assets referenced inside `block`, in reading order.
fn block_image_assets(block: &Block) -> Vec<AssetId> {
    let mut ids = Vec::new();
    collect_block_assets(block, &mut ids);
    ids
}

fn collect_block_assets(block: &Block, out: &mut Vec<AssetId>) {
    fn walk_inlines(inlines: &[Inline], out: &mut Vec<AssetId>) {
        for inline in inlines {
            match inline {
                Inline::Image {
                    source: ImageSource::Asset(id),
                    ..
                } => out.push(*id),
                Inline::Link { content, .. } => walk_inlines(content, out),
                _ => {}
            }
        }
    }
    fn walk_blocks(blocks: &[Block], out: &mut Vec<AssetId>) {
        for block in blocks {
            collect_block_assets(block, out);
        }
    }
    match block {
        Block::Paragraph(inlines)
        | Block::Heading {
            content: inlines, ..
        } => walk_inlines(inlines, out),
        Block::List(list) => {
            for item in &list.items {
                walk_blocks(&item.blocks, out);
            }
        }
        Block::Table(table) => {
            for row in &table.grid {
                for slot in row {
                    if let CellSlot::Origin(cell) = slot {
                        walk_blocks(&cell.blocks, out);
                    }
                }
            }
        }
        Block::BlockQuote(blocks) => walk_blocks(blocks, out),
        Block::CodeBlock { .. } | Block::Rule | Block::Math(_) => {}
    }
}

fn render_block(block: &Block, rc: &Ctx) -> Option<String> {
    match block {
        Block::Heading { level, content, .. } => {
            let text = render_inlines(content, InlineContext::Heading, rc);
            let text = text.trim();
            if text.is_empty() {
                return None;
            }
            let level = (*level).clamp(1, 6) as usize;
            Some(format!("{} {}", "#".repeat(level), text))
        }
        Block::Paragraph(inlines) => {
            let text = render_inlines(inlines, InlineContext::Block, rc);
            let trimmed = trim_paragraph(&text);
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        }
        Block::List(list) => render_list(list, rc),
        // Trivial layout tables are scaffolding; render their content directly.
        Block::Table(t) if t.kind == TableKind::Layout && t.is_single_cell() => {
            let crate::model::CellSlot::Origin(cell) = &t.grid[0][0] else {
                unreachable!()
            };
            let inner = render_blocks(&cell.blocks, rc);
            if inner.is_empty() { None } else { Some(inner) }
        }
        Block::Table(t) => table::render_table(t, rc),
        Block::BlockQuote(blocks) => {
            let inner = render_blocks(blocks, rc);
            if inner.is_empty() {
                return None;
            }
            let quoted: Vec<String> = inner
                .lines()
                .map(|l| {
                    if l.is_empty() {
                        ">".to_string()
                    } else {
                        format!("> {l}")
                    }
                })
                .collect();
            Some(quoted.join("\n"))
        }
        Block::CodeBlock { lang, text } => {
            let fence = backtick_fence(text, 3);
            let lang = lang.as_deref().unwrap_or("");
            let body = text.trim_end_matches('\n');
            Some(format!("{fence}{lang}\n{body}\n{fence}"))
        }
        Block::Rule => Some("---".to_string()),
        Block::Math(tex) => {
            let tex = tex.trim();
            if tex.is_empty() {
                return None;
            }
            // A bare `$` is never valid inside math; escaped, it cannot
            // close the block early.
            let mut source = String::with_capacity(tex.len());
            let mut backslashes = 0;
            for c in tex.chars() {
                if c == '$' && backslashes % 2 == 0 {
                    source.push('\\');
                }
                source.push(c);
                backslashes = if c == '\\' { backslashes + 1 } else { 0 };
            }
            Some(format!("$$\n{source}\n$$"))
        }
    }
}

fn render_list(list: &List, rc: &Ctx) -> Option<String> {
    if list.items.is_empty() {
        return None;
    }
    let mut rendered_items: Vec<String> = Vec::new();
    let mut loose = false;
    for (i, item) in list.items.iter().enumerate() {
        // GFM has decimal ordered lists only, so Roman/alphabetic levels
        // render as bullets carrying the source marker as literal text
        // (`- iv. …`) — the source marker semantics stay visible. Items with
        // an explicit label (composite number text) render it the same way.
        let marker = match (&item.marker_label, list.marker) {
            (Some(label), _) => {
                format!("- {} ", escape_marker_label(label, InlineContext::Block))
            }
            (None, MarkerKind::Bullet) => "- ".to_string(),
            (None, MarkerKind::Decimal) => format!("{}. ", list.start.saturating_add(i as u64)),
            (None, kind) => format!("- {} ", kind.label(list.start.saturating_add(i as u64))),
        };
        let body = render_blocks(&item.blocks, rc);
        if item.blocks.len() > 1 {
            loose = true;
        }
        let indent = " ".repeat(marker.chars().count());
        let mut lines = body.lines();
        let first = lines.next().unwrap_or("");
        let mut s = format!("{marker}{first}");
        for line in lines {
            s.push('\n');
            if line.is_empty() {
                loose = true;
            } else {
                s.push_str(&indent);
                s.push_str(line);
            }
        }
        rendered_items.push(s);
    }
    let sep = if loose { "\n\n" } else { "\n" };
    Some(rendered_items.join(sep))
}

/// Trim paragraph lines, keeping hard-break backslashes intact.
fn trim_paragraph(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(|l| {
            let t = l.trim_start();
            let t = if ends_with_hard_break(t) {
                t
            } else {
                t.trim_end()
            };
            if t.trim_end_matches('\\').trim().is_empty() {
                ""
            } else {
                t
            }
        })
        .collect();
    let start = lines.iter().position(|l| !l.is_empty());
    let end = lines.iter().rposition(|l| !l.is_empty());
    match (start, end) {
        (Some(s), Some(e)) => {
            let mut out = lines[s..=e].join("\n");
            if ends_with_hard_break(&out) {
                out.pop();
                out.truncate(out.trim_end().len());
            }
            out
        }
        _ => String::new(),
    }
}

fn ends_with_hard_break(line: &str) -> bool {
    line.chars().rev().take_while(|&c| c == '\\').count() % 2 == 1
}
