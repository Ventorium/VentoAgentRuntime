// SPDX-License-Identifier: MIT

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, RwLock};
use std::time::Instant;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use vento_agent_protocol::{AgentRequest, AgentResponse, PROTOCOL_VERSION, validate_guest_path};
use vento_runtime_types::{CommandResult, FileEntry, now_ms};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    prepare_directories().await?;
    if std::env::var_os("VENTO_AGENTD_SKIP_PREPARE").is_none() && cfg!(target_os = "linux") {
        return serve_vsock().await;
    }
    serve_stdio().await
}

async fn serve_stdio() -> Result<(), Box<dyn std::error::Error>> {
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

async fn serve_vsock() -> Result<(), Box<dyn std::error::Error>> {
    const PORT: u32 = 10_000;
    let listener = vsock::VsockListener::bind_with_cid_port(vsock::VMADDR_CID_ANY, PORT)?;
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        loop {
            let (mut stream, _) = listener.accept().map_err(std::io::Error::other)?;
            let runtime = runtime.clone();
            std::thread::spawn(move || {
                let reader_stream = match stream.try_clone() {
                    Ok(value) => value,
                    Err(_) => return,
                };
                let mut reader = std::io::BufReader::new(reader_stream);
                let mut line = String::new();
                if reader.read_line(&mut line).is_err()
                    || line.len() > vento_agent_protocol::MAX_FRAME_BYTES
                {
                    return;
                }
                let response = match serde_json::from_str::<AgentRequest>(line.trim_end()) {
                    Ok(request) => runtime.block_on(handle(request)),
                    Err(error) => AgentResponse::Error {
                        code: "INVALID_REQUEST".into(),
                        message: error.to_string(),
                    },
                };
                if let Ok(mut bytes) = serde_json::to_vec(&response) {
                    bytes.push(b'\n');
                    let _ = stream.write_all(&bytes);
                }
            });
        }
    })
    .await??;
    Ok(())
}

async fn prepare_directories() -> std::io::Result<()> {
    #[cfg(debug_assertions)]
    if std::env::var_os("VENTO_AGENTD_SKIP_PREPARE").is_some() {
        return Ok(());
    }
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
        AgentRequest::Configure { env, max_processes } => {
            #[cfg(target_os = "linux")]
            if let Err(error) = nix::sys::resource::setrlimit(
                nix::sys::resource::Resource::RLIMIT_NPROC,
                max_processes.into(),
                max_processes.into(),
            ) {
                return AgentResponse::Error {
                    code: "RESOURCE_LIMIT".into(),
                    message: error.to_string(),
                };
            }
            #[cfg(not(target_os = "linux"))]
            let _ = max_processes;
            *BASE_ENV.write().expect("base env lock") = env;
            AgentResponse::Empty
        }
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
        AgentRequest::Sync => {
            nix::unistd::sync();
            AgentResponse::Empty
        }
        AgentRequest::Kill { command_id } => {
            let pid = RUNNING_COMMANDS.lock().await.get(&command_id).copied();
            match pid {
                Some(pid) => match nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(pid.cast_signed()),
                    nix::sys::signal::Signal::SIGKILL,
                ) {
                    Ok(()) => AgentResponse::Empty,
                    Err(error) => AgentResponse::Error {
                        code: "KILL_FAILED".into(),
                        message: error.to_string(),
                    },
                },
                None => AgentResponse::Error {
                    code: "COMMAND_NOT_FOUND".into(),
                    message: "command is not running".into(),
                },
            }
        }
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
    if request.stdin.is_some() {
        command.stdin(std::process::Stdio::piped());
    }
    let base_env = BASE_ENV.read().expect("base env lock").clone();
    command
        .args(&request.command[1..])
        .current_dir(request.cwd)
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", "/home")
        .envs(base_env)
        .envs(request.env)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let input = request.stdin;
    let command_id = next_command_id();
    let registry_id = command_id.clone();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_millis(request.timeout_ms),
        async {
            let mut child = command.spawn()?;
            if let Some(pid) = child.id() {
                RUNNING_COMMANDS.lock().await.insert(registry_id, pid);
            }
            if let Some(input) = input
                && let Some(mut stdin) = child.stdin.take()
            {
                stdin.write_all(&input).await?;
            }
            child.wait_with_output().await
        },
    )
    .await;
    RUNNING_COMMANDS.lock().await.remove(&command_id);
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
        command_id,
        exit_code,
        stdout,
        stderr,
        duration_ms: started.elapsed().as_millis() as u64,
        timed_out,
    })
}

static BASE_ENV: LazyLock<RwLock<BTreeMap<String, String>>> =
    LazyLock::new(|| RwLock::new(BTreeMap::new()));
static RUNNING_COMMANDS: LazyLock<tokio::sync::Mutex<HashMap<String, u32>>> =
    LazyLock::new(|| tokio::sync::Mutex::new(HashMap::new()));
static COMMAND_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn next_command_id() -> String {
    let sequence = COMMAND_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("cmd_{}_{sequence}", now_ms())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_ids_are_unique_without_guest_entropy() {
        let first = next_command_id();
        let second = next_command_id();
        assert_ne!(first, second);
        assert!(first.starts_with("cmd_"));
        assert!(second.starts_with("cmd_"));
    }
}
