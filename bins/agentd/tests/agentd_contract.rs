// SPDX-License-Identifier: MIT

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

fn exchange(requests: &[Value]) -> Vec<Value> {
    let binary = env!("CARGO_BIN_EXE_vento-agentd");
    let mut child = Command::new(binary)
        .env("VENTO_AGENTD_SKIP_PREPARE", "1")
        .env("SHOULD_NOT_LEAK", "host-secret")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn agentd");
    let mut stdin = child.stdin.take().unwrap();
    for request in requests {
        writeln!(stdin, "{}", serde_json::to_string(request).unwrap()).unwrap();
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn readiness_reports_protocol_version_and_invalid_json_is_structured() {
    let responses = exchange(&[json!({"method":"ready"})]);
    assert_eq!(responses[0], json!({"result":"ready","data":{"version":1}}));
}

#[test]
fn command_executes_with_clean_environment_and_caller_variables() {
    let responses = exchange(&[json!({"method":"run","params":{
        "command":["/bin/sh","-c","printf '%s|%s' \"${INJECTED:-}\" \"${SHOULD_NOT_LEAK:-}\""],
        "cwd":"/tmp","env":{"INJECTED":"allowed"},"timeoutMs":2000,"stdin":null
    }})]);
    let bytes = responses[0]["data"]["stdout"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap() as u8)
        .collect::<Vec<_>>();
    assert_eq!(String::from_utf8(bytes).unwrap(), "allowed|");
}

#[test]
fn stdin_is_delivered_and_timeout_is_reported() {
    let responses = exchange(&[
        json!({"method":"run","params":{"command":["/bin/sh","-c","cat"],"cwd":"/tmp","env":{},"timeoutMs":2000,"stdin":[118,101,110,116,111]}}),
        json!({"method":"run","params":{"command":["/bin/sh","-c","sleep 2"],"cwd":"/tmp","env":{},"timeoutMs":20,"stdin":null}}),
    ]);
    assert_eq!(
        responses[0]["data"]["stdout"],
        json!([118, 101, 110, 116, 111])
    );
    assert_eq!(responses[1]["data"]["timedOut"], true);
}

#[test]
fn output_is_capped_to_one_mebibyte_per_stream() {
    let responses = exchange(&[json!({"method":"run","params":{
        "command":["/bin/sh","-c","head -c 1100000 /dev/zero"],"cwd":"/tmp","env":{},"timeoutMs":5000,"stdin":null
    }})]);
    assert_eq!(
        responses[0]["data"]["stdout"].as_array().unwrap().len(),
        1024 * 1024
    );
}

#[test]
fn filesystem_operations_reject_knowledge_writes_and_traversal() {
    let responses = exchange(&[
        json!({"method":"write_file","params":{"path":"/knowledge/poison.md","data":[120],"mode":null}}),
        json!({"method":"read_file","params":{"path":"/workspace/../etc/passwd","max_bytes":1024}}),
    ]);
    assert_eq!(responses[0]["result"], "error");
    assert_eq!(responses[1]["result"], "error");
}
