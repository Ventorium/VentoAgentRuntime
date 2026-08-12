# Sandbox acceptance tests

The suite is intentionally layered so a fake backend cannot be mistaken for a working microVM runtime.

1. `cargo test -p vento-agent-protocol` verifies the guest path policy.
2. `cargo test -p vento-vm-runtime --test runtime_contract` verifies lifecycle, concurrency, isolation, secrets and limits through `VmRuntime`.
3. `cargo test -p vento-agentd --test agentd_contract` runs the guest agent as a subprocess and verifies command/file behavior.
4. `cargo test -p vento-runtime-server` verifies authentication and HTTP error contracts through the Router.
5. `run-host-acceptance.sh` verifies KVM, cgroup v2, reflink and Firecracker artifacts on a provisioned Linux host.
6. `run-real-sandbox.sh` starts the real daemon and requires create, command, file, pause/resume, snapshot and destroy to work against Firecracker.

The last test is a release gate. It must never be replaced by the fake backend.

