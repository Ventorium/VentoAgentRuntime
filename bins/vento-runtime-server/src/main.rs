// SPDX-License-Identifier: MIT

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use serde::Deserialize;
use serde_json::{Value, json};
use vento_firecracker_runtime::{FirecrackerConfig, FirecrackerFactory};
use vento_runtime_types::{CommandRequest, CreateSandboxRequest, ErrorBody, new_id};
use vento_vm_runtime::{InMemoryBackendFactory, RuntimeError, SandboxBackendFactory, VmRuntime};

#[derive(Debug, Parser)]
struct Cli {
    #[arg(long, default_value = "127.0.0.1:8088")]
    listen: String,
    #[arg(long, env = "VENTO_RUNTIME_TOKEN")]
    token: String,
    #[arg(long)]
    firecracker_config: Option<std::path::PathBuf>,
}

#[derive(Clone, Debug)]
struct AppState {
    runtime: VmRuntime,
    token: Arc<str>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    if cli.token.len() < 24 {
        return Err("runtime token must contain at least 24 characters".into());
    }
    let factory: Arc<dyn SandboxBackendFactory> = if let Some(path) = cli.firecracker_config {
        let config: FirecrackerConfig = serde_json::from_slice(&tokio::fs::read(path).await?)?;
        Arc::new(FirecrackerFactory::new(config))
    } else {
        Arc::new(InMemoryBackendFactory)
    };
    let state = AppState {
        runtime: VmRuntime::new(factory),
        token: Arc::from(cli.token),
    };
    let app = Router::new()
        .route("/health", get(health))
        .route("/sandboxes", post(create_sandbox).get(list_sandboxes))
        .route("/sandboxes/{id}", get(get_sandbox).delete(destroy_sandbox))
        .route("/sandboxes/{id}/start", post(start_sandbox))
        .route("/sandboxes/{id}/pause", post(pause_sandbox))
        .route("/sandboxes/{id}/resume", post(resume_sandbox))
        .route("/sandboxes/{id}/stop", post(stop_sandbox))
        .route("/sandboxes/{id}/commands", post(run_command))
        .route(
            "/sandboxes/{id}/commands/{command_id}/kill",
            post(kill_command),
        )
        .route("/sandboxes/{id}/files", get(list_files).delete(remove_file))
        .route(
            "/sandboxes/{id}/files/content",
            get(read_file).put(write_file),
        )
        .route("/sandboxes/{id}/snapshots", post(create_snapshot))
        .route("/snapshots/{id}", get(get_snapshot).delete(delete_snapshot))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(&cli.listen).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
async fn health() -> StatusCode {
    StatusCode::NO_CONTENT
}

fn authorize(headers: &HeaderMap, state: &AppState) -> Result<(), ApiError> {
    let supplied = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if supplied == Some(state.token.as_ref()) {
        Ok(())
    } else {
        Err(ApiError::unauthorized())
    }
}

async fn create_sandbox(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateSandboxRequest>,
) -> Result<impl IntoResponse, ApiError> {
    authorize(&headers, &state)?;
    let idempotency = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok());
    let info = state.runtime.create(request, idempotency).await?;
    Ok((StatusCode::CREATED, Json(info)))
}
async fn list_sandboxes(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers, &state)?;
    Ok(Json(json!(state.runtime.list().await)))
}
async fn get_sandbox(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers, &state)?;
    Ok(Json(json!(state.runtime.get(&id).await?)))
}
async fn destroy_sandbox(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    authorize(&headers, &state)?;
    state.runtime.destroy(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

macro_rules! lifecycle_handler {
    ($name:ident, $method:ident) => {
        async fn $name(
            State(state): State<AppState>,
            headers: HeaderMap,
            Path(id): Path<String>,
        ) -> Result<Json<Value>, ApiError> {
            authorize(&headers, &state)?;
            Ok(Json(json!(state.runtime.$method(&id).await?)))
        }
    };
}
lifecycle_handler!(start_sandbox, start);
lifecycle_handler!(pause_sandbox, pause);
lifecycle_handler!(resume_sandbox, resume);
lifecycle_handler!(stop_sandbox, stop);

async fn run_command(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<CommandRequest>,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers, &state)?;
    Ok(Json(json!(state.runtime.run_command(&id, request).await?)))
}
async fn kill_command(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, command_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    authorize(&headers, &state)?;
    state.runtime.kill_command(&id, &command_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct FileQuery {
    path: String,
    #[serde(default)]
    recursive: bool,
    #[serde(default = "default_read_limit")]
    max_bytes: u64,
}
fn default_read_limit() -> u64 {
    16 * 1024 * 1024
}

async fn list_files(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<FileQuery>,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers, &state)?;
    Ok(Json(json!(state.runtime.list_dir(&id, &query.path).await?)))
}
async fn read_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<FileQuery>,
) -> Result<Bytes, ApiError> {
    authorize(&headers, &state)?;
    Ok(Bytes::from(
        state
            .runtime
            .read_file(&id, &query.path, query.max_bytes)
            .await?,
    ))
}
async fn write_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<FileQuery>,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    authorize(&headers, &state)?;
    state.runtime.write_file(&id, &query.path, &body).await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn remove_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<FileQuery>,
) -> Result<StatusCode, ApiError> {
    authorize(&headers, &state)?;
    state
        .runtime
        .remove(&id, &query.path, query.recursive)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn create_snapshot(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    authorize(&headers, &state)?;
    Ok((
        StatusCode::CREATED,
        Json(state.runtime.create_snapshot(&id).await?),
    ))
}
async fn get_snapshot(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers, &state)?;
    Ok(Json(json!(state.runtime.get_snapshot(&id).await?)))
}
async fn delete_snapshot(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    authorize(&headers, &state)?;
    state.runtime.delete_snapshot(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    body: ErrorBody,
}
impl ApiError {
    fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            body: ErrorBody {
                code: "UNAUTHORIZED".into(),
                message: "authentication required".into(),
                request_id: Some(new_id("req")),
            },
        }
    }
}
impl From<RuntimeError> for ApiError {
    fn from(error: RuntimeError) -> Self {
        let status = match error {
            RuntimeError::NotFound => StatusCode::NOT_FOUND,
            RuntimeError::Conflict(_) | RuntimeError::SecretsCannotBeSnapshotted => {
                StatusCode::CONFLICT
            }
            RuntimeError::Invalid(_) => StatusCode::BAD_REQUEST,
            RuntimeError::Backend(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self {
            status,
            body: ErrorBody {
                code: error.code().into(),
                message: error.to_string(),
                request_id: Some(new_id("req")),
            },
        }
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}
