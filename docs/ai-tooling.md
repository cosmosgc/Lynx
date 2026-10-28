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

## AI performance guide for Lynx

The fastest way to get correct results in this repository is to ground the task
in the project’s explicit source of truth before writing code or docs.

### Read order for a new task

1. `README.md` for the current project status and scope.
2. `docs/TODO/README.md` and the relevant backlog file in `docs/TODO/`.
3. The current design docs that match the area (`docs/security.md`,
   `docs/networking.md`, `docs/operations.md`, `docs/adr/*`, etc.).
4. The relevant Rust module or crate that owns the behavior.
5. Only then write the patch, issue, or documentation update.

This order prevents drift between design intent, backlog status, and the code
implementation. When the docs and code disagree, the code and the design docs
win; the backlog and issue notes should then be corrected in the same change.

### Repository conventions for AI agents

- Prefer surgical edits over broad rewrites.
- Use the existing naming patterns and status labels already used in the repo.
- Keep task descriptions specific, testable, and linked to the authoritative
  design or backlog item.
- Do not invent new phases, status values, or document structures unless the
  repository already uses them.
- If a fix is documentation drift rather than a real defect, say so plainly and
  close the issue as a drift fix, not as an implementation gap.

### Issue format

Use the issue files under `docs/issues/` for defect reports and unresolved
operational gaps. The format should be:

```md
# <PREFIX>-### — Short title

- **Status:** Open | Resolved | Closed
- **Reported:** YYYY-MM-DD
- **Resolved:** YYYY-MM-DD or —
- **Related tasks:** [Task](../TODO/...)
- **Affected workflow:** <system or workflow>

## Resolution

Explain the actual state, the root cause, and how the issue is addressed or
closed.

## Defect

Describe the thing that is wrong or missing in clear, factual language.

## Impact

List the practical consequences for operators, users, or maintainers.

## Acceptance criteria

- [ ] concrete, testable condition
- [ ] another condition
```

Rules:

- Use an issue only when the repository has a real gap or a real incident.
- Mark the issue as closed if it was false, stale, or corrected by a doc fix.
- Keep acceptance criteria checkable and tied to code, CI, or documented
  process.
- Never leave unbounded statements like “must be production-ready” without a
  concrete gate.

### Todo format

Backlog items under `docs/TODO/` are the canonical task list. Use this pattern:

```md
## <TASK-ID> — Title

- **Priority:** P0 | P1 | P2 | P3
- **Status:** Ready | Blocked | Proposed | Icebox | Done
- **Depends on:** <other task ids or —>
- **References:** design docs, code, ADRs

**Why:** explain the reason and the risk.

**Scope**

- bullet list of what is in scope
- bullet list of explicit non-goals if needed

**Acceptance criteria**

- [ ] testable requirement
- [ ] another requirement
```

Rules:

- `docs/TODO/README.md` is the index; add the new entry there when relevant.
- Prefer a single, concrete problem and a bounded implementation scope.
- Keep `Depends on` honest; blocked work must name the dependency, not implied
  assumptions.
- Add references to the relevant docs and code, not vague descriptions.

### How to write docs in this project

Good project documentation in Lynx should be:

- specific to a decision, workflow, or invariant
- linked to the code or ADR it explains
- actionable for operators, contributors, or AI assistants
- short enough to be maintained by people who are not the original author

Preferred structure:

1. Start with the decision or problem statement.
2. Describe the relevant system boundary and trust model.
3. State the invariant, rule, or risk that governs the behavior.
4. Show how to validate or confirm the behavior.
5. Link to the code or a related ADR/TODO item.

When writing docs:

- Prefer imperative, factual sentences over marketing language.
- Use relative Markdown links to the underlying repo artifacts.
- Keep headings stable and low-friction to scan.
- If a doc mentions a temporary or development-only path, say so explicitly.
- Never document a nonexistent file or a feature that is not implemented;
  document the design and the status separately.

### Code documentation directory for AI assistance

The repository-wide documentation structure is:

- `README.md` — current state and repo summary.
- `docs/adr/` — architecture decisions and design intent.
- `docs/TODO/` — backlog items and acceptance criteria.
- `docs/issues/` — issue reports and resolved gaps.
- `docs/` — user-facing engineering docs and operational references.
- `crates/*/README.md` or module-level comments — local contract documentation
  for the code that owns the behavior.

For AI-assisted work, prefer a compact, discoverable law of the repository:

- design docs explain “why”
- TODO files explain “what remains”
- issue files explain “what broke or drifted”
- code comments explain “how the local contract works”
- tests explain the expected behavior and the compatibility rules

See [`ai/README.md`](ai/README.md) for the AI-facing project workflow and
conventions used by contributors and assistants working in this repository.

## Documentation directory for AI assistance

The project-specific AI guidance lives in [`ai/README.md`](ai/README.md). This
folder is the place for operating notes, repo conventions, and workflow
checklists that help AI assistants work productively without drifting from the
project’s source of truth.

Keep AI-facing docs here when the guidance is:

- specific to this repository and its workflows
- intended to reduce bad assumptions during coding or review
- describing how to file issues, write TODOs, or update design docs
- describing the expected code-documentation layout for a crate or module

This directory should remain small and practical: instructions first, examples
second, and long rationale only when it prevents repeated mistakes.
