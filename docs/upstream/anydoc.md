# anydoc source baseline

- Upstream: `https://github.com/firecrawl/anydoc`
- Reference checkout: `.repos/anydoc`
- Synced commit: `261fc25` (2026-08-29; 25 commits after the initial import)
- Imported: Rust document model, package readers, format parsers and GFM renderer, plus the upstream test suite (`tests/common`, `tests/fixtures`, `tests/snapshots`, `tests/robustness.rs`, `fixture-src/gen_fixtures.py`).
- Excluded: Node, Python, WASM, CLI, benchmark and release infrastructure.
- Local crate: `crates/file-parser` (`vento-file-parser`). Upstream modules are kept verbatim; `formats/pdf.rs` calls `vento-pdf-engine` instead of the `pdf-inspector` crate.
- Local additions: `runtime.rs` (format routing, text/code passthrough, image→OCR fallback, `FileParser`/`convert` API) and `ocr/` (remote PaddleOCR provider), exposed through `lib.rs`.
- Local deltas on top of the baseline: `formats/pdf.rs` passes `MarkdownOptions { include_images: true }` so image items emit `![图片N](image-N)` markers; `ocr/paddle.rs` implements the AI Studio job protocol (`api/v2/ocr/jobs`: multipart submit → poll `data.state` → download the JSONL `resultUrl` and concatenate `layoutParsingResults[].markdown.text` per page), with `PaddleOcrConfig { endpoint, headers, model, timeoutMs (total job budget, default 5 min), retries, optionalPayload }`.

