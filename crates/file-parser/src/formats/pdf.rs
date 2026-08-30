//! PDF via [pdf-inspector]: classification plus direct Markdown extraction.
//!
//! Unlike the other frontends, pdf-inspector emits Markdown itself, so PDFs
//! bypass the document model and the shared GFM writer. OCR is out of scope
//! here: a document with scanned or image-only pages errors naming them,
//! whether that is every page or one of a hundred, because output missing
//! those pages would read as complete.
//!
//! [pdf-inspector]: https://github.com/firecrawl/pdf-inspector

use crate::error::ConvertError;
use vento_pdf_engine::PdfError;

/// PDF conversion output: the markdown plus any embedded images extracted
/// alongside it.
pub struct PdfMarkdown {
    /// The converted GitHub-Flavored Markdown.
    pub markdown: String,
    /// Embedded images; `name` matches the `![图片N](name)` marker URLs.
    pub images: Vec<vento_pdf_engine::PdfImage>,
}

pub fn to_markdown(bytes: &[u8]) -> Result<String, ConvertError> {
    to_markdown_with_images(bytes).map(|output| output.markdown)
}

pub fn to_markdown_with_images(bytes: &[u8]) -> Result<PdfMarkdown, ConvertError> {
    // Emit `![图片N](image-N)` markers so image content stays visible in the
    // Markdown flow; embedded JPEG/JPEG2000 objects are also extracted so
    // callers can write the actual files next to the markdown.
    let opts = vento_pdf_engine::PdfOptions::new().markdown(vento_pdf_engine::MarkdownOptions {
        include_images: true,
        ..Default::default()
    });
    let result = vento_pdf_engine::process_pdf_mem_with_options(bytes, opts).map_err(map_error)?;
    if !result.pages_needing_ocr.is_empty() {
        // Detection samples content streams and over-reports short or
        // image-heavy text pages; extraction knows which of them yielded none.
        let flagged: Vec<u32> = result
            .pages_needing_ocr
            .iter()
            .map(|page| page - 1)
            .collect();
        let pages = vento_pdf_engine::extract_pages_markdown_mem(bytes, Some(&flagged))
            .map_err(map_error)?
            .pages_needing_ocr;
        if !pages.is_empty() {
            return Err(ConvertError::NeedsOcr {
                pages,
                page_count: result.page_count,
            });
        }
    }
    if result.has_encoding_issues {
        log::warn!("broken font encodings detected; extracted text may be garbled");
    }
    match result.markdown {
        Some(mut markdown) if !markdown.trim().is_empty() => {
            if !markdown.ends_with('\n') {
                markdown.push('\n');
            }
            Ok(PdfMarkdown {
                markdown,
                images: result.images,
            })
        }
        _ => Err(ConvertError::Unsupported(format!(
            "PDF has no extractable text ({:?}, {} pages)",
            result.pdf_type, result.page_count
        ))),
    }
}

fn map_error(e: PdfError) -> ConvertError {
    match e {
        PdfError::Encrypted => ConvertError::Encrypted,
        PdfError::Io(e) => ConvertError::Io(e),
        PdfError::NotAPdf(detail) => ConvertError::malformed(format!("not a PDF: {detail}")),
        PdfError::InvalidStructure => ConvertError::malformed("invalid PDF structure"),
        PdfError::Parse(detail) => ConvertError::malformed(detail),
    }
}
