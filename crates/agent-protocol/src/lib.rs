// SPDX-License-Identifier: MIT

use serde::{Deserialize, Serialize};
use vento_runtime_types::{CommandRequest, CommandResult, FileEntry};

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum AgentRequest {
    Ready,
    Run(CommandRequest),
    Kill {
        command_id: String,
    },
    ReadFile {
        path: String,
        max_bytes: u64,
    },
    WriteFile {
        path: String,
        data: Vec<u8>,
        mode: Option<u32>,
    },
    ListDir {
        path: String,
    },
    Stat {
        path: String,
    },
    Mkdir {
        path: String,
        recursive: bool,
    },
    Remove {
        path: String,
        recursive: bool,
    },
    Shutdown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "result", content = "data", rename_all = "snake_case")]
pub enum AgentResponse {
    Ready { version: u16 },
    Command(CommandResult),
    Bytes(Vec<u8>),
    Entries(Vec<FileEntry>),
    Entry(FileEntry),
    Empty,
    Error { code: String, message: String },
}

pub fn validate_guest_path(path: &str, writable: bool) -> Result<(), &'static str> {
    if !path.starts_with('/') || path.contains('\0') || path.split('/').any(|part| part == "..") {
        return Err("path must be absolute and cannot contain traversal");
    }
    let allowed = if writable {
        path == "/workspace"
            || path.starts_with("/workspace/")
            || path == "/tmp"
            || path.starts_with("/tmp/")
    } else {
        path == "/workspace"
            || path.starts_with("/workspace/")
            || path == "/knowledge"
            || path.starts_with("/knowledge/")
            || path == "/tmp"
            || path.starts_with("/tmp/")
    };
    allowed
        .then_some(())
        .ok_or("path is outside sandbox data roots")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knowledge_is_read_only() {
        assert!(validate_guest_path("/knowledge/a.md", false).is_ok());
        assert!(validate_guest_path("/knowledge/a.md", true).is_err());
    }

    #[test]
    fn path_policy_rejects_traversal_nul_relative_and_system_paths() {
        for path in [
            "workspace/a",
            "/workspace/../etc/passwd",
            "/etc/passwd",
            "/workspace/a\0b",
        ] {
            assert!(
                validate_guest_path(path, false).is_err(),
                "accepted {path:?}"
            );
        }
    }

    #[test]
    fn workspace_and_tmp_are_writable_but_roots_are_scoped() {
        for path in ["/workspace", "/workspace/a/b", "/tmp", "/tmp/output"] {
            assert!(validate_guest_path(path, true).is_ok(), "rejected {path}");
        }
        for path in ["/", "/home", "/proc", "/knowledge", "/knowledge/a"] {
            assert!(validate_guest_path(path, true).is_err(), "accepted {path}");
        }
    }
}
