# Content, editor, and tooling backlog

Scope: everything a creator touches — canonical formats, project scaffolding, the
editor, navigation/behavior authoring, asset policy, localization tooling,
user-content moderation, content migration, and MCP/CLI automation.

**What exists today:** RON definitions and catalogs, world-map atlases with
explicit routes, UI panel schemas, faction/calendar content, manifests, hashing,
compatibility-aware diffs, and publish checks. The `lynx` CLI supports
`new`, `doctor`, `validate`, `content hash`, `content diff`, and
`content publish-check`. There is no editor, no MCP server, no asset import
pipeline, and no content-migration tool.

**Related design:** [content authoring](../content-authoring.md),
[world simulation](../world-simulation.md), [AI tooling](../ai-tooling.md),
[platforms](../platforms.md),
[ADR 0006](../adr/0006-content-and-protocol-formats.md),
[ADR 0017](../adr/0017-content-manifest-versioning.md),
[ADR 0019](../adr/0019-world-map-atlas-and-routes.md).

---

## CNT-001 — Editor application shell

- **Priority:** P1
- **Status:** Proposed — editor stack decision (D5)
- **Depends on:** D5, `CLT-001`
- **References:** [project guide §23](../lightweight_mmorpg_engine_project_guide.md#23-suggested-implementation-roadmap),
  [content authoring](../content-authoring.md)

**Why:** Phase 5 requires a creator to build a small zone without editing engine
source. Whether that is an in-engine Bevy editor, a standalone tool, or a
CLI-first workflow is an open decision; the shell and undo model depend on it.

**Scope**

- Open/save a project with the canonical manifest and content directories.
- Scene viewport with selection, transform gizmos, and a docked inspector.
- Central undo/redo command stack that covers placement, property edits, and
  deletion.
- Validation panel that runs the same gates as `lynx validate`.
- Preview/play mode against a local server or embedded simulation.
- Crash-safe autosave and recovery.

**Acceptance criteria**

- [ ] D5 is recorded as an ADR before implementation starts.
- [ ] A creator can open a generated project, edit, undo/redo, save, and reopen.
- [ ] Validation errors point to the offending definition and field.
- [ ] Autosave/recovery never leaves a half-written project.
- [ ] Editor code is isolated from engine/runtime crates as D5 requires.

---

## CNT-002 — World atlas and map editor

- **Priority:** P1
- **Status:** Blocked — `CNT-001`, `SIM-014`
- **Depends on:** `CNT-001`, `SIM-014`
- **References:** ADR 0019, [content authoring §World atlas](../content-authoring.md#world-atlas-and-routes)

**Scope**

- Place zone IDs on map cells; edit passage/gateway/portal routes.
- Validate duplicate, missing, and orphaned references with actionable
  diagnostics.
- Territory/faction overlays and calendar event locations (`CNT-016`).
- Snapping, layering, and budget indicators for routes/entities.
- Preview the route graph and travel paths.

**Acceptance criteria**

- [ ] The editor produces atlases that `lynx validate` accepts without manual
      edits.
- [ ] Duplicate/missing/orphaned references are shown at the offending cell.
- [ ] A route can be previewed and traversed in play mode.
- [ ] Overlays do not become the source of truth; the authored atlas remains it.

---

## CNT-003 — Entity placement and inspector

- **Priority:** P1
- **Status:** Blocked — `CNT-001`
- **Depends on:** `CNT-001`
- **References:** [content authoring §Definitions vs. instances](../content-authoring.md#definitions-vs-instances)

**Scope**

- Place entity instances from definitions with position/rotation and property
  overrides.
- Inspector for components (health, door, inventory, UI provider, mind,
  persistence policy, links).
- Bounds/collision/occupancy validation and placement budgets.
- Bulk select/move/delete with undo.
- Property ownership/lease rules once defined (`QUA-017`).

**Acceptance criteria**

- [ ] Placement produces valid instances with no engine edit.
- [ ] Invalid positions/overrides are rejected before save.
- [ ] Inspector changes round-trip through the canonical format.
- [ ] Undo/redo restores exact prior state.

---

## CNT-004 — Link graph editor

- **Priority:** P2
- **Status:** Blocked — `CNT-003`
- **Depends on:** `CNT-003`
- **References:** `engine_sim::link`, ADR 0016

**Scope**

- Visualize typed entity links (lever → door, controller → mechanism, etc.).
- Create/delete links and edit permissions/conditions.
- Detect cycles, dangling endpoints, and unreachable mechanisms.
- Preview link activation in play mode.

**Acceptance criteria**

- [ ] The editor prevents or clearly flags invalid cycles.
- [ ] A link graph authored in the UI activates correctly in game.
- [ ] Dangling links are reported with the entity IDs involved.
- [ ] Deletion updates both endpoints atomically.

---

## CNT-005 — Definition editor

- **Priority:** P1
- **Status:** Blocked — `CNT-001`
- **Depends on:** `CNT-001`
- **References:** [content authoring §Definition shape](../content-authoring.md#definition-shape)

**Scope**

- Form-based editing for item/entity/UI panel/faction/calendar definitions.
- Namespaced UI schema editing with localized labels and allowed actions.
- Reference pickers for assets, definitions, factions, and locales.
- Validation and compatibility impact preview (`lynx content diff`).
- Version bump assistance for changed definitions.

**Acceptance criteria**

- [ ] A new item/entity can be authored and loaded without hand-editing RON.
- [ ] Invalid references are caught before save.
- [ ] The editor proposes the required version bump from the actual diff.
- [ ] Localized labels are edited through keys, not embedded strings.

---

## CNT-006 — Navigation authoring and visualization

- **Priority:** P2
- **Status:** Blocked — `CNT-002`, `SIM-007`
- **Depends on:** `CNT-002`, `SIM-007`
- **References:** ADR 0010, `SIM-007`

**Scope**

- Bake/preview nav grids for a zone and visualize blocked/expensive areas.
- Path preview between points with budget expansion counts.
- Report unreachable entities/spawn points.
- Keep authoring and runtime nav data identical and versioned.

**Acceptance criteria**

- [ ] A baked nav grid loads in the runtime and matches the preview.
- [ ] Unreachable placements are reported before publish.
- [ ] Path preview shows expansion count and failure reason.
- [ ] Nav data changes are included in content hashes/diffs.

---

## CNT-007 — Behavior graph editor and runtime debugger

- **Priority:** P3
- **Status:** Blocked — `SIM-006`
- **Depends on:** `SIM-006`
- **References:** `SIM-006`, ADR 0008

**Scope**

- Visual behavior/state-machine editing with a bounded instruction set.
- Runtime trace: current state, target, last decision, budget use.
- Breakpoints/pause for a single AI entity in a local preview.
- Validation for unreachable states, missing references, and budget overruns.

**Acceptance criteria**

- [ ] A behavior authored visually runs without engine code.
- [ ] The debugger shows the same decision the server made for a tick.
- [ ] Invalid graphs fail validation with a precise node/edge.
- [ ] Runtime tracing is opt-in and does not leak hidden state to clients.

---

## CNT-008 — User-content schemas and moderation states

- **Priority:** P2
- **Status:** Proposed — user-content policy decision (D6)
- **Depends on:** D6
- **References:** [security §Content and user-generated content](../security.md#content-and-user-generated-content),
  migration `0002_content.sql`

**Why:** The schema already has content bundles and moderation history, but
ownership, visibility, revision, and revocation rules for player-created content
are not implemented. This must not be improvised after uploads exist.

**Scope**

- Schemas for ownership, visibility, revision history, moderation status, and
  revocation.
- Size/asset-type limits and disallowed-content scanning before publish.
- Draft → review → published → revoked lifecycle with audit.
- Quotas and abuse reporting.
- A migration path for existing authored content.

**Acceptance criteria**

- [ ] Draft content is invisible to other sessions.
- [ ] Publishing requires the configured review/authorization path.
- [ ] Revocation removes content from new sessions without deleting audit.
- [ ] Ownership and revision history survive restart and export.

---

## CNT-009 — MCP server: read-only tools

- **Priority:** P2
- **Status:** Ready
- **Depends on:** `CNT-011`
- **References:** [AI tooling §Planned MCP tools](../ai-tooling.md#planned-mcp-tools)

**Scope**

- Workspace-scoped MCP server exposing read-only tools:
  `project.inspect`, `content.validate`, `content.diff`, `doctor`, and
  `content.hash`.
- Reuse the CLI/validators rather than a second implementation.
- Machine-readable JSON with stable schemas and bounded output.
- Explicit configuration; disabled by default; no shell execution.
- Path traversal and workspace escape protection.

**Acceptance criteria**

- [ ] Tools return the same results as the CLI for the same project.
- [ ] A tool cannot read outside the configured workspace.
- [ ] Output size and depth are bounded.
- [ ] MCP remains optional; no build/test path requires it.

---

## CNT-010 — MCP server: write and publish tools

- **Priority:** P3
- **Status:** Blocked — `CNT-009`, `OPS-004`
- **Depends on:** `CNT-009`, `OPS-004`
- **References:** [AI tooling](../ai-tooling.md), [security §AI and MCP tools](../security.md#ai-and-mcp-tools)

**Scope**

- Scaffolding, content edits, staging, and publish tools with explicit approval.
- Show the exact diff/command before a consequential action.
- Audit tool name, inputs, approval, and outcome.
- Confine writes to the project; no arbitrary shell.
- Rollback path for published changes.

**Acceptance criteria**

- [ ] Destructive/publishing actions require explicit approval.
- [ ] Every write is auditable and reversible through the normal pipeline.
- [ ] The tool cannot escape the workspace or execute returned shell text.
- [ ] A denied approval leaves the project unchanged.

---

## CNT-011 — CLI expansion

- **Priority:** P1
- **Status:** In progress — command handlers are modularized; automation contracts incomplete
- **Depends on:** —
- **References:** `tools/project_cli/src/main.rs`,
  [AI tooling](../ai-tooling.md)

**Why:** The CLI covers scaffolding, validation, basic project execution, and
content checks, but migrations, richer diagnostics, and consistent automation
contracts are still needed.

**Scope**

- `lynx build`, `lynx run` and `lynx test` now validate content and invoke Cargo for the
  project; generated templates remain starter executables, not playable servers.
- `lynx content build` now writes a validated manifest and hashes to a stable
  `content/manifest.json` output by default.
- `lynx run` and `lynx test` for a generated project, with headless modes.
- `lynx migrate` for content/schema migrations (`CNT-014`).
- `lynx doctor --json` coverage for database/schema/content/protocol versions.
- Consistent exit codes and machine-readable output across commands.
- Shell completion and generated command reference.

**Acceptance criteria**

- [x] A generated project can be built, validated, run, and tested from the CLI
      while its generated path dependencies remain available.
- [ ] Every command has `--json` output with a documented schema.
- [ ] Exit codes distinguish validation failure, incompatibility, and internal
      error.
- [x] Hosted CI exercises the generated-project build, validation, run, and
      test commands.

The Ubuntu CI workflow exercises these commands and completed green in the
hosted CI run confirmed on 2026-10-29.

---

## CNT-012 — Playable MMORPG template

- **Priority:** P1
- **Status:** Blocked — `CLT-009`, `SIM-008`
- **Depends on:** `CLT-009`, `SIM-008`, `CNT-011`
- **References:** [project guide §24](../lightweight_mmorpg_engine_project_guide.md#24-initial-vertical-slice-keep-it-small)

**Scope**

- Extend the `mmorpg` template to a playable slice: one zone, player spawn,
  movement, chat, inventory, a door, a container, a terminal/panel, one AI, and
  one lever/door link.
- Seed localized content, a demo conflict, and sensible defaults.
- Include a README/run instructions and a smoke test.
- Keep the `minimal` template minimal.

**Acceptance criteria**

- [ ] `lynx new --template mmorpg` + documented commands produces a playable
      slice.
- [ ] The template validates, builds, and passes its smoke test in CI.
- [ ] No engine source edits are required to run or tweak the slice.
- [ ] The template demonstrates the authoritative loop (move/chat/interact).

---

## CNT-013 — Asset pipeline and import policy

- **Priority:** P2
- **Status:** Ready
- **Depends on:** `CNT-005`
- **References:** [content authoring §Required fields and validation](../content-authoring.md#required-fields-and-validation)

**Scope**

- Define supported asset formats, import settings, and size/dimension budgets.
- Hash assets, validate references, and reject disallowed types.
- Generate thumbnails/previews and stable asset IDs.
- Detect missing/orphaned assets and unused-bundle growth.
- Document licensing/provenance metadata.

**Acceptance criteria**

- [ ] Assets referenced by content are validated and hashed in the manifest.
- [ ] Oversized/disallowed assets fail with an actionable diagnostic.
- [ ] The editor/preview uses the same imported asset as the runtime.
- [ ] Asset changes are included in diffs and publish gates.

---

## CNT-014 — Content migration and upgrade tooling

- **Priority:** P2
- **Status:** Ready
- **Depends on:** `CNT-011`
- **References:** ADR 0017, `PST-013`

**Scope**

- Declarative schema migrations for definitions/catalogs with dry-run and
  rollback.
- Version-aware upgrade from a deployed bundle to a candidate.
- Report entities/instances affected and incompatible cases.
- Apply migrations to content files and, where required, persisted instances.
- Integrate with `lynx migrate` and publish checks.

**Acceptance criteria**

- [ ] A dry run lists every affected definition/instance without writing.
- [ ] A failed migration leaves the project unchanged.
- [ ] Rollback restores the previous compatible content.
- [ ] CI validates migrations against a fixture project.

---

## CNT-015 — Localization tooling

- **Priority:** P2
- **Status:** Ready
- **Depends on:** `CLT-010`
- **References:** [internationalization](../internationalization.md), ADR 0011

**Scope**

- Missing-key and unused-key reports; placeholder/parameter validation.
- Pseudo-locale generation for layout testing.
- Export/import workflow for translators with review status.
- Locale completeness reporting and a release gate.
- Editor support for editing keys and seeing translations in context.

**Acceptance criteria**

- [ ] `pt-BR` completeness is enforced as the source catalog.
- [ ] Missing/extra placeholders fail validation.
- [ ] A pseudo-locale reveals truncation/overflow before release.
- [ ] Translators can work without editing Rust or engine source.

---

## CNT-016 — Calendar and faction editor overlays

- **Priority:** P2
- **Status:** Blocked — `CNT-002`, `SIM-015`
- **Depends on:** `CNT-002`, `SIM-015`
- **References:** [world simulation §World calendar](../world-simulation.md#world-calendar-and-conflicts),
  ADR 0021

**Scope**

- Schedule/edit events, sides, eligibility, objectives, and resolution rules.
- Territory/faction overlays with current ownership and history.
- Preview enrollment and conflict resolution without touching live state.
- Validation for impossible schedules/overlaps and missing sides.

**Acceptance criteria**

- [ ] An event authored in the editor runs through the gateway lifecycle.
- [ ] Preview cannot mutate the live world.
- [ ] Invalid schedules/sides fail before publish.
- [ ] Territory overlays match the authoritative state after refresh.

---

## CNT-017 — Schema reference and documentation generator

- **Priority:** P3
- **Status:** Ready
- **Depends on:** `CNT-005`
- **References:** [content authoring](../content-authoring.md)

**Scope**

- Generate a human-readable reference for definition kinds, fields, defaults,
  validation codes, and examples from the canonical schemas.
- Keep the reference in sync with code in CI (fail on drift).
- Link diagnostics to remediation guidance.
- Publish a versioned reference for each engine release.

**Acceptance criteria**

- [ ] The reference is generated, not hand-maintained.
- [ ] CI fails when schemas change without regenerating the reference.
- [ ] Every validation code has a documented remedy.
- [ ] The reference is versioned with the engine.
