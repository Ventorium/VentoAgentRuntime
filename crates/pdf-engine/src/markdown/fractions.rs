//! Inline fraction recovery.
//!
//! Word processors render equations with vertical fractions as numerator text,
//! a short horizontal rule, and denominator text. The rule sits just below the
//! numerator baseline, so the geometric underline pass marks the numerator
//! `<u>` and the denominator drops to a separate line below the surrounding
//! text. Recovering the triple lets markdown carry the fraction inline as
//! `numerator/denominator`.

use crate::types::{ItemType, PdfLine, PdfRect, TextItem};
use std::collections::HashSet;

/// Shortest rule considered as a fraction bar (pt).
const MIN_BAR_LEN: f32 = 4.0;
/// Longest rule considered as a fraction bar (pt). Table rulings and axes are
/// far wider than any equation.
const MAX_BAR_LEN: f32 = 250.0;
/// Max distance from the numerator baseline down to the bar, in ems. Matches
/// the underline window (0.72em) so every bar-caused underline is recovered.
const NUM_MAX_EM: f32 = 0.72;
/// Max distance from the bar down to the denominator baseline, in ems. Real
/// fractions stack at roughly 1.0-1.15em; table rows put the next row's text
/// at about 1.45em below a ruling, which must not qualify.
const DEN_MAX_EM: f32 = 1.25;
/// How far the bar may extend past the recovered text (pt).
const BAR_TEXT_PAD: f32 = 12.0;
/// Minimum horizontal overlap between numerator and denominator unions,
/// relative to the narrower one.
const MIN_HALF_ALIGNMENT: f32 = 0.5;

/// Replacement text for the trailing numerator item of a recovered fraction.
type FractionEdit = (usize, String);
/// Items absorbed into a numerator and removed from the item stream.
type FractionDropped = HashSet<usize>;
/// `(item index, target baseline)` pairs pulling mid-stack text up a line.
type FractionYSnap = (usize, f32);

/// Plan inline-fraction merges over the raw extraction.
///
/// Returns `(edits, dropped, y_snaps)`:
/// - `edits`: replacement for the trailing numerator item of each recovered
///   fraction. Callers should also clear `is_underline` on those items — the
///   rule is a fraction bar, not an underline.
/// - `dropped`: input indices of denominator items that were absorbed into
///   the numerator text and should be removed from the item stream.
/// - `y_snaps`: for short text sitting beside the bar at the fraction's mid
///   height (equation layout centers `得分 = … = 48.5` on the stack), the
///   numerator baseline to snap onto so the equation stays on one
///   x-ordered line.
pub(crate) fn plan_inline_fractions(
    items: &[TextItem],
    rects: &[PdfRect],
    lines: &[PdfLine],
) -> (Vec<FractionEdit>, FractionDropped, Vec<FractionYSnap>) {
    // Candidate bars: horizontal rules (lines) and thin filled rects.
    let mut bars: Vec<(u32, f32, f32, f32)> = Vec::new(); // (page, x1, x2, y)
    for l in lines {
        if (l.y1 - l.y2).abs() > 1.5 {
            continue;
        }
        let (x1, x2) = if l.x1 <= l.x2 {
            (l.x1, l.x2)
        } else {
            (l.x2, l.x1)
        };
        let len = x2 - x1;
        if (MIN_BAR_LEN..=MAX_BAR_LEN).contains(&len) {
            bars.push((l.page, x1, x2, l.y1));
        }
    }
    for r in rects {
        let (mut x, mut y, mut w, mut h) = (r.x, r.y, r.width, r.height);
        if w < 0.0 {
            x += w;
            w = -w;
        }
        if h < 0.0 {
            y += h;
            h = -h;
        }
        if h < 2.5 && (MIN_BAR_LEN..=MAX_BAR_LEN).contains(&w) {
            bars.push((r.page, x, x + w, y + h / 2.0));
        }
    }
    if bars.is_empty() {
        return (Vec::new(), HashSet::new(), Vec::new());
    }

    // Drop cell rulings: table borders repeat with the same left AND right
    // edge across rows (one horizontal ruling per row boundary). Real
    // fraction bars track their numerators, so 3+ bars sharing both edges
    // within 1pt are a ruled grid, not equations.
    let bars: Vec<(u32, f32, f32, f32)> = bars
        .iter()
        .filter(|&&(p, x1, x2, _)| {
            bars.iter()
                .filter(|&&(p2, a, b, _)| p2 == p && (a - x1).abs() <= 1.0 && (b - x2).abs() <= 1.0)
                .count()
                < 3
        })
        .copied()
        .collect();

    let pages: Vec<u32> = items
        .iter()
        .map(|i| i.page)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let mut used: HashSet<usize> = HashSet::new();
    let mut edits: Vec<(usize, String)> = Vec::new();
    let mut dropped: HashSet<usize> = HashSet::new();
    let mut y_snaps: Vec<(usize, f32)> = Vec::new();

    for &(page, bx1, bx2, by) in &bars {
        if !pages.contains(&page) {
            continue;
        }
        let mut nums: Vec<usize> = Vec::new();
        let mut dens: Vec<usize> = Vec::new();
        for (idx, item) in items.iter().enumerate() {
            if item.page != page
                || !matches!(item.item_type, ItemType::Text)
                || item.text.trim().is_empty()
                || used.contains(&idx)
            {
                continue;
            }
            let em = item.font_size.max(item.height).max(1.0);
            // Baselines above the bar within the underline window.
            let dy = item.y - by;
            if dy >= 0.0 && dy <= em * NUM_MAX_EM {
                nums.push(idx);
                continue;
            }
            // Baselines below the bar within the fraction stack.
            if dy < 0.0 && -dy <= em * DEN_MAX_EM {
                dens.push(idx);
            }
        }
        if nums.is_empty() || dens.is_empty() {
            continue;
        }
        // Halves sit under the bar, not merely grazing it: most of each
        // item's width must lie inside the bar span.
        let x_under_bar = |item: &TextItem| {
            let overlap = (item.x + item.width).min(bx2) - item.x.max(bx1);
            overlap >= item.width * 0.5
        };
        let nums: Vec<usize> = nums
            .into_iter()
            .filter(|&i| x_under_bar(&items[i]))
            .collect();
        let dens: Vec<usize> = dens
            .into_iter()
            .filter(|&i| x_under_bar(&items[i]))
            .collect();
        if nums.is_empty() || dens.is_empty() {
            continue;
        }

        let span = |idxs: &[usize]| {
            let min = idxs.iter().map(|&i| items[i].x).fold(f32::MAX, f32::min);
            let max = idxs
                .iter()
                .map(|&i| items[i].x + items[i].width)
                .fold(f32::MIN, f32::max);
            (min, max)
        };
        // Each half must be one contiguous run: equation terms are adjacent
        // glyphs. A table row's cells share the ruling but sit a column
        // gutter apart (id "202" + date "1/10"), which never happens in a
        // real fraction stack.
        let contiguous = |idxs: &[usize]| -> bool {
            let mut sorted = idxs.to_vec();
            sorted.sort_by(|&a, &b| items[a].x.total_cmp(&items[b].x));
            sorted.windows(2).all(|w| {
                let (a, b) = (&items[w[0]], &items[w[1]]);
                let em = a.font_size.max(a.height).max(1.0);
                b.x - (a.x + a.width) <= em * 0.8
            })
        };
        if !contiguous(&nums) || !contiguous(&dens) {
            continue;
        }
        let (n1, n2) = span(&nums);
        let (d1, d2) = span(&dens);
        // Fraction halves are vertically aligned by construction; unrelated
        // neighboring text on either side is not.
        let overlap = n2.min(d2) - n1.max(d1);
        let shorter = (n2 - n1).min(d2 - d1);
        if overlap < shorter * MIN_HALF_ALIGNMENT {
            continue;
        }
        // The bar tracks the fraction text, not the page.
        let all_min = n1.min(d1);
        let all_max = n2.max(d2);
        if bx1 < all_min - BAR_TEXT_PAD || bx2 > all_max + BAR_TEXT_PAD {
            continue;
        }

        nums.iter().chain(dens.iter()).for_each(|&i| {
            used.insert(i);
        });

        // Snap short text centered beside the stack onto the numerator line.
        let num_baseline = items[nums[0]].y;
        let den_baseline = items[dens[0]].y;
        for (idx, item) in items.iter().enumerate() {
            if used.contains(&idx)
                || item.page != page
                || !matches!(item.item_type, ItemType::Text)
                || item.text.trim().is_empty()
                || item.text.chars().count() > 8
            {
                continue;
            }
            if item.y >= num_baseline - 1.0 || item.y <= den_baseline + 1.0 {
                continue;
            }
            let em = item.font_size.max(item.height).max(1.0);
            let dist = 0.0_f32.max(bx1 - (item.x + item.width)).max(item.x - bx2);
            if dist > em * 2.5 {
                continue;
            }
            used.insert(idx);
            y_snaps.push((idx, num_baseline));
        }

        let mut den_text = String::new();
        let mut dens = dens;
        dens.sort_by(|&a, &b| items[a].x.total_cmp(&items[b].x));
        for &i in &dens {
            den_text.push_str(&items[i].text);
        }
        let mut nums = nums;
        nums.sort_by(|&a, &b| items[a].x.total_cmp(&items[b].x));
        let last = *nums.last().expect("nums is non-empty");
        let new_text = format!("{}/{}", items[last].text, den_text);
        edits.push((last, new_text));
        dropped.extend(dens);
    }

    (edits, dropped, y_snaps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ItemType;

    fn item(text: &str, x: f32, y: f32, w: f32) -> TextItem {
        TextItem {
            text: text.to_string(),
            x,
            y,
            width: w,
            height: 14.0,
            font: String::new(),
            font_tag: String::new(),
            font_size: 14.0,
            page: 1,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: ItemType::Text,
            mcid: None,
        }
    }

    fn bar(x1: f32, y: f32, x2: f32) -> PdfLine {
        PdfLine {
            x1,
            y1: y,
            x2,
            y2: y,
            page: 1,
        }
    }

    #[test]
    fn merges_simple_fraction() {
        let items = vec![
            item("950", 279.0, 699.4, 27.0),
            item("19.6", 277.7, 679.5, 31.2),
        ];
        let lines = vec![bar(277.7, 692.9, 308.8)];
        let (edits, dropped, snaps) = plan_inline_fractions(&items, &[], &lines);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].1, "950/19.6");
        assert_eq!(dropped.len(), 1);
        assert!(dropped.contains(&1));
        assert!(snaps.is_empty());
    }

    #[test]
    fn snaps_beside_text_onto_numerator_line() {
        let items = vec![
            item("950", 279.0, 699.4, 27.0),
            item("19.6", 277.7, 679.5, 31.2),
            item("得分 =", 230.8, 688.9, 43.0),
            item("= 48.5", 312.7, 688.9, 46.1),
        ];
        let lines = vec![bar(277.7, 692.9, 308.8)];
        let (edits, dropped, snaps) = plan_inline_fractions(&items, &[], &lines);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].1, "950/19.6");
        assert_eq!(snaps.len(), 2);
        for &(idx, y) in &snaps {
            assert_eq!(y, 699.4);
            assert!(idx == 2 || idx == 3);
        }
        assert!(dropped.len() == 1 && dropped.contains(&1));
    }

    #[test]
    fn ignores_underline_without_text_below() {
        let items = vec![
            item("强调", 100.0, 700.0, 30.0),
            item("下一行", 100.0, 650.0, 40.0),
        ];
        let lines = vec![bar(100.0, 694.0, 130.0)];
        let (edits, dropped, _) = plan_inline_fractions(&items, &[], &lines);
        assert!(edits.is_empty());
        assert!(dropped.is_empty());
    }

    #[test]
    fn ignores_table_ruling() {
        // Wide ruling with the next row's text 20.8pt below (1.48em) — beyond
        // the denominator window.
        let items = vec![
            item("塑料连接件", 70.0, 207.9, 70.0),
            item("胶带", 70.0, 176.2, 28.0),
        ];
        let lines = vec![bar(64.7, 197.0, 491.2)];
        let (edits, dropped, _) = plan_inline_fractions(&items, &[], &lines);
        assert!(edits.is_empty());
        assert!(dropped.is_empty());
    }

    #[test]
    fn misaligned_halves_do_not_merge() {
        // "得分 =" sits beside the bar, not under it; must not join as a
        // denominator even when it grazes the bar's x-range.
        let items = vec![
            item("950", 279.0, 699.4, 27.0),
            item("19.6", 277.7, 679.5, 31.2),
            item("得分 =", 240.0, 688.9, 43.0),
        ];
        let lines = vec![bar(277.7, 692.9, 308.8)];
        let (edits, dropped, _) = plan_inline_fractions(&items, &[], &lines);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].1, "950/19.6");
        assert!(!dropped.contains(&2));
    }
}
