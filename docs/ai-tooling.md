# AI tooling and MCP

MCP support is **optional** and disabled by default. Building, running, and
testing a Lynx project never requires an MCP server.

## Goals

- Let an authorized AI assistant scaffold a project, author starter content,
  validate, build, test, and diagnose failures through typed commands.
- Keep the same schemas, templates, validation, and compatibility rules as
  manual project creation.
- Make every consequential action visible, scoped, and reversible.

## Supported surfaces

| Surface | Status | Notes |
|---|---|---|
| CLI (`lynx new/doctor/validate/content hash/content diff`) | Implemented | Stable, typed, machine-readable JSON via `--json` |
| Engine MCP server | Planned | Narrow tools wrapping the CLI and content validators |
| External MCP client | Planned | Explicit per-workspace configuration |
| Shell execution | Not exposed by default | Allowlist or user approval per command |

## Planned MCP tools

Read-only:

- `lynx.project.inspect` — manifest, engine version, content versions.
- `lynx.content.validate` — schema and reference diagnostics.
- `lynx.content.diff` — compare two manifests and report the required version
  bump, additions, removals, changes, and compatibility diagnostics.
- `lynx.diagnostics` — backend and environment checks.
- `lynx.build.status` — last build result and error summary.

Write or consequential (approval required):

- `lynx.project.create` — scaffold from a template into a new directory.
- `lynx.content.write` — write a validated definition inside the workspace.
- `lynx.build.run` — run a build with a bounded timeout.
- `lynx.test.run` — run selected test targets.

Never exposed: production admin APIs, database credentials, arbitrary shell,
unrestricted filesystem access, secret stores, or publishing without approval.

## Permission model

- Tools are configured per user, per workspace, and per environment.
- Filesystem access is confined to the project root; `..` traversal and
  symlinks escaping the root are rejected.
- Commands are allowlisted (`cargo test`, `cargo build`, `lynx validate`) or
  require explicit confirmation showing the exact argv.
- Destructive operations require approval and show the affected paths.
- Tool calls have timeouts, output caps, cancellation, and audit records.
- Tool results are untrusted: validate, bound, and never execute returned text.

## Approval flow

1. Assistant proposes a tool call with inputs.
2. The host shows the tool, inputs, and expected side effects.
3. User approves, edits, or denies.
4. The tool runs with a bounded timeout and returns structured output.
5. The assistant may iterate using the same tools, never bypassing validation.

## AI-assisted project creation (target workflow)

```bash
lynx new my-world --template mmorpg --output ../my-world --json
lynx validate --project ../my-world --json
cargo build --manifest-path ../my-world/Cargo.toml
```

Expected result: created-file summary, validation diagnostics, build result,
and actionable errors with file paths and error codes. The AI iterates on the
same commands a human would use.

Generated projects author player-facing text in Brazilian Portuguese (`pt-BR`)
by default. When an assistant drafts another locale, it must preserve message
keys and placeholders exactly; `lynx validate` rejects missing keys and
placeholder mismatches, and machine translations require human review before
release. See [`internationalization.md`](internationalization.md).

## Audit and privacy

- Log tool name, version, inputs (redacted), duration, outcome, and actor.
- Never log credentials, tokens, private player data, or full chat.
- External MCP servers are documented with their requested capabilities and
  trust assumptions. Credentials are never forwarded implicitly.
- Users can disable MCP entirely without affecting the project.

## Trust boundaries

- Tool definitions from external servers are untrusted and version-pinned.
- Tool output is data, not authority.
- AI-generated content passes the same validation, licensing, moderation, and
  review gates as human content.
