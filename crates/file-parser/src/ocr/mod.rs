//! Remote OCR integration. OCR is delegated to an HTTP provider (currently
//! PaddleOCR); the runtime never bundles OCR models itself.

mod paddle;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub use paddle::{PaddleOcrConfig, PaddleOcrProvider};

/// A single OCR job: the file (image or PDF) plus its name and language.
#[derive(Clone, Debug)]
pub struct OcrRequest {
    /// File content (image or whole PDF).
    pub bytes: Vec<u8>,
    /// File name; used for file-type inference by providers.
    pub file_name: String,
    /// Per-request language override.
    pub language: Option<String>,
}

/// Provider recognition result.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OcrOutput {
    /// Recognized text as markdown (typically plain lines).
    pub markdown: String,
    /// Mean recognition confidence in `[0, 1]`, when the provider reports it.
    pub confidence: Option<f32>,
    /// Provider name that produced this output.
    pub provider: String,
}

/// Provider failure. `code` is stable for programmatic handling.
#[derive(Debug, Error)]
#[error("{message}")]
pub struct OcrError {
    /// Stable error code (e.g. `invalid_config`, `transient`, `upstream_error`).
    pub code: &'static str,
    /// Human-readable message.
    pub message: String,
    /// Whether retrying the same request could succeed.
    pub retryable: bool,
}

impl OcrError {
    /// Construct an error.
    pub fn new(code: &'static str, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
        }
    }
}

/// Per-call provider options.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrOptions {
    /// Language hint; falls back to the provider config.
    pub language: Option<String>,
    /// For PDFs: the pages to recognize (from the `NeedsOcr` report).
    /// Empty means the whole input.
    pub page_numbers: Vec<u32>,
    /// Provider-specific extra parameters.
    pub extra: serde_json::Map<String, Value>,
}

/// A remote OCR backend. Implementations must be cheap to share across
/// await points (`Send + Sync`).
#[async_trait]
pub trait OcrProvider: Send + Sync {
    /// Provider name, surfaced in [`OcrOutput::provider`]-adjacent logging.
    fn name(&self) -> &'static str;
    /// Recognize text in `request`, honoring `options`.
    async fn recognize(
        &self,
        request: OcrRequest,
        options: OcrOptions,
    ) -> Result<OcrOutput, OcrError>;
}
