#!/usr/bin/env bash
set -euo pipefail

project_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
config=${VENTO_FIRECRACKER_CONFIG:?set VENTO_FIRECRACKER_CONFIG}
if [[ -f "$HOME/.cargo/env" ]]; then source "$HOME/.cargo/env"; fi
listen=${VENTO_TEST_LISTEN:-127.0.0.1:18089}
token=${VENTO_TEST_TOKEN:-vento-real-acceptance-token-0001}
base="http://$listen"
log=$(mktemp)
pid=
cleanup() {
  if [[ -n "$pid" ]]; then kill "$pid" 2>/dev/null || true; fi
  rm -f "$log"
}
trap cleanup EXIT

cd "$project_root"
cargo build --bin vento-runtime-server
target/debug/vento-runtime-server --listen "$listen" --token "$token" --firecracker-config "$config" >"$log" 2>&1 &
pid=$!
for _ in $(seq 1 50); do curl -fsS "$base/health" >/dev/null && break; sleep .1; done
auth=(-H "Authorization: Bearer $token")
sandbox_json=$(curl -fsS "${auth[@]}" -H 'Content-Type: application/json' -H 'Idempotency-Key: real-acceptance' -d '{}' "$base/sandboxes")
sandbox_id=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["sandboxId"])' <<<"$sandbox_json")

command_json=$(curl -fsS "${auth[@]}" -H 'Content-Type: application/json' -d '{"command":["/bin/sh","-c","printf real-agent"],"cwd":"/workspace","env":{},"timeoutMs":5000}' "$base/sandboxes/$sandbox_id/commands")
python3 -c 'import json,sys; d=json.load(sys.stdin); assert bytes(d["stdout"]) == b"real-agent" and d["exitCode"] == 0' <<<"$command_json"

printf 'persistent-file' | curl -fsS "${auth[@]}" -X PUT --data-binary @- "$base/sandboxes/$sandbox_id/files/content?path=%2Fworkspace%2Facceptance.txt"
test "$(curl -fsS "${auth[@]}" "$base/sandboxes/$sandbox_id/files/content?path=%2Fworkspace%2Facceptance.txt")" = persistent-file
curl -fsS "${auth[@]}" -X POST "$base/sandboxes/$sandbox_id/pause" >/dev/null
curl -fsS "${auth[@]}" -X POST "$base/sandboxes/$sandbox_id/resume" >/dev/null
curl -fsS "${auth[@]}" -H 'Content-Type: application/json' -d '{}' "$base/sandboxes/$sandbox_id/snapshots" >/dev/null
curl -fsS "${auth[@]}" -X DELETE "$base/sandboxes/$sandbox_id"
echo "real sandbox acceptance passed"
