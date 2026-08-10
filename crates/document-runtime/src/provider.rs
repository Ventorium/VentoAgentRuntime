// SPDX-License-Identifier: MIT

use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct ProviderInput {
    pub bytes: Vec<u8>,
    pub file_name: String,
    pub mime_type: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrOptions {
    pub language: Option<String>,
    pub page_numbers: Vec<u32>,
    pub extra: serde_json::Map<String, Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderOutput {
    pub markdown: String,
    pub confidence: Option<f32>,
    pub provider: String,
    pub metadata: serde_json::Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderError {
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
}

impl ProviderError {
    pub fn new(code: &'static str, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
        }
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProviderError {}

#[async_trait]
pub trait OcrProvider: Send + Sync {
    fn name(&self) -> &'static str;
    async fn recognize(
        &self,
        input: ProviderInput,
        options: OcrOptions,
    ) -> Result<ProviderOutput, ProviderError>;
}

#[async_trait]
pub trait VisionProvider: Send + Sync {
    fn name(&self) -> &'static str;
    async fn describe(&self, input: ProviderInput) -> Result<ProviderOutput, ProviderError>;
}

#[async_trait]
pub trait TranscriptionProvider: Send + Sync {
    fn name(&self) -> &'static str;
    async fn transcribe(&self, input: ProviderInput) -> Result<ProviderOutput, ProviderError>;
}
