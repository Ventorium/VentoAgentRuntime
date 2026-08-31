---
'@ventostack/file-parser': minor
---

convert_bytes now returns the markdown string directly instead of a JSON envelope, and embedded images (docx/doc/rtf/odt/pptx/epub/pdf assets) are OCR'd through the configured provider, rendered as `![图片，OCR识别文字是：…](?)` markers with SHA-256 dedup, >1024px downscaling, bounded concurrency and a per-image 60s timeout; failures degrade non-fatally to `图片，OCR识别失败`.
