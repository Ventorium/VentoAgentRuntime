# VentoAgentRuntime

Rust-native document parsing and Firecracker sandbox runtime for AI agents.

## Packages

- `@ventostack/file-parser`: any file to LLM-oriented Markdown (Office, PDF, images via remote OCR, plain text).
- `@ventostack/vm-runtime`: E2B-shaped client for the sandbox runtime.

The document implementation lives in `crates/file-parser` (Office/OpenDocument/RTF/EPUB/CSV
parsers and the Markdown renderer, derived from `anydoc`) and `crates/pdf-engine` (PDF text
and table extraction, derived from `pdf-inspector`). Both are maintained in-tree; upstream
references are kept under `.repos/` and neither is an external dependency (see `NOTICE.md`).
Scanned PDFs and standalone images route through a configurable remote PaddleOCR provider.

## Development

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Local Firecracker execution requires Linux, KVM and a data directory on reflink-capable XFS
or Btrfs. Document conversion and remote runtime clients build on other supported platforms.

The Firecracker backend requires `jailer` and expects the guest image to contain the matching
`vento-agentd` binary.
`agentd` listens on virtio-vsock port `10000`; rebuild the guest rootfs whenever the agent
protocol changes. A minimal Firecracker configuration is:

```json
{
  "firecrackerBinary": "/usr/local/bin/firecracker",
  "jailerBinary": "/usr/local/bin/jailer",
  "jailerUid": 1000,
  "jailerGid": 1000,
  "kernelImage": "/var/lib/vento/vmlinux",
  "baseRootfs": "/var/lib/vento/debian-slim.ext4",
  "templateRootfs": {},
  "dataDir": "/var/lib/vento/runtime"
}
```

The host also needs `resize2fs` when a sandbox requests a disk larger than its template.
Networking is fail-closed: guests receive no network interface. Requests that ask to allow
network access, or request a knowledge mount without a configured storage backend, are rejected.
