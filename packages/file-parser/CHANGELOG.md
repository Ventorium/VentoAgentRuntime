# @ventostack/file-parser

## 0.3.1

### Patch Changes

- [`e2495c3`](https://github.com/Ventorium/VentoAgentRuntime/commit/e2495c3939360417666a52b3151b93aabbdc7d40) Thanks [@erguotou520](https://github.com/erguotou520)! - Embedded images no longer carry their OCR text inside the `![]` alt. Each image renders as a short numbered marker — `![图片N]()` for documents, `![图片N](image-N)` for PDFs (which keeps pointing at the extracted asset) — plus the document's own alt when it has one; the destination is left empty rather than a `?` placeholder, which a renderer resolves back to the containing document and fetches again. The recognition result follows the marker as a quoted annotation block (`> 图片N的OCR解析结果如下：…`). Arbitrary OCR output (brackets, pipes, headings, tables) can no longer break the surrounding Markdown, recognized tables and lists still render inside the block, and failed recognition is reported in the block rather than the alt. An image referenced repeatedly is annotated once, next to its first occurrence.

## 0.3.0

### Minor Changes

- [`e1ec26d`](https://github.com/Ventorium/VentoAgentRuntime/commit/e1ec26df17aa55855ca5e26e1f0247cbf37f3b24) Thanks [@erguotou520](https://github.com/erguotou520)! - Render tables containing merged cells as an HTML `<table>` with `colspan`/`rowspan` instead of blank placeholder cells, since GFM pipe tables cannot represent spans. Span-less tables still render as GFM markdown.

## 0.2.0

### Minor Changes

- [`72c812f`](https://github.com/Ventorium/VentoAgentRuntime/commit/72c812fdf0f47f7ed1a0e22b21cc247605d27e4c) Thanks [@erguotou520](https://github.com/erguotou520)! - convert_bytes now returns the markdown string directly instead of a JSON envelope, and embedded images (docx/doc/rtf/odt/pptx/epub/pdf assets) are OCR'd through the configured provider, rendered as `![图片，OCR识别文字是：…](?)` markers with SHA-256 dedup, >1024px downscaling, bounded concurrency and a per-image 60s timeout; failures degrade non-fatally to `图片，OCR识别失败`.

  HTML documents (`.html`/`.htm`/`.xhtml`) now convert directly to Markdown on the same raw-markdown path (no document model), including script/style/nav/footer/header stripping, entity decoding, tables and inline formatting; detection uses the file extension with a conservative doctype/`<html` content sniff as fallback.

### Patch Changes

- [`f228123`](https://github.com/Ventorium/VentoAgentRuntime/commit/f2281236c73867b44e44f707765b5a3c51b87dce) Thanks [@erguotou520](https://github.com/erguotou520)! - Set up automated npm publishing: explicit cross-platform build targets, public access, and per-platform packages (`@ventostack/*-darwin-arm64` etc.).
