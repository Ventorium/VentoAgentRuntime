---
"@ventostack/file-parser": patch
---

Embedded images no longer carry their OCR text inside the `![]` alt. Each image renders as a short numbered marker — `![图片N]()` for documents, `![图片N](image-N)` for PDFs (which keeps pointing at the extracted asset) — plus the document's own alt when it has one; the destination is left empty rather than a `?` placeholder, which a renderer resolves back to the containing document and fetches again. The recognition result follows the marker as a quoted annotation block (`> 图片N的OCR解析结果如下：…`). Arbitrary OCR output (brackets, pipes, headings, tables) can no longer break the surrounding Markdown, recognized tables and lists still render inside the block, and failed recognition is reported in the block rather than the alt. An image referenced repeatedly is annotated once, next to its first occurrence.
