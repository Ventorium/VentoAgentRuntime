// SPDX-License-Identifier: MIT

use std::sync::Arc;

use napi::bindgen_prelude::{Buffer, Error, Result, Status};
use napi_derive::napi;
use vento_document_providers::{PaddleOcrConfig, PaddleOcrProvider};
use vento_document_runtime::{
    ConvertOptions, DocumentInput, DocumentRuntime, get_supported_formats,
};

#[napi]
pub async fn convert_bytes(
    data: Buffer,
    file_name: String,
    options_json: Option<String>,
) -> Result<String> {
    let options = options_json
        .as_deref()
        .map(serde_json::from_str::<BindingOptions>)
        .transpose()
        .map_err(invalid)?
        .unwrap_or_default();
    let mut runtime = DocumentRuntime::new();
    if let Some(config) = options.paddle_ocr {
        runtime = runtime.with_ocr(Arc::new(
            PaddleOcrProvider::new(config).map_err(provider_error)?,
        ));
    }
    let result = runtime
        .convert(
            DocumentInput::Bytes {
                data: data.to_vec(),
                file_name,
            },
            options.convert,
        )
        .await
        .map_err(runtime_error)?;
    serde_json::to_string(&result).map_err(invalid)
}

#[napi]
pub fn supported_formats() -> Result<String> {
    serde_json::to_string(&get_supported_formats()).map_err(invalid)
}

#[derive(Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BindingOptions {
    #[serde(default)]
    convert: ConvertOptions,
    paddle_ocr: Option<PaddleOcrConfig>,
}

fn invalid(error: impl std::fmt::Display) -> Error {
    Error::new(Status::InvalidArg, error.to_string())
}
fn provider_error(error: impl std::fmt::Display) -> Error {
    Error::new(Status::GenericFailure, error.to_string())
}
fn runtime_error(error: vento_document_runtime::RuntimeError) -> Error {
    Error::new(Status::GenericFailure, format!("{}: {error}", error.code()))
}
