# pdf-inspector source baseline

- Upstream: `https://github.com/firecrawl/pdf-inspector`
- Reference checkout: `.repos/pdf-inspector`
- Synced commit: `23cf1ad` (2026-08-29; 58 commits after the initial import, bringing content decoding, furniture removal, font tags and structure-element extraction)
- Imported: PDF classification, extraction, layout, tables, fonts and Markdown generation, plus the upstream integration test suite and Hebrew fixtures.
- Excluded: Python bindings, N-API, WASM, CLI, site and release infrastructure; the `vision` module is also excluded because OCR is delegated to the remote provider in `vento-file-parser`.
- Local crate: `crates/pdf-engine` (`vento-pdf-engine`); private workspace implementation, consumed by `vento-file-parser` and the `vento-file-parser` N-API binding.

## Local patches

Deltas carried on top of `23cf1ad`, each with local regression tests:

- **GFM row padding** (`tables/format.rs`): every table row, including the `|---|` separator, is padded to the widest row's column count so a short header cannot leave data rows wider than the separator declares.
- **Junk-majority page fills** (`tables/detect_rects.rs`): the origin-anchored page-background heuristic additionally flags rects at page scale (≥90% of the page-content extents) when page fills dominate the rect population (median height itself at page scale, ≥8 rects). Word-style PDFs repeat a page-size fill per content operation; the old median-height rule went deaf there and the fills fused every cell cluster into one page-spanning table.
- **Merged-header column rescue** (`tables/detect_rects.rs`): the interior-empty-column grid rejection spares columns spanned almost fully (>80% width) by an assigned text item — merged header cells such as `IG 1 IG 2 IG 3` assign by center into their middle column only.
- **Inline fractions** (`markdown/fractions.rs`, wired in `markdown/mod.rs`): vertical fractions (numerator / short rule / denominator) are recovered before line grouping and emitted inline as `num/den`; beside-text at mid-stack height y-snaps onto the numerator line. Cell rulings are excluded: bars repeating both edges within 1pt across ≥3 rows are a ruled grid, and each half must be one contiguous run (a table row's cells share a ruling but sit a column gutter apart).
- **Image markers** (`markdown/mod.rs`): with `MarkdownOptions::include_images` (set by `vento-file-parser`), image items emit `![图片N](image-N)` markers interleaved into the reading flow instead of being dropped silently.