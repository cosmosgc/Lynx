# Architecture

This document describes the boundaries that the code is expected to respect.
Decisions and their rationale live in [`adr/`](adr/).

## System view

```text
┌──────────────────────── Client / Editor (Bevy) ────────────────────────┐
│ input → camera → action map → prediction → reconciliation               │
│ content loading → client ECS → rendering → entity-provided UI           │
│ network client (engine_net) → Nakama session → world gateway connection │
└───────────────────────────────┬────────────────────────────────────────┘
                                │ authenticated protocol (JSON, versioned)
┌───────────────────────────────▼────────────────────────────────────────┐
│ World service (Rust, headless)                                          │
│ world_gateway: sessions, resume tokens, action routing, replication     │
│ engine_sim:    zones, entities, action pipeline, minds, links, AI, nav  │
│ game_api:      auth/session/social/chat/world/character/inventory/admin │
└──────────────┬──────────────────────────────────┬──────────────────────┘
               │ HTTPS (health, login, storage)    │ SQL (transactions)
┌──────────────▼──────────────┐      ┌────────────▼─────────────────────┐
│ Nakama 3.41 (native)        │      │ PostgreSQL 17 (native)            │
│ accounts, sessions, chat,   │      │ characters, inventory, property,  │
│ groups, matchmaking         │      │ factions, audit, content manifest │
└─────────────────────────────┘      └───────────────────────────────────┘
```

## Crates and dependency direction

```text
engine_core   ← engine_sim ← game_api ← backend_nakama
     ↑              ↑
engine_content ─────┘
     ↑
engine_net (protocol envelopes; no transport implementation)

world_gateway → engine_core, engine_sim, engine_net, game_api
project_cli → engine_content, engine_core, backend_nakama
```

Rules:

- `engine_core` depends on no other workspace crate.
- `engine_sim` depends on `engine_core` only. It has no I/O, no clock reads, no
  Bevy, and no database driver.
- `engine_content` depends on `engine_core` and owns RON parsing, manifests,
  and content validation.
- `engine_net` owns protocol envelopes, interest management, and version
  negotiation. It does not own a socket, a runtime, or a Nakama type.
- `game_api` defines backend interfaces and DTOs. It depends on `engine_core`,
  never on Nakama.
- `world_gateway` composes the simulation, protocol, and backend DTOs into
  session lifecycle, action routing, and per-session replication. It owns no
  socket, runtime, or clock; a transport adapter supplies authenticated claims
  and explicit timestamps.
- `backend_nakama` is the only crate that knows Nakama's HTTP API.
- Client/editor crates (introduced with Bevy) depend on the crates above and
  never the reverse.

## Key domain concepts

| Concept | Type | Notes |
|---|---|---|
| Durable identity | `StableId` | Prefixed ULID-like; 48-bit ms + 80-bit random |
| Authored key | `DefinitionId` | Validated slug, e.g. `door.wooden` |
| Definition | content records | Immutable per content version |
| Instance | `EntityRecord` | Mutable, zone-scoped, stable ID |
| Component schema | `ComponentSchema` | Version, fields, visibility, policy |
| Mind | `Mind` + controller | Player, AI, scripted, or none |
| Action | `ActionRequest` → `ActionResult` | Server-validated, structured errors |
| Link | `EntityLink` | Typed, stable IDs, validated endpoints |
| Faction relation | `FactionRelation` | Directional, -100..100 |
| Locale | BCP 47 tag | Default `pt-BR`; canonicalized at boundaries |
| Localized text | message key + catalog | Source catalog is `pt-BR` |
| Interest set | `InterestSet` | Per-session visible entities, distance + visibility |
| Session | `GatewaySession` | Server-issued ID, authenticated principal, actor, staff flag |
| Resume token | `ResumeToken` | Server-issued, short-lived, single-use, redacted |
| Delivery | `SessionDelivery` | One session's interest delta and filtered batch per tick |

## Request lifecycle

1. A transport authenticates the player and supplies `SessionClaims` (account,
   character principal, staff flag) to `WorldGateway::connect`. The client
   never chooses any of these values.
2. The gateway negotiates the handshake, resolves the principal's actor in the
   zone, and binds a player controller. A character already connected returns
   `Conflict`; an incompatible handshake creates no session.
3. The client sends an `action.request` whose body carries only intent: verb,
   target, parameters, and an optional client tick. The gateway rejects any
   body that names an actor, principal, session, or other unknown field.
4. The gateway scopes the client request ID with the session ID and calls
   `Zone::apply_action`. `ActionPipeline` validates target existence, zone,
   range, visibility, permission, component state, cooldown, required items,
   and action-specific preconditions.
5. On success the action mutates authoritative state, emits domain events, and
   writes audit records for privileged or high-impact operations. On failure it
   returns a stable `ErrorCode` and typed parameters; the client renders the
   message from its locale catalog.
6. The server replies with `action.result` and, on the next tick, one
   `SessionDelivery` per session containing only that session's interest delta
   and audience-filtered replication. Clients reconcile predicted state with
   the authoritative result.

## Localization

Player-facing strings are addressed by stable message keys, and the source
catalog is Brazilian Portuguese (`pt-BR`). Definitions, UI panels, and server
responses carry keys and parameters, never embedded English text. The server
keeps English only for logs and audit records. Locale resolution, catalog
format, validation rules, and formatting policy are specified in
[`internationalization.md`](internationalization.md) and
[ADR 0011](adr/0011-internationalization-and-locale-defaults.md).

## Simulation model

- A `Zone` owns an `EntityStore`, a tick counter, a nav grid, and budgets.
- `tick()` advances controllers, applies queued validated actions, expires
  cooldowns and temporary modifiers, and produces `TickStats`.
- Simulation is deterministic given the same inputs, IDs, and tick sequence.
  Time-dependent features take an explicit `now_ms` argument.
- Entity removal is a tombstone transition so links and audit records never
  dangle silently. Missing link endpoints are reported as diagnostics.

## Persistence model

- Character, inventory, property, faction, and audit data live in PostgreSQL.
- World snapshots store changed entities per zone; the event log captures
  economy and ownership transitions.
- Writes that can be retried carry idempotency keys stored in a ledger table.
- Content definitions are versioned bundles; instances record the content
  version they were created from.

## Failure and recovery

- Structured errors use `ErrorCode`, never free-form strings alone.
- Missing or obsolete component data loads through the migration chain; if a
  migration is impossible, the entity is quarantined and reported instead of
  silently reset.
- Zone restart: load last snapshot, replay events after the snapshot, validate
  references, then accept sessions. Target recovery: 60 seconds.
- Admin actions and reputation changes are appended to the audit log in the
  same transaction as the mutation.

## What is deliberately not here yet

- Bevy client crates, rendering, input, and camera modules (Phase 1 client
  work).
- A transport and process loop around `world_gateway` — WebSocket or UDP
  framing, rate limiting, and Nakama token refresh. The gateway library itself
  is transport-agnostic and tested; `lynx-world` is not yet a binary.
- A durable session store. Resume windows live in memory, so a zone restart
  still requires players to log in again.
- The editor application, behavior graph editor, and admin spectator client.
- WebAssembly scripting, which needs a sandbox design review before adoption.
