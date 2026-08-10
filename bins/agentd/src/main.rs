// SPDX-License-Identifier: MIT

use std::time::Instant;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use vento_agent_protocol::{AgentRequest, AgentResponse, PROTOCOL_VERSION, validate_guest_path};
use vento_runtime_types::{CommandResult, FileEntry, new_id, now_ms};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    prepare_directories().await?;
    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        let response = match serde_json::from_str::<AgentRequest>(&line) {
            Ok(request) => handle(request).await,
            Err(error) => AgentResponse::Error {
                code: "INVALID_REQUEST".into(),
                message: error.to_string(),
            },
        };
        stdout
            .write_all(serde_json::to_string(&response)?.as_bytes())
            .await?;
        stdout.write_all(b"\n").await?;
        stdout.flush().await?;
        if matches!(response, AgentResponse::Empty) && line.contains("shutdown") {
            break;
        }
    }
    Ok(())
}

async fn prepare_directories() -> std::io::Result<()> {
    for path in ["/workspace", "/knowledge", "/tmp", "/home"] {
        tokio::fs::create_dir_all(path).await?;
    }
    Ok(())
}

async fn handle(request: AgentRequest) -> AgentResponse {
    match request {
        AgentRequest::Ready => AgentResponse::Ready {
            version: PROTOCOL_VERSION,
        },
        AgentRequest::Run(request) => run(request).await,
        AgentRequest::ReadFile { path, max_bytes } => match read_file(&path, max_bytes).await {
            Ok(data) => AgentResponse::Bytes(data),
            Err(error) => agent_error(error),
        },
        AgentRequest::WriteFile {
            path,
            data,
            mode: _,
        } => {
            if let Err(error) = validate_guest_path(&path, true).map_err(std::io::Error::other) {
                return agent_error(error);
            }
            match tokio::fs::write(path, data).await {
                Ok(()) => AgentResponse::Empty,
                Err(error) => agent_error(error),
            }
        }
        AgentRequest::ListDir { path } => match list_dir(&path).await {
            Ok(entries) => AgentResponse::Entries(entries),
            Err(error) => agent_error(error),
        },
        AgentRequest::Stat { path } => match stat(&path).await {
            Ok(entry) => AgentResponse::Entry(entry),
            Err(error) => agent_error(error),
        },
        AgentRequest::Mkdir { path, recursive: _ } => {
            if let Err(message) = validate_guest_path(&path, true) {
                return AgentResponse::Error {
                    code: "ACCESS_DENIED".into(),
                    message: message.into(),
                };
            }
            match tokio::fs::create_dir_all(path).await {
                Ok(()) => AgentResponse::Empty,
                Err(error) => agent_error(error),
            }
        }
        AgentRequest::Remove { path, recursive } => {
            if let Err(message) = validate_guest_path(&path, true) {
                return AgentResponse::Error {
                    code: "ACCESS_DENIED".into(),
                    message: message.into(),
                };
            }
            let result = if recursive {
                tokio::fs::remove_dir_all(path).await
            } else {
                tokio::fs::remove_file(path).await
            };
            match result {
                Ok(()) => AgentResponse::Empty,
                Err(error) => agent_error(error),
            }
        }
        AgentRequest::Kill { .. } => AgentResponse::Error {
            code: "NOT_IMPLEMENTED".into(),
            message: "process registry is not enabled".into(),
        },
        AgentRequest::Shutdown => AgentResponse::Empty,
    }
}

async fn run(request: vento_runtime_types::CommandRequest) -> AgentResponse {
    if request.command.is_empty() {
        return AgentResponse::Error {
            code: "INVALID_COMMAND".into(),
            message: "command cannot be empty".into(),
        };
    }
    if let Err(message) = validate_guest_path(&request.cwd, false) {
        return AgentResponse::Error {
            code: "ACCESS_DENIED".into(),
            message: message.into(),
        };
    }
    let started = Instant::now();
    let mut command = tokio::process::Command::new(&request.command[0]);
    command
        .args(&request.command[1..])
        .current_dir(request.cwd)
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", "/home")
        .envs(request.env)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let outcome = tokio::time::timeout(
        std::time::Duration::from_millis(request.timeout_ms),
        command.output(),
    )
    .await;
    let (exit_code, stdout, stderr, timed_out) = match outcome {
        Ok(Ok(output)) => (
            output.status.code(),
            truncate(output.stdout),
            truncate(output.stderr),
            false,
        ),
        Ok(Err(error)) => (None, Vec::new(), error.to_string().into_bytes(), false),
        Err(_) => (None, Vec::new(), b"command timed out".to_vec(), true),
    };
    AgentResponse::Command(CommandResult {
        command_id: new_id("cmd"),
        exit_code,
        stdout,
        stderr,
        duration_ms: started.elapsed().as_millis() as u64,
        timed_out,
    })
}

fn truncate(mut bytes: Vec<u8>) -> Vec<u8> {
    bytes.truncate(1024 * 1024);
    bytes
}
async fn read_file(path: &str, max_bytes: u64) -> std::io::Result<Vec<u8>> {
    validate_guest_path(path, false).map_err(std::io::Error::other)?;
    let metadata = tokio::fs::metadata(path).await?;
    if metadata.len() > max_bytes {
        return Err(std::io::Error::other("file exceeds read limit"));
    }
    tokio::fs::read(path).await
}
async fn stat(path: &str) -> std::io::Result<FileEntry> {
    validate_guest_path(path, false).map_err(std::io::Error::other)?;
    let metadata = tokio::fs::metadata(path).await?;
    Ok(FileEntry {
        path: path.into(),
        kind: if metadata.is_dir() {
            "directory"
        } else {
            "file"
        }
        .into(),
        size: metadata.len(),
        modified_at_ms: now_ms(),
    })
}
async fn list_dir(path: &str) -> std::io::Result<Vec<FileEntry>> {
    validate_guest_path(path, false).map_err(std::io::Error::other)?;
    let mut reader = tokio::fs::read_dir(path).await?;
    let mut entries = Vec::new();
    while let Some(entry) = reader.next_entry().await? {
        entries.push(stat(&entry.path().to_string_lossy()).await?);
    }
    Ok(entries)
}
fn agent_error(error: std::io::Error) -> AgentResponse {
    AgentResponse::Error {
        code: "IO_ERROR".into(),
        message: error.to_string(),
    }
}
