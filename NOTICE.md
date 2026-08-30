# Third-party notices

VentoAgentRuntime contains modified source derived from these MIT-licensed projects:

- Firecrawl `anydoc`.
  The shared document model, Office/OpenDocument/RTF/EPUB parsers and Markdown renderer were
  imported into `crates/file-parser` and are maintained as VentoAgentRuntime source.
- Firecrawl `pdf-inspector`.
  PDF detection, text extraction, layout, table and Markdown code was imported into
  `crates/pdf-engine` and is maintained as VentoAgentRuntime source.

The original license texts are stored in `LICENSES/`. Neither upstream project is a Cargo,
npm, git-submodule or runtime dependency of VentoAgentRuntime.

