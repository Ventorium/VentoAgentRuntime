//! High-level conversion runtime: format routing, plain-text passthrough,
//! remote OCR fallback for scanned pages and standalone images, size limits,
//! and path sandboxing. Everything funnels into [`FileParser::convert`].

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use base64::Engine as _;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ConvertError;
use crate::Format;
use crate::ocr::{
    ImageOcrConfig, OcrImage, OcrOptions, OcrOutcome, OcrProvider, OcrRequest, compose_alt,
    content_hash, ensure_image_extension, run_image_ocr,
};
use crate::render::markdown::document_to_markdown_with_ocr;

/// What to convert: raw bytes, or a path inside the configured roots.
#[derive(Clone, Debug)]
pub enum DocumentInput {
    /// In-memory file content plus its name (used for format fallback and
    /// reporting; the name never touches the filesystem).
    Bytes {
        /// File content.
        data: Vec<u8>,
        /// File name, without path separators.
        file_name: String,
    },
    /// Path to a file on disk. Canonicalized and checked against
    /// `allowed_roots` before reading.
    Path {
        /// Absolute or relative path to the file.
        path: PathBuf,
    },
}

/// Per-call conversion options; unset fields fall back to the [`FileParser`]
/// defaults.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConvertOptions {
    /// BCP-47-ish language hint forwarded to the OCR provider.
    pub ocr_language: Option<String>,
    /// Per-call override of the maximum accepted input size.
    pub max_input_bytes: Option<u64>,
    /// Per-call override of the path sandbox roots.
    pub allowed_roots: Vec<PathBuf>,
    /// OCR the images embedded in documents (docx/pdf/…), rendering them as
    /// `![图片，OCR识别文字是：…](?)`. Defaults to true; a no-op without a
    /// configured provider.
    pub image_ocr: Option<bool>,
    /// Long-side cap (px) for images sent to OCR; larger ones are downscaled
    /// proportionally first. Defaults to 1024.
    pub image_ocr_max_dimension: Option<u32>,
    /// Maximum concurrent per-image OCR requests. Defaults to 4.
    pub image_ocr_concurrency: Option<usize>,
    /// Per-image OCR budget in milliseconds. Defaults to 60000.
    pub image_ocr_timeout_ms: Option<u64>,
}

/// Per-conversion OCR setup threaded through format dispatch: the language
/// hint and the embedded-image pass knobs, resolved once from
/// [`ConvertOptions`].
struct OcrSetup<'a> {
    language: Option<&'a str>,
    images: ImageOcrConfig,
}

/// One routing decision the runtime made while converting (extract, ocr, ...).
/// Decisions form an audit trail of how the markdown was produced.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    /// What was routed: a file name or `page:N` for a PDF page.
    pub target: String,
    /// What was done: `extract` or `ocr`.
    pub action: String,
    /// Why this route was taken.
    pub reason: String,
    /// Provider-reported confidence, when available (OCR).
    pub confidence: Option<f32>,
}

/// Describes the input as received, before conversion.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceMetadata {
    /// File name as provided (bytes) or as resolved (path).
    pub file_name: String,
    /// MIME type inferred from the extension.
    pub mime_type: String,
    /// Input size in bytes.
    pub size: u64,
    /// `bytes` or `path`.
    pub source_type: String,
}

/// A binary asset emitted alongside the markdown (e.g. an embedded PDF
/// image). `name` matches the URL used by the corresponding markdown marker.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageAsset {
    /// Asset file name, e.g. `report-image-1.jpg`.
    pub name: String,
    /// MIME type, e.g. `image/jpeg`.
    pub mime_type: String,
    /// File bytes, base64-encoded for JSON transport.
    pub data_base64: String,
}

/// Successful conversion output.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConvertResult {
    /// The converted GitHub-Flavored Markdown.
    pub markdown: String,
    /// File name without extension; a display title.
    pub title: String,
    /// Input description.
    pub source: SourceMetadata,
    /// Extra conversion metadata (e.g. `pageCount` on the OCR path).
    pub metadata: Map<String, Value>,
    /// Routing decisions taken during conversion.
    pub decisions: Vec<Decision>,
    /// Non-fatal issues (e.g. empty markdown).
    pub warnings: Vec<String>,
    /// Binary assets referenced by the markdown, if any.
    #[serde(default)]
    pub images: Vec<ImageAsset>,
    /// Wall-clock conversion time in milliseconds.
    pub duration_ms: u64,
}

/// Runtime-level failure, with a stable machine-readable [`code`].
///
/// [`code`]: RuntimeError::code
#[derive(Debug)]
pub enum RuntimeError {
    /// The input name or shape is unusable (empty name, path separators, ...).
    InvalidInput(String),
    /// A path input resolved outside the configured roots.
    AccessDenied(String),
    /// No parser and no fallback (text/image) applies to this input.
    Unsupported(String),
    /// The input exceeds the configured size limit.
    ResourceLimit(String),
    /// OCR is needed (scanned pages or a standalone image) but no provider
    /// is configured. `pages` lists the page numbers, empty for images.
    OcrRequired {
        /// Page numbers lacking a text layer; empty for standalone images.
        pages: Vec<u32>,
    },
    /// The OCR provider failed.
    Ocr {
        /// Provider error code.
        code: &'static str,
        /// Provider error message.
        message: String,
    },
    /// The format parser rejected the document.
    Conversion(ConvertError),
    /// Filesystem error.
    Io(std::io::Error),
}

impl RuntimeError {
    /// Stable error code for programmatic handling (also prefixed into the
    /// napi error message).
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidInput(_) => "INVALID_INPUT",
            Self::AccessDenied(_) => "ACCESS_DENIED",
            Self::Unsupported(_) => "UNSUPPORTED",
            Self::ResourceLimit(_) => "RESOURCE_LIMIT",
            Self::OcrRequired { .. } => "OCR_REQUIRED",
            Self::Ocr { .. } => "OCR_ERROR",
            Self::Conversion(_) => "CONVERSION_ERROR",
            Self::Io(_) => "IO_ERROR",
        }
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(message)
            | Self::AccessDenied(message)
            | Self::Unsupported(message)
            | Self::ResourceLimit(message) => formatter.write_str(message),
            Self::OcrRequired { pages } => match pages.as_slice() {
                [] => formatter.write_str("OCR is required for this input"),
                [page] => write!(formatter, "OCR is required for page {page}"),
                _ => write!(
                    formatter,
                    "OCR is required for pages {}",
                    pages
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            },
            Self::Ocr { code, message } => {
                write!(formatter, "ocr provider error ({code}): {message}")
            }
            Self::Conversion(error) => write!(formatter, "conversion failed: {error}"),
            Self::Io(error) => write!(formatter, "io error: {error}"),
        }
    }
}

impl std::error::Error for RuntimeError {}

impl From<ConvertError> for RuntimeError {
    fn from(error: ConvertError) -> Self {
        match error {
            ConvertError::NeedsOcr { pages, .. } => Self::OcrRequired { pages },
            other => Self::Conversion(other),
        }
    }
}

impl From<std::io::Error> for RuntimeError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

fn mime_for(file_name: &str) -> &'static str {
    let ext = file_name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "docm" => "application/vnd.ms-word.document.macroEnabled.main+xml",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "xlsm" => "application/vnd.ms-excel.sheet.macroEnabled.main+xml",
        "xlsb" => "application/vnd.ms-excel.sheet.binary.macroEnabled.main+xml",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "pptm" | "ppsm" => "application/vnd.ms-powerpoint.presentation.macroEnabled.main+xml",
        "ppsx" | "pps" => "application/vnd.openxmlformats-officedocument.presentationml.slideshow",
        "pot" => "application/vnd.ms-powerpoint.template.macroEnabled.main+xml",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "odp" => "application/vnd.oasis.opendocument.presentation",
        "rtf" => "application/rtf",
        "epub" => "application/epub+zip",
        "csv" => "text/csv",
        "html" | "htm" | "xhtml" => "text/html",
        "txt" | "md" | "mdx" | "json" | "yaml" | "yml" | "toml" | "xml" | "rs" | "ts" | "js"
        | "py" => "text/plain",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        _ => "application/octet-stream",
    }
}

/// Collectors threaded through a single conversion: routing decisions,
/// warnings, and extra metadata for the final [`ConvertResult`].
#[derive(Default)]
struct ConvertContext {
    decisions: Vec<Decision>,
    warnings: Vec<String>,
    metadata: Map<String, Value>,
}

/// Reusable converter configured with sandbox roots, size limits, and an
/// optional OCR provider. Clone-cheap enough to build per request.
pub struct FileParser {
    allowed_roots: Vec<PathBuf>,
    max_input_bytes: u64,
    ocr: Option<Arc<dyn OcrProvider>>,
}

impl std::fmt::Debug for FileParser {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FileParser")
            .field("allowed_roots", &self.allowed_roots)
            .field("max_input_bytes", &self.max_input_bytes)
            .field("ocr", &self.ocr.as_ref().map(|p| p.name()))
            .finish()
    }
}

impl Default for FileParser {
    fn default() -> Self {
        Self::new()
    }
}

impl FileParser {
    /// A parser with no sandbox, a 500 MiB size limit, and no OCR provider.
    pub fn new() -> Self {
        Self {
            allowed_roots: Vec::new(),
            max_input_bytes: 500 * 1024 * 1024,
            ocr: None,
        }
    }

    /// Restrict [`DocumentInput::Path`] inputs to these canonical roots.
    /// Empty (the default) allows any path.
    pub fn with_allowed_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.allowed_roots = roots;
        self
    }

    /// Set the maximum accepted input size in bytes (at least 1).
    pub fn with_max_input_bytes(mut self, value: u64) -> Self {
        self.max_input_bytes = value.max(1);
        self
    }

    /// Attach an OCR provider used for scanned PDF pages and standalone
    /// images. Without one those inputs fail with
    /// [`RuntimeError::OcrRequired`].
    pub fn with_ocr(mut self, provider: Arc<dyn OcrProvider>) -> Self {
        self.ocr = Some(provider);
        self
    }

    /// Convert one input to markdown. Routes by content-detected format
    /// first, then by extension: images to OCR, text files to passthrough.
    pub async fn convert(
        &self,
        input: DocumentInput,
        options: ConvertOptions,
    ) -> Result<ConvertResult, RuntimeError> {
        let started = Instant::now();
        let max = options.max_input_bytes.unwrap_or(self.max_input_bytes);
        let ocr = OcrSetup {
            language: options.ocr_language.as_deref(),
            images: ImageOcrConfig::from(&options),
        };
        let allowed_roots = if options.allowed_roots.is_empty() {
            self.allowed_roots.clone()
        } else {
            options.allowed_roots
        };

        let (bytes, file_name, source_type) = match input {
            DocumentInput::Bytes { data, file_name } => {
                let name = safe_file_name(&file_name)?;
                (data, name, "bytes")
            }
            DocumentInput::Path { path } => {
                let canonical = tokio::fs::canonicalize(&path).await?;
                if !allowed_roots.is_empty()
                    && !allowed_roots.iter().any(|root| canonical.starts_with(root))
                {
                    return Err(RuntimeError::AccessDenied(format!(
                        "path {} is outside configured roots",
                        path.display()
                    )));
                }
                let name = canonical
                    .file_name()
                    .and_then(|value| value.to_str())
                    .ok_or_else(|| {
                        RuntimeError::InvalidInput("path has no UTF-8 file name".into())
                    })?
                    .to_owned();
                let bytes = tokio::fs::read(&canonical).await?;
                (bytes, safe_file_name(&name)?, "path")
            }
        };

        if bytes.len() as u64 > max {
            return Err(RuntimeError::ResourceLimit(format!(
                "input has {} bytes; maximum is {}",
                bytes.len(),
                max
            )));
        }

        let format = Format::from_bytes(&bytes)
            .or_else(|| Format::from_extension(file_name.rsplit('.').next().unwrap_or("")));

        let mut context = ConvertContext::default();

        let mut images: Vec<ImageAsset> = Vec::new();
        let markdown = match format {
            Some(format) => {
                self.convert_format(format, &bytes, &file_name, &ocr, &mut context, &mut images)
                    .await?
            }
            None => {
                // No document format applies. Fall back by extension:
                // images go to OCR, recognized text files pass through.
                if is_image_extension(&file_name) {
                    self.run_ocr(&bytes, &file_name, ocr.language, &mut context)
                        .await?
                } else if let Some(kind) = text_kind(&file_name) {
                    let markdown = text_to_markdown(&bytes, kind)?;
                    context.decisions.push(Decision {
                        target: file_name.clone(),
                        action: "extract".into(),
                        reason: "plain text passthrough".into(),
                        confidence: None,
                    });
                    markdown
                } else {
                    return Err(RuntimeError::Unsupported(format!(
                        "unrecognized file content and extension: {file_name}"
                    )));
                }
            }
        };

        if markdown.trim().is_empty() {
            context
                .warnings
                .push("conversion produced empty markdown".into());
        }

        let ConvertContext {
            decisions,
            warnings,
            metadata,
        } = context;

        Ok(ConvertResult {
            markdown,
            title: file_stem(&file_name),
            source: SourceMetadata {
                file_name: file_name.clone(),
                mime_type: mime_for(&file_name).to_owned(),
                size: bytes.len() as u64,
                source_type: source_type.into(),
            },
            metadata,
            decisions,
            warnings,
            images,
            duration_ms: started.elapsed().as_millis() as u64,
        })
    }

    /// Convert via a detected document format. PDF gets its own path with an
    /// OCR fallback; everything else goes through the document model, with
    /// the embedded-image OCR pass folded into the markdown rendering.
    async fn convert_format(
        &self,
        format: Format,
        bytes: &[u8],
        file_name: &str,
        ocr: &OcrSetup<'_>,
        context: &mut ConvertContext,
        images: &mut Vec<ImageAsset>,
    ) -> Result<String, RuntimeError> {
        match format {
            Format::Pdf => {
                self.convert_pdf(bytes, file_name, ocr, context, images)
                    .await
            }
            // Direct-markdown path, like PDF but with no OCR fallback: HTML
            // pages always carry extractable text.
            Format::Html => {
                let markdown = crate::formats::html::to_markdown(bytes, &file_stem(file_name))?;
                context.decisions.push(Decision {
                    target: file_name.to_owned(),
                    action: "extract".into(),
                    reason: "html tag conversion".into(),
                    confidence: None,
                });
                Ok(markdown)
            }
            _ => {
                let doc = crate::to_document(bytes, Some(format))?;
                let asset_ocr = self.ocr_document_assets(&doc, ocr).await;
                let markdown = document_to_markdown_with_ocr(&doc, asset_ocr);
                context.decisions.push(Decision {
                    target: file_name.to_owned(),
                    action: "extract".into(),
                    reason: "document model parse".into(),
                    confidence: None,
                });
                Ok(markdown)
            }
        }
    }

    /// Run the embedded-image OCR pass over a parsed document's image assets
    /// and key the outcomes by asset id for the markdown renderer.
    async fn ocr_document_assets(
        &self,
        doc: &crate::model::Document,
        ocr: &OcrSetup<'_>,
    ) -> HashMap<crate::model::AssetId, OcrOutcome> {
        let targets: Vec<OcrImage> = doc
            .assets
            .iter()
            .filter(|asset| asset.media_type.starts_with("image/"))
            .map(|asset| OcrImage {
                bytes: asset.bytes.clone(),
                file_name: ensure_image_extension(&asset.origin_part, &asset.media_type),
            })
            .collect();
        if targets.is_empty() {
            return HashMap::new();
        }
        let outcomes = run_image_ocr(&targets, &ocr.images, self.ocr.clone(), ocr.language).await;
        doc.assets
            .iter()
            .filter_map(|asset| {
                let outcome = outcomes.get(&content_hash(&asset.bytes)).cloned()?;
                Some((asset.id, outcome))
            })
            .collect()
    }

    /// Extract the PDF text layer; on `NeedsOcr`, reroute the named pages
    /// through the OCR provider.
    async fn convert_pdf(
        &self,
        bytes: &[u8],
        file_name: &str,
        ocr: &OcrSetup<'_>,
        context: &mut ConvertContext,
        images: &mut Vec<ImageAsset>,
    ) -> Result<String, RuntimeError> {
        match crate::formats::pdf::to_markdown_with_images(bytes) {
            Ok(output) => {
                context.decisions.push(Decision {
                    target: file_name.to_owned(),
                    action: "extract".into(),
                    reason: "pdf-inspector text layer".into(),
                    confidence: None,
                });
                // OCR the extracted figures and fold the text into their
                // `![图片N](name)` markers before the asset renaming below.
                let mut markdown = output.markdown;
                let targets: Vec<OcrImage> = output
                    .images
                    .iter()
                    .map(|image| OcrImage {
                        bytes: image.data.clone(),
                        file_name: ensure_image_extension(&image.name, &image.mime_type),
                    })
                    .collect();
                let outcomes =
                    run_image_ocr(&targets, &ocr.images, self.ocr.clone(), ocr.language).await;
                for image in &output.images {
                    let marker = format!(
                        "![{}](?)",
                        compose_alt("", outcomes.get(&content_hash(&image.data)))
                    );
                    markdown = replace_pdf_image_markers(&markdown, &image.name, &marker);
                }
                // Namespace the asset names by the document stem so several
                // PDFs converted into one directory do not collide, and keep
                // the markdown marker URLs in sync. Figures that resolved to
                // no extracted object keep their placeholder URLs.
                let stem = file_stem(file_name);
                if markdown.contains("](image-") {
                    markdown = markdown.replace("](image-", &format!("]({stem}-image-"));
                }
                for image in output.images {
                    images.push(ImageAsset {
                        name: format!("{stem}-{}", image.name),
                        mime_type: image.mime_type,
                        data_base64: base64::engine::general_purpose::STANDARD.encode(image.data),
                    });
                }
                Ok(markdown)
            }
            Err(ConvertError::NeedsOcr { pages, page_count }) => {
                context
                    .metadata
                    .insert("pageCount".into(), Value::from(page_count));
                let provider = self.ocr.as_ref().ok_or_else(|| RuntimeError::OcrRequired {
                    pages: pages.clone(),
                })?;
                let language = ocr.language.map(str::to_owned);
                let result = provider
                    .recognize(
                        OcrRequest {
                            bytes: bytes.to_vec(),
                            file_name: file_name.to_owned(),
                            language: language.clone(),
                        },
                        OcrOptions {
                            language,
                            page_numbers: pages.clone(),
                            extra: Map::new(),
                        },
                    )
                    .await
                    .map_err(|error| RuntimeError::Ocr {
                        code: error.code,
                        message: error.message,
                    })?;
                for page in &pages {
                    context.decisions.push(Decision {
                        target: format!("page:{page}"),
                        action: "ocr".into(),
                        reason: "missing or unreliable text layer".into(),
                        confidence: result.confidence,
                    });
                }
                if result.markdown.trim().is_empty() {
                    return Ok(String::new());
                }
                Ok(format!("## OCR 补充\n\n{}\n", result.markdown))
            }
            Err(error) => Err(error.into()),
        }
    }

    /// OCR a standalone image end to end.
    async fn run_ocr(
        &self,
        bytes: &[u8],
        file_name: &str,
        ocr_language: Option<&str>,
        context: &mut ConvertContext,
    ) -> Result<String, RuntimeError> {
        let provider = self
            .ocr
            .as_ref()
            .ok_or(RuntimeError::OcrRequired { pages: Vec::new() })?;
        let language = ocr_language.map(str::to_owned);
        let result = provider
            .recognize(
                OcrRequest {
                    bytes: bytes.to_vec(),
                    file_name: file_name.to_owned(),
                    language: language.clone(),
                },
                OcrOptions {
                    language,
                    page_numbers: Vec::new(),
                    extra: Map::new(),
                },
            )
            .await
            .map_err(|error| RuntimeError::Ocr {
                code: error.code,
                message: error.message,
            })?;
        context.decisions.push(Decision {
            target: file_name.to_owned(),
            action: "ocr".into(),
            reason: "standalone image".into(),
            confidence: result.confidence,
        });
        Ok(result.markdown)
    }
}

fn is_image_extension(file_name: &str) -> bool {
    let ext = file_name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "tif" | "tiff"
    )
}

/// How a recognized text file is rendered to markdown.
#[derive(Clone, Copy, Debug)]
enum TextKind {
    /// Already markdown: returned verbatim.
    Markdown,
    /// Prose: returned verbatim (plain text is trivially valid markdown).
    Plain,
    /// Structured or code: wrapped in a fenced block tagged with the
    /// language.
    Code(&'static str),
}

fn text_kind(file_name: &str) -> Option<TextKind> {
    let ext = file_name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    Some(match ext.as_str() {
        "md" | "mdx" => TextKind::Markdown,
        "txt" => TextKind::Plain,
        "json" => TextKind::Code("json"),
        "yaml" | "yml" => TextKind::Code("yaml"),
        "toml" => TextKind::Code("toml"),
        "xml" => TextKind::Code("xml"),
        "rs" => TextKind::Code("rust"),
        "ts" => TextKind::Code("ts"),
        "js" => TextKind::Code("js"),
        "py" => TextKind::Code("python"),
        _ => return None,
    })
}

fn text_to_markdown(bytes: &[u8], kind: TextKind) -> Result<String, RuntimeError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| RuntimeError::Unsupported("text input is not valid UTF-8".into()))?;
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    Ok(match kind {
        TextKind::Markdown | TextKind::Plain => text.to_owned(),
        TextKind::Code(language) => {
            let body = text.strip_suffix('\n').unwrap_or(text);
            format!("```{language}\n{body}\n```\n")
        }
    })
}

fn safe_file_name(name: &str) -> Result<String, RuntimeError> {
    if name.is_empty() {
        return Err(RuntimeError::InvalidInput("file name is empty".into()));
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(RuntimeError::InvalidInput(format!(
            "unsafe file name: {name}"
        )));
    }
    Ok(name.to_owned())
}

/// Replace every `![图片N](name)` marker — for any sequence number `N` —
/// with `replacement`. pdf-engine numbers markers independently of the
/// extracted asset names, so the match keys on the URL only.
fn replace_pdf_image_markers(markdown: &str, name: &str, replacement: &str) -> String {
    let needle = format!("]({name})");
    let marker_alt = "![图片";
    let mut out = String::with_capacity(markdown.len());
    let mut rest = markdown;
    while let Some(pos) = rest.find(&needle) {
        let before = &rest[..pos];
        if let Some(start) = before.rfind(marker_alt) {
            let digits = &before[start + marker_alt.len()..];
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
                out.push_str(&rest[..start]);
                out.push_str(replacement);
                rest = &rest[pos + needle.len()..];
                continue;
            }
        }
        // Not an image marker; keep the occurrence intact.
        out.push_str(&rest[..pos + needle.len()]);
        rest = &rest[pos + needle.len()..];
    }
    out.push_str(rest);
    out
}

fn file_stem(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(stem, _)| stem.to_owned())
        .unwrap_or_else(|| name.to_owned())
}

/// Extension inventory by category: `text` passes through, `document` and
/// `pdf` go through the format parsers, `image` requires an OCR provider.
pub const SUPPORTED_FORMATS: &[(&str, &[&str])] = &[
    (
        "text",
        &[
            "txt", "md", "mdx", "json", "yaml", "yml", "toml", "xml", "rs", "ts", "js", "py",
        ],
    ),
    (
        "document",
        &[
            "doc", "docx", "docm", "ppt", "pptx", "pptm", "xls", "xlsx", "xlsm", "xlsb", "odt",
            "ods", "odp", "rtf", "epub", "csv",
        ],
    ),
    ("pdf", &["pdf"]),
    ("html", &["html", "htm", "xhtml"]),
    (
        "image",
        &["png", "jpg", "jpeg", "webp", "gif", "bmp", "tif", "tiff"],
    ),
];

/// Convert with a default-configured [`FileParser`]: no sandbox, no OCR.
pub async fn convert(
    input: DocumentInput,
    options: ConvertOptions,
) -> Result<ConvertResult, RuntimeError> {
    FileParser::new().convert(input, options).await
}

/// Convert in-memory bytes with a default-configured [`FileParser`].
pub async fn convert_bytes(
    data: Vec<u8>,
    file_name: impl Into<String>,
    options: ConvertOptions,
) -> Result<ConvertResult, RuntimeError> {
    convert(
        DocumentInput::Bytes {
            data,
            file_name: file_name.into(),
        },
        options,
    )
    .await
}
