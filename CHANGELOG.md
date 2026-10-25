# Changelog

All notable changes to Lynx are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
semantic versioning for engine releases and content bundles.

## [Unreleased]

### Added

- Phase 0/1 workspace foundation: pinned Rust 1.99 toolchain, workspace lints,
  and crate boundaries.
- Architecture decision records for toolchain, backend topology, persistence,
  content formats, IDs, and authority.
- `engine_core`: stable prefixed IDs, versioned component schemas with
  migrations, faction alignment and reputation resolution, inventory transfers
  with idempotency, structured errors, and audit events.
- `engine_sim`: headless authoritative zone, action pipeline, minds and
  possession, typed entity links with cycle detection, bounded grid pathfinding,
  and faction-aware ally assistance.
- `engine_content`: RON content definitions, manifest hashing, and validation.
- `engine_content`: declarative UI panel and minigame definitions. Panels
  declare a namespaced schema, localized labels, allowed actions, field
  bindings, and an optional fallback for read-only clients. Minigames declare
  their panel schema, explicit `server`/`hybrid`/`client` authority, and
  reward references. Validation rejects malformed IDs and keys, duplicate
  actions or fields, missing fallbacks, unknown rewards, and
  client-authoritative minigames that grant rewards. The loader reads
  `content/ui/`, `content/panels/`, and `content/minigames/`, and bundle
  manifests record the `ui` and `minigame` kinds.
- `engine_content::diff`: manifest version compatibility for publishing. The
  diff reports added, removed, and changed definitions and locales, computes the
  smallest safe semantic version bump, and rejects regressions, too-small
  content bumps, and definition byte changes without a definition version bump.
  Removing a definition or locale, or changing a definition kind, requires a
  major bump.
- `lynx content diff --base <manifest> --next <manifest>`: human-readable and
  `--json` manifest comparison for publish gates and CI.
- `engine_net` and `game_api`: protocol envelopes and backend adapter traits.
- `engine_net::interest`: per-session interest sets with entry/exit hysteresis,
  visibility classes, deterministic entity caps, audience filtering for every
  replication category, and fail-closed handling of unattributable updates.
- `world_gateway`: transport-agnostic session lifecycle with server-issued
  identities, per-character connection limits, single-use rotated resume
  tokens, session-scoped idempotency, envelope routing, dirty-entity tracking,
  and interest-filtered per-session replication (ADR 0013).
- `engine_net::framing`: four-byte big-endian length-prefixed envelope frames
  with a 64 KiB cap, strict truncation handling, and clean-EOF rules for byte
  streams (ADR 0014).
- `world_server`: connection IDs, `world.ready`/`world.snapshot`/`world.delta`
  wire kinds, snapshot/delta encoding, action reply routing, and stale-session
  closure on top of the gateway, with no socket or runtime (ADR 0014).
- `world_gateway::WorldGateway::close_session` for immediate revocation without
  a resume window.
- `world_gateway`: server-owned UI panel session state. A validated `UiOpened`
  effect becomes a session `OpenPanel`; `ui.action` and `ui.close` operate on
  that server-resolved target and `ui.state` carries the authoritative tick and
  panel (ADR 0015). Panel actions run through the normal action pipeline, so
  range, visibility, permission, capabilities, and cooldowns are re-validated.
  Panel state clears on disconnect, resume, close, or provider loss.
- `engine_sim::action::Verb::parse` for wire and panel action names, including
  bounded custom names.
- `engine_sim::minigame`: a server-authoritative combination lock with a
  combination derived from a server-held seed and the authenticated principal,
  per-digit higher/lower/exact hints, a per-principal attempt budget, and a
  server-tick lockout. Solving unlocks the provider's door and closes the panel.
  Minigame progress is never replicated (ADR 0016).
- `lynx-world`: runnable Axum/Tokio WebSocket world service with hashed static
  bearer auth, `/healthz`, `/readyz`, a `session.hello` handshake, one
  `world.token` control message per admission, per-connection backpressure, a
  tick loop, and graceful shutdown. The demo zone spawns one character per
  configured token hash, plus a door, barrel, owner-only crate, and locked
  lockpick vault; state is in memory.
- `world_server` tests and a `lynx-world` WebSocket integration test covering
  handshake, ready/snapshot/token delivery, correlated errors, resume, and
  session expiry.
- `engine_core::ErrorCode::SessionExpired` for expired or unknown world
  sessions; `game_api::ApiError::SessionExpired` now maps to it.
- `backend_nakama`: native Nakama HTTP adapter with health and authentication.
- `backend_memory`: non-persistent backend adapter for tests and offline
  development; it is explicitly not a production identity provider.
- `lynx` project CLI: `new`, `doctor`, `validate`, `content hash`, and
  `content diff`, with minimal and MMORPG templates containing validated
  starter zones, entities, items, interactions, UI panels, and a `pt-BR`
  source catalog.
- Cross-platform CI for Rust formatting, Clippy, tests, builds, generated
  project validation, ShellCheck, and PostgreSQL 17 migrations.
- Native non-Docker Nakama/PostgreSQL setup scripts and SQL migrations.
- Internationalization design: key-based RON catalogs, `pt-BR` as the default
  and source language, locale negotiation, validation codes, and client-side
  formatting (ADR 0011).
