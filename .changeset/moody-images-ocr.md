---
'@ventostack/file-parser': minor
---

convert_bytes now returns the markdown string directly instead of a JSON envelope, and embedded images (docx/doc/rtf/odt/pptx/epub/pdf assets) are OCR'd through the configured provider, rendered as `![图片，OCR识别文字是：…](?)` markers with SHA-256 dedup, >1024px downscaling, bounded concurrency and a per-image 60s timeout; failures degrade non-fatally to `图片，OCR识别失败`.

HTML documents (`.html`/`.htm`/`.xhtml`) now convert directly to Markdown on the same raw-markdown path (no document model), including script/style/nav/footer/header stripping, entity decoding, tables and inline formatting; detection uses the file extension with a conservative doctype/`<html` content sniff as fallback.
