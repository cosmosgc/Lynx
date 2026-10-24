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
- `lynx-world`: runnable Axum/Tokio WebSocket world service with hashed static
  bearer auth, `/healthz`, `/readyz`, a `session.hello` handshake, one
  `world.token` control message per admission, per-connection backpressure, a
  tick loop, and graceful shutdown. The demo zone spawns one character per
  configured token hash; state is in memory.
- `world_server` tests and a `lynx-world` WebSocket integration test covering
  handshake, ready/snapshot/token delivery, correlated errors, resume, and
  session expiry.
- `engine_core::ErrorCode::SessionExpired` for expired or unknown world
  sessions; `game_api::ApiError::SessionExpired` now maps to it.
- `backend_nakama`: native Nakama HTTP adapter with health and authentication.
- `lynx` project CLI: `new`, `doctor`, `validate`, and `content hash`.
- Native non-Docker Nakama/PostgreSQL setup scripts and SQL migrations.
- Internationalization design: key-based RON catalogs, `pt-BR` as the default
  and source language, locale negotiation, validation codes, and client-side
  formatting (ADR 0011).
