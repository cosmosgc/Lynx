# CNT-009 — MCP server: read-only tools

- **Status:** Done
- **Completed:** 2026-11-12
- **Source task:** [MCP server: read-only tools](../50-content-editor-tooling.md#cnt-009--mcp-server-read-only-tools)
- **Documentation:** [AI tooling](../../ai-tooling.md#planned-mcp-tools)

## Delivered

- Added the optional `lynx-mcp` stdio server using the official Rust MCP SDK.
- Exposed `project.inspect`, `content.validate`, `content.diff`, `doctor`, and
  `content.hash`. Validation, diff, diagnostics, and hashing call the same CLI
  command implementations without starting a shell or child command.
- Bound the server to one canonical workspace root. Tool paths must be
  relative, cannot traverse parents, and must resolve inside the workspace;
  symlinks and special files in the content tree are rejected.
- Bounded results to 256 KiB and JSON depth 16; limited path input length and
  content-tree scan size.
- Kept MCP opt-in behind the `mcp-server` feature and a separate `lynx-mcp`
  binary, so default builds and tests do not require MCP dependencies.

## Verification

- `cargo test -p project_cli --features mcp-server --bin lynx-mcp`
- `cargo clippy -p project_cli --features mcp-server --bin lynx-mcp -- -D warnings`
- `cargo clippy -p project_cli --no-default-features --all-targets -- -D warnings`
- Sent MCP `initialize` and `tools/list` requests over stdio; the server
  advertised the five read-only tools.
