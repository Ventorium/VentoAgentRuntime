// SPDX-License-Identifier: MIT

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{Mutex, RwLock};
use vento_runtime_types::{
    CommandRequest, CommandResult, CreateSandboxRequest, FileEntry, IdleAction, SandboxId,
    SandboxInfo, SandboxState, SnapshotId, SnapshotInfo, new_id, now_ms,
};

#[derive(Debug)]
pub enum RuntimeError {
    NotFound,
    Conflict(String),
    Invalid(String),
    SecretsCannotBeSnapshotted,
    Backend(String),
}

impl RuntimeError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound => "SANDBOX_NOT_FOUND",
            Self::Conflict(_) => "STATE_CONFLICT",
            Self::Invalid(_) => "INVALID_INPUT",
            Self::SecretsCannotBeSnapshotted => "SECRETS_CANNOT_BE_SNAPSHOTTED",
            Self::Backend(_) => "BACKEND_ERROR",
        }
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => formatter.write_str("sandbox was not found"),
            Self::Conflict(message) | Self::Invalid(message) | Self::Backend(message) => {
                formatter.write_str(message)
            }
            Self::SecretsCannotBeSnapshotted => {
                formatter.write_str("sandbox contains secrets and cannot be paused or snapshotted")
            }
        }
    }
}
impl std::error::Error for RuntimeError {}

#[derive(Clone, Debug)]
pub struct BackendSnapshot {
    pub size_bytes: u64,
}

#[async_trait]
pub trait SandboxBackend: Send + Sync {
    async fn start(&mut self) -> Result<(), RuntimeError>;
    async fn pause(&mut self) -> Result<(), RuntimeError>;
    async fn resume(&mut self) -> Result<(), RuntimeError>;
    async fn stop(&mut self) -> Result<(), RuntimeError>;
    async fn destroy(&mut self) -> Result<(), RuntimeError>;
    async fn run_command(&mut self, request: CommandRequest)
    -> Result<CommandResult, RuntimeError>;
    async fn kill_command(&mut self, command_id: &str) -> Result<(), RuntimeError>;
    async fn read_file(&self, path: &str, max_bytes: u64) -> Result<Vec<u8>, RuntimeError>;
    async fn write_file(&mut self, path: &str, data: &[u8]) -> Result<(), RuntimeError>;
    async fn list_dir(&self, path: &str) -> Result<Vec<FileEntry>, RuntimeError>;
    async fn remove(&mut self, path: &str, recursive: bool) -> Result<(), RuntimeError>;
    async fn snapshot(&mut self, snapshot_id: &str) -> Result<BackendSnapshot, RuntimeError>;
}

#[async_trait]
pub trait SandboxBackendFactory: Send + Sync {
    async fn create(
        &self,
        sandbox_id: &str,
        request: &CreateSandboxRequest,
    ) -> Result<Box<dyn SandboxBackend>, RuntimeError>;
}

struct SandboxRecord {
    info: SandboxInfo,
    request: CreateSandboxRequest,
    backend: Box<dyn SandboxBackend>,
}

impl fmt::Debug for SandboxRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SandboxRecord")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

type SharedSandbox = Arc<Mutex<SandboxRecord>>;

#[derive(Clone)]
pub struct VmRuntime {
    factory: Arc<dyn SandboxBackendFactory>,
    sandboxes: Arc<RwLock<HashMap<SandboxId, SharedSandbox>>>,
    snapshots: Arc<RwLock<HashMap<SnapshotId, SnapshotInfo>>>,
    idempotency: Arc<Mutex<HashMap<String, SandboxId>>>,
    create_guard: Arc<Mutex<()>>,
}

impl fmt::Debug for VmRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("VmRuntime").finish_non_exhaustive()
    }
}

impl VmRuntime {
    pub fn new(factory: Arc<dyn SandboxBackendFactory>) -> Self {
        Self {
            factory,
            sandboxes: Arc::new(RwLock::new(HashMap::new())),
            snapshots: Arc::new(RwLock::new(HashMap::new())),
            idempotency: Arc::new(Mutex::new(HashMap::new())),
            create_guard: Arc::new(Mutex::new(())),
        }
    }

    pub async fn create(
        &self,
        request: CreateSandboxRequest,
        idempotency_key: Option<&str>,
    ) -> Result<SandboxInfo, RuntimeError> {
        request
            .resources
            .validate()
            .map_err(|error| RuntimeError::Invalid(error.to_string()))?;
        if request.timeout_seconds == 0 || request.timeout_seconds > 86_400 {
            return Err(RuntimeError::Invalid(
                "timeoutSeconds must be 1..=86400".into(),
            ));
        }
        let idempotency_key = idempotency_key.filter(|key| !key.trim().is_empty());
        // Serialize the lookup + backend creation + insertion transaction. This
        // makes concurrent retries with the same key observe one sandbox.
        let _create_guard = self.create_guard.lock().await;
        if let Some(key) = idempotency_key
            && let Some(existing) = self.idempotency.lock().await.get(key).cloned()
        {
            return self.get(&existing).await;
        }
        let sandbox_id = new_id("sbx");
        let session_id = request.session_id.clone().unwrap_or_else(|| new_id("ses"));
        let timestamp = now_ms();
        let mut backend = self.factory.create(&sandbox_id, &request).await?;
        let mut info = SandboxInfo {
            sandbox_id: sandbox_id.clone(),
            session_id,
            state: SandboxState::Creating,
            resources: request.resources.clone(),
            created_at_ms: timestamp,
            last_active_at_ms: timestamp,
            expires_at_ms: timestamp.saturating_add(request.timeout_seconds.saturating_mul(1_000)),
            has_secrets: !request.secrets.is_empty(),
            knowledge_version: request
                .knowledge
                .as_ref()
                .map(|knowledge| knowledge.version.clone()),
            failure_reason: None,
        };
        if let Err(error) = backend.start().await {
            info.state = SandboxState::Failed;
            info.failure_reason = Some(error.to_string());
            let record = Arc::new(Mutex::new(SandboxRecord {
                info: info.clone(),
                request,
                backend,
            }));
            self.sandboxes.write().await.insert(sandbox_id, record);
            return Err(error);
        }
        info.state = SandboxState::Running;
        let record = Arc::new(Mutex::new(SandboxRecord {
            info: info.clone(),
            request,
            backend,
        }));
        self.sandboxes
            .write()
            .await
            .insert(sandbox_id.clone(), record);
        if let Some(key) = idempotency_key {
            self.idempotency
                .lock()
                .await
                .insert(key.to_owned(), sandbox_id);
        }
        Ok(info)
    }

    pub async fn get(&self, sandbox_id: &str) -> Result<SandboxInfo, RuntimeError> {
        let record = self.record(sandbox_id).await?;
        let info = record.lock().await.info.clone();
        Ok(info)
    }

    pub async fn list(&self) -> Vec<SandboxInfo> {
        let records: Vec<_> = self.sandboxes.read().await.values().cloned().collect();
        let mut result = Vec::with_capacity(records.len());
        for record in records {
            result.push(record.lock().await.info.clone());
        }
        result.sort_by_key(|value| value.created_at_ms);
        result
    }

    pub async fn start(&self, sandbox_id: &str) -> Result<SandboxInfo, RuntimeError> {
        self.transition(
            sandbox_id,
            &[SandboxState::Stopped],
            SandboxState::Running,
            BackendAction::Start,
        )
        .await
    }

    pub async fn pause(&self, sandbox_id: &str) -> Result<SandboxInfo, RuntimeError> {
        let record = self.record(sandbox_id).await?;
        let mut record = record.lock().await;
        if record.info.has_secrets {
            return Err(RuntimeError::SecretsCannotBeSnapshotted);
        }
        require_state(record.info.state, &[SandboxState::Running])?;
        record.backend.pause().await?;
        update_state(&mut record.info, SandboxState::Paused)?;
        Ok(record.info.clone())
    }

    pub async fn resume(&self, sandbox_id: &str) -> Result<SandboxInfo, RuntimeError> {
        self.transition(
            sandbox_id,
            &[SandboxState::Paused],
            SandboxState::Running,
            BackendAction::Resume,
        )
        .await
    }

    pub async fn stop(&self, sandbox_id: &str) -> Result<SandboxInfo, RuntimeError> {
        self.transition(
            sandbox_id,
            &[SandboxState::Running, SandboxState::Paused],
            SandboxState::Stopped,
            BackendAction::Stop,
        )
        .await
    }

    pub async fn destroy(&self, sandbox_id: &str) -> Result<(), RuntimeError> {
        let record = self.record(sandbox_id).await?;
        {
            let mut record = record.lock().await;
            if record.info.state != SandboxState::Destroyed {
                record.backend.destroy().await?;
                update_state(&mut record.info, SandboxState::Destroyed)?;
            }
        }
        self.sandboxes.write().await.remove(sandbox_id);
        self.idempotency
            .lock()
            .await
            .retain(|_, value| value != sandbox_id);
        Ok(())
    }

    pub async fn run_command(
        &self,
        sandbox_id: &str,
        request: CommandRequest,
    ) -> Result<CommandResult, RuntimeError> {
        if request.command.is_empty() {
            return Err(RuntimeError::Invalid("command cannot be empty".into()));
        }
        if request.timeout_ms == 0 || request.timeout_ms > 3_600_000 {
            return Err(RuntimeError::Invalid(
                "timeoutMs must be 1..=3600000".into(),
            ));
        }
        let record = self.record(sandbox_id).await?;
        let mut record = record.lock().await;
        require_state(record.info.state, &[SandboxState::Running])?;
        let result = record.backend.run_command(request).await?;
        let timeout_seconds = record.request.timeout_seconds;
        touch(&mut record.info, timeout_seconds);
        Ok(result)
    }

    pub async fn kill_command(
        &self,
        sandbox_id: &str,
        command_id: &str,
    ) -> Result<(), RuntimeError> {
        let record = self.record(sandbox_id).await?;
        let mut record = record.lock().await;
        require_state(record.info.state, &[SandboxState::Running])?;
        record.backend.kill_command(command_id).await
    }

    pub async fn read_file(
        &self,
        sandbox_id: &str,
        path: &str,
        max_bytes: u64,
    ) -> Result<Vec<u8>, RuntimeError> {
        vento_agent_protocol::validate_guest_path(path, false)
            .map_err(|message| RuntimeError::Invalid(message.into()))?;
        let record = self.record(sandbox_id).await?;
        let record = record.lock().await;
        require_state(record.info.state, &[SandboxState::Running])?;
        record
            .backend
            .read_file(path, max_bytes.min(100 * 1024 * 1024))
            .await
    }

    pub async fn write_file(
        &self,
        sandbox_id: &str,
        path: &str,
        data: &[u8],
    ) -> Result<(), RuntimeError> {
        vento_agent_protocol::validate_guest_path(path, true)
            .map_err(|message| RuntimeError::Invalid(message.into()))?;
        if data.len() > 100 * 1024 * 1024 {
            return Err(RuntimeError::Invalid("file exceeds 100 MiB".into()));
        }
        let record = self.record(sandbox_id).await?;
        let mut record = record.lock().await;
        require_state(record.info.state, &[SandboxState::Running])?;
        record.backend.write_file(path, data).await?;
        let timeout_seconds = record.request.timeout_seconds;
        touch(&mut record.info, timeout_seconds);
        Ok(())
    }

    pub async fn list_dir(
        &self,
        sandbox_id: &str,
        path: &str,
    ) -> Result<Vec<FileEntry>, RuntimeError> {
        vento_agent_protocol::validate_guest_path(path, false)
            .map_err(|message| RuntimeError::Invalid(message.into()))?;
        let record = self.record(sandbox_id).await?;
        let record = record.lock().await;
        require_state(record.info.state, &[SandboxState::Running])?;
        record.backend.list_dir(path).await
    }

    pub async fn remove(
        &self,
        sandbox_id: &str,
        path: &str,
        recursive: bool,
    ) -> Result<(), RuntimeError> {
        vento_agent_protocol::validate_guest_path(path, true)
            .map_err(|message| RuntimeError::Invalid(message.into()))?;
        let record = self.record(sandbox_id).await?;
        let mut record = record.lock().await;
        require_state(record.info.state, &[SandboxState::Running])?;
        record.backend.remove(path, recursive).await
    }

    pub async fn create_snapshot(&self, sandbox_id: &str) -> Result<SnapshotInfo, RuntimeError> {
        let record = self.record(sandbox_id).await?;
        let mut record = record.lock().await;
        if record.info.has_secrets {
            return Err(RuntimeError::SecretsCannotBeSnapshotted);
        }
        require_state(
            record.info.state,
            &[SandboxState::Running, SandboxState::Paused],
        )?;
        let snapshot_id = new_id("snp");
        let snapshot = record.backend.snapshot(&snapshot_id).await?;
        let info = SnapshotInfo {
            snapshot_id: snapshot_id.clone(),
            source_sandbox_id: sandbox_id.into(),
            created_at_ms: now_ms(),
            size_bytes: snapshot.size_bytes,
            manifest_version: 1,
        };
        self.snapshots
            .write()
            .await
            .insert(snapshot_id, info.clone());
        Ok(info)
    }

    pub async fn get_snapshot(&self, snapshot_id: &str) -> Result<SnapshotInfo, RuntimeError> {
        self.snapshots
            .read()
            .await
            .get(snapshot_id)
            .cloned()
            .ok_or(RuntimeError::NotFound)
    }

    pub async fn delete_snapshot(&self, snapshot_id: &str) -> Result<(), RuntimeError> {
        self.snapshots
            .write()
            .await
            .remove(snapshot_id)
            .map(|_| ())
            .ok_or(RuntimeError::NotFound)
    }

    pub async fn reap_idle(&self, timestamp_ms: u64) {
        let infos = self.list().await;
        for info in infos.into_iter().filter(|value| {
            value.state == SandboxState::Running && value.expires_at_ms <= timestamp_ms
        }) {
            let action = self
                .record(&info.sandbox_id)
                .await
                .ok()
                .map(|record| async move { record.lock().await.request.idle_action });
            let action = match action {
                Some(action) => action.await,
                None => continue,
            };
            match action {
                IdleAction::Pause if !info.has_secrets => {
                    let _ = self.pause(&info.sandbox_id).await;
                }
                IdleAction::Pause | IdleAction::Destroy => {
                    let _ = self.destroy(&info.sandbox_id).await;
                }
            }
        }
    }

    async fn transition(
        &self,
        sandbox_id: &str,
        expected: &[SandboxState],
        next: SandboxState,
        action: BackendAction,
    ) -> Result<SandboxInfo, RuntimeError> {
        let record = self.record(sandbox_id).await?;
        let mut record = record.lock().await;
        if record.info.state == next {
            return Ok(record.info.clone());
        }
        require_state(record.info.state, expected)?;
        match action {
            BackendAction::Start => record.backend.start().await?,
            BackendAction::Resume => record.backend.resume().await?,
            BackendAction::Stop => record.backend.stop().await?,
        }
        update_state(&mut record.info, next)?;
        Ok(record.info.clone())
    }

    async fn record(&self, sandbox_id: &str) -> Result<SharedSandbox, RuntimeError> {
        self.sandboxes
            .read()
            .await
            .get(sandbox_id)
            .cloned()
            .ok_or(RuntimeError::NotFound)
    }
}

enum BackendAction {
    Start,
    Resume,
    Stop,
}

fn require_state(current: SandboxState, expected: &[SandboxState]) -> Result<(), RuntimeError> {
    if expected.contains(&current) {
        Ok(())
    } else {
        Err(RuntimeError::Conflict(format!(
            "operation is not allowed while sandbox is {current:?}"
        )))
    }
}
fn update_state(info: &mut SandboxInfo, next: SandboxState) -> Result<(), RuntimeError> {
    if !info.state.can_transition_to(next) {
        return Err(RuntimeError::Conflict(format!(
            "illegal transition {:?} -> {next:?}",
            info.state
        )));
    }
    info.state = next;
    info.last_active_at_ms = now_ms();
    Ok(())
}
fn touch(info: &mut SandboxInfo, timeout_seconds: u64) {
    let timestamp = now_ms();
    info.last_active_at_ms = timestamp;
    info.expires_at_ms = timestamp.saturating_add(timeout_seconds.saturating_mul(1_000));
}

#[derive(Clone, Debug, Default)]
pub struct InMemoryBackendFactory;

#[derive(Debug, Default)]
struct InMemoryBackend {
    running: bool,
    files: HashMap<String, Vec<u8>>,
}

#[async_trait]
impl SandboxBackendFactory for InMemoryBackendFactory {
    async fn create(
        &self,
        _sandbox_id: &str,
        _request: &CreateSandboxRequest,
    ) -> Result<Box<dyn SandboxBackend>, RuntimeError> {
        Ok(Box::new(InMemoryBackend::default()))
    }
}

#[async_trait]
impl SandboxBackend for InMemoryBackend {
    async fn start(&mut self) -> Result<(), RuntimeError> {
        self.running = true;
        Ok(())
    }
    async fn pause(&mut self) -> Result<(), RuntimeError> {
        self.running = false;
        Ok(())
    }
    async fn resume(&mut self) -> Result<(), RuntimeError> {
        self.running = true;
        Ok(())
    }
    async fn stop(&mut self) -> Result<(), RuntimeError> {
        self.running = false;
        Ok(())
    }
    async fn destroy(&mut self) -> Result<(), RuntimeError> {
        self.running = false;
        self.files.clear();
        Ok(())
    }
    async fn run_command(
        &mut self,
        request: CommandRequest,
    ) -> Result<CommandResult, RuntimeError> {
        Ok(CommandResult {
            command_id: new_id("cmd"),
            exit_code: Some(0),
            stdout: request.command.join(" ").into_bytes(),
            stderr: Vec::new(),
            duration_ms: 0,
            timed_out: false,
        })
    }
    async fn kill_command(&mut self, _command_id: &str) -> Result<(), RuntimeError> {
        Ok(())
    }
    async fn read_file(&self, path: &str, max_bytes: u64) -> Result<Vec<u8>, RuntimeError> {
        let data = self
            .files
            .get(path)
            .cloned()
            .ok_or(RuntimeError::NotFound)?;
        if data.len() as u64 > max_bytes {
            return Err(RuntimeError::Invalid("file exceeds read limit".into()));
        }
        Ok(data)
    }
    async fn write_file(&mut self, path: &str, data: &[u8]) -> Result<(), RuntimeError> {
        self.files.insert(path.into(), data.to_vec());
        Ok(())
    }
    async fn list_dir(&self, path: &str) -> Result<Vec<FileEntry>, RuntimeError> {
        Ok(self
            .files
            .iter()
            .filter(|(key, _)| key.starts_with(path))
            .map(|(key, value)| FileEntry {
                path: key.clone(),
                kind: "file".into(),
                size: value.len() as u64,
                modified_at_ms: now_ms(),
            })
            .collect())
    }
    async fn remove(&mut self, path: &str, _recursive: bool) -> Result<(), RuntimeError> {
        self.files.remove(path);
        Ok(())
    }
    async fn snapshot(&mut self, _snapshot_id: &str) -> Result<BackendSnapshot, RuntimeError> {
        Ok(BackendSnapshot {
            size_bytes: self.files.values().map(|value| value.len() as u64).sum(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lifecycle_and_file_isolation() {
        let runtime = VmRuntime::new(Arc::new(InMemoryBackendFactory));
        let first = runtime
            .create(CreateSandboxRequest::default(), Some("one"))
            .await
            .expect("create");
        let second = runtime
            .create(CreateSandboxRequest::default(), Some("two"))
            .await
            .expect("create");
        runtime
            .write_file(&first.sandbox_id, "/workspace/a.txt", b"secret")
            .await
            .expect("write");
        assert!(
            runtime
                .read_file(&second.sandbox_id, "/workspace/a.txt", 100)
                .await
                .is_err()
        );
        assert_eq!(
            runtime.pause(&first.sandbox_id).await.expect("pause").state,
            SandboxState::Paused
        );
        assert_eq!(
            runtime
                .resume(&first.sandbox_id)
                .await
                .expect("resume")
                .state,
            SandboxState::Running
        );
    }

    #[tokio::test]
    async fn secrets_block_snapshot_and_idle_pause_destroys() {
        let runtime = VmRuntime::new(Arc::new(InMemoryBackendFactory));
        let mut request = CreateSandboxRequest::default();
        request.secrets.insert("TOKEN".into(), "redacted".into());
        let sandbox = runtime.create(request, None).await.expect("create");
        assert!(matches!(
            runtime.create_snapshot(&sandbox.sandbox_id).await,
            Err(RuntimeError::SecretsCannotBeSnapshotted)
        ));
        runtime.reap_idle(u64::MAX).await;
        assert!(runtime.get(&sandbox.sandbox_id).await.is_err());
    }

    #[tokio::test]
    async fn idempotency_returns_same_sandbox() {
        let runtime = VmRuntime::new(Arc::new(InMemoryBackendFactory));
        let first = runtime
            .create(CreateSandboxRequest::default(), Some("key"))
            .await
            .expect("create");
        let second = runtime
            .create(CreateSandboxRequest::default(), Some("key"))
            .await
            .expect("repeat");
        assert_eq!(first.sandbox_id, second.sandbox_id);
    }
}
