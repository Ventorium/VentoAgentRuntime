// SPDX-License-Identifier: MIT

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use vento_runtime_types::{
    CommandRequest, CommandResult, CreateSandboxRequest, FileEntry, SandboxState,
};
use vento_vm_runtime::{BackendSnapshot, RuntimeError, SandboxBackend, SandboxBackendFactory};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FirecrackerConfig {
    pub firecracker_binary: PathBuf,
    pub jailer_binary: Option<PathBuf>,
    #[serde(default = "default_jailer_id")]
    pub jailer_uid: u32,
    #[serde(default = "default_jailer_id")]
    pub jailer_gid: u32,
    pub kernel_image: PathBuf,
    pub base_rootfs: PathBuf,
    #[serde(default)]
    pub template_rootfs: BTreeMap<String, PathBuf>,
    pub data_dir: PathBuf,
    #[serde(default = "default_agent_port")]
    pub agent_vsock_port: u32,
    #[serde(default = "default_boot_timeout")]
    pub boot_timeout_ms: u64,
}

fn default_jailer_id() -> u32 {
    1000
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
        if self.config.agent_vsock_port != default_agent_port() {
            return Err(RuntimeError::Invalid(format!(
                "agentVsockPort must be {} for the bundled agentd",
                default_agent_port()
            )));
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
        let Some(path) = &self.config.jailer_binary else {
            return Err(RuntimeError::Invalid(
                "jailerBinary is required for the Firecracker backend".into(),
            ));
        };
        if !tokio::fs::try_exists(path).await.map_err(backend_error)? {
            return Err(RuntimeError::Backend(format!(
                "jailer does not exist: {}",
                path.display()
            )));
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

    fn sandbox_dir(&self, sandbox_id: &str) -> Result<PathBuf, RuntimeError> {
        if self.config.jailer_binary.is_some() {
            let executable = self.config.firecracker_binary.file_name().ok_or_else(|| {
                RuntimeError::Invalid("firecracker binary has no file name".into())
            })?;
            Ok(self
                .config
                .data_dir
                .join("jailer")
                .join(executable)
                .join(jailer_id(sandbox_id))
                .join("root"))
        } else {
            Ok(self.config.data_dir.join("sandboxes").join(sandbox_id))
        }
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
        validate_supported_request(request)?;
        let jailed = self.config.jailer_binary.is_some();
        if request.snapshot_id.is_some() && !jailed {
            return Err(RuntimeError::Invalid(
                "snapshot restore requires jailer so device paths remain stable".into(),
            ));
        }
        let sandbox_dir = self.sandbox_dir(sandbox_id)?;
        tokio::fs::create_dir_all(&sandbox_dir)
            .await
            .map_err(backend_error)?;
        let source_rootfs = if request.template == "debian-slim" {
            &self.config.base_rootfs
        } else {
            self.config
                .template_rootfs
                .get(&request.template)
                .ok_or_else(|| {
                    RuntimeError::Invalid(format!("unknown template: {}", request.template))
                })?
        };
        let rootfs = sandbox_dir.join("rootfs.ext4");
        reflink_clone(source_rootfs, &rootfs).await?;
        resize_rootfs(&rootfs, request.resources.disk_mb).await?;
        let kernel = sandbox_dir.join("kernel.bin");
        reflink_clone(&self.config.kernel_image, &kernel).await?;
        if jailed {
            chown_for_jailer(&sandbox_dir, self.config.jailer_uid, self.config.jailer_gid)?;
            chown_for_jailer(&rootfs, self.config.jailer_uid, self.config.jailer_gid)?;
            chown_for_jailer(&kernel, self.config.jailer_uid, self.config.jailer_gid)?;
        }
        let socket = sandbox_dir.join("firecracker.sock");
        let vsock = sandbox_dir.join("agent.vsock");
        let paused_snapshot = if let Some(snapshot_id) = &request.snapshot_id {
            let source = self.config.data_dir.join("snapshots").join(snapshot_id);
            let restore = sandbox_dir.join("restore");
            tokio::fs::create_dir_all(&restore)
                .await
                .map_err(backend_error)?;
            let state = restore.join("vmstate.bin");
            let memory = restore.join("memory.bin");
            let snapshot_rootfs = restore.join("rootfs.ext4");
            for (name, target) in [
                ("vmstate.bin", &state),
                ("memory.bin", &memory),
                ("rootfs.ext4", &snapshot_rootfs),
            ] {
                reflink_clone(&source.join(name), target)
                    .await
                    .map_err(|_| {
                        RuntimeError::Invalid(format!(
                            "snapshot is unavailable or incomplete: {snapshot_id}"
                        ))
                    })?;
            }
            reflink_clone(&snapshot_rootfs, &rootfs).await?;
            if jailed {
                for path in [&restore, &state, &memory, &snapshot_rootfs, &rootfs] {
                    chown_for_jailer(path, self.config.jailer_uid, self.config.jailer_gid)?;
                }
            }
            Some(SnapshotPaths {
                state,
                memory,
                rootfs: snapshot_rootfs,
            })
        } else {
            None
        };
        Ok(Box::new(FirecrackerBackend {
            config: self.config.clone(),
            request: request.clone(),
            sandbox_dir,
            rootfs,
            kernel,
            socket,
            vsock,
            child: None,
            paused_snapshot,
            running: false,
            jailed,
        }))
    }

    async fn recover(
        &self,
        sandbox_id: &str,
        request: &CreateSandboxRequest,
        state: SandboxState,
    ) -> Result<Box<dyn SandboxBackend>, RuntimeError> {
        self.preflight().await?;
        validate_supported_request(request)?;
        let sandbox_dir = self.sandbox_dir(sandbox_id)?;
        let rootfs = sandbox_dir.join("rootfs.ext4");
        let kernel = sandbox_dir.join("kernel.bin");
        for (name, path) in [("sandbox rootfs", &rootfs), ("sandbox kernel", &kernel)] {
            if !tokio::fs::try_exists(path).await.map_err(backend_error)? {
                return Err(RuntimeError::Backend(format!(
                    "cannot recover {sandbox_id}: {name} is missing at {}",
                    path.display()
                )));
            }
        }
        let paused_directory = sandbox_dir.join("paused");
        let paused_candidate = SnapshotPaths {
            state: paused_directory.join("vmstate.bin"),
            memory: paused_directory.join("memory.bin"),
            rootfs: paused_directory.join("rootfs.ext4"),
        };
        let has_paused_snapshot = tokio::fs::try_exists(&paused_candidate.state)
            .await
            .map_err(backend_error)?
            && tokio::fs::try_exists(&paused_candidate.memory)
                .await
                .map_err(backend_error)?
            && tokio::fs::try_exists(&paused_candidate.rootfs)
                .await
                .map_err(backend_error)?;
        let paused_snapshot = if state == SandboxState::Paused || has_paused_snapshot {
            let snapshot = paused_candidate;
            for path in [&snapshot.state, &snapshot.memory, &snapshot.rootfs] {
                if !tokio::fs::try_exists(path).await.map_err(backend_error)? {
                    return Err(RuntimeError::Backend(format!(
                        "cannot recover paused sandbox {sandbox_id}: {} is missing",
                        path.display()
                    )));
                }
            }
            Some(snapshot)
        } else {
            None
        };
        let mut backend = FirecrackerBackend {
            config: self.config.clone(),
            request: request.clone(),
            socket: sandbox_dir.join("firecracker.sock"),
            vsock: sandbox_dir.join("agent.vsock"),
            sandbox_dir,
            rootfs,
            kernel,
            child: None,
            paused_snapshot,
            running: false,
            jailed: self.config.jailer_binary.is_some(),
        };
        if state == SandboxState::Running {
            backend.start().await?;
        }
        Ok(Box::new(backend))
    }
}

struct FirecrackerBackend {
    config: FirecrackerConfig,
    request: CreateSandboxRequest,
    sandbox_dir: PathBuf,
    rootfs: PathBuf,
    kernel: PathBuf,
    socket: PathBuf,
    vsock: PathBuf,
    child: Option<Child>,
    paused_snapshot: Option<SnapshotPaths>,
    running: bool,
    jailed: bool,
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
        let start_result = if let Some(snapshot) = self.paused_snapshot.clone() {
            self.load_snapshot(&snapshot).await
        } else {
            self.configure_fresh().await
        };
        if let Err(error) = start_result {
            if let Err(cleanup_error) = self.terminate().await {
                return Err(RuntimeError::Backend(format!(
                    "{error}; additionally failed to terminate Firecracker: {cleanup_error}"
                )));
            }
            let _ = tokio::fs::remove_dir_all(&self.sandbox_dir).await;
            return Err(error);
        }
        self.bootstrap_agent().await?;
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
        request: CommandRequest,
    ) -> Result<CommandResult, RuntimeError> {
        match self
            .agent_request(vento_agent_protocol::AgentRequest::Run(request))
            .await?
        {
            vento_agent_protocol::AgentResponse::Command(result) => Ok(result),
            response => Err(agent_response_error(response)),
        }
    }
    async fn kill_command(&mut self, command_id: &str) -> Result<(), RuntimeError> {
        self.expect_empty(vento_agent_protocol::AgentRequest::Kill {
            command_id: command_id.into(),
        })
        .await
    }
    async fn read_file(&self, path: &str, max_bytes: u64) -> Result<Vec<u8>, RuntimeError> {
        match self
            .agent_request(vento_agent_protocol::AgentRequest::ReadFile {
                path: path.into(),
                max_bytes,
            })
            .await?
        {
            vento_agent_protocol::AgentResponse::Bytes(bytes) => Ok(bytes),
            response => Err(agent_response_error(response)),
        }
    }
    async fn write_file(&mut self, path: &str, data: &[u8]) -> Result<(), RuntimeError> {
        self.expect_empty(vento_agent_protocol::AgentRequest::WriteFile {
            path: path.into(),
            data: data.into(),
            mode: None,
        })
        .await
    }
    async fn list_dir(&self, path: &str) -> Result<Vec<FileEntry>, RuntimeError> {
        match self
            .agent_request(vento_agent_protocol::AgentRequest::ListDir { path: path.into() })
            .await?
        {
            vento_agent_protocol::AgentResponse::Entries(entries) => Ok(entries),
            response => Err(agent_response_error(response)),
        }
    }
    async fn remove(&mut self, path: &str, recursive: bool) -> Result<(), RuntimeError> {
        self.expect_empty(vento_agent_protocol::AgentRequest::Remove {
            path: path.into(),
            recursive,
        })
        .await
    }

    async fn snapshot(&mut self, snapshot_id: &str) -> Result<BackendSnapshot, RuntimeError> {
        if !self.running {
            return Err(RuntimeError::Conflict(
                "persistent snapshot requires a running sandbox".into(),
            ));
        }
        let directory = self.sandbox_dir.join("snapshots").join(snapshot_id);
        let snapshot = self.capture_to(&directory).await?;
        firecracker_patch(&self.socket, "/vm", &serde_json::json!({"state":"Resumed"})).await?;
        let size_bytes = file_size(&snapshot.state).await?
            + file_size(&snapshot.memory).await?
            + file_size(&snapshot.rootfs).await?;
        let persistent = self.config.data_dir.join("snapshots").join(snapshot_id);
        tokio::fs::create_dir_all(&persistent)
            .await
            .map_err(backend_error)?;
        for (source, name) in [
            (&snapshot.state, "vmstate.bin"),
            (&snapshot.memory, "memory.bin"),
            (&snapshot.rootfs, "rootfs.ext4"),
        ] {
            reflink_clone(source, &persistent.join(name)).await?;
        }
        Ok(BackendSnapshot { size_bytes })
    }
}

impl FirecrackerBackend {
    async fn agent_request(
        &self,
        request: vento_agent_protocol::AgentRequest,
    ) -> Result<vento_agent_protocol::AgentResponse, RuntimeError> {
        let mut stream = tokio::net::UnixStream::connect(&self.vsock)
            .await
            .map_err(backend_error)?;
        stream
            .write_all(format!("CONNECT {}\n", self.config.agent_vsock_port).as_bytes())
            .await
            .map_err(backend_error)?;
        let mut handshake = Vec::new();
        read_line_capped(&mut stream, &mut handshake, 128).await?;
        if !handshake.starts_with(b"OK ") {
            return Err(RuntimeError::Backend(format!(
                "vsock handshake failed: {}",
                String::from_utf8_lossy(&handshake)
            )));
        }
        let mut frame =
            serde_json::to_vec(&request).map_err(|e| RuntimeError::Backend(e.to_string()))?;
        frame.push(b'\n');
        stream.write_all(&frame).await.map_err(backend_error)?;
        let mut response = Vec::new();
        read_line_capped(
            &mut stream,
            &mut response,
            vento_agent_protocol::MAX_FRAME_BYTES,
        )
        .await?;
        serde_json::from_slice(&response)
            .map_err(|e| RuntimeError::Backend(format!("invalid agent response: {e}")))
    }

    async fn expect_empty(
        &self,
        request: vento_agent_protocol::AgentRequest,
    ) -> Result<(), RuntimeError> {
        match self.agent_request(request).await? {
            vento_agent_protocol::AgentResponse::Empty => Ok(()),
            response => Err(agent_response_error(response)),
        }
    }

    async fn spawn(&mut self) -> Result<(), RuntimeError> {
        let _ = tokio::fs::remove_file(&self.socket).await;
        let mut command = if let Some(jailer) = &self.config.jailer_binary {
            clean_jailer_runtime_files(&self.sandbox_dir).await?;
            let mut command = Command::new(jailer);
            command
                .args(["--id", self.sandbox_id()?])
                .arg("--exec-file")
                .arg(&self.config.firecracker_binary)
                .args([
                    "--uid",
                    &self.config.jailer_uid.to_string(),
                    "--gid",
                    &self.config.jailer_gid.to_string(),
                ])
                .arg("--chroot-base-dir")
                .arg(self.config.data_dir.join("jailer"))
                .arg("--")
                .args(["--api-sock", "/firecracker.sock"]);
            command
        } else {
            let mut command = Command::new(&self.config.firecracker_binary);
            command.arg("--api-sock").arg(&self.socket);
            command
        };
        let child = command.kill_on_drop(true).spawn().map_err(backend_error)?;
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
        let kernel = self.fc_path(&self.kernel);
        let rootfs = self.fc_path(&self.rootfs);
        let vsock = self.fc_path(&self.vsock);
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
                "kernel_image_path": kernel,
                "boot_args": "console=ttyS0 reboot=k panic=1 pci=off init=/agentd",
            }),
        )
        .await?;
        firecracker_put(
            &self.socket,
            "/drives/rootfs",
            &serde_json::json!({
                "drive_id": "rootfs", "path_on_host": rootfs,
                "is_root_device": true, "is_read_only": false,
            }),
        )
        .await?;
        firecracker_put(
            &self.socket,
            "/vsock",
            &serde_json::json!({
                "guest_cid": guest_cid(&self.sandbox_dir), "uds_path": vsock,
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
        self.expect_empty(vento_agent_protocol::AgentRequest::Sync)
            .await?;
        tokio::fs::create_dir_all(directory)
            .await
            .map_err(backend_error)?;
        if self.jailed {
            chown_for_jailer(directory, self.config.jailer_uid, self.config.jailer_gid)?;
        }
        firecracker_patch(&self.socket, "/vm", &serde_json::json!({"state":"Paused"})).await?;
        let state = directory.join("vmstate.bin");
        let memory = directory.join("memory.bin");
        let rootfs = directory.join("rootfs.ext4");
        reflink_clone(&self.rootfs, &rootfs).await?;
        firecracker_put(
            &self.socket,
            "/snapshot/create",
            &serde_json::json!({
                "snapshot_type": "Full", "snapshot_path": self.fc_path(&state), "mem_file_path": self.fc_path(&memory),
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
        if self.jailed {
            chown_for_jailer(&self.rootfs, self.config.jailer_uid, self.config.jailer_gid)?;
        }
        firecracker_put(
            &self.socket,
            "/snapshot/load",
            &serde_json::json!({
                "snapshot_path": self.fc_path(&snapshot.state),
                "mem_backend": {"backend_type":"File", "backend_path": self.fc_path(&snapshot.memory)},
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

    fn fc_path(&self, path: &Path) -> PathBuf {
        if self.jailed {
            PathBuf::from("/").join(path.strip_prefix(&self.sandbox_dir).unwrap_or(path))
        } else {
            path.to_owned()
        }
    }

    fn sandbox_id(&self) -> Result<&str, RuntimeError> {
        self.sandbox_dir
            .ancestors()
            .nth(1)
            .and_then(Path::file_name)
            .and_then(|v| v.to_str())
            .ok_or_else(|| RuntimeError::Backend("invalid jail sandbox path".into()))
    }

    async fn bootstrap_agent(&self) -> Result<(), RuntimeError> {
        let deadline = Instant::now() + Duration::from_millis(self.config.boot_timeout_ms);
        loop {
            match self
                .agent_request(vento_agent_protocol::AgentRequest::Ready)
                .await
            {
                Ok(vento_agent_protocol::AgentResponse::Ready { version })
                    if version == vento_agent_protocol::PROTOCOL_VERSION =>
                {
                    break;
                }
                _ if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(25)).await
                }
                other => {
                    return Err(RuntimeError::Backend(format!(
                        "agentd did not become ready: {other:?}"
                    )));
                }
            }
        }
        let mut env = self.request.env.clone();
        env.extend(self.request.secrets.clone());
        self.expect_empty(vento_agent_protocol::AgentRequest::Configure {
            env,
            max_processes: self.request.resources.max_processes,
        })
        .await
    }
}

async fn clean_jailer_runtime_files(sandbox_dir: &Path) -> Result<(), RuntimeError> {
    for directory in ["dev", "run"] {
        match tokio::fs::remove_dir_all(sandbox_dir.join(directory)).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(backend_error(error)),
        }
    }
    for file in [
        "firecracker",
        "firecracker.pid",
        "firecracker.sock",
        "agent.vsock",
    ] {
        match tokio::fs::remove_file(sandbox_dir.join(file)).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(backend_error(error)),
        }
    }
    Ok(())
}

fn validate_supported_request(request: &CreateSandboxRequest) -> Result<(), RuntimeError> {
    if request.knowledge.is_some() {
        return Err(RuntimeError::Invalid(
            "knowledge storage is not configured yet".into(),
        ));
    }
    if !request.network.deny_private_network
        || !request.network.allow_cidrs.is_empty()
        || !request.network.allow_domains.is_empty()
    {
        return Err(RuntimeError::Invalid(
            "network access is disabled; allow rules require a configured network backend".into(),
        ));
    }
    Ok(())
}

fn agent_response_error(response: vento_agent_protocol::AgentResponse) -> RuntimeError {
    match response {
        vento_agent_protocol::AgentResponse::Error { code, message } => {
            RuntimeError::Backend(format!("agent {code}: {message}"))
        }
        other => RuntimeError::Backend(format!("unexpected agent response: {other:?}")),
    }
}

async fn read_line_capped(
    stream: &mut tokio::net::UnixStream,
    output: &mut Vec<u8>,
    cap: usize,
) -> Result<(), RuntimeError> {
    loop {
        if output.len() >= cap {
            return Err(RuntimeError::Backend("agent frame exceeds limit".into()));
        }
        let byte = stream.read_u8().await.map_err(backend_error)?;
        if byte == b'\n' {
            return Ok(());
        }
        output.push(byte);
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
    let mut response = Vec::new();
    while !response.ends_with(b"\r\n\r\n") {
        if response.len() >= 16 * 1024 {
            return Err(RuntimeError::Backend(
                "Firecracker response headers exceed 16 KiB".into(),
            ));
        }
        response.push(stream.read_u8().await.map_err(backend_error)?);
    }
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

async fn resize_rootfs(path: &Path, disk_mb: u32) -> Result<(), RuntimeError> {
    let requested = u64::from(disk_mb) * 1024 * 1024;
    let current = tokio::fs::metadata(path)
        .await
        .map_err(backend_error)?
        .len();
    if requested < current {
        return Err(RuntimeError::Invalid(format!(
            "diskMB is smaller than the {} MiB base image",
            current.div_ceil(1024 * 1024)
        )));
    }
    if requested == current {
        return Ok(());
    }
    let file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .await
        .map_err(backend_error)?;
    file.set_len(requested).await.map_err(backend_error)?;
    let status = Command::new("resize2fs")
        .arg(path)
        .status()
        .await
        .map_err(|error| {
            RuntimeError::Backend(format!("resize2fs is required to enforce diskMB: {error}"))
        })?;
    if !status.success() {
        return Err(RuntimeError::Backend(format!(
            "resize2fs failed with {status}"
        )));
    }
    Ok(())
}

fn chown_for_jailer(path: &Path, uid: u32, gid: u32) -> Result<(), RuntimeError> {
    nix::unistd::chown(
        path,
        Some(nix::unistd::Uid::from_raw(uid)),
        Some(nix::unistd::Gid::from_raw(gid)),
    )
    .map_err(|error| RuntimeError::Backend(error.to_string()))
}

async fn reflink_clone(source: &Path, destination: &Path) -> Result<(), RuntimeError> {
    let _ = tokio::fs::remove_file(destination).await;
    let output = Command::new("cp")
        .arg("--reflink=always")
        .arg("--sparse=auto")
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

fn jailer_id(sandbox_id: &str) -> String {
    sandbox_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else {
                '-'
            }
        })
        .collect()
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

    #[test]
    fn runtime_id_is_mapped_to_a_valid_jailer_id() {
        assert_eq!(jailer_id("sbx_019ff8a6"), "sbx-019ff8a6");
        assert!(
            jailer_id("sbx_019ff8a6")
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '-')
        );
    }

    #[test]
    fn unsupported_security_policies_fail_closed() {
        let mut request = CreateSandboxRequest::default();
        request.network.deny_private_network = false;
        assert!(validate_supported_request(&request).is_err());

        let request = CreateSandboxRequest {
            knowledge: Some(vento_runtime_types::KnowledgeMount {
                bucket: "documents".into(),
                prefix: "tenant".into(),
                version: "v1".into(),
                mount_path: "/knowledge".into(),
            }),
            ..CreateSandboxRequest::default()
        };
        assert!(validate_supported_request(&request).is_err());
    }

    #[tokio::test]
    async fn preflight_rejects_missing_runtime_artifacts() {
        let temp = tempfile::tempdir().unwrap();
        let factory = FirecrackerFactory::new(FirecrackerConfig {
            firecracker_binary: temp.path().join("missing-firecracker"),
            jailer_binary: None,
            jailer_uid: default_jailer_id(),
            jailer_gid: default_jailer_id(),
            kernel_image: temp.path().join("missing-kernel"),
            base_rootfs: temp.path().join("missing-rootfs"),
            template_rootfs: BTreeMap::new(),
            data_dir: temp.path().join("data"),
            agent_vsock_port: default_agent_port(),
            boot_timeout_ms: default_boot_timeout(),
        });
        let error = factory.preflight().await.unwrap_err();
        let message = error.to_string();
        if cfg!(target_os = "linux") {
            assert!(message.contains("firecracker does not exist"));
        } else {
            assert!(message.contains("requires Linux"));
        }
    }

    #[tokio::test]
    async fn firecracker_request_keeps_connection_open_for_response() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("firecracker.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(stream.read_u8().await.unwrap());
            }
            let headers = String::from_utf8(request).unwrap();
            let length = headers
                .lines()
                .find_map(|line| line.strip_prefix("Content-Length: "))
                .unwrap()
                .parse::<usize>()
                .unwrap();
            let mut body = vec![0; length];
            stream.read_exact(&mut body).await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(50), stream.read_u8())
                    .await
                    .is_err(),
                "client closed its write half before reading the response"
            );
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_secs(1)).await;
        });

        tokio::time::timeout(
            Duration::from_millis(100),
            firecracker_put(&socket, "/machine-config", &serde_json::json!({})),
        )
        .await
        .expect("client waited for the server to close the connection")
        .unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn jailer_restart_removes_only_transient_chroot_files() {
        let temp = tempfile::tempdir().unwrap();
        for directory in ["dev/net", "run", "paused"] {
            tokio::fs::create_dir_all(temp.path().join(directory))
                .await
                .unwrap();
        }
        for file in [
            "dev/net/tun",
            "firecracker",
            "firecracker.pid",
            "firecracker.sock",
            "agent.vsock",
            "rootfs.ext4",
            "kernel.bin",
            "paused/vmstate.bin",
        ] {
            tokio::fs::write(temp.path().join(file), b"fixture")
                .await
                .unwrap();
        }

        clean_jailer_runtime_files(temp.path()).await.unwrap();

        for removed in [
            "dev",
            "run",
            "firecracker",
            "firecracker.pid",
            "firecracker.sock",
            "agent.vsock",
        ] {
            assert!(!temp.path().join(removed).exists(), "{removed} remains");
        }
        for preserved in ["rootfs.ext4", "kernel.bin", "paused/vmstate.bin"] {
            assert!(temp.path().join(preserved).exists(), "{preserved} removed");
        }
    }

    #[tokio::test]
    #[ignore = "requires Linux, KVM, Firecracker, guest kernel/rootfs and XFS/Btrfs; run via tests/sandbox/run-host-acceptance.sh"]
    async fn real_host_preflight_accepts_provisioned_environment() {
        let config_path =
            std::env::var("VENTO_FIRECRACKER_CONFIG").expect("VENTO_FIRECRACKER_CONFIG");
        let config: FirecrackerConfig =
            serde_json::from_slice(&tokio::fs::read(config_path).await.unwrap()).unwrap();
        FirecrackerFactory::new(config).preflight().await.unwrap();
    }
}
