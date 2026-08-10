// SPDX-License-Identifier: MIT

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::header::{CONTENT_DISPOSITION, CONTENT_TYPE, LOCATION};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::provider::{
    OcrOptions, OcrProvider, ProviderError, ProviderInput, TranscriptionProvider, VisionProvider,
};
use crate::{Format, to_markdown_bytes};

const DEFAULT_MAX_INPUT_BYTES: usize = 500 * 1024 * 1024;
const DEFAULT_MAX_REDIRECTS: usize = 5;

#[derive(Clone, Debug)]
pub enum DocumentInput {
    Bytes { data: Vec<u8>, file_name: String },
    Path { path: PathBuf },
    Url { url: String },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConvertOptions {
    pub ocr_language: Option<String>,
    pub enable_vision: bool,
    pub ffmpeg_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupportedFormat {
    pub category: String,
    pub extensions: Vec<String>,
    pub requires_provider: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceMetadata {
    pub file_name: String,
    pub mime_type: String,
    pub size: u64,
    pub source_type: String,
    pub source_url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentSection {
    pub title: Option<String>,
    pub markdown: String,
    pub page: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetDescriptor {
    pub id: String,
    pub mime_type: String,
    pub role: String,
    pub size: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionDecision {
    pub target: String,
    pub action: String,
    pub reason: String,
    pub confidence: Option<f32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectionResult {
    pub source: SourceMetadata,
    pub format: String,
    pub supported: bool,
    pub requires_provider: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConvertResult {
    pub markdown: String,
    pub title: String,
    pub source: SourceMetadata,
    pub sections: Vec<DocumentSection>,
    pub assets: Vec<AssetDescriptor>,
    pub metadata: Map<String, Value>,
    pub decisions: Vec<ExtractionDecision>,
    pub warnings: Vec<String>,
    pub duration_ms: u64,
}

#[derive(Debug)]
pub enum RuntimeError {
    InvalidInput(String),
    AccessDenied(String),
    Unsupported(String),
    ResourceLimit(String),
    ProviderRequired(&'static str),
    Provider(ProviderError),
    Io(std::io::Error),
    Http(String),
    Conversion(crate::ConvertError),
}

impl RuntimeError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidInput(_) => "INVALID_INPUT",
            Self::AccessDenied(_) => "ACCESS_DENIED",
            Self::Unsupported(_) => "UNSUPPORTED",
            Self::ResourceLimit(_) => "RESOURCE_LIMIT",
            Self::ProviderRequired(_) => "PROVIDER_REQUIRED",
            Self::Provider(error) => error.code,
            Self::Io(_) => "IO_ERROR",
            Self::Http(_) => "HTTP_ERROR",
            Self::Conversion(error) => error.code(),
        }
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(message)
            | Self::AccessDenied(message)
            | Self::Unsupported(message)
            | Self::ResourceLimit(message)
            | Self::Http(message) => formatter.write_str(message),
            Self::ProviderRequired(kind) => write!(formatter, "{kind} provider is required"),
            Self::Provider(error) => write!(formatter, "provider failed: {error}"),
            Self::Io(error) => write!(formatter, "I/O failed: {error}"),
            Self::Conversion(error) => write!(formatter, "conversion failed: {error}"),
        }
    }
}

impl std::error::Error for RuntimeError {}

impl From<std::io::Error> for RuntimeError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<crate::ConvertError> for RuntimeError {
    fn from(error: crate::ConvertError) -> Self {
        Self::Conversion(error)
    }
}

#[derive(Clone, Default)]
pub struct DocumentRuntime {
    allowed_roots: Vec<PathBuf>,
    max_input_bytes: usize,
    ocr: Option<Arc<dyn OcrProvider>>,
    vision: Option<Arc<dyn VisionProvider>>,
    transcription: Option<Arc<dyn TranscriptionProvider>>,
}

impl fmt::Debug for DocumentRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DocumentRuntime")
            .field("allowed_roots", &self.allowed_roots)
            .field("max_input_bytes", &self.max_input_bytes)
            .field("ocr", &self.ocr.as_ref().map(|value| value.name()))
            .field("vision", &self.vision.as_ref().map(|value| value.name()))
            .field(
                "transcription",
                &self.transcription.as_ref().map(|value| value.name()),
            )
            .finish()
    }
}

impl DocumentRuntime {
    pub fn new() -> Self {
        Self {
            max_input_bytes: DEFAULT_MAX_INPUT_BYTES,
            ..Self::default()
        }
    }

    pub fn with_allowed_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.allowed_roots = roots;
        self
    }

    pub fn with_max_input_bytes(mut self, value: usize) -> Self {
        self.max_input_bytes = value.max(1);
        self
    }

    pub fn with_ocr(mut self, provider: Arc<dyn OcrProvider>) -> Self {
        self.ocr = Some(provider);
        self
    }

    pub fn with_vision(mut self, provider: Arc<dyn VisionProvider>) -> Self {
        self.vision = Some(provider);
        self
    }

    pub fn with_transcription(mut self, provider: Arc<dyn TranscriptionProvider>) -> Self {
        self.transcription = Some(provider);
        self
    }

    pub async fn inspect(&self, input: DocumentInput) -> Result<InspectionResult, RuntimeError> {
        let resolved = self.resolve_input(input).await?;
        let category = category_for(&resolved.file_name);
        Ok(InspectionResult {
            source: resolved.metadata,
            format: category.to_owned(),
            supported: category != "unsupported",
            requires_provider: matches!(category, "image" | "audio" | "video"),
        })
    }

    pub async fn convert(
        &self,
        input: DocumentInput,
        options: ConvertOptions,
    ) -> Result<ConvertResult, RuntimeError> {
        let started = Instant::now();
        let resolved = self.resolve_input(input).await?;
        let category = category_for(&resolved.file_name);
        let title = file_stem(&resolved.file_name);
        let mut decisions = Vec::new();
        let mut warnings = Vec::new();
        let mut metadata = Map::new();

        let markdown = match category {
            "text" => String::from_utf8(resolved.bytes.clone())
                .map_err(|_| RuntimeError::InvalidInput("text input is not valid UTF-8".into()))?,
            "html" => html_to_markdown(&String::from_utf8_lossy(&resolved.bytes)),
            "image" => {
                let provider = self
                    .ocr
                    .as_ref()
                    .ok_or(RuntimeError::ProviderRequired("OCR"))?;
                let output = provider
                    .recognize(
                        provider_input(&resolved),
                        OcrOptions {
                            language: options.ocr_language,
                            ..OcrOptions::default()
                        },
                    )
                    .await
                    .map_err(RuntimeError::Provider)?;
                decisions.push(ExtractionDecision {
                    target: resolved.file_name.clone(),
                    action: "ocr".into(),
                    reason: "standalone image".into(),
                    confidence: output.confidence,
                });
                output.markdown
            }
            "audio" | "video" => {
                let provider = self
                    .transcription
                    .as_ref()
                    .ok_or(RuntimeError::ProviderRequired("transcription"))?;
                let normalized = normalize_media(&resolved, options.ffmpeg_path.as_deref()).await?;
                let output = provider
                    .transcribe(normalized)
                    .await
                    .map_err(RuntimeError::Provider)?;
                decisions.push(ExtractionDecision {
                    target: resolved.file_name.clone(),
                    action: "transcribe".into(),
                    reason: "media audio track".into(),
                    confidence: output.confidence,
                });
                output.markdown
            }
            "pdf" => {
                let result = vento_pdf_engine::process_pdf_mem(&resolved.bytes)
                    .map_err(|error| RuntimeError::Unsupported(error.to_string()))?;
                metadata.insert("pageCount".into(), Value::from(result.page_count));
                metadata.insert(
                    "pdfType".into(),
                    Value::from(format!("{:?}", result.pdf_type)),
                );
                let direct = result.markdown.unwrap_or_default();
                if result.pages_needing_ocr.is_empty() {
                    decisions.push(ExtractionDecision {
                        target: "pdf".into(),
                        action: "extract".into(),
                        reason: "usable text layer".into(),
                        confidence: Some(result.confidence),
                    });
                    direct
                } else if let Some(provider) = self.ocr.as_ref() {
                    let pages = result.pages_needing_ocr.clone();
                    let output = provider
                        .recognize(
                            provider_input(&resolved),
                            OcrOptions {
                                language: options.ocr_language,
                                page_numbers: pages.clone(),
                                extra: Map::new(),
                            },
                        )
                        .await
                        .map_err(RuntimeError::Provider)?;
                    for page in pages {
                        decisions.push(ExtractionDecision {
                            target: format!("page:{page}"),
                            action: "ocr".into(),
                            reason: "missing or unreliable text layer".into(),
                            confidence: output.confidence,
                        });
                    }
                    if direct.trim().is_empty() {
                        output.markdown
                    } else {
                        format!("{direct}\n\n## OCR 补充\n\n{}", output.markdown)
                    }
                } else {
                    return Err(RuntimeError::ProviderRequired("OCR"));
                }
            }
            "document" => {
                let format = format_from_name(&resolved.file_name);
                to_markdown_bytes(&resolved.bytes, format)?
            }
            _ => {
                return Err(RuntimeError::Unsupported(format!(
                    "unsupported file: {}",
                    resolved.file_name
                )));
            }
        };

        if markdown.trim().is_empty() {
            warnings.push("conversion produced empty Markdown".into());
        }
        let sections = vec![DocumentSection {
            title: Some(title.clone()),
            markdown: markdown.clone(),
            page: None,
        }];
        Ok(ConvertResult {
            markdown,
            title,
            source: resolved.metadata,
            sections,
            assets: Vec::new(),
            metadata,
            decisions,
            warnings,
            duration_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        })
    }

    async fn resolve_input(&self, input: DocumentInput) -> Result<ResolvedInput, RuntimeError> {
        let (bytes, file_name, source_type, source_url) = match input {
            DocumentInput::Bytes { data, file_name } => {
                (data, safe_file_name(&file_name)?, "bytes", None)
            }
            DocumentInput::Path { path } => {
                let canonical = tokio::fs::canonicalize(&path).await?;
                let allowed = self
                    .allowed_roots
                    .iter()
                    .any(|root| canonical.starts_with(root));
                if !allowed {
                    return Err(RuntimeError::AccessDenied(
                        "path is outside configured roots".into(),
                    ));
                }
                let name = canonical
                    .file_name()
                    .and_then(|value| value.to_str())
                    .ok_or_else(|| {
                        RuntimeError::InvalidInput("path has no UTF-8 file name".into())
                    })?;
                (
                    tokio::fs::read(&canonical).await?,
                    safe_file_name(name)?,
                    "path",
                    None,
                )
            }
            DocumentInput::Url { url } => {
                let (bytes, name, final_url) = fetch_url(&url, self.max_input_bytes).await?;
                (bytes, name, "url", Some(final_url))
            }
        };
        if bytes.len() > self.max_input_bytes {
            return Err(RuntimeError::ResourceLimit(format!(
                "input has {} bytes; maximum is {}",
                bytes.len(),
                self.max_input_bytes
            )));
        }
        let mime_type = mime_for(&file_name).to_owned();
        Ok(ResolvedInput {
            metadata: SourceMetadata {
                file_name: file_name.clone(),
                mime_type,
                size: bytes.len() as u64,
                source_type: source_type.into(),
                source_url,
            },
            bytes,
            file_name,
        })
    }
}

struct ResolvedInput {
    bytes: Vec<u8>,
    file_name: String,
    metadata: SourceMetadata,
}

pub async fn inspect(input: DocumentInput) -> Result<InspectionResult, RuntimeError> {
    DocumentRuntime::new().inspect(input).await
}

pub async fn convert(
    input: DocumentInput,
    options: ConvertOptions,
) -> Result<ConvertResult, RuntimeError> {
    DocumentRuntime::new().convert(input, options).await
}

pub fn get_supported_formats() -> Vec<SupportedFormat> {
    vec![
        format_group(
            "text",
            &[
                "txt", "md", "mdx", "json", "yaml", "yml", "toml", "xml", "rs", "ts", "js", "py",
            ],
            false,
        ),
        format_group(
            "document",
            &[
                "doc", "docx", "docm", "ppt", "pptx", "pptm", "xls", "xlsx", "xlsm", "xlsb", "odt",
                "ods", "odp", "rtf", "epub", "csv",
            ],
            false,
        ),
        format_group("pdf", &["pdf"], false),
        format_group(
            "image",
            &["png", "jpg", "jpeg", "webp", "gif", "bmp", "tif", "tiff"],
            true,
        ),
        format_group(
            "audio",
            &["mp3", "wav", "m4a", "aac", "flac", "ogg", "opus"],
            true,
        ),
        format_group("video", &["mp4", "webm", "mov", "mkv"], true),
    ]
}

fn format_group(category: &str, extensions: &[&str], requires_provider: bool) -> SupportedFormat {
    SupportedFormat {
        category: category.into(),
        extensions: extensions.iter().map(|value| (*value).to_owned()).collect(),
        requires_provider,
    }
}

fn provider_input(input: &ResolvedInput) -> ProviderInput {
    ProviderInput {
        bytes: input.bytes.clone(),
        file_name: input.file_name.clone(),
        mime_type: input.metadata.mime_type.clone(),
    }
}

fn category_for(name: &str) -> &'static str {
    match extension(name).as_str() {
        "txt" | "md" | "mdx" | "log" | "json" | "yaml" | "yml" | "toml" | "xml" | "rs" | "ts"
        | "tsx" | "js" | "jsx" | "py" | "go" | "java" | "c" | "cpp" | "sh" | "sql" => "text",
        "html" | "htm" => "html",
        "pdf" => "pdf",
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "tif" | "tiff" => "image",
        "mp3" | "wav" | "m4a" | "aac" | "flac" | "ogg" | "opus" => "audio",
        "mp4" | "webm" | "mov" | "mkv" => "video",
        "doc" | "docx" | "docm" | "ppt" | "pptx" | "pptm" | "pps" | "ppsx" | "xls" | "xlsx"
        | "xlsm" | "xlsb" | "odt" | "ods" | "odp" | "rtf" | "epub" | "csv" => "document",
        _ => "unsupported",
    }
}

fn format_from_name(name: &str) -> Option<Format> {
    Format::from_extension(&extension(name))
}
fn extension(name: &str) -> String {
    Path::new(name)
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}
fn file_stem(name: &str) -> String {
    Path::new(name)
        .file_stem()
        .and_then(|v| v.to_str())
        .unwrap_or("document")
        .to_owned()
}

fn mime_for(name: &str) -> &'static str {
    match extension(name).as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "html" | "htm" => "text/html",
        "md" | "mdx" => "text/markdown",
        "txt" => "text/plain",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
}

fn safe_file_name(name: &str) -> Result<String, RuntimeError> {
    let candidate = Path::new(name)
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| RuntimeError::InvalidInput("invalid file name".into()))?;
    if candidate.is_empty() || candidate.contains('\0') {
        return Err(RuntimeError::InvalidInput("invalid file name".into()));
    }
    Ok(candidate.to_owned())
}

fn html_to_markdown(html: &str) -> String {
    let mut output = String::with_capacity(html.len());
    let mut in_tag = false;
    for character in html.chars() {
        match character {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                output.push(' ');
            }
            _ if !in_tag => output.push(character),
            _ => {}
        }
    }
    output.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(value) => {
            value.is_private()
                || value.is_loopback()
                || value.is_link_local()
                || value.is_unspecified()
                || value.is_broadcast()
                || value.is_documentation()
        }
        IpAddr::V6(value) => {
            value.is_loopback()
                || value.is_unspecified()
                || value.is_unique_local()
                || value.is_unicast_link_local()
        }
    }
}

async fn validate_public_url(url: &reqwest::Url) -> Result<(), RuntimeError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(RuntimeError::AccessDenied(
            "only HTTP(S) URLs are allowed".into(),
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| RuntimeError::InvalidInput("URL has no host".into()))?;
    let port = url.port_or_known_default().unwrap_or(443);
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|error| RuntimeError::Http(format!("DNS lookup failed: {error}")))?
        .collect();
    if addresses.is_empty() || addresses.iter().any(|address| blocked_ip(address.ip())) {
        return Err(RuntimeError::AccessDenied(
            "URL resolves to a blocked network".into(),
        ));
    }
    Ok(())
}

async fn fetch_url(url: &str, max_bytes: usize) -> Result<(Vec<u8>, String, String), RuntimeError> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| RuntimeError::Http(error.to_string()))?;
    let mut current =
        reqwest::Url::parse(url).map_err(|error| RuntimeError::InvalidInput(error.to_string()))?;
    for _ in 0..=DEFAULT_MAX_REDIRECTS {
        validate_public_url(&current).await?;
        let response = client
            .get(current.clone())
            .send()
            .await
            .map_err(|error| RuntimeError::Http(error.to_string()))?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| RuntimeError::Http("redirect has no valid Location".into()))?;
            current = current
                .join(location)
                .map_err(|error| RuntimeError::Http(error.to_string()))?;
            continue;
        }
        if !response.status().is_success() {
            return Err(RuntimeError::Http(format!(
                "source returned HTTP {}",
                response.status()
            )));
        }
        if response
            .content_length()
            .is_some_and(|size| size > max_bytes as u64)
        {
            return Err(RuntimeError::ResourceLimit(
                "remote content is too large".into(),
            ));
        }
        let headers = response.headers().clone();
        let bytes = response
            .bytes()
            .await
            .map_err(|error| RuntimeError::Http(error.to_string()))?;
        if bytes.len() > max_bytes {
            return Err(RuntimeError::ResourceLimit(
                "remote content is too large".into(),
            ));
        }
        let name = name_from_headers(&headers)
            .or_else(|| current.path_segments()?.next_back().map(str::to_owned))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "download.bin".into());
        let _content_type = headers.get(CONTENT_TYPE);
        return Ok((bytes.to_vec(), safe_file_name(&name)?, current.to_string()));
    }
    Err(RuntimeError::ResourceLimit("too many redirects".into()))
}

fn name_from_headers(headers: &reqwest::header::HeaderMap) -> Option<String> {
    let value = headers.get(CONTENT_DISPOSITION)?.to_str().ok()?;
    value.split(';').find_map(|part| {
        part.trim()
            .strip_prefix("filename=")
            .map(|name| name.trim_matches(['"', '\'']).to_owned())
    })
}

async fn normalize_media(
    input: &ResolvedInput,
    ffmpeg: Option<&Path>,
) -> Result<ProviderInput, RuntimeError> {
    let id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!("vento-media-{}-{id}", std::process::id()));
    tokio::fs::create_dir(&directory).await?;
    let source = directory.join(&input.file_name);
    let output = directory.join("audio.wav");
    tokio::fs::write(&source, &input.bytes).await?;
    let result = tokio::process::Command::new(ffmpeg.unwrap_or_else(|| Path::new("ffmpeg")))
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(&source)
        .args(["-vn", "-ac", "1", "-ar", "16000", "-f", "wav", "-y"])
        .arg(&output)
        .kill_on_drop(true)
        .output()
        .await;
    let converted = match result {
        Ok(status) if status.status.success() => tokio::fs::read(&output).await,
        Ok(status) => Err(std::io::Error::other(format!(
            "ffmpeg failed: {}",
            String::from_utf8_lossy(&status.stderr)
        ))),
        Err(error) => Err(error),
    };
    let _ = tokio::fs::remove_dir_all(&directory).await;
    let bytes = converted?;
    Ok(ProviderInput {
        bytes,
        file_name: "audio.wav".into(),
        mime_type: "audio/wav".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn converts_utf8_text() {
        let result = convert(
            DocumentInput::Bytes {
                data: b"# hello".to_vec(),
                file_name: "a.md".into(),
            },
            ConvertOptions::default(),
        )
        .await
        .expect("text should convert");
        assert_eq!(result.markdown, "# hello");
    }

    #[test]
    fn blocks_private_networks() {
        assert!(blocked_ip("127.0.0.1".parse().expect("IP")));
        assert!(blocked_ip("169.254.169.254".parse().expect("IP")));
        assert!(!blocked_ip("1.1.1.1".parse().expect("IP")));
    }

    #[test]
    fn rejects_path_components_in_names() {
        assert_eq!(
            safe_file_name("../../safe.txt").expect("basename"),
            "safe.txt"
        );
    }
}
