// SPDX-License-Identifier: MIT

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;

use vento_runtime_types::{CommandRequest, CreateSandboxRequest, IdleAction, SandboxState};
use vento_runtime_types::{CommandResult, FileEntry};
use vento_vm_runtime::{
    BackendSnapshot, InMemoryBackendFactory, RuntimeError, SandboxBackend, SandboxBackendFactory,
    VmRuntime,
};

fn runtime() -> VmRuntime {
    VmRuntime::new(Arc::new(InMemoryBackendFactory))
}

#[tokio::test]
async fn concurrent_idempotent_creates_publish_exactly_one_sandbox() {
    let creations = Arc::new(AtomicUsize::new(0));
    let runtime = VmRuntime::new(Arc::new(SlowFactory(creations.clone())));
    let mut tasks = Vec::new();
    for _ in 0..32 {
        let runtime = runtime.clone();
        tasks.push(tokio::spawn(async move {
            runtime
                .create(CreateSandboxRequest::default(), Some("retry-key"))
                .await
                .unwrap()
        }));
    }
    let mut ids = BTreeSet::new();
    for task in tasks {
        ids.insert(task.await.unwrap().sandbox_id);
    }
    assert_eq!(ids.len(), 1);
    assert_eq!(runtime.list().await.len(), 1);
    assert_eq!(creations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn blank_idempotency_keys_do_not_alias_unrelated_requests() {
    let runtime = runtime();
    let first = runtime
        .create(CreateSandboxRequest::default(), Some(" "))
        .await
        .unwrap();
    let second = runtime
        .create(CreateSandboxRequest::default(), Some(" "))
        .await
        .unwrap();
    assert_ne!(first.sandbox_id, second.sandbox_id);
}

#[tokio::test]
async fn lifecycle_rejects_operations_in_incompatible_states() {
    let runtime = runtime();
    let sandbox = runtime
        .create(CreateSandboxRequest::default(), None)
        .await
        .unwrap();
    runtime.stop(&sandbox.sandbox_id).await.unwrap();
    assert!(matches!(
        runtime.pause(&sandbox.sandbox_id).await,
        Err(RuntimeError::Conflict(_))
    ));
    assert!(matches!(
        runtime.run_command(&sandbox.sandbox_id, command()).await,
        Err(RuntimeError::Conflict(_))
    ));
    assert_eq!(
        runtime.start(&sandbox.sandbox_id).await.unwrap().state,
        SandboxState::Running
    );
}

#[tokio::test]
async fn command_and_resource_limits_are_enforced_before_backend_dispatch() {
    let runtime = runtime();
    let sandbox = runtime
        .create(CreateSandboxRequest::default(), None)
        .await
        .unwrap();
    let mut empty = command();
    empty.command.clear();
    assert!(matches!(
        runtime.run_command(&sandbox.sandbox_id, empty).await,
        Err(RuntimeError::Invalid(_))
    ));
    let mut excessive = command();
    excessive.timeout_ms = 3_600_001;
    assert!(matches!(
        runtime.run_command(&sandbox.sandbox_id, excessive).await,
        Err(RuntimeError::Invalid(_))
    ));

    let mut invalid = CreateSandboxRequest::default();
    invalid.resources.memory_mb = 64;
    assert!(matches!(
        runtime.create(invalid, None).await,
        Err(RuntimeError::Invalid(_))
    ));
}

#[tokio::test]
async fn file_roots_and_knowledge_read_only_policy_are_enforced() {
    let runtime = runtime();
    let sandbox = runtime
        .create(CreateSandboxRequest::default(), None)
        .await
        .unwrap();
    for path in [
        "relative",
        "/workspace/../etc/passwd",
        "/etc/passwd",
        "/knowledge/a.md",
    ] {
        assert!(
            matches!(
                runtime.write_file(&sandbox.sandbox_id, path, b"x").await,
                Err(RuntimeError::Invalid(_))
            ),
            "accepted {path}"
        );
    }
    runtime
        .write_file(&sandbox.sandbox_id, "/workspace/a", b"one")
        .await
        .unwrap();
    assert_eq!(
        runtime
            .read_file(&sandbox.sandbox_id, "/workspace/a", 3)
            .await
            .unwrap(),
        b"one"
    );
    assert!(
        runtime
            .read_file(&sandbox.sandbox_id, "/workspace/a", 2)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn idle_policy_pauses_plain_sandbox_but_destroys_secret_sandbox() {
    let runtime = runtime();
    let plain = runtime
        .create(CreateSandboxRequest::default(), None)
        .await
        .unwrap();
    let mut secret_request = CreateSandboxRequest {
        idle_action: IdleAction::Pause,
        ..CreateSandboxRequest::default()
    };
    secret_request
        .secrets
        .insert("TOKEN".into(), "never-log-this".into());
    let secret = runtime.create(secret_request, None).await.unwrap();
    runtime.reap_idle(u64::MAX).await;
    assert_eq!(
        runtime.get(&plain.sandbox_id).await.unwrap().state,
        SandboxState::Paused
    );
    assert!(matches!(
        runtime.get(&secret.sandbox_id).await,
        Err(RuntimeError::NotFound)
    ));
}

#[tokio::test]
async fn secret_sandbox_cannot_enter_pause_or_snapshot_paths() {
    let runtime = runtime();
    let mut request = CreateSandboxRequest::default();
    request
        .secrets
        .insert("TOKEN".into(), "never-log-this".into());
    let sandbox = runtime.create(request, None).await.unwrap();
    assert!(matches!(
        runtime.pause(&sandbox.sandbox_id).await,
        Err(RuntimeError::SecretsCannotBeSnapshotted)
    ));
    assert!(matches!(
        runtime.create_snapshot(&sandbox.sandbox_id).await,
        Err(RuntimeError::SecretsCannotBeSnapshotted)
    ));
}

#[tokio::test]
async fn sandbox_files_are_isolated_and_destroy_removes_access() {
    let runtime = runtime();
    let first = runtime
        .create(CreateSandboxRequest::default(), None)
        .await
        .unwrap();
    let second = runtime
        .create(CreateSandboxRequest::default(), None)
        .await
        .unwrap();
    runtime
        .write_file(&first.sandbox_id, "/workspace/private", b"first")
        .await
        .unwrap();
    assert!(
        runtime
            .read_file(&second.sandbox_id, "/workspace/private", 100)
            .await
            .is_err()
    );
    runtime.destroy(&first.sandbox_id).await.unwrap();
    assert!(matches!(
        runtime.get(&first.sandbox_id).await,
        Err(RuntimeError::NotFound)
    ));
}

fn command() -> CommandRequest {
    CommandRequest {
        command: vec!["true".into()],
        cwd: "/workspace".into(),
        env: Default::default(),
        timeout_ms: 1_000,
        stdin: None,
    }
}

struct SlowFactory(Arc<AtomicUsize>);

#[async_trait]
impl SandboxBackendFactory for SlowFactory {
    async fn create(
        &self,
        _id: &str,
        _request: &CreateSandboxRequest,
    ) -> Result<Box<dyn SandboxBackend>, RuntimeError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        Ok(Box::new(NoopBackend))
    }
}

struct NoopBackend;

#[async_trait]
impl SandboxBackend for NoopBackend {
    async fn start(&mut self) -> Result<(), RuntimeError> {
        Ok(())
    }
    async fn pause(&mut self) -> Result<(), RuntimeError> {
        Ok(())
    }
    async fn resume(&mut self) -> Result<(), RuntimeError> {
        Ok(())
    }
    async fn stop(&mut self) -> Result<(), RuntimeError> {
        Ok(())
    }
    async fn destroy(&mut self) -> Result<(), RuntimeError> {
        Ok(())
    }
    async fn run_command(
        &mut self,
        _request: CommandRequest,
    ) -> Result<CommandResult, RuntimeError> {
        unreachable!()
    }
    async fn kill_command(&mut self, _id: &str) -> Result<(), RuntimeError> {
        unreachable!()
    }
    async fn read_file(&self, _path: &str, _max: u64) -> Result<Vec<u8>, RuntimeError> {
        unreachable!()
    }
    async fn write_file(&mut self, _path: &str, _data: &[u8]) -> Result<(), RuntimeError> {
        unreachable!()
    }
    async fn list_dir(&self, _path: &str) -> Result<Vec<FileEntry>, RuntimeError> {
        unreachable!()
    }
    async fn remove(&mut self, _path: &str, _recursive: bool) -> Result<(), RuntimeError> {
        unreachable!()
    }
    async fn snapshot(&mut self, _id: &str) -> Result<BackendSnapshot, RuntimeError> {
        unreachable!()
    }
}
