# Architecture

This document describes the boundaries that the code is expected to respect.
Decisions and their rationale live in [`adr/`](adr/).
The system view and recovery model also include target capabilities; the
implementation gaps are listed below and in the [backlog](TODO/README.md).

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
│ lynx-world:    WebSocket/TLS edge, health, tick loop, shutdown          │
│ world_server:  connection IDs, wire kinds, snapshot/delta envelopes     │
│ world_gateway: sessions, resume tokens, action routing, replication     │
│ engine_sim:    zones, entities, action pipeline, minds, links, AI,      │
│                nav, minigames                                           │
│ game_api:      auth/session/social/chat/world/character/inventory/admin │
└──────────────┬──────────────────────────────────┬──────────────────────┘
               │ HTTPS (health, login, storage)    │ SQL (transactions)
┌──────────────▼──────────────┐      ┌────────────▼─────────────────────┐
│ Nakama 3.41 (native)        │      │ PostgreSQL 17 (native)            │
│ accounts, sessions, chat,   │      │ characters, inventory, property,  │
│ groups, matchmaking         │      │ factions, audit, content manifest │
└─────────────────────────────┘      └───────────────────────────────────┘
```

The [Nakama capability matrix](nakama.md) distinguishes features supplied by
Nakama from the smaller set currently integrated into Lynx. It is the source of
truth for platform-feature ownership and implementation status.

## Crates and dependency direction

```text
engine_core   ← engine_sim ← game_api ← backend_nakama
     ↑                              ← backend_memory
engine_content
     ↑
engine_net (protocol envelopes; no transport implementation)

world_gateway → engine_core, engine_sim, engine_net, game_api
world_server  → world_gateway, engine_net, engine_core
lynx_world    → world_server, world_gateway, engine_sim, engine_net, game_api
project_cli → engine_content, engine_core, backend_nakama
```

Rules:

- `engine_core` depends on no other workspace crate.
- `engine_sim` depends on `engine_core` only. It has no I/O, no clock reads, no
  Bevy, and no database driver.
- `engine_content` depends on `engine_core` and owns RON parsing, manifests,
  content validation, and manifest version compatibility for publishing.
- `engine_net` owns protocol envelopes, interest management, and version
  negotiation. It does not own a socket, a runtime, or a Nakama type.
- `game_api` defines backend interfaces and DTOs. It depends on `engine_core`,
  never on Nakama.
- `backend_memory` implements the shared backend traits for tests and offline
  development. Its ephemeral, predictable tokens are not suitable for real
  player authentication.
- `world_gateway` composes the simulation, protocol, and backend DTOs into
  session lifecycle, action routing, server-owned UI panel state, per-session
  replication, and the world calendar: `calendar.command`/`calendar.result`
  carry replay-protected calendar commands, `calendar.query`/`calendar.state`
  expose the player-visible calendar, and applied snapshots pass through a
  trusted `CalendarPersistence` seam. It owns no socket, runtime, or clock; a
  transport adapter supplies authenticated claims and explicit timestamps.
- `world_server` adds connection IDs, `world.ready`/`world.snapshot`/
  `world.delta` encoding, and connection-close bookkeeping on top of
  `world_gateway`. It still owns no socket, runtime, or clock.
- `lynx_world` is the runnable service: Axum/Tokio WebSocket transport, static
  hashed-token admission, health endpoints, the tick loop, and graceful
  shutdown. It publishes one open demo conflict over the demo zone and commits
  calendar/faction snapshots into the in-memory backend. With `--state-file`
  or `--state-postgres`, it loads a whole-world snapshot at startup and rewrites
  it on an interval and at shutdown: `engine_sim`'s versioned `ZoneSnapshot`
  carries persistent entities, links, cooldowns, idempotency caches, and
  transfer receipts, and `lynx_world::persistence` pairs it with the calendar
  and faction snapshots behind a trusted `SnapshotStore` seam. The PostgreSQL
  adapter uses the same `LYNX_PG_*` environment contract as the migration
  tooling; the file store writes atomically to a sibling temp file before
  replacing the active snapshot. `lynx_world::startup` selects between a saved
  world and the authored demo; `--fresh` always selects the authored demo and
  writes it over the snapshot, giving operators a one-flag reset that play can
  never contaminate. All gameplay authority stays in the crates below it.
- `backend_nakama` is the only crate that knows Nakama's HTTP API.
- Client/editor crates (introduced with Bevy) depend on the crates above and
  never the reverse.

## Key domain concepts

| Concept | Type | Notes |
|---|---|---|
| Durable identity | `StableId` | Prefixed ULID-like; 48-bit ms + 80-bit random |
| Authored key | `DefinitionId` | Validated slug, e.g. `door.wooden` |
| Definition | content records | Immutable per content version |
| World map | `WorldMapDefinition` | Authored zone placements, cells, and explicit routes |
| Instance | `EntityRecord` | Mutable, zone-scoped, stable ID |
| Component schema | `ComponentSchema` | Version, fields, visibility, policy |
| Mind | `Mind` + controller | Player, AI, scripted, or none |
| Action | `ActionRequest` → `ActionResult` | Server-validated, structured errors |
| Link | `EntityLink` | Typed, stable IDs, validated endpoints |
| Faction relation | `FactionRelation` | Directional, -100..100 |
| Faction | `Faction` | Controller, policy, members, treasury, and power |
| Treasury | `Treasury` | Append-only signed ledger; balance is derived |
| Territory | `TerritoryBook` | Per-zone owner, expiry, contest state, and history |
| World event | `WorldEvent` | Published schedule, two sides, rules, and lifecycle status |
| World calendar | `WorldCalendar` | Durable events, transitions, enrollment, and results |
| Conflict result | `ConflictResolution` | Versioned inputs, scores, outcome, and capture proof |
| Calendar commands | `CalendarCommands` | Replay-protected participant and staff intents over one calendar |
| Calendar stage | `CalendarStage` | Gateway-owned calendar + faction world; executes commands and serves `calendar.state` |
| Locale | BCP 47 tag | Default `pt-BR`; canonicalized at boundaries |
| Localized text | message key + catalog | Source catalog is `pt-BR` |
| Interest set | `InterestSet` | Per-session visible entities, distance + visibility |
| Session | `GatewaySession` | Server-issued ID, authenticated principal, actor, staff flag |
| Open panel | `OpenPanel` | Server-resolved target, schema, and title for one session |
| Minigame | `Minigame` | Server-held combination, per-principal attempts, and lockout |
| Resume token | `ResumeToken` | Server-issued, short-lived, single-use, redacted |
| Delivery | `SessionDelivery` | One session's interest delta and filtered batch per tick |

## Persistent world dynamics

The shared faction, territory, event-calendar, and engine-upgrade design is
specified in [`world-simulation.md`](world-simulation.md). These capabilities
extend the existing authoritative simulation and persistence boundaries.

- Authored map topology and stable zone IDs are content. `engine_content`
  validates world-map placements, explicit routes, and zone references (ADR
  0019). Territory ownership, faction treasury, event lifecycle, and event
  results remain mutable durable state. Faction ownership, wealth, and territory
  history are implemented in `engine_core::faction` (ADR 0020); the world
  calendar, enrollment, and bounded conflict resolution are implemented in
  `engine_core::world_event` (ADR 0021).
- A guild is a player-governed faction. Player- and NPC-governed factions use
  the same domain operations; controller type does not confer authority.
- Faction membership, leadership succession, treasury spending, and territory
  captures are permission-checked and idempotent. A transfer or ownership change
  either commits fully or leaves every affected faction unchanged.
- The server validates faction actions, event enrollment, conflict
  participation, and outcomes. Calendar and faction state are cloned before a
  conflict result is applied, so a failed resolution leaves both unchanged and a
  replay returns the original result. A successful attacker victory captures the
  territory through the trusted faction path and records the proof in the
  result.
- Engine/runtime releases and content bundles have distinct versions and
  compatibility checks. Games adopt releases through controlled upgrades;
  breaking changes require explicit migration rather than silent live mutation.

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
7. If the action opens an entity-provided interface, the gateway records the
   server-resolved target, schema, and title as the session's open panel and
   sends `ui.state`. Later `ui.action` and `ui.close` messages operate only on
   that stored panel and re-run the normal range, visibility, and permission
   checks.
8. A minigame guess is a `ui.action` with the `play` verb. The simulation
   compares it against a combination derived from a server-held seed and the
   authenticated principal, returns per-digit hints, and owns the attempt
   budget, lockout, and unlock. The client never receives the combination and
   never decides the outcome.

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

- The runnable service can persist a whole-world aggregate containing the zone,
  calendar, and factions with either the file store or `PostgresSnapshotStore`
  (`--state-postgres`). The file store remains available as an alternative; neither
  store persists sessions or resume tokens.
- With `--state-postgres`, one process owns the zone lease at a time. A lease
  carries a fencing token that every snapshot upsert and event append presents
  in its own transaction, so a released or superseded owner cannot write stale
  state after a takeover. Lifecycle markers (`world.started`, `world.snapshot`,
  `world.stopped`) go to the append-only zone event log.
- `backend_nakama` provides PostgreSQL-backed account mapping and character
  services, including character-bound world-entry records. `lynx-world` uses
  them for production session and ownership admission, but admits only actors
  already present in the authoritative zone; character population remains a
  separate concern. Inventory, normalized faction/calendar persistence, and
  event replay remain backlog work.
- The zone snapshot persists transfer receipts so retries stay idempotent across
  a restart. A general durable SQL idempotency ledger is not yet implemented.
- Content definitions are versioned bundles; instances record the content
  version they were created from.

## Failure and recovery

- Structured errors use `ErrorCode`, never free-form strings alone.
- Target: missing or obsolete component data loads through the migration chain; if a
  migration is impossible, the entity is quarantined and reported instead of
  silently reset.
- Today startup loads the configured whole-world snapshot; state since its last
  successful write can be lost. Event replay and quarantine are not implemented.
  The target is to replay events, validate references, and accept sessions within
  60 seconds; this target is unverified.
- With `--state-postgres`, an expired lease may be taken over by a replacement.
  The token increment fences the old owner out of both snapshot writes and the
  event log, and the displaced process stops rather than continuing to serve.
  Reading those events back to reconstruct state is `PST-005`.
- Target: admin actions and reputation changes are appended to a durable audit
  log in the same transaction as the mutation.

## What is deliberately not here yet

- Bevy client crates, rendering, input, and camera modules (Phase 1 client
  work).
- Production authentication at the world edge. `lynx-world` currently accepts
  static hashed bearer tokens and spawns one demo character per token;
  validating a Nakama session and resolving character ownership is still
  required.
- Rate limiting, per-session message budgets, metrics export, and TLS
  termination. Plain WebSocket is intended for loopback development behind a
  trusted proxy.
- A durable session store. Resume windows live in memory, so a zone restart
  still requires players to log in again. Whole-world snapshots (`--state-file`
  or `--state-postgres`)
  are durable, but they restore the zone, calendar, and factions — not
  sessions.
- Production admission through the account/character services and database-backed
  inventory services and event replay. The PostgreSQL whole-world snapshot
  adapter does not implement those services.
- The editor application, behavior graph editor, and admin spectator client.
- WebAssembly scripting, which needs a sandbox design review before adoption.
