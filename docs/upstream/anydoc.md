# anydoc source baseline

- Upstream: `https://github.com/firecrawl/anydoc`
- Imported commit: `4a45addbd607e8b59f0c263bca26aab228e10370`
- Imported: Rust document model, package readers, format parsers and GFM renderer.
- Excluded: Node, Python, WASM, CLI, benchmark and release infrastructure.
- Local seam: `document_runtime::{inspect, convert}` replaces the upstream public surface.

