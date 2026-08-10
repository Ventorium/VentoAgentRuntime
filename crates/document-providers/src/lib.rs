// SPDX-License-Identifier: MIT

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use vento_document_runtime::provider::{
    OcrOptions, OcrProvider, ProviderError, ProviderInput, ProviderOutput, TranscriptionProvider,
    VisionProvider,
};

const MAX_EXTRA_DEPTH: usize = 4;
const MAX_EXTRA_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaddleOcrConfig {
    pub endpoint: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    #[serde(default = "default_retries")]
    pub retries: u8,
    #[serde(default)]
    pub use_doc_orientation_classify: Option<bool>,
    #[serde(default)]
    pub use_doc_unwarping: Option<bool>,
    #[serde(default)]
    pub use_textline_orientation: Option<bool>,
    #[serde(default)]
    pub extra_params: Map<String, Value>,
}

impl Default for PaddleOcrConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://127.0.0.1:8080/ocr".into(),
            headers: BTreeMap::new(),
            timeout_ms: default_timeout(),
            retries: default_retries(),
            use_doc_orientation_classify: None,
            use_doc_unwarping: None,
            use_textline_orientation: None,
            extra_params: Map::new(),
        }
    }
}

fn default_timeout() -> u64 {
    30_000
}
fn default_retries() -> u8 {
    2
}

#[derive(Clone, Debug)]
pub struct PaddleOcrProvider {
    config: PaddleOcrConfig,
    client: reqwest::Client,
    headers: HeaderMap,
}

impl PaddleOcrProvider {
    pub fn new(config: PaddleOcrConfig) -> Result<Self, ProviderError> {
        validate_extra_params(&config.extra_params)?;
        let mut headers = HeaderMap::new();
        for (name, value) in &config.headers {
            let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
                ProviderError::new("INVALID_PROVIDER_CONFIG", "invalid header name", false)
            })?;
            let value = HeaderValue::from_str(value).map_err(|_| {
                ProviderError::new("INVALID_PROVIDER_CONFIG", "invalid header value", false)
            })?;
            headers.insert(name, value);
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(config.timeout_ms.max(1)))
            .build()
            .map_err(|error| {
                ProviderError::new("INVALID_PROVIDER_CONFIG", error.to_string(), false)
            })?;
        Ok(Self {
            config,
            client,
            headers,
        })
    }
}

#[async_trait]
impl OcrProvider for PaddleOcrProvider {
    fn name(&self) -> &'static str {
        "paddleocr"
    }

    async fn recognize(
        &self,
        input: ProviderInput,
        options: OcrOptions,
    ) -> Result<ProviderOutput, ProviderError> {
        let mut body = self.config.extra_params.clone();
        body.insert(
            "file".into(),
            Value::String(base64::engine::general_purpose::STANDARD.encode(&input.bytes)),
        );
        body.insert(
            "fileType".into(),
            Value::from(if input.mime_type == "application/pdf" {
                0
            } else {
                1
            }),
        );
        insert_optional(
            &mut body,
            "useDocOrientationClassify",
            self.config.use_doc_orientation_classify,
        );
        insert_optional(&mut body, "useDocUnwarping", self.config.use_doc_unwarping);
        insert_optional(
            &mut body,
            "useTextlineOrientation",
            self.config.use_textline_orientation,
        );
        if let Some(language) = options.language {
            body.insert("lang".into(), Value::String(language));
        }
        if !options.page_numbers.is_empty() {
            body.insert(
                "pageNumbers".into(),
                serde_json::to_value(&options.page_numbers).unwrap_or(Value::Null),
            );
        }

        let attempts = self.config.retries.saturating_add(1);
        let mut last_error = None;
        for attempt in 0..attempts {
            let response = self
                .client
                .post(&self.config.endpoint)
                .headers(self.headers.clone())
                .json(&body)
                .send()
                .await;
            match response {
                Ok(response) if response.status().is_success() => {
                    let value: Value = response.json().await.map_err(|error| {
                        ProviderError::new("PROVIDER_RESPONSE_INVALID", error.to_string(), false)
                    })?;
                    return parse_paddle_response(value);
                }
                Ok(response) => {
                    let retryable =
                        response.status().is_server_error() || response.status().as_u16() == 429;
                    last_error = Some(ProviderError::new(
                        "PROVIDER_HTTP_ERROR",
                        format!("PaddleOCR returned HTTP {}", response.status()),
                        retryable,
                    ));
                    if !retryable {
                        break;
                    }
                }
                Err(error) => {
                    last_error = Some(ProviderError::new(
                        "PROVIDER_UNAVAILABLE",
                        error.to_string(),
                        true,
                    ))
                }
            }
            if attempt + 1 < attempts {
                tokio::time::sleep(Duration::from_millis(100 * u64::from(attempt + 1))).await;
            }
        }
        Err(last_error.unwrap_or_else(|| {
            ProviderError::new("PROVIDER_UNAVAILABLE", "PaddleOCR failed", true)
        }))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenAiCompatibleConfig {
    pub endpoint: String,
    pub api_key: String,
    pub model: String,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
}

#[derive(Clone, Debug)]
pub struct OpenAiCompatibleProvider {
    config: OpenAiCompatibleConfig,
    client: reqwest::Client,
}

impl OpenAiCompatibleProvider {
    pub fn new(config: OpenAiCompatibleConfig) -> Result<Self, ProviderError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(config.timeout_ms.max(1)))
            .build()
            .map_err(|error| {
                ProviderError::new("INVALID_PROVIDER_CONFIG", error.to_string(), false)
            })?;
        Ok(Self { config, client })
    }
}

#[async_trait]
impl VisionProvider for OpenAiCompatibleProvider {
    fn name(&self) -> &'static str {
        "openai-compatible-vision"
    }

    async fn describe(&self, input: ProviderInput) -> Result<ProviderOutput, ProviderError> {
        let data_url = format!(
            "data:{};base64,{}",
            input.mime_type,
            base64::engine::general_purpose::STANDARD.encode(input.bytes)
        );
        let body = json!({
            "model": self.config.model,
            "messages": [{"role":"user","content":[
                {"type":"text","text":"Describe this image as concise Markdown. Preserve visible text, structure, charts and tables."},
                {"type":"image_url","image_url":{"url":data_url}}
            ]}]
        });
        let value = self.post_json(body).await?;
        let markdown = value
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ProviderError::new("PROVIDER_RESPONSE_INVALID", "missing vision content", false)
            })?;
        Ok(provider_output(markdown, "openai-compatible-vision"))
    }
}

#[async_trait]
impl TranscriptionProvider for OpenAiCompatibleProvider {
    fn name(&self) -> &'static str {
        "openai-compatible-transcription"
    }

    async fn transcribe(&self, input: ProviderInput) -> Result<ProviderOutput, ProviderError> {
        let form = reqwest::multipart::Form::new()
            .text("model", self.config.model.clone())
            .part(
                "file",
                reqwest::multipart::Part::bytes(input.bytes)
                    .file_name(input.file_name)
                    .mime_str(&input.mime_type)
                    .map_err(|error| {
                        ProviderError::new("INVALID_INPUT", error.to_string(), false)
                    })?,
            );
        let response = self
            .client
            .post(&self.config.endpoint)
            .bearer_auth(&self.config.api_key)
            .multipart(form)
            .send()
            .await
            .map_err(|error| ProviderError::new("PROVIDER_UNAVAILABLE", error.to_string(), true))?;
        if !response.status().is_success() {
            return Err(ProviderError::new(
                "PROVIDER_HTTP_ERROR",
                format!("transcription returned HTTP {}", response.status()),
                response.status().is_server_error(),
            ));
        }
        let value: Value = response.json().await.map_err(|error| {
            ProviderError::new("PROVIDER_RESPONSE_INVALID", error.to_string(), false)
        })?;
        let text = value.get("text").and_then(Value::as_str).ok_or_else(|| {
            ProviderError::new(
                "PROVIDER_RESPONSE_INVALID",
                "missing transcription text",
                false,
            )
        })?;
        Ok(provider_output(text, "openai-compatible-transcription"))
    }
}

impl OpenAiCompatibleProvider {
    async fn post_json(&self, body: Value) -> Result<Value, ProviderError> {
        let response = self
            .client
            .post(&self.config.endpoint)
            .bearer_auth(&self.config.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|error| ProviderError::new("PROVIDER_UNAVAILABLE", error.to_string(), true))?;
        if !response.status().is_success() {
            return Err(ProviderError::new(
                "PROVIDER_HTTP_ERROR",
                format!("provider returned HTTP {}", response.status()),
                response.status().is_server_error(),
            ));
        }
        response.json().await.map_err(|error| {
            ProviderError::new("PROVIDER_RESPONSE_INVALID", error.to_string(), false)
        })
    }
}

fn parse_paddle_response(value: Value) -> Result<ProviderOutput, ProviderError> {
    if value.get("errorCode").and_then(Value::as_i64).unwrap_or(0) != 0 {
        return Err(ProviderError::new(
            "PROVIDER_REJECTED",
            value
                .get("errorMsg")
                .and_then(Value::as_str)
                .unwrap_or("PaddleOCR rejected input"),
            false,
        ));
    }
    let results = value
        .pointer("/result/ocrResults")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ProviderError::new(
                "PROVIDER_RESPONSE_INVALID",
                "missing result.ocrResults",
                false,
            )
        })?;
    let mut pages = Vec::new();
    let mut confidences = Vec::new();
    for (index, result) in results.iter().enumerate() {
        let text = result
            .get("markdown")
            .or_else(|| result.get("rec_texts"))
            .or_else(|| result.get("text"));
        let rendered = match text {
            Some(Value::String(value)) => value.clone(),
            Some(Value::Array(values)) => values
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        if let Some(score) = result.get("rec_scores").and_then(Value::as_array) {
            confidences.extend(score.iter().filter_map(Value::as_f64));
        }
        pages.push(format!("## Page {}\n\n{}", index + 1, rendered.trim()));
    }
    let confidence = (!confidences.is_empty())
        .then(|| (confidences.iter().sum::<f64>() / confidences.len() as f64) as f32);
    Ok(ProviderOutput {
        markdown: pages.join("\n\n"),
        confidence,
        provider: "paddleocr".into(),
        metadata: Map::new(),
    })
}

fn insert_optional(body: &mut Map<String, Value>, key: &str, value: Option<bool>) {
    if let Some(value) = value {
        body.insert(key.into(), Value::Bool(value));
    }
}

fn validate_extra_params(params: &Map<String, Value>) -> Result<(), ProviderError> {
    if params.contains_key("file") || params.contains_key("fileType") {
        return Err(ProviderError::new(
            "INVALID_PROVIDER_CONFIG",
            "extraParams cannot override file or fileType",
            false,
        ));
    }
    if serde_json::to_vec(params).map_or(true, |bytes| bytes.len() > MAX_EXTRA_BYTES)
        || params
            .values()
            .any(|value| value_depth(value) > MAX_EXTRA_DEPTH)
    {
        return Err(ProviderError::new(
            "INVALID_PROVIDER_CONFIG",
            "extraParams exceed size or depth limit",
            false,
        ));
    }
    Ok(())
}

fn value_depth(value: &Value) -> usize {
    match value {
        Value::Array(values) => 1 + values.iter().map(value_depth).max().unwrap_or(0),
        Value::Object(values) => 1 + values.values().map(value_depth).max().unwrap_or(0),
        _ => 0,
    }
}

fn provider_output(markdown: &str, provider: &str) -> ProviderOutput {
    ProviderOutput {
        markdown: markdown.into(),
        confidence: None,
        provider: provider.into(),
        metadata: Map::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_paddle_fields_cannot_be_overridden() {
        let mut config = PaddleOcrConfig::default();
        config
            .extra_params
            .insert("file".into(), Value::String("bad".into()));
        assert!(PaddleOcrProvider::new(config).is_err());
    }

    #[test]
    fn parses_paddle_text_arrays() {
        let output = parse_paddle_response(json!({
            "errorCode": 0,
            "result": {"ocrResults": [{"rec_texts": ["hello", "world"], "rec_scores": [0.9, 0.8]}]}
        }))
        .expect("valid response");
        assert!(output.markdown.contains("hello\nworld"));
        assert!(output.confidence.is_some());
    }
}
