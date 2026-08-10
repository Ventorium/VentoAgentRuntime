# Third-party notices

VentoAgentRuntime contains modified source derived from these MIT-licensed projects:

- Firecrawl `anydoc`, commit `4a45addbd607e8b59f0c263bca26aab228e10370`.
  The shared document model, Office/OpenDocument/RTF/EPUB parsers and Markdown renderer were
  imported into `crates/document-runtime` and are maintained as VentoAgentRuntime source.
- Firecrawl `pdf-inspector`, commit `f4b8c9e8546703e3817d9c47498d822cb2db81e3`.
  PDF detection, text extraction, layout, table and Markdown code was imported into
  `crates/pdf-engine` and is maintained as VentoAgentRuntime source.

The original license texts are stored in `LICENSES/`. Neither upstream project is a Cargo,
npm, git-submodule or runtime dependency of VentoAgentRuntime.

