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
- `backend_nakama`: native Nakama HTTP adapter with health and authentication.
- `lynx` project CLI: `new`, `doctor`, `validate`, and `content hash`.
- Native non-Docker Nakama/PostgreSQL setup scripts and SQL migrations.
- Internationalization design: key-based RON catalogs, `pt-BR` as the default
  and source language, locale negotiation, validation codes, and client-side
  formatting (ADR 0011).
