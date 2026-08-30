// SPDX-License-Identifier: MIT

use std::sync::Arc;

use napi::bindgen_prelude::{Buffer, Error, Result, Status};
use napi_derive::napi;
use serde::Deserialize;
use vento_file_parser::{ConvertOptions, FileParser, PaddleOcrConfig, PaddleOcrProvider};

#[napi]
pub async fn convert_bytes(
    data: Buffer,
    file_name: String,
    options_json: Option<String>,
) -> Result<String> {
    let binding = options_json
        .as_deref()
        .map(serde_json::from_str::<BindingOptions>)
        .transpose()
        .map_err(invalid)?
        .unwrap_or_default();
    let mut parser = FileParser::new();
    if let Some(ocr) = binding.paddle_ocr {
        let provider = PaddleOcrProvider::new(ocr).map_err(provider_error)?;
        parser = parser.with_ocr(Arc::new(provider));
    }
    let result = parser
        .convert(
            vento_file_parser::DocumentInput::Bytes {
                data: data.to_vec(),
                file_name,
            },
            binding.convert,
        )
        .await
        .map_err(runtime_error)?;
    serde_json::to_string(&result).map_err(invalid)
}

#[napi]
pub fn supported_formats() -> Result<String> {
    let mapped: Vec<FormatEntry> = vento_file_parser::SUPPORTED_FORMATS
        .iter()
        .map(|(category, extensions)| FormatEntry {
            category: (*category).to_owned(),
            extensions: extensions.iter().map(|value| (*value).to_owned()).collect(),
            requires_provider: *category == "image",
        })
        .collect();
    serde_json::to_string(&mapped).map_err(invalid)
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BindingOptions {
    #[serde(default)]
    convert: ConvertOptions,
    #[serde(default)]
    paddle_ocr: Option<PaddleOcrConfig>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct FormatEntry {
    category: String,
    extensions: Vec<String>,
    requires_provider: bool,
}

fn invalid(error: impl std::fmt::Display) -> Error {
    Error::new(Status::InvalidArg, error.to_string())
}
fn provider_error(error: impl std::fmt::Display) -> Error {
    Error::new(Status::GenericFailure, error.to_string())
}
fn runtime_error(error: vento_file_parser::RuntimeError) -> Error {
    Error::new(Status::GenericFailure, format!("{}: {error}", error.code()))
}
