// SPDX-License-Identifier: MIT

use napi::bindgen_prelude::{Buffer, Error, Result, Status};
use napi_derive::napi;

#[napi]
pub async fn request_runtime(
    base_url: String,
    token: String,
    method: String,
    path: String,
    body_json: Option<String>,
) -> Result<String> {
    if !path.starts_with('/') || path.contains("..") {
        return Err(invalid("path must be absolute without traversal"));
    }
    let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(invalid)?;
    let url = format!("{}{}", base_url.trim_end_matches('/'), path);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(generic)?;
    let mut request = client.request(method, url).bearer_auth(token);
    if let Some(body) = body_json {
        let value: serde_json::Value = serde_json::from_str(&body).map_err(invalid)?;
        request = request.json(&value);
    }
    let response = request.send().await.map_err(generic)?;
    let status = response.status();
    let body = response.text().await.map_err(generic)?;
    if !status.is_success() {
        return Err(Error::new(
            Status::GenericFailure,
            format!("runtime returned HTTP {status}: {body}"),
        ));
    }
    Ok(body)
}

#[napi]
pub async fn request_runtime_binary(
    base_url: String,
    token: String,
    method: String,
    path: String,
    body: Option<Buffer>,
) -> Result<Buffer> {
    if !path.starts_with('/') || path.contains("..") {
        return Err(invalid("path must be absolute without traversal"));
    }
    let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(invalid)?;
    let url = format!("{}{}", base_url.trim_end_matches('/'), path);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(generic)?;
    let mut request = client.request(method, url).bearer_auth(token);
    if let Some(body) = body {
        request = request
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(body.to_vec());
    }
    let response = request.send().await.map_err(generic)?;
    let status = response.status();
    let bytes = response.bytes().await.map_err(generic)?;
    if !status.is_success() {
        return Err(Error::new(
            Status::GenericFailure,
            format!(
                "runtime returned HTTP {status}: {}",
                String::from_utf8_lossy(&bytes)
            ),
        ));
    }
    Ok(bytes.to_vec().into())
}

#[napi]
pub fn start_local_runtime(
    binary_path: String,
    listen: String,
    token: String,
    config_path: Option<String>,
) -> Result<u32> {
    if !cfg!(target_os = "linux") {
        return Err(Error::new(
            Status::GenericFailure,
            "local Firecracker runtime requires Linux",
        ));
    }
    if token.len() < 24 {
        return Err(invalid("token must contain at least 24 characters"));
    }
    let mut command = std::process::Command::new(binary_path);
    command
        .arg("--listen")
        .arg(listen)
        .arg("--token")
        .arg(token)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(config) = config_path {
        command.arg("--firecracker-config").arg(config);
    }
    let child = command.spawn().map_err(generic)?;
    Ok(child.id())
}

fn invalid(error: impl std::fmt::Display) -> Error {
    Error::new(Status::InvalidArg, error.to_string())
}
fn generic(error: impl std::fmt::Display) -> Error {
    Error::new(Status::GenericFailure, error.to_string())
}
