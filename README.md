# VentoAgentRuntime

Rust-native document ingestion and Firecracker sandbox runtime for AI agents.

## Packages

- `@ventostack/document-runtime`: files, URLs and media to LLM-oriented Markdown.
- `@ventostack/vm-runtime`: E2B-shaped client for the sandbox runtime.

The document implementation owns its Office and PDF parsing code. It does not depend on
`anydoc`, `pdf-inspector` or `liteparse`.

## Development

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Local Firecracker execution requires Linux, KVM and a data directory on reflink-capable XFS
or Btrfs. Document conversion and remote runtime clients build on other supported platforms.

