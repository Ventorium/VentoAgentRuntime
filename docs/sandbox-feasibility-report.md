# Agent Sandbox feasibility report

## Verdict

The architecture is viable, but the current Firecracker implementation is not yet an operational Agent Sandbox. The state machine, HTTP contract, guest command executor and host prerequisites are usable independently. The real backend cannot satisfy the command/file API because host-to-guest transport and readiness are not implemented.

## Evidence by seam

| Seam | Status | Evidence |
|---|---|---|
| `VmRuntime` | Pass after fix | Lifecycle, isolation, limits, secrets, idle policy and 32-way concurrent idempotency tests |
| HTTP control plane | Pass | Authentication, stable errors, traversal, secret redaction and idempotency Router tests |
| `agentd` subprocess | Partial pass after fix | Readiness, env isolation, stdin, timeout, output cap and path policy pass; Kill remains unimplemented |
| Linux host | Pass on PVE VM 105 | Debian 12, nested KVM R/W, cgroup v2, XFS reflink, Firecracker/jailer 1.16.1; `run-host-acceptance.sh` passed |
| Real Firecracker create/readiness | Fail on VM 105 | Official Firecracker quickstart kernel/rootfs reached the real backend, but create returned HTTP 500; `InstanceStart` is treated as ready and the image has no `/agentd`; no authenticated readiness exchange exists |
| Real command/files | Fail by construction | Backend methods return `agentd transport is not ready` |
| Network isolation | Not implemented | No netns, TAP or nftables setup in `FirecrackerFactory`; the deny table is unused |
| cgroup limits | Not implemented | No per-sandbox cgroup creation or attachment |
| jailer | Not implemented | Config field exists but the backend launches Firecracker directly |
| Snapshot | Unsafe/incomplete | Snapshot publication is not temp+fsync+atomic rename; persistent snapshot attempts load on a running VMM |
| Knowledge drive | Not implemented | No artifact sync/verification or read-only drive attachment |

## Release blockers

1. Implement an authenticated length-delimited vsock protocol and make create wait for agent readiness.
2. Implement process registry and Kill in agentd; ensure timeout reaps process groups.
3. Launch through jailer and create per-sandbox cgroup v2, network namespace, TAP and nftables policy.
4. Attach `/workspace` writable and versioned `/knowledge` read-only drives.
5. Rework snapshot create/load into a stopped/new-VMM sequence with atomic manifests and cleanup.
6. Run `tests/sandbox/run-real-sandbox.sh` as a mandatory self-hosted KVM release gate.

## Defects found and fixed during the audit

1. Concurrent requests sharing an `Idempotency-Key` could create multiple backends. Creation is now serialized around idempotency publication; a 32-way race test proves one backend is created.
2. `agentd` ignored `CommandRequest.stdin`. Stdin is now piped before waiting for output, with subprocess coverage.
3. Reflink probing used the invalid GNU `cp` combination `--reflink=always --sparse=always`, causing every XFS host to be rejected. It now uses `--sparse=auto` and passes a real XFS reflink test.
4. A failed Firecracker start leaked its VMM process and sandbox directory. Startup errors now terminate the child and remove the failed directory; the repeated VM test left no new process or directory.

## Verification summary (2026-08-12)

- `cargo test --workspace`: 846 passed, 0 failed.
- Sandbox-focused Clippy (`agent-protocol`, `vm-runtime`, `firecracker-runtime`, `agentd`, `runtime-server`) with `-D warnings`: passed.
- Whole-workspace Clippy with `-D warnings`: blocked by 20 pre-existing `pdf-engine` test warnings; none are in the Sandbox change set.
- Host acceptance on `192.168.8.210`: passed.
- Real Sandbox acceptance: failed at create with HTTP 500, as expected for the missing guest image/transport contract. This remains a release blocker, not an allowed expected-success test.
