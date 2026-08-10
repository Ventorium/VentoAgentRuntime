// SPDX-License-Identifier: MIT

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use vento_runtime_types::{CommandRequest, CommandResult, CreateSandboxRequest, FileEntry};
use vento_vm_runtime::{BackendSnapshot, RuntimeError, SandboxBackend, SandboxBackendFactory};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FirecrackerConfig {
    pub firecracker_binary: PathBuf,
    pub jailer_binary: Option<PathBuf>,
    pub kernel_image: PathBuf,
    pub base_rootfs: PathBuf,
    pub data_dir: PathBuf,
    #[serde(default = "default_agent_port")]
    pub agent_vsock_port: u32,
    #[serde(default = "default_boot_timeout")]
    pub boot_timeout_ms: u64,
}

fn default_agent_port() -> u32 {
    10_000
}
fn default_boot_timeout() -> u64 {
    5_000
}

#[derive(Clone, Debug)]
pub struct FirecrackerFactory {
    config: FirecrackerConfig,
}

impl FirecrackerFactory {
    pub fn new(config: FirecrackerConfig) -> Self {
        Self { config }
    }

    pub async fn preflight(&self) -> Result<(), RuntimeError> {
        if !cfg!(target_os = "linux") {
            return Err(RuntimeError::Backend(
                "local Firecracker runtime requires Linux".into(),
            ));
        }
        for (name, path) in [
            ("firecracker", self.config.firecracker_binary.as_path()),
            ("kernel", self.config.kernel_image.as_path()),
            ("base rootfs", self.config.base_rootfs.as_path()),
        ] {
            if !tokio::fs::try_exists(path).await.map_err(backend_error)? {
                return Err(RuntimeError::Backend(format!(
                    "{name} does not exist: {}",
                    path.display()
                )));
            }
        }
        if !tokio::fs::try_exists("/dev/kvm")
            .await
            .map_err(backend_error)?
        {
            return Err(RuntimeError::Backend("/dev/kvm is unavailable".into()));
        }
        tokio::fs::create_dir_all(&self.config.data_dir)
            .await
            .map_err(backend_error)?;
        probe_reflink(&self.config.data_dir).await
    }
}

#[async_trait]
impl SandboxBackendFactory for FirecrackerFactory {
    async fn create(
        &self,
        sandbox_id: &str,
        request: &CreateSandboxRequest,
    ) -> Result<Box<dyn SandboxBackend>, RuntimeError> {
        self.preflight().await?;
        let sandbox_dir = self.config.data_dir.join("sandboxes").join(sandbox_id);
        tokio::fs::create_dir_all(&sandbox_dir)
            .await
            .map_err(backend_error)?;
        let rootfs = sandbox_dir.join("rootfs.ext4");
        reflink_clone(&self.config.base_rootfs, &rootfs).await?;
        let socket = sandbox_dir.join("firecracker.sock");
        let vsock = sandbox_dir.join("agent.vsock");
        Ok(Box::new(FirecrackerBackend {
            config: self.config.clone(),
            request: request.clone(),
            sandbox_dir,
            rootfs,
            socket,
            vsock,
            child: None,
            paused_snapshot: None,
            running: false,
        }))
    }
}

struct FirecrackerBackend {
    config: FirecrackerConfig,
    request: CreateSandboxRequest,
    sandbox_dir: PathBuf,
    rootfs: PathBuf,
    socket: PathBuf,
    vsock: PathBuf,
    child: Option<Child>,
    paused_snapshot: Option<SnapshotPaths>,
    running: bool,
}

impl std::fmt::Debug for FirecrackerBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FirecrackerBackend")
            .field("sandbox_dir", &self.sandbox_dir)
            .field("running", &self.running)
            .finish()
    }
}

#[derive(Clone, Debug)]
struct SnapshotPaths {
    state: PathBuf,
    memory: PathBuf,
    rootfs: PathBuf,
}

#[async_trait]
impl SandboxBackend for FirecrackerBackend {
    async fn start(&mut self) -> Result<(), RuntimeError> {
        if self.running {
            return Ok(());
        }
        self.spawn().await?;
        if let Some(snapshot) = self.paused_snapshot.clone() {
            self.load_snapshot(&snapshot).await?;
        } else {
            self.configure_fresh().await?;
        }
        self.running = true;
        Ok(())
    }

    async fn pause(&mut self) -> Result<(), RuntimeError> {
        if !self.running {
            return Ok(());
        }
        let directory = self.sandbox_dir.join("paused");
        let snapshot = self.capture_to(&directory).await?;
        self.terminate().await?;
        self.paused_snapshot = Some(snapshot);
        self.running = false;
        Ok(())
    }

    async fn resume(&mut self) -> Result<(), RuntimeError> {
        self.start().await
    }

    async fn stop(&mut self) -> Result<(), RuntimeError> {
        self.terminate().await?;
        self.running = false;
        self.paused_snapshot = None;
        Ok(())
    }

    async fn destroy(&mut self) -> Result<(), RuntimeError> {
        self.terminate().await?;
        tokio::fs::remove_dir_all(&self.sandbox_dir)
            .await
            .map_err(backend_error)?;
        self.running = false;
        Ok(())
    }

    async fn run_command(
        &mut self,
        _request: CommandRequest,
    ) -> Result<CommandResult, RuntimeError> {
        Err(RuntimeError::Backend(format!(
            "agentd transport is not ready on {}:{}",
            self.vsock.display(),
            self.config.agent_vsock_port
        )))
    }
    async fn kill_command(&mut self, _command_id: &str) -> Result<(), RuntimeError> {
        Err(RuntimeError::Backend(
            "agentd transport is not ready".into(),
        ))
    }
    async fn read_file(&self, _path: &str, _max_bytes: u64) -> Result<Vec<u8>, RuntimeError> {
        Err(RuntimeError::Backend(
            "agentd transport is not ready".into(),
        ))
    }
    async fn write_file(&mut self, _path: &str, _data: &[u8]) -> Result<(), RuntimeError> {
        Err(RuntimeError::Backend(
            "agentd transport is not ready".into(),
        ))
    }
    async fn list_dir(&self, _path: &str) -> Result<Vec<FileEntry>, RuntimeError> {
        Err(RuntimeError::Backend(
            "agentd transport is not ready".into(),
        ))
    }
    async fn remove(&mut self, _path: &str, _recursive: bool) -> Result<(), RuntimeError> {
        Err(RuntimeError::Backend(
            "agentd transport is not ready".into(),
        ))
    }

    async fn snapshot(&mut self, snapshot_id: &str) -> Result<BackendSnapshot, RuntimeError> {
        if !self.running {
            return Err(RuntimeError::Conflict(
                "persistent snapshot requires a running sandbox".into(),
            ));
        }
        let directory = self.config.data_dir.join("snapshots").join(snapshot_id);
        let snapshot = self.capture_to(&directory).await?;
        self.load_snapshot(&snapshot).await?;
        let size_bytes = file_size(&snapshot.state).await?
            + file_size(&snapshot.memory).await?
            + file_size(&snapshot.rootfs).await?;
        Ok(BackendSnapshot { size_bytes })
    }
}

impl FirecrackerBackend {
    async fn spawn(&mut self) -> Result<(), RuntimeError> {
        let _ = tokio::fs::remove_file(&self.socket).await;
        let child = Command::new(&self.config.firecracker_binary)
            .arg("--api-sock")
            .arg(&self.socket)
            .kill_on_drop(true)
            .spawn()
            .map_err(backend_error)?;
        self.child = Some(child);
        let deadline = Instant::now() + Duration::from_millis(self.config.boot_timeout_ms);
        while Instant::now() < deadline {
            if tokio::fs::try_exists(&self.socket)
                .await
                .map_err(backend_error)?
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.terminate().await?;
        Err(RuntimeError::Backend(
            "Firecracker API socket did not become ready".into(),
        ))
    }

    async fn configure_fresh(&self) -> Result<(), RuntimeError> {
        firecracker_put(
            &self.socket,
            "/machine-config",
            &serde_json::json!({
                "vcpu_count": self.request.resources.cpu_count,
                "mem_size_mib": self.request.resources.memory_mb,
                "smt": false,
            }),
        )
        .await?;
        firecracker_put(
            &self.socket,
            "/boot-source",
            &serde_json::json!({
                "kernel_image_path": self.config.kernel_image,
                "boot_args": "console=ttyS0 reboot=k panic=1 pci=off init=/agentd",
            }),
        )
        .await?;
        firecracker_put(
            &self.socket,
            "/drives/rootfs",
            &serde_json::json!({
                "drive_id": "rootfs", "path_on_host": self.rootfs,
                "is_root_device": true, "is_read_only": false,
            }),
        )
        .await?;
        firecracker_put(
            &self.socket,
            "/vsock",
            &serde_json::json!({
                "guest_cid": guest_cid(&self.sandbox_dir), "uds_path": self.vsock,
            }),
        )
        .await?;
        firecracker_put(
            &self.socket,
            "/actions",
            &serde_json::json!({"action_type":"InstanceStart"}),
        )
        .await
    }

    async fn capture_to(&self, directory: &Path) -> Result<SnapshotPaths, RuntimeError> {
        tokio::fs::create_dir_all(directory)
            .await
            .map_err(backend_error)?;
        firecracker_patch(&self.socket, "/vm", &serde_json::json!({"state":"Paused"})).await?;
        let state = directory.join("vmstate.bin");
        let memory = directory.join("memory.bin");
        let rootfs = directory.join("rootfs.ext4");
        reflink_clone(&self.rootfs, &rootfs).await?;
        firecracker_put(
            &self.socket,
            "/snapshot/create",
            &serde_json::json!({
                "snapshot_type": "Full", "snapshot_path": state, "mem_file_path": memory,
            }),
        )
        .await?;
        Ok(SnapshotPaths {
            state,
            memory,
            rootfs,
        })
    }

    async fn load_snapshot(&mut self, snapshot: &SnapshotPaths) -> Result<(), RuntimeError> {
        reflink_clone(&snapshot.rootfs, &self.rootfs).await?;
        firecracker_put(
            &self.socket,
            "/snapshot/load",
            &serde_json::json!({
                "snapshot_path": snapshot.state,
                "mem_backend": {"backend_type":"File", "backend_path": snapshot.memory},
                "enable_diff_snapshots": true,
                "resume_vm": true,
            }),
        )
        .await
    }

    async fn terminate(&mut self) -> Result<(), RuntimeError> {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        let _ = tokio::fs::remove_file(&self.socket).await;
        Ok(())
    }
}

async fn firecracker_put(
    socket: &Path,
    path: &str,
    value: &serde_json::Value,
) -> Result<(), RuntimeError> {
    firecracker_request(socket, "PUT", path, value).await
}
async fn firecracker_patch(
    socket: &Path,
    path: &str,
    value: &serde_json::Value,
) -> Result<(), RuntimeError> {
    firecracker_request(socket, "PATCH", path, value).await
}
async fn firecracker_request(
    socket: &Path,
    method: &str,
    path: &str,
    value: &serde_json::Value,
) -> Result<(), RuntimeError> {
    let body =
        serde_json::to_vec(value).map_err(|error| RuntimeError::Backend(error.to_string()))?;
    let mut stream = tokio::net::UnixStream::connect(socket)
        .await
        .map_err(backend_error)?;
    let headers = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(headers.as_bytes())
        .await
        .map_err(backend_error)?;
    stream.write_all(&body).await.map_err(backend_error)?;
    stream.shutdown().await.map_err(backend_error)?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .map_err(backend_error)?;
    let status = String::from_utf8_lossy(&response)
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned();
    if status.contains(" 200 ") || status.contains(" 204 ") {
        Ok(())
    } else {
        Err(RuntimeError::Backend(format!(
            "Firecracker {method} {path} failed: {status}"
        )))
    }
}

async fn probe_reflink(directory: &Path) -> Result<(), RuntimeError> {
    let source = directory.join(".reflink-source");
    let target = directory.join(".reflink-target");
    tokio::fs::write(&source, b"vento-reflink-probe")
        .await
        .map_err(backend_error)?;
    let result = reflink_clone(&source, &target).await;
    let _ = tokio::fs::remove_file(source).await;
    let _ = tokio::fs::remove_file(target).await;
    result.map_err(|_| {
        RuntimeError::Backend("data directory must be reflink-capable XFS or Btrfs".into())
    })
}

async fn reflink_clone(source: &Path, destination: &Path) -> Result<(), RuntimeError> {
    let _ = tokio::fs::remove_file(destination).await;
    let output = Command::new("cp")
        .arg("--reflink=always")
        .arg("--sparse=always")
        .arg("--")
        .arg(source)
        .arg(destination)
        .output()
        .await
        .map_err(backend_error)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(RuntimeError::Backend(format!(
            "reflink clone failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

async fn file_size(path: &Path) -> Result<u64, RuntimeError> {
    Ok(tokio::fs::metadata(path)
        .await
        .map_err(backend_error)?
        .len())
}
fn backend_error(error: std::io::Error) -> RuntimeError {
    RuntimeError::Backend(error.to_string())
}
fn guest_cid(path: &Path) -> u32 {
    let hash = path.to_string_lossy().bytes().fold(0_u32, |value, byte| {
        value.wrapping_mul(31).wrapping_add(u32::from(byte))
    });
    3 + hash % (u32::MAX - 3)
}

#[allow(dead_code)]
fn default_private_denies() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("127.0.0.0/8", "loopback"),
        ("10.0.0.0/8", "private"),
        ("172.16.0.0/12", "private"),
        ("192.168.0.0/16", "private"),
        ("169.254.0.0/16", "link-local and metadata"),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guest_cid_is_not_reserved() {
        assert!(guest_cid(Path::new("sandbox")) >= 3);
    }
}
