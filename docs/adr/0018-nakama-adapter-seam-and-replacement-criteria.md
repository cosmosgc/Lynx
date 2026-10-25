# ADR 0018: Keep Nakama behind a backend seam

- **Status:** Accepted
- **Date:** 2026-10-24

## Context

Lynx needs authentication and social platform capabilities without letting a
vendor SDK define gameplay behavior. Nakama provides the selected account,
session, social, chat, group, and matchmaking capabilities. The authoritative
simulation and durable world state remain outside Nakama.

The existing `game_api` traits define the integration boundary, and
`backend_nakama` implements the production HTTP adapter. Tests and offline
development also need an implementation that does not require a running
Nakama or PostgreSQL service.

## Decision

- Keep Nakama as the selected backend for authentication, sessions, and
  platform social features. Do not replace it with a custom Rust backend.
- Keep gameplay-facing backend operations behind `game_api` contracts. The
  Nakama adapter may translate those contracts to Nakama HTTP APIs, but Nakama
  types and protocol details must not leak into simulation or gameplay code.
- Keep authoritative zone simulation in the Rust world service. PostgreSQL
  migrations and Lynx-owned persistence define durable game state; Nakama
  storage is not the source of truth for world state.
- Provide `backend_memory` as an in-memory implementation for tests and offline
  development. It is not an identity provider: it has no durable storage,
  password verification, cryptographic token generation, or production
  security guarantees. It must not authenticate real players.
- Add backend interfaces as capabilities are needed, preserving the same
  adapter boundary for character, inventory, social, chat, and administration
  operations.

## Replacement criteria

Reconsider Nakama only when there is measured evidence that one or more
requirements cannot be met acceptably by the chosen Nakama release, such as:

1. A required authentication or social capability cannot be implemented with
   supported APIs or a maintainable adapter.
2. Verified availability, scaling, operating cost, or deployment constraints
   miss documented project targets after configuration and capacity tuning.
3. Data ownership, exportability, compliance, or platform support creates a
   material blocker that cannot be mitigated while retaining Nakama.

Before approving a replacement, document the measured gap and alternatives;
demonstrate an adapter-compatible implementation of every required
`game_api` capability; prove account/session and required social-data migration
or export; and run functional, load, failure-recovery, and operational tests
against the target deployment. A replacement must not move authoritative
simulation or world-state ownership into a backend merely to simplify the
integration.

## Consequences

- Nakama remains the supported production adapter, while `game_api` limits
  coupling and makes focused alternatives testable.
- Offline tests can exercise authentication and world-service contract behavior
  without claiming production-grade identity security.
- Replacing the platform requires evidence, a data migration plan, and
  contract-level parity rather than a speculative backend rewrite.
