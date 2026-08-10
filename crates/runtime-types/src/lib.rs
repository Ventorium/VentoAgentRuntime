// SPDX-License-Identifier: MIT

use std::collections::BTreeMap;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub type SandboxId = String;
pub type SnapshotId = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SandboxState {
    Creating,
    Running,
    Paused,
    Stopped,
    Failed,
    Destroyed,
}

impl SandboxState {
    pub fn can_transition_to(self, next: Self) -> bool {
        use SandboxState::{Creating, Destroyed, Failed, Paused, Running, Stopped};
        matches!(
            (self, next),
            (Creating, Running | Failed | Destroyed)
                | (Running, Paused | Stopped | Failed | Destroyed)
                | (Paused, Running | Stopped | Failed | Destroyed)
                | (Stopped, Running | Failed | Destroyed)
                | (Failed, Destroyed)
        ) || self == next
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdleAction {
    #[default]
    Pause,
    Destroy,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceLimits {
    pub cpu_count: u8,
    pub memory_mb: u32,
    pub disk_mb: u32,
    pub max_processes: u32,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            cpu_count: 1,
            memory_mb: 256,
            disk_mb: 2_048,
            max_processes: 64,
        }
    }
}

impl ResourceLimits {
    pub fn validate(&self) -> Result<(), RuntimeTypeError> {
        if !(1..=32).contains(&self.cpu_count) {
            return Err(RuntimeTypeError::Invalid("cpuCount must be 1..=32".into()));
        }
        if !(128..=131_072).contains(&self.memory_mb) {
            return Err(RuntimeTypeError::Invalid(
                "memoryMB must be 128..=131072".into(),
            ));
        }
        if !(256..=1_048_576).contains(&self.disk_mb) {
            return Err(RuntimeTypeError::Invalid(
                "diskMB must be 256..=1048576".into(),
            ));
        }
        if !(1..=32_768).contains(&self.max_processes) {
            return Err(RuntimeTypeError::Invalid(
                "maxProcesses must be 1..=32768".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NetworkPolicy {
    #[serde(default = "default_true")]
    pub deny_private_network: bool,
    #[serde(default)]
    pub allow_cidrs: Vec<String>,
    #[serde(default)]
    pub deny_cidrs: Vec<String>,
    #[serde(default)]
    pub allow_domains: Vec<String>,
}

impl Default for NetworkPolicy {
    fn default() -> Self {
        Self {
            deny_private_network: true,
            allow_cidrs: Vec::new(),
            deny_cidrs: Vec::new(),
            allow_domains: Vec::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KnowledgeMount {
    pub bucket: String,
    pub prefix: String,
    pub version: String,
    #[serde(default = "default_knowledge_path")]
    pub mount_path: String,
}

fn default_knowledge_path() -> String {
    "/knowledge".into()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateSandboxRequest {
    #[serde(default = "default_template")]
    pub template: String,
    #[serde(default)]
    pub resources: ResourceLimits,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub idle_action: IdleAction,
    #[serde(default)]
    pub network: NetworkPolicy,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub secrets: BTreeMap<String, String>,
    pub knowledge: Option<KnowledgeMount>,
    pub snapshot_id: Option<SnapshotId>,
    pub session_id: Option<String>,
}

fn default_template() -> String {
    "debian-slim".into()
}
fn default_timeout_seconds() -> u64 {
    3_600
}

impl Default for CreateSandboxRequest {
    fn default() -> Self {
        Self {
            template: default_template(),
            resources: ResourceLimits::default(),
            timeout_seconds: default_timeout_seconds(),
            idle_action: IdleAction::Pause,
            network: NetworkPolicy::default(),
            env: BTreeMap::new(),
            secrets: BTreeMap::new(),
            knowledge: None,
            snapshot_id: None,
            session_id: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxInfo {
    pub sandbox_id: SandboxId,
    pub session_id: String,
    pub state: SandboxState,
    pub resources: ResourceLimits,
    pub created_at_ms: u64,
    pub last_active_at_ms: u64,
    pub expires_at_ms: u64,
    pub has_secrets: bool,
    pub knowledge_version: Option<String>,
    pub failure_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandRequest {
    pub command: Vec<String>,
    #[serde(default = "default_cwd")]
    pub cwd: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_command_timeout")]
    pub timeout_ms: u64,
    pub stdin: Option<Vec<u8>>,
}

fn default_cwd() -> String {
    "/workspace".into()
}
fn default_command_timeout() -> u64 {
    30_000
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandResult {
    pub command_id: String,
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub duration_ms: u64,
    pub timed_out: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub path: String,
    pub kind: String,
    pub size: u64,
    pub modified_at_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotInfo {
    pub snapshot_id: SnapshotId,
    pub source_sandbox_id: SandboxId,
    pub created_at_ms: u64,
    pub size_bytes: u64,
    pub manifest_version: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    pub request_id: Option<String>,
}

#[derive(Debug)]
pub enum RuntimeTypeError {
    Invalid(String),
    IllegalTransition {
        from: SandboxState,
        to: SandboxState,
    },
}

impl fmt::Display for RuntimeTypeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => formatter.write_str(message),
            Self::IllegalTransition { from, to } => {
                write!(formatter, "illegal state transition {from:?} -> {to:?}")
            }
        }
    }
}
impl std::error::Error for RuntimeTypeError {}

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::now_v7().simple())
}
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destroyed_is_terminal() {
        assert!(!SandboxState::Destroyed.can_transition_to(SandboxState::Running));
    }

    #[test]
    fn defaults_meet_mvp_baseline() {
        let config = CreateSandboxRequest::default();
        assert_eq!(config.resources.memory_mb, 256);
        assert!(config.network.deny_private_network);
    }
}
