# anydoc source baseline

- Upstream: `https://github.com/firecrawl/anydoc`
- Reference checkout: `.repos/anydoc`
- Synced commit: `261fc25` (2026-08-29; 25 commits after the initial import)
- Imported: Rust document model, package readers, format parsers and GFM renderer, plus the upstream test suite (`tests/common`, `tests/fixtures`, `tests/snapshots`, `tests/robustness.rs`, `fixture-src/gen_fixtures.py`).
- Excluded: Node, Python, WASM, CLI, benchmark and release infrastructure.
- Local crate: `crates/file-parser` (`vento-file-parser`). Upstream modules are kept verbatim; `formats/pdf.rs` calls `vento-pdf-engine` instead of the `pdf-inspector` crate.
- Local additions: `runtime.rs` (format routing, text/code passthrough, image→OCR fallback, `FileParser`/`convert` API) and `ocr/` (remote PaddleOCR provider), exposed through `lib.rs`.
- Local deltas on top of the baseline: `formats/pdf.rs` passes `MarkdownOptions { include_images: true }` so image items emit `![图片N](image-N)` markers; `ocr/paddle.rs` implements the AI Studio job protocol (`api/v2/ocr/jobs`: multipart submit → poll `data.state` → download the JSONL `resultUrl` and concatenate `layoutParsingResults[].markdown.text` per page), with `PaddleOcrConfig { endpoint, headers, model (must be `PaddleOCR-VL*`/`PP-StructureV3*`; PP-OCRv5's `ocrResults` shape is rejected), timeoutMs (total job budget, default 5 min), retries, optionalPayload }`. `NeedsOcr` page numbers are forwarded as the upstream `pageRanges` form field; HTTP 429 (error 12002) is retried with a longer backoff instead of failing the job.
- Embedded-image OCR rendering: an image keeps a short numbered marker (`![图片N]()` from the document model, `![图片N](image-N)` from pdf-engine, plus the source alt when the document has one) and the recognized text follows it as a quoted annotation block (`> 图片N的OCR解析结果如下：…`). OCR text never goes inside `![]`, so brackets, pipes, headings or tables in the output cannot restructure the surrounding Markdown. Repeated references to one image are annotated once, after the first.

