# Lynx

A lightweight, data-driven multiplayer game engine and reference MMORPG built
with Rust, Bevy, and Heroic Labs Nakama.

Lynx is two deliverables in one repository:

1. **The engine/toolkit** — authoritative simulation, content pipeline,
   networking adapters, UI and camera modules, world editor, and operations
   tooling.
2. **The reference game** — a small, playable MMORPG that exercises the engine
   and keeps its architecture honest.

The project guide lives at
[`docs/lightweight_mmorpg_engine_project_guide.md`](docs/lightweight_mmorpg_engine_project_guide.md).
The cross-system design for faction parity, territory, scheduled world events,
map authoring, and engine upgrades is in
[`docs/world-simulation.md`](docs/world-simulation.md).
Architecture decisions are recorded in [`docs/adr/`](docs/adr/), and the
localization model is described in
[`docs/internationalization.md`](docs/internationalization.md).

## At a glance

| Metric | Value |
| --- | --- |
| Workspace crates | 11 (10 crates + the `lynx` CLI) |
| Rust edition / toolchain | 2024 edition, pinned to Rust **1.99** via `rust-toolchain.toml` |
| Source code | ~33,000 LOC across 81 `.rs` files |
| Tests | 353 test functions (unit + integration), zero unsafe code (`unsafe_code = "forbid"`) |
| ADRs | 21 accepted architecture decision records |
| Platform database | 5 SQL migrations (Nakama/PostgreSQL) |
| Docs | 11 design docs + full ADR index (see [Documentation](#documentation)) |

## Status

Early implementation — the server foundation is real and tested, the game
client is not started yet. The repository currently contains the Phase 0/1
foundation plus a runnable headless world service: pinned toolchain, workspace
layout, stable entity IDs, versioned component schemas, validated world-map
atlases with explicit zone routes, faction governance, treasury, and territory
rules, a durable world calendar with bounded faction conflicts, enrollment, and
deterministic territory-changing results, inventory domain rules, a headless
authoritative simulation with action validation, versioned protocol envelopes,
per-session interest management, a transport-agnostic gateway with resumable
sessions and interest-filtered replication, a framed connection loop, a
WebSocket `lynx-world` binary, and the `lynx` project CLI.

**Not started:** Bevy client crates (rendering, input, camera), the world
editor, and durable world persistence — world state is currently in memory.

Brazilian Portuguese (`pt-BR`) is the default language for generated projects,
templates, and player-facing content. See
[`docs/internationalization.md`](docs/internationalization.md).

Nothing in this repository should be treated as production-ready yet. See
[`docs/platforms.md`](docs/platforms.md) for the supported matrix, which is
intentionally small until each target is tested in CI.

## Progress

The roadmap in the project guide ([§23 — Suggested implementation
roadmap](docs/lightweight_mmorpg_engine_project_guide.md#23-suggested-implementation-roadmap))
is the source of truth for phase scope and exit criteria; this section tracks
perceived progress against it. Percentages are engineering estimates; the
detailed record of what shipped is [`CHANGELOG.md`](CHANGELOG.md).

### Overview

```text
Engine project · 9 phases · last reviewed: October 2026

Phase 0  Architecture proof          [██████████████████] 100%  Complete
Phase 1  Engine skeleton             [███████████████░░░]  85%  In progress — Bevy client work pending
Phase 2  Authoritative simulation    [███████████████░░░]  85%  In progress — durable persistence pending
Phase 3  Interaction & minds         [█████████████░░░░░]  75%  In progress
Phase 4  UI & minigames              [███████████░░░░░░░]  65%  In progress
Phase 5  Editor & content pipeline   [█████░░░░░░░░░░░░░]  30%  In progress
Phase 6  Live operations             [███░░░░░░░░░░░░░░░]  20%  Early
Phase 7  MMORPG vertical slice       [█░░░░░░░░░░░░░░░░░]  10%  Blocked on client
Phase 8  Expand & stabilize          [██░░░░░░░░░░░░░░░░]  15%  Selected systems delivered early
```

### Phase 0 — Architecture proof · Complete

- [x] Pin Rust, Bevy, Nakama, and PostgreSQL versions (ADR 0001, 0003;
      `rust-toolchain.toml` pins Rust 1.99).
- [x] Validate native, non-Docker Nakama setup on target development OSes
      ([`docs/getting-started.md`](docs/getting-started.md), `scripts/`, `BAT/`).
- [x] Decide the authoritative simulation topology (ADR 0002, 0004).
- [x] Minimal login path and server health checks (`/healthz`, `/readyz`,
      hashed bearer auth, `backend_nakama` device-auth client).
- [x] Architecture decision records for key choices (21 accepted ADRs).

### Phase 1 — Engine skeleton · In progress

- [x] Workspace and crate boundaries (see [Repository layout](#repository-layout)).
- [x] Project CLI and minimal template (`lynx new`, `lynx doctor`).
- [x] Stable IDs, schema definitions, and content loading (ADR 0007, 0006).
- [x] Network protocol adapter and structured logging (`engine_net`, `tracing`).
- [ ] Basic Bevy scene and input abstraction — next up; Bevy client crates
      arrive in the next phase of work.
- [ ] Exit criterion: a generated project opens a playable scene and runs the
      test pipeline (server-side pipeline already runs; scene is pending).

### Phase 2 — Authoritative entity simulation · In progress

- [x] Server-owned entity IDs and component state; clients send requests,
      never outcomes.
- [x] Spawn/despawn in the headless zone simulation (`engine_sim`).
- [x] Replication with per-session interest sets, hysteresis, visibility
      classes, and deterministic entity caps (ADR 0012).
- [x] One zone plus reconnect recovery: resumable sessions, single-use rotated
      resume tokens, session-scoped idempotency (ADR 0013).
- [x] Server-side action validation: range, permission, state, and cooldown
      checks proven by tests.
- [~] Persistence: Postgres migrations 0001–0005 exist and backend adapters
      are in place, but the runnable world service is still memory-only.
- [ ] Exit criterion: two graphical clients observing consistent state
      (headless integration tests cover consistency; clients are pending).

### Phase 3 — Interaction and mind system · In progress

- [x] Interactable components and verbs (`engine_sim::action`).
- [x] Mind system: every entity may have a mind; possession and control
      transfer rules are validated by tests.
- [x] Inventory transfers with idempotency and container rules.
- [x] Typed entity links with cycle detection (lever-controlled doors).
- [x] Faction definitions, alignment thresholds, and reputation resolution
      (ADR 0009); governance, treasury, and territory ship early from Phase 8
      scope (ADR 0020, migration 0004).
- [~] Basic chat and presence: `ChatService`/`SocialService` exist in
      `game_api` and the in-memory backend; world-level chat wiring is pending.

### Phase 4 — UI and minigame framework · In progress

- [x] Entity-provided UI schemas: declarative panels with namespaced schemas,
      localized labels, allowed actions, and field bindings (ADR 0015).
- [x] Server-owned UI panel session state: `ui.open`/`ui.action`/`ui.close`
      run through the normal action pipeline, so permission, range,
      capabilities, and cooldowns are re-validated.
- [x] One server-validated minigame: combination lock with per-principal
      attempt budget and tick-based lockout; progress never replicated
      (ADR 0016).
- [~] Inventory, terminal, dialogue, and crafting panels: the panel framework
      is generic and validated, but most concrete panel definitions are not
      authored yet.
- [x] Exit criterion (headless): different entity types open different
      interfaces through one common protocol; the viewport/editor exit
      criterion is pending client work.

### Phase 5 — Editor and content pipeline · In progress

- [x] Canonical RON content formats, manifest hashing, and validation gates
      ([`docs/content-authoring.md`](docs/content-authoring.md)).
- [x] Authored world-map atlases with explicit `passage`/`gateway`/`portal`
      routes (ADR 0019).
- [x] Bundles, versioning, and diffs: compatibility-aware semantic bumping,
      publish gating via `lynx content diff` / `lynx content publish-check`
      (ADR 0017).
- [ ] World atlas/map editor, inspector, snapping, undo/redo.
- [ ] Item/entity definition editor and link-graph editor.
- [ ] User-content schemas, ownership metadata, moderation states.
- [ ] Optional MCP authoring tools ([`docs/ai-tooling.md`](docs/ai-tooling.md)
      records the planned read-only-first surface).
- [ ] Behavior graph editor and runtime debugger; navigation
      authoring/visualization.

### Phase 6 — Live operations · Early

- [x] Audit events and structured error codes as the observability base.
- [x] Admin service seam with read-only views in the backend adapters.
- [x] Content/protocol compatibility checks at boundaries (manifest versioning).
- [~] Role-based admin actions documented in
      [`docs/security.md`](docs/security.md); moderation UI not built.
- [ ] Admin spectator mode and live metrics/read-only inspection tooling.
- [ ] Staging publish, rollout, and rollback automation.
- [ ] Engine/content upgrade workflow for generated projects.

### Phase 7 — MMORPG vertical slice · Blocked on client

- [~] A runnable WebSocket world service with a demo zone (characters, doors,
      barrels, owner-only crate, locked vault, and one publishable faction
      conflict) is playable headless today.
- [~] Device authentication exists in `backend_nakama`; character selection
      and the in-game flow await the client.
- [ ] One polished zone with movement, chat, inventory, and persistence in a
      graphical client.
- [ ] Performance profiling and abuse testing at scale.
- [ ] Packaging and the first supported-platform matrix
      ([`docs/platforms.md`](docs/platforms.md)).

### Phase 8 — Expand and stabilize · Selected systems delivered early

Faction governance, treasury, territory, calendar, and bounded conflicts were
pulled forward from this phase and are shipped and tested (ADR 0020, 0021;
migrations 0004, 0005) — see [`docs/world-simulation.md`](docs/world-simulation.md)
for what is implemented versus design-only. The gateway now routes calendar
enrollment and staff lifecycle commands over the wire, and the runnable demo
service publishes one open conflict at startup.

- [x] Player/NPC faction parity groundwork, faction wealth, territory.
- [x] Scheduled, interruptible world events with deterministic capture.
- [~] Calendar command routing through the world gateway and the demo
      service: commands, inspection, replay protection, and paired
      persistence are live; editor overlays and event objectives stay
      design-only.
- [x] Internationalization framework (`pt-BR` default; ADR 0011).
- [ ] Guilds, quests, progression, economy, trading, parties.
- [ ] Additional zones, loot systems, and moderation tooling.
- [ ] Load testing, backups, operational drills, and release automation.
- [ ] Additional platform support after automated validation.

## Quick start

Prerequisites: Rust 1.99 (installed automatically by `rustup` through
`rust-toolchain.toml`). No Docker is required at any point.

```bash
# Build and test the engine workspace
cargo test --workspace

# Create and check a new game project
cargo run -p project_cli -- new my-world --template minimal --output ../my-world
cargo run -p project_cli -- doctor --project ../my-world

# Run the world service. Configure a hash, not a plaintext token.
printf '%s' 'replace-with-a-long-random-token' | cargo run -p lynx-world -- token-hash
cargo run -p lynx-world -- serve --bind 127.0.0.1:8090 --token-hash <sha256-hex>
# Add --staff on loopback to drive calendar lifecycle commands (demo only).
```

The service exposes `GET /healthz`, `GET /readyz`, and the WebSocket endpoint
`GET /world` with `Authorization: Bearer <token>`. The first frame must be a
`session.hello` envelope. The demo publishes one open faction conflict over
the demo zone; player sessions enroll through `calendar.command` and inspect
it with `calendar.query` (lifecycle transitions require `--staff`). Wire
kinds and reconnection rules are documented in
[`docs/networking.md`](docs/networking.md); operation flags and shutdown
behavior are in [`docs/operations.md`](docs/operations.md). There is no client
crate yet, and world state is in memory.

### The `lynx` CLI

```text
lynx new <name> --template minimal|mmorpg --output <dir>   Scaffold a project
lynx doctor [--project <dir>] [--backend]                  Report key diagnostics
lynx validate --project <dir>                              Enforce all validation gates
lynx content hash <path>                                   Content manifest hashing
lynx content diff --base <manifest> --next <manifest>      Publish-gate comparison
lynx content publish-check --manifest <m> --base <b>       Full publish review
```

The CLI is also exposed as `lynx` when installed with
`cargo install --path tools/project_cli`.

Generated projects include a Brazilian Portuguese catalog and default to
`pt-BR`; additional locales are opt-in translations.

Nakama and PostgreSQL are optional for engine development and required only
for multiplayer work. The native, non-Docker setup is documented in
[`docs/getting-started.md`](docs/getting-started.md); Windows helper scripts
live in `BAT/` and portable PowerShell shell scripts in `scripts/`.

## Repository layout

```text
crates/engine_core      Stable IDs, schemas, factions, world events, inventory, audit, errors
crates/engine_sim       Headless authoritative zone simulation and action pipeline
crates/engine_content   Content definitions, world maps, manifests, validation, versioning
crates/engine_net       Protocol envelopes, framing, replication categories, interest management
crates/world_gateway    Sessions, resume tokens, calendar command routing, per-session replication
crates/world_server     Connection IDs, wire messages, and delivery envelopes
crates/lynx_world       Runnable WebSocket world service (`lynx-world`)
crates/backend_nakama   Native Nakama HTTP adapter (no Docker required)
crates/backend_memory   Non-persistent test/offline adapter (not production auth)
crates/game_api         Backend service interfaces and gameplay DTOs
tools/project_cli       `lynx` project scaffolding, doctor, and validation
server/                 Native Nakama config, SQL migrations, run scripts
scripts/                Portable PowerShell/sh setup, migration, and run scripts
BAT/                    Windows batch helpers (build, test, doctor, format, ...)
docs/                   Architecture, operations, security, localization, ADRs
.github/                Cross-platform CI workflows
```

Note: engine templates and canonical schemas are generated/validated by the
CLI in code (`tools/project_cli`), not stored as loose directories at the repo
root.

## Design rules

- The server is authoritative; clients send requests, never outcomes.
- Client messages carry intent and correlation, never identity: actor,
  principal, staff role, session ID, view radius, and resume tokens are
  server-issued.
- The transport authenticates first and hands trusted claims to the gateway.
  Configuration stores hashes of credentials where possible, never plaintext
  secrets.
- Gameplay is composed from versioned data, not inheritance trees.
- Every persistent entity has a stable ID and an explicit persistence policy.
- Every entity may have a mind; a player is one controller among many.
- Content and protocol versions are explicit and checked at boundaries.
- Player-facing text is localized; Brazilian Portuguese is the default language.
- Tools, diagnostics, and recovery paths are part of the product.
- `unsafe` is forbidden at workspace level. Docker is optional; native
  operation is the supported baseline.

## Documentation

| Doc | Covers |
| --- | --- |
| [`docs/lightweight_mmorpg_engine_project_guide.md`](docs/lightweight_mmorpg_engine_project_guide.md) | Master guide: vision, tech baseline, phase roadmap, definition of done. |
| [`docs/architecture.md`](docs/architecture.md) | Current architecture, crate responsibilities, failure/recovery targets. |
| [`docs/getting-started.md`](docs/getting-started.md) | Native setup: engine workspace, Nakama, PostgreSQL — no Docker. |
| [`docs/networking.md`](docs/networking.md) | Protocol envelopes, wire kinds, version handshake, reconnect/resume. |
| [`docs/operations.md`](docs/operations.md) | Environments, configuration, backups, shutdown, rollout. |
| [`docs/security.md`](docs/security.md) | Threat model, authority invariants, moderation rules. |
| [`docs/content-authoring.md`](docs/content-authoring.md) | RON formats, kind directories, validation gates, bundles/manifests. |
| [`docs/world-simulation.md`](docs/world-simulation.md) | Faction parity, territory, world events, calendar, map authoring. |
| [`docs/internationalization.md`](docs/internationalization.md) | `pt-BR` defaults, key-based catalogs, locale negotiation. |
| [`docs/platforms.md`](docs/platforms.md) | Which targets are supported and how they are CI-tested. |
| [`docs/ai-tooling.md`](docs/ai-tooling.md) | Optional MCP tooling policy and shell-execution rules. |
| [`docs/adr/`](docs/adr/) | 21 architecture decision records (0001–0021). |

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md). Commit early and often; keep the
workspace building and tested at every commit. Every commit should pass:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## License

MIT. See [`LICENSE`](LICENSE).
