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

