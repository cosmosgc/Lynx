# Changelog

All notable changes to Lynx are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
semantic versioning for engine releases and content bundles.

## [Unreleased]

### Fixed

- `lynx_client`: the no-default-features headless build again compiles the
  secure Nakama credential module required by reconnect-time token refresh, so
  the GPU-free CI build/test path works instead of failing on a window-gated
  module import.

### Added

- `engine_core`: `proptest` coverage for inventory transfers and JSON round trips.
  Generated stackable and unique inventories verify per-definition conservation,
  all-or-nothing failure, idempotent replay, and snapshot-style deserialization
  validation, with shrinking to a minimal failing case (`QUA-002`).
- `lynx_world`: PostgreSQL integration tests now use collision-resistant per-run
  IDs and a scoped cleanup guard that removes test rows on success or panic; a
  live regression verifies cleanup during unwinding (`QUA-004`).
- `lynx_world`: malformed pre-hello WebSocket frames are covered end to end;
  rejection closes before gateway admission or action side effects and does not
  echo the bearer token. Failed identity/ownership admission also asserts zero
  session attaches and applied actions; a failed-auth burst is rate-limited
  without echoing the guessed credential, and duplicate sessions cannot displace
  the active connection (`QUA-006`).
- `world_gateway`: real-gateway movement budget boundary tests verify that a
  zero budget rejects without consuming sequence state and that a saturated
  cross-session queue accepts the rejected intent after the next tick; a
  controller-change regression denies stale-session combat without health
  mutation; vertical movement and a nav-valid move beyond configured zone bounds
  are rejected without moving the actor, as is a pending move after the actor
  dies while its controller remains bound (`QUA-001`).
- `lynx_world`: configurable per-connection `--outbound-queue` capacity (8–256
  frames, default 64) with startup validation, replacing the fixed internal
  limit (`NET-006`).
- `lynx_world`: observed outbound queue high-water gauges and bounded snapshot/
  delta WebSocket write outcome and completed-byte metrics; slow-peer isolation
  and reconnect snapshot recovery are covered (`NET-006`).
- `scripts/backup-restore-smoke.sh`: guarded loopback PostgreSQL backup/restore
  verification, including world snapshot replay checks, audit trigger checks,
  restored-service readiness, and a disposable CI database (`PST-009`).
- `lynx_client` and `lynx_world`: owner inventory presentation and the first
  server-validated container transfer. The
  client retains session-private `owner_only` inventory only for the controlled
  actor, validates and orders it, clears it on disconnect and fresh snapshots,
  and redacts its contents from diagnostics. A toggleable `bevy_immediate`
  window shows slots, derived item names, quantities, and unique items, while
  the demo actor starts with deterministic items. `bevy_immediate` also renders
  server-described panel fields/actions; a real-socket demo action takes items
  and verifies the replicated inventory after resume. Equip, split/merge,
  localization, and richer authored layouts remain (`CLT-008`).
- `lynx_client`: account-screen server browser with validated host/IP and port
  profiles, pinning, bounded recent history, atomic OS-config persistence, and
  an explicit local/remote transport policy (`ws://`/`http://` only for loopback
  and private networks, `wss://`/`https://` otherwise). The selected profile
  drives the character API and world endpoints (`CLT-002`).
- `lynx_client`: reusable immediate-mode floating-window chrome and a chat window
  with draggable/resizable/closable behavior, remembered geometry, dynamic
  zone/whisper/group/party tabs, unread counts, presence, and bounded history
  navigation. Channel sends go through the new `send_chat_to` seam; the current
  gateway authorizes zone sends only.
- `lynx_client`: injected transport-drop coverage. A local TCP proxy closes the
  active socket so the client's automatic backoff, rotated-token resume, and
  post-resume movement run without an explicit `reconnect()` (`CLT-005`).
- `lynx_client`: deterministic delayed-correction coverage over a real WebSocket.
  `delayed_private_corrections_remain_bounded_over_websocket` delays
  `movement.queued` and the private movement result by 100 ms and 200 ms,
  proving one-in-flight prediction stays bounded and presentation converges to
  authority without mutating it. It asserts measured round-trip delay and
  credential-free movement trace data for controlled server rejection
  (`CLT-007`, `CLT-016`, `CLT-017`).
- `engine_sim`: server-owned timed stun. `EntityRecord::stun_until` persists in
  snapshots, `Zone::apply_stun` keeps the later expiry instead of adding
  durations, and `cleanse_stun` removes only the timed stun. Expiry happens on
  the tick boundary; pending movement, actions, and public replication honor it
  (`SIM-005`).
- `engine_sim`: server-owned timed slow. Slow expiry and the strongest-multiplier
  plus latest-expiry stacking rule are deterministic on tick boundaries and
  survive snapshots. The multiplier is enforced by authoritative movement
  validation and is not exposed by public replication (`SIM-005`).
- `world_gateway` and `lynx_client`: public replication exposes a boolean `slowed`
  status without its timer or movement multiplier. The controlled actor's
  session-private movement state carries the exact factor for prediction; the
  online client displays a `SLOW` label for visible affected entities (`CLT-018`).
- `engine_sim`: typed mitigation. `DamageType` and flat `Resistances` add a
  versioned combat path where armor mitigates only physical damage and each
  school uses its own server-owned resistance; the existing physical formula is
  unchanged for zero resistances (`SIM-005`).
- `engine_sim`: versioned zone snapshots. `Zone::snapshot` captures persistent
  entities (honoring each record's explicit persistence policy), links whose
  endpoints were saved, the tick counter, active cooldowns, the action and
  link replay caches, and applied inventory-transfer receipts, in a canonical
  order. `Zone::restore` rebuilds a zone from a snapshot plus runtime-only
  navigation and item rules and re-validates every entry through the normal
  spawn, link, and range checks, so a stale or tampered file fails startup
  instead of yielding a partial world.
- `engine_core::inventory::TransferLedger` gained `applied` and `adopt` so
  idempotency receipts recorded before a restart survive the reload.
- `lynx_world`: durable whole-world snapshots. `--state-file <path>` makes the
  runnable service load that file at startup and rewrite it on
  `--snapshot-ms` (default 10 s) and once more at shutdown
  (`Service::with_persistence`). The `WorldSnapshot` pairs the zone snapshot
  with the calendar and faction snapshots, mirroring the trusted
  `CalendarPersistence` commit contract; load validation rejects unsupported
  schema versions and an unpaired calendar/faction aggregate, and the gateway
  cross-validates the restored calendar before any session is accepted. The
  demo marks every entity `PersistentEntity`, and `demo::restore_zone` rebuilds
  the zone with its saved bounds. Sessions, resume tokens, and minigame
  replication stay in memory and a save failure never stops the authority
  loop.
- `docs` updates for the snapshot path (architecture, operations, README
  progress) and an end-to-end restart integration test that drives save →
  restart → reconnect against the real gateway.
- `lynx_world`: `--fresh` reset. With `--state-file`, it ignores the played
  snapshot (even a corrupt one), rebuilds the authored demo, and writes that
  pristine world over the file before serving, so resetting a development
  world no longer means deleting the file by hand. The rule lives in
  `lynx_world::startup::load` and is covered by a test that first resumes a
  played world and then proves `--fresh` returns it to tick zero and the
  authored entity placement.
- `lynx_world`: the startup baseline snapshot is written before the authority
  loop starts, so the authored world (or a `--fresh` reset) survives a crash
  during the first save interval instead of existing only in memory. Under
  `--fresh` a failed overwrite aborts startup rather than silently serving from
  a world the played file would replace on the next restart.
- `lynx_world`: single-writer zone leases with fencing and an event log
  (`PST-004`). With `--state-postgres`, the service acquires the demo zone lease
  before loading the snapshot and refuses to start while another live process
  holds it; `--lease-ttl-ms` (default 30 s) bounds the lifetime. Snapshot upserts
  and event appends validate the owner, fencing token, and expiry in the same
  transaction, so a superseded process is rejected with `conflict` instead of
  overwriting newer state. A background task renews every third of the TTL,
  turns readiness false, and stops the process when the lease is taken over or
  expires unrenewed. `world.started`, `world.snapshot`, and `world.stopped`
  markers are appended to `lynx.zone_events`, and
  `lynx_world_zone_lease_held` / `lynx_world_zone_lease_renewals_total` expose
  lease state. File-backed and memory-only modes remain lease-free.
- `lynx_world`: snapshot/event replay, durable quarantine, retention, and measured
  recovery (`PST-005`). The service appends one atomic fenced event batch after
  each authoritative tick for changed persistent entities, links, action and link
  cooldowns, newly applied transfer receipts, and paired calendar/faction state;
  a failed append stops the authority loop. The reducer rejects sequence gaps,
  malformed IDs, mismatched actors, non-persistent records, unsupported event
  schemas, and non-monotonic ticks, and applies deltas through a stable-ID map so
  repeated recovery yields identical canonical state. `Zone::restore_quarantining`
  re-validates entity identity, persistence policy, inventories, health ranges,
  links, cooldowns, and transfer receipts; individually impossible entries are
  skipped and reported while structural corruption still fails closed. The report
  is stored in `lynx.recovery_quarantine` (migration
  `0012_recovery_quarantine.sql`) or a `*.quarantine.json` sidecar for file
  snapshots, and startup persists it before serving or aborts. `lynx-world
  verify-snapshot` performs the same replay and dry-run restore read-only and
  prints detected and previously stored quarantine records. Startup logs measured
  recovery time; the reference-dataset test (1,000 entities, 5,000 upserts, 400
  removals) enforces the 60-second target, and the manual 55,001-event check
  measured a p95 of 1.71 s. A checkpoint save prunes every earlier zone event, so
  retention is bounded to one snapshot interval. A multi-tick PostgreSQL restart
  test covers every replayed aggregate and replay after pruning, including
  action/link idempotency caches and zone counters.

- `engine_core`: durable world calendar and bounded faction conflicts. Events
  have an audited `Draft -> Scheduled -> Open -> Active` lifecycle with
  suspension, cancellation policy, and exactly one attacker and one defender
  over one target territory. Enrollment reads authoritative faction membership
  and enforces side and capacity rules. Deterministic resolution records both
  sides' inputs, uses participants or faction power, gives ties to the defender,
  and applies at most one territory capture through the trusted faction path.
  Resolution is idempotent and atomic across calendar and faction state, and
  snapshot recovery validates history, enrollment, results, and cross-aggregate
  consistency (ADR 0021). Canonical calendar and post-siege faction fixtures
  cover deserialization, participant counts, capture, and tamper rejection.
- `engine_sim`: a world-scoped calendar command stage. Participant enrollment
  and staff-gated lifecycle commands carry intent only, are deduplicated by
  request ID, produce structured effects with stable error codes, and must be
  committed by persisting the new calendar and faction snapshots together.
- `world_gateway`: a calendar stage over the wire. `calendar.command`/
  `calendar.result` carry replay-protected calendar commands with actor,
  principal, and staff flag resolved from the authenticated session, and
  repeated request IDs return the original outcome without re-applying or
  re-committing. `calendar.query`/`calendar.state` expose the player-visible
  calendar with drafts hidden, and applied snapshots pass through a trusted
  `CalendarPersistence` seam.
- `backend_memory`: `replace_world_state` commits calendar and faction
  snapshots as one cross-validated pair under a single lock.
- `lynx-world`: the demo publishes one open faction conflict over the demo
  zone at startup; players enroll and inspect through the new wire kinds,
  staff sessions (new `--staff` flag) drive lifecycle transitions, and
  committed state flows into the in-memory backend.
- `game_api` and `backend_memory`: read-only `CalendarService` views for public
  events, enrollment, and conflict results plus the trusted `CalendarStore`
  snapshot load/replace seam. The in-memory adapter cross-validates calendar and
  faction snapshots in both directions before replacing state.
- `server/migrations/0005_world_events.sql`: calendar events, bounded sides,
  append-only lifecycle transitions, enrollment, conflict results, deferred
  lifecycle/side/enrollment/result checks, reservation uniqueness, and public
  calendar views.
- `engine_core`: durable faction governance, an append-only treasury ledger,
  inspectable power rules, and per-zone territory ownership. Player, NPC, and
  system factions share explicit roles, permissions, spend limits, leader
  succession, idempotent transfers, capture protection/cooldown, contest state,
  and validated snapshot recovery (ADR 0020). A canonical faction-world fixture
  covers deserialization, power, history, and tamper rejection.
- `game_api` and `backend_memory`: read-only `FactionService` views for
  authenticated accounts plus the trusted `FactionStore` snapshot load/replace
  seam used by the authoritative world service.
- `server/migrations/0004_factions.sql`: faction catalog, role permissions,
  membership, treasury ledger and transfer receipts, territory ownership and
  append-only history, derived views, deferred leader/treasury/transfer checks,
  and the `map` content kind.
- `engine_content`: authored world-map atlases. `WorldMapDefinition` places
  stable zone definition IDs on integer atlas cells and declares explicit
  `passage`, `gateway`, or `portal` routes with an optional bidirectional flag.
  Validation rejects duplicate placements and cells, self-routes, duplicate
  routes, route endpoints missing from the map, unknown zone references, and a
  zone placed on multiple maps, and warns about zones that no map references.
  Maps are a manifest kind and participate in content diffs (ADR 0019).
- `lynx new` templates now generate `content/maps/overworld.ron` plus a second
  connected starter zone; `lynx doctor` reports the map count.
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
