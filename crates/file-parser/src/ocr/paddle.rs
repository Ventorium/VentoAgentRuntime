//! PaddleOCR job backend, following the AI Studio `api/v2/ocr/jobs`
//! protocol: submit the file as multipart, poll the job resource until it
//! completes, then download the JSONL result and concatenate each page's
//! markdown text.

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{OcrError, OcrOptions, OcrOutput, OcrProvider, OcrRequest};

const DEFAULT_MODEL: &str = "PaddleOCR-VL-1.6";
/// Cap on any single HTTP request (submit, one poll, result download).
const PER_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Delay between job-state polls.
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Backoff when the upstream rate-limits us (error 12002 / HTTP 429).
const RATE_LIMIT_BACKOFF: Duration = Duration::from_secs(5);

/// Connection settings for the PaddleOCR job service.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaddleOcrConfig {
    /// Job-collection URL (…/api/v2/ocr/jobs); the job resource is polled at
    /// `{endpoint}/{jobId}`.
    pub endpoint: String,
    /// Extra HTTP headers (e.g. `Authorization: bearer …`).
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Model name; defaults to `PaddleOCR-VL-1.6`.
    #[serde(default)]
    pub model: Option<String>,
    /// Total budget for submit + poll + result download. Defaults to 5 min.
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    /// Submit attempts (including the first). Defaults to 2.
    #[serde(default = "default_retries")]
    pub retries: u8,
    /// Fields merged into the request's `optionalPayload` (e.g.
    /// `useDocOrientationClassify`). The reserved pipeline switches are
    /// rejected so callers cannot desynchronize the markdown contract.
    #[serde(default)]
    pub optional_payload: Map<String, Value>,
}

impl Default for PaddleOcrConfig {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            headers: BTreeMap::new(),
            model: None,
            timeout_ms: default_timeout(),
            retries: default_retries(),
            optional_payload: Map::new(),
        }
    }
}

fn default_timeout() -> u64 {
    300_000
}
fn default_retries() -> u8 {
    2
}

impl PaddleOcrConfig {
    /// Deserialize and validate a config from JSON.
    pub fn from_value(value: Value) -> Result<Self, OcrError> {
        let config: PaddleOcrConfig = serde_json::from_value(value).map_err(|e| {
            OcrError::new(
                "invalid_config",
                format!("invalid paddleOcr config: {e}"),
                false,
            )
        })?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), OcrError> {
        if self.endpoint.trim().is_empty() {
            return Err(OcrError::new(
                "invalid_config",
                "paddleOcr.endpoint is required",
                false,
            ));
        }
        if let Some(model) = &self.model
            && !is_supported_model(model)
        {
            return Err(OcrError::new(
                "invalid_config",
                format!(
                    "paddleOcr.model '{model}' is not supported: only PaddleOCR-VL* and \
                     PP-StructureV3* return layoutParsingResults markdown (PP-OCRv5 returns \
                     ocrResults, which the markdown contract cannot consume)"
                ),
                false,
            ));
        }
        for key in self.optional_payload.keys() {
            if is_reserved_payload_key(key) {
                return Err(OcrError::new(
                    "invalid_config",
                    format!("paddleOcr.optionalPayload.{key} is reserved and cannot be set"),
                    false,
                ));
            }
        }
        Ok(())
    }
}

/// An [`OcrProvider`] talking to the PaddleOCR job service.
#[derive(Debug)]
pub struct PaddleOcrProvider {
    config: PaddleOcrConfig,
    client: reqwest::Client,
}

impl PaddleOcrProvider {
    /// Build a provider; fails if the config is invalid or the HTTP client
    /// cannot be created.
    pub fn new(config: PaddleOcrConfig) -> Result<Self, OcrError> {
        config.validate()?;
        let client = reqwest::Client::builder()
            .timeout(PER_REQUEST_TIMEOUT)
            .build()
            .map_err(|e| {
                OcrError::new("client_init", format!("reqwest init failed: {e}"), false)
            })?;
        Ok(Self { config, client })
    }

    /// Build a provider from a JSON config value.
    pub fn from_value(value: Value) -> Result<Self, OcrError> {
        Self::new(PaddleOcrConfig::from_value(value)?)
    }

    fn auth_headers(&self) -> Result<reqwest::header::HeaderMap, OcrError> {
        let mut headers = reqwest::header::HeaderMap::new();
        for (key, value) in &self.config.headers {
            let name = reqwest::header::HeaderName::try_from(key).map_err(|e| {
                OcrError::new(
                    "invalid_config",
                    format!("invalid header '{key}': {e}"),
                    false,
                )
            })?;
            let header = reqwest::header::HeaderValue::from_str(value).map_err(|e| {
                OcrError::new(
                    "invalid_config",
                    format!("invalid header value for '{key}': {e}"),
                    false,
                )
            })?;
            headers.insert(name, header);
        }
        Ok(headers)
    }

    /// Submit the file; returns the job id. Transient failures are retried
    /// up to the configured attempt count (each retry creates a new job).
    /// `page_ranges` is the upstream `pageRanges` syntax ("2,4-6"), empty to
    /// process the whole file.
    async fn submit_job(
        &self,
        request: &OcrRequest,
        page_ranges: &str,
    ) -> Result<String, OcrError> {
        let optional_payload = Value::Object(self.config.optional_payload.clone()).to_string();
        let attempts = self.config.retries.max(1);
        let mut last_error: Option<OcrError> = None;
        for attempt in 0..attempts {
            let part = reqwest::multipart::Part::bytes(request.bytes.clone())
                .file_name(request.file_name.clone())
                .mime_str(mime_for(&request.file_name))
                .map_err(|e| {
                    OcrError::new("invalid_input", format!("bad file name: {e}"), false)
                })?;
            let mut form = reqwest::multipart::Form::new()
                .text(
                    "model",
                    self.config
                        .model
                        .clone()
                        .unwrap_or_else(|| DEFAULT_MODEL.into()),
                )
                .text("optionalPayload", optional_payload.clone());
            if !page_ranges.is_empty() {
                form = form.text("pageRanges", page_ranges.to_owned());
            }
            let form = form.part("file", part);
            match self
                .client
                .post(&self.config.endpoint)
                .headers(self.auth_headers()?)
                .multipart(form)
                .send()
                .await
            {
                Ok(response) => {
                    let status = response.status();
                    if !status.is_success() {
                        let text = response.text().await.unwrap_or_default();
                        let error = OcrError::new(
                            "upstream_error",
                            format!("paddleOcr submit returned HTTP {status}: {text}"),
                            status.is_server_error()
                                || status == reqwest::StatusCode::TOO_MANY_REQUESTS,
                        );
                        if error.retryable && attempt + 1 < attempts {
                            last_error = Some(error);
                            continue;
                        }
                        return Err(error);
                    }
                    let value: Value = response.json().await.map_err(|e| {
                        OcrError::new(
                            "upstream_error",
                            format!("paddleOcr submit returned invalid JSON: {e}"),
                            true,
                        )
                    })?;
                    return value
                        .pointer("/data/jobId")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .ok_or_else(|| {
                            OcrError::new(
                                "upstream_error",
                                "paddleOcr submit response missing data.jobId",
                                false,
                            )
                        });
                }
                Err(error) => {
                    let error = OcrError::new(
                        "transient",
                        format!("paddleOcr submit failed: {error}"),
                        true,
                    );
                    if attempt + 1 < attempts {
                        last_error = Some(error);
                        continue;
                    }
                    return Err(error);
                }
            }
        }
        Err(last_error
            .unwrap_or_else(|| OcrError::new("transient", "paddleOcr retries exhausted", true)))
    }

    /// Poll the job resource until `done`, returning the JSONL result URL.
    /// Network hiccups keep polling until the overall budget is spent.
    async fn wait_for_result_url(
        &self,
        job_id: &str,
        deadline: Instant,
    ) -> Result<String, OcrError> {
        loop {
            if Instant::now() >= deadline {
                return Err(OcrError::new(
                    "timeout",
                    "paddleOcr job did not finish within timeoutMs",
                    true,
                ));
            }
            let response = self
                .client
                .get(format!("{}/{job_id}", self.config.endpoint))
                .headers(self.auth_headers()?)
                .send()
                .await
                .map_err(|e| {
                    OcrError::new("transient", format!("paddleOcr poll failed: {e}"), true)
                })?;
            let status = response.status();
            if !status.is_success() {
                // Rate-limited (error 12002): back off longer and keep
                // polling — the job itself is unaffected.
                if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    tokio::time::sleep(RATE_LIMIT_BACKOFF).await;
                    continue;
                }
                let text = response.text().await.unwrap_or_default();
                let error = OcrError::new(
                    "upstream_error",
                    format!("paddleOcr poll returned HTTP {status}: {text}"),
                    status.is_server_error(),
                );
                if !error.retryable {
                    return Err(error);
                }
                tokio::time::sleep(POLL_INTERVAL).await;
                continue;
            }
            let value: Value = response.json().await.map_err(|e| {
                OcrError::new(
                    "upstream_error",
                    format!("paddleOcr poll returned invalid JSON: {e}"),
                    true,
                )
            })?;
            let state = value.pointer("/data/state").and_then(Value::as_str);
            match state {
                Some("done") => {
                    return value
                        .pointer("/data/resultUrl/jsonUrl")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .ok_or_else(|| {
                            OcrError::new(
                                "upstream_error",
                                "paddleOcr done response missing data.resultUrl.jsonUrl",
                                false,
                            )
                        });
                }
                Some("failed") => {
                    let message = value
                        .pointer("/data/errorMsg")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error");
                    return Err(OcrError::new(
                        "upstream_error",
                        format!("paddleOcr job failed: {message}"),
                        false,
                    ));
                }
                // pending / running / anything unrecognized: keep waiting.
                _ => tokio::time::sleep(POLL_INTERVAL).await,
            }
        }
    }
}

use std::time::Instant;

#[async_trait]
impl OcrProvider for PaddleOcrProvider {
    fn name(&self) -> &'static str {
        "paddleocr"
    }

    async fn recognize(
        &self,
        request: OcrRequest,
        options: OcrOptions,
    ) -> Result<OcrOutput, OcrError> {
        if request.bytes.is_empty() {
            return Err(OcrError::new("invalid_input", "OCR input is empty", false));
        }
        let deadline = Instant::now() + Duration::from_millis(self.config.timeout_ms.max(1_000));
        let page_ranges = format_page_ranges(&options.page_numbers);
        let job_id = self.submit_job(&request, &page_ranges).await?;
        let json_url = self.wait_for_result_url(&job_id, deadline).await?;

        // The result URL is pre-signed; authentication headers are not
        // required (and upstream rejects none either way).
        let body = self
            .client
            .get(&json_url)
            .send()
            .await
            .map_err(|e| {
                OcrError::new(
                    "transient",
                    format!("paddleOcr result fetch failed: {e}"),
                    true,
                )
            })?
            .error_for_status()
            .map_err(|e| {
                OcrError::new(
                    "upstream_error",
                    format!("paddleOcr result fetch: {e}"),
                    true,
                )
            })?
            .text()
            .await
            .map_err(|e| {
                OcrError::new(
                    "upstream_error",
                    format!("paddleOcr result read failed: {e}"),
                    true,
                )
            })?;
        parse_jsonl_markdown(&body)
    }
}

fn is_reserved_payload_key(key: &str) -> bool {
    matches!(key, "file" | "fileUrl" | "model" | "lang" | "pageRanges")
}

/// Models whose job results carry `result.layoutParsingResults` markdown —
/// the only shape [`parse_jsonl_markdown`] understands. PP-OCRv5 returns
/// `result.ocrResults` (result images, no markdown) and cannot feed the
/// markdown contract.
fn is_supported_model(model: &str) -> bool {
    let model = model.trim();
    model.starts_with("PaddleOCR-VL") || model.starts_with("PP-StructureV3")
}

/// Collapse 1-indexed page numbers into the upstream `pageRanges` syntax:
/// sorted, deduped, consecutive runs joined (`[6, 2, 3, 4, 2]` → `"2-4,6"`).
/// Empty input formats to the empty string (the field is then omitted).
fn format_page_ranges(pages: &[u32]) -> String {
    let mut sorted = pages.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut out = String::new();
    let mut run_start = 0;
    for idx in 0..sorted.len() {
        let run_ends = idx + 1 == sorted.len() || sorted[idx + 1] != sorted[idx] + 1;
        if !run_ends {
            continue;
        }
        if !out.is_empty() {
            out.push(',');
        }
        if idx == run_start {
            out.push_str(&sorted[idx].to_string());
        } else {
            out.push_str(&format!("{}-{}", sorted[run_start], sorted[idx]));
        }
        run_start = idx + 1;
    }
    out
}

fn mime_for(file_name: &str) -> &'static str {
    let ext = file_name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        _ => "application/octet-stream",
    }
}

/// Parse the job's JSONL result: one object per line, each carrying
/// `result.layoutParsingResults[].markdown.text` for that page.
fn parse_jsonl_markdown(body: &str) -> Result<OcrOutput, OcrError> {
    let mut pages: Vec<String> = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line).map_err(|e| {
            OcrError::new(
                "upstream_error",
                format!("paddleOcr result line is not JSON: {e}"),
                false,
            )
        })?;
        let results = value
            .pointer("/result/layoutParsingResults")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                OcrError::new(
                    "upstream_error",
                    "paddleOcr result missing result.layoutParsingResults",
                    false,
                )
            })?;
        for result in results {
            if let Some(text) = result
                .pointer("/markdown/text")
                .and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty())
            {
                pages.push(text.trim().to_owned());
            }
        }
    }
    if pages.is_empty() {
        return Err(OcrError::new(
            "upstream_error",
            "paddleOcr result contained no recognized text",
            false,
        ));
    }
    Ok(OcrOutput {
        markdown: format!("{}\n", pages.join("\n\n")),
        confidence: None,
        provider: "paddleocr".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multi_page_jsonl() {
        let body = concat!(
            "{\"result\":{\"layoutParsingResults\":[{\"markdown\":{\"text\":\"# 第一页\\n\"}}]}}\n",
            "{\"result\":{\"layoutParsingResults\":[{\"markdown\":{\"text\":\"第二页\"}}]}}\n",
            "\n"
        );
        let output = parse_jsonl_markdown(body).unwrap();
        assert_eq!(output.markdown, "# 第一页\n\n第二页\n");
        assert_eq!(output.provider, "paddleocr");
    }

    #[test]
    fn rejects_result_without_text() {
        let body = "{\"result\":{\"layoutParsingResults\":[]}}\n";
        assert!(parse_jsonl_markdown(body).is_err());
    }

    #[test]
    fn config_requires_endpoint() {
        let error = PaddleOcrConfig::from_value(serde_json::json!({})).unwrap_err();
        assert_eq!(error.code, "invalid_config");
    }

    #[test]
    fn config_rejects_reserved_payload_keys() {
        let error = PaddleOcrConfig::from_value(serde_json::json!({
            "endpoint": "https://example.test/jobs",
            "optionalPayload": {"model": "other"}
        }))
        .unwrap_err();
        assert!(error.message.contains("reserved"));
        let error = PaddleOcrConfig::from_value(serde_json::json!({
            "endpoint": "https://example.test/jobs",
            "optionalPayload": {"pageRanges": "1-2"}
        }))
        .unwrap_err();
        assert!(error.message.contains("reserved"));
    }

    #[test]
    fn config_rejects_markdown_incapable_models() {
        for model in ["PP-OCRv5", "PaddleOCR-X", "paddleocr-vl-1.6"] {
            let error = PaddleOcrConfig::from_value(serde_json::json!({
                "endpoint": "https://example.test/jobs",
                "model": model
            }))
            .unwrap_err();
            assert_eq!(
                error.code, "invalid_config",
                "model {model} should be rejected"
            );
        }
    }

    #[test]
    fn config_accepts_markdown_capable_models() {
        for model in [
            None,
            Some("PaddleOCR-VL"),
            Some("PaddleOCR-VL-1.6"),
            Some("PP-StructureV3"),
        ] {
            let mut config = serde_json::json!({"endpoint": "https://example.test/jobs"});
            if let Some(model) = model {
                config["model"] = Value::from(model);
            }
            PaddleOcrConfig::from_value(config).expect("markdown-capable model should validate");
        }
    }

    #[test]
    fn page_ranges_formatting() {
        assert_eq!(format_page_ranges(&[]), "");
        assert_eq!(format_page_ranges(&[1]), "1");
        assert_eq!(format_page_ranges(&[2, 3, 4, 6]), "2-4,6");
        // Unsorted + duplicated input collapses the same way.
        assert_eq!(format_page_ranges(&[6, 2, 3, 4, 2]), "2-4,6");
        assert_eq!(format_page_ranges(&[1, 2, 3, 5, 6, 7, 9]), "1-3,5-7,9");
    }
}
