# Simulation and gameplay backlog

Scope: everything that changes what happens inside a zone or between zones —
movement, combat, AI, interaction, chat/social, economy, progression, calendar
gameplay, and the determinism budget that keeps it testable.

The zone simulation is already server-authoritative for entity state, actions,
inventory transfers, links, minigames, factions, territory, and the calendar.
The gaps below are the ones that prevent a player-facing game from exercising
that authority end to end.

**Related design:** [architecture §Simulation model](../architecture.md#simulation-model),
[world simulation](../world-simulation.md), [networking](../networking.md),
[security](../security.md), ADRs [0009](../adr/0009-faction-alignment-and-reputation.md),
[0010](../adr/0010-navigation-and-pathfinding.md),
[0016](../adr/0016-server-validated-minigame.md),
[0020](../adr/0020-faction-governance-treasury-and-territory.md),
[0021](../adr/0021-world-calendar-and-bounded-conflicts.md),
[0022](../adr/0022-dynamic-character-classes-and-combat-targeting.md).

---

## SIM-001 — Server-authoritative movement

- **Priority:** P0
- **Status:** Done — queued, collision-checked, rate-limited movement authority
- **Depends on:** —
- **References:** [architecture §Simulation model](../architecture.md#simulation-model),
  [networking §Reconnect and recovery](../networking.md#reconnect-and-recovery),
  ADR 0010, `crates/engine_sim/src/nav.rs`

**Why:** Client movement needs one server authority for control, liveness,
status, speed, nav geometry, occupancy, and body collision. The gateway now
validates queued intents against all of these before applying an endpoint, and
replicates one authoritative position to every observing session.

**Scope**

- Add a movement intent (destination or heading plus input sequence) to the
  action/verb surface or a dedicated high-frequency message. Keep it separate
  from low-rate interactions if the action pipeline cannot meet the tick budget.
- Validate actor control, alive state, speed budget per tick, collision, nav
  grid bounds, and occupancy. Reject teleports, wall clipping, and out-of-range
  destinations.
- Advance movement deterministically in `Zone::tick()` using an explicit
  `now_ms`/tick delta; do not depend on wall-clock time.
- Replicate authoritative position and facing in snapshots and deltas; keep
  movement out of the client's authority.
- Emit diagnostics for rejected movement without leaking hidden geometry.

**Acceptance criteria**

- [x] A headless test proves a client cannot cross a wall, exceed the per-tick
      speed budget, move while dead/stunned, or move an entity it does not
      control.
- [x] Two sessions observe the same positions for the same tick sequence.
- [x] Movement is deterministic across repeated runs with identical inputs.
- [x] A slow or spamming client cannot stall the authority loop.
- [x] Protocol and tick-budget documentation is updated.

**Foundation:** `NavGrid::can_step` checks a single adjacent cell transition,
including blocked endpoints, bounds, and both diagonal corner cells; A* uses
the same rule. `NavGrid::allows_displacement` adds a finite horizontal position
and distance check against a caller-supplied speed and elapsed time, and permits
only same-cell or adjacent-cell moves. Diagonals conservatively require both
corner cells open. Its budget inputs must come from trusted server state; it is
not itself a per-tick budget or an authority boundary. It does not check zone
bounds, occupancy, or fine collision, expose requests, or mutate transforms.
`MovementProfile` is server-owned runtime state: a horizontal radius, an
occupancy flag, a speed multiplier, and a `Mobile`/`Stunned` status. Controlled
entities without an explicit profile use a mobile default body with a 0.35 m
radius; static content opts in to blocking by carrying a profile.
`Zone::queue_movement` accepts a player intent only from a server-resolved
principal and session with Move capability, one pending input per actor and at
most `max_actions` pending inputs per tick. Sequences increase per actor and
control session; accepted queue entries consume their sequence even on failure.
`advance_movement_tick` rechecks control, liveness, and status, applies one
endpoint against a trusted caller-supplied speed and elapsed time, and advances
the tick; ordinary `advance_tick` does not process movement. It rejects
out-of-zone destinations, blocked or non-adjacent nav transitions, destination
cells occupied by a blocking body, and segments that intersect another blocking
body. Processing uses stable entity order and earlier accepted positions, so
contested destinations have one deterministic winner. Rejections are generic to
clients and retained in a 256-entry server-only diagnostic buffer with no
coordinates. Sequence and pending state are runtime only and reset on despawn or
restore. Facing is unchanged. Engine tests cover budget, walls, control,
dead/stunned and zero-speed actors, wrong sessions, occupancy, body collision,
bounded diagnostics, and repeated deterministic runs. Gateway and websocket
tests cover cross-session position agreement, resume, rate limiting, and
slow-client detachment.

**Gateway contract:** `movement.request` carries only `sequence` (u64) and
`destination: {x, y, z}` in world meters. Unknown fields, including actor,
principal, speed and duration, are rejected. Actor, principal and session come
from the active authenticated connection. `movement.queued` echoes the sequence
and means only that the intent was queued, not that it was applied. Queue
rejections use a generic `protocol.error`; processed inputs produce a private
tick-stamped applied/rejected result and authoritative position without revealing
geometry. The authoritative position is also
replicated through the existing interest-filtered snapshot/delta path; facing
is not changed. A queue-accepted sequence is consumed even if processing fails
or the session disconnects. Queue-rejected sequences are not consumed. Sequences
are strictly increasing for the same actor/control session, including resume;
new sessions use their own sequence domain. This state is not persisted.

The service uses its configured `--tick-ms` as a fixed budget for each processed
tick, with a provisional server-owned speed of 2 m/s. A delayed tick does not
bank elapsed time or provide extra distance; a tick advances the zone once.
Direct gateway/server callers using `tick` use a 50 ms default; trusted callers
may supply their fixed interval with `tick_with_interval`. Per-session token
buckets bound movement spam, and a full outbound queue detaches the slow client
without waiting on its socket. Client reconciliation remains tracked by SIM-002;
this task owns the authoritative server side.

---

## SIM-002 — Client prediction and reconciliation contract

- **Priority:** P0
- **Status:** Done — reconnect and destination-snapshot resets are covered; graphical review remains tracked in `CLT-019`
- **Depends on:** `SIM-001`, `CLT-005`
- **References:** ADR 0014, `SIM-001`

**Why:** A graphical client cannot wait a full round trip for every step, but
prediction must never become client authority. The server needs to acknowledge
input sequences and send bounded corrections the client can replay.

**Scope**

- Define input sequence numbers, server acknowledgements, and correction
  payloads (authoritative position, tick, and last processed input). Velocity is
  not part of this protocol version.
- Keep the bounded one-entry in-flight prediction that the server's
  single-pending-input rule permits; a correction replaces that prediction and
  never retries an ambiguously delivered endpoint.
- Smooth presentation error toward the latest private authoritative result
  without changing simulation, collision, or request state.
- Define behavior for teleports, zone changes, deaths, and reconnect snapshots,
  which reset the prediction history.
- Document thresholds for "accept small error" versus "snap".

**Acceptance criteria**

- [x] Delayed queue/result coverage at 100 ms and 200 ms over a real WebSocket
       shows bounded divergence and recovery after a forced correction.
- [x] Corrections never grant the client authority over position, speed, or
       collision.
- [x] Reconnect and zone-transfer paths reset prediction state explicitly.
- [x] The contract is documented in `docs/networking.md`.

**Foundation:** Initial and resumed snapshots carry the session-private current
position and optional last queue-consumed sequence (including cancelled input).
Processed inputs carry a tick-stamped applied/rejected result and position for
that exact session only. Disconnect and delivery failure recover via a baseline,
not replay of per-input results. Exhausting u64 requires a new session. The
client implements bounded prediction, presentation smoothing, and an explicit
reset seam; velocity remains deliberately absent.

The client reconciliation module is wired through `WorldState` and the bounded
background transport. It predicts the server-permitted single pending endpoint,
keeps it in a one-entry replay buffer, and consumes only correlated private
results. Its authoritative state is never mutable by presentation smoothing.
Small correction error is interpolated by the renderer; correction beyond the
local prediction distance snaps. Disconnect and fresh snapshots reset through
`disconnect`/`baseline`; the explicit `reset` API is ready for teleport, death,
and zone-transfer callers. A destination snapshot is tested as a new prediction
domain: pending source-zone input is discarded, the destination position becomes
authoritative, and sequence numbering restarts. The latency/loss test proves
that dropped replies cannot accumulate unbounded divergence and a forced server
correction recovers to authority. The local distance bound is not server movement
validation. The caller discards messages from old connections and distinguishes
resumed from new sessions.

`delayed_private_corrections_remain_bounded_over_websocket` delays both
`movement.queued` and the private rejected result by 100 ms and 200 ms against a
stub gateway. While the result is pending, additional requests fail, the
presentation endpoint stays bounded, and authority is unchanged. Presentation
then interpolates to authority without mutating it. This is deterministic
transport delay, not a jitter/loss simulation and not a 100–200 ms round-trip
polish measurement; CLT-016 remains the UI-response measurement and CLT-019 the
graphical acceptance.

Graphical validation of the online correction journey is deferred to
[`CLT-019`](40-bevy-client.md#clt-019--online-reconciliation-visual-validation),
which requires a controlled correction and a graphical demo environment.

---

## SIM-003 — Server-derived combat resolution

- **Priority:** P0
- **Status:** Done
- **Depends on:** —
- **References:** [security §Authority invariants](../security.md#authority-invariants),
  `crates/engine_sim/src/action.rs::apply_attack`

**Why:** Client-supplied attack parameters are rejected, and the server applies
a temporary fixed damage of one hit point per attack. This closes the direct
outcome-spoofing hole, but damage still needs server-owned actor state,
equipment, defenses, and content rules.

**Scope**

- Remove `damage` from the accepted request parameters (or ignore it) and derive
  damage from the actor's server-owned stats/loadout and the target's defenses.
- Validate range, line of sight, target liveness, control capability, cooldown,
  and friendly-fire/faction rules before applying damage.
- Resolve death, controller release, and loot/credit hooks through the existing
  effect pipeline. Do not let the caller choose the result.
- Keep the formula data-driven and versioned so content can tune it without an
  engine fork.
- Emit `Damaged`/`EntityDied` effects and audit-relevant events.

**Acceptance criteria**

- [x] A request carrying an inflated `damage` field is rejected as an unknown
      field or ignored; damage matches the actor's server-side stats.
- [x] Out-of-range, dead-actor, dead-target, and cooldown cases are tested.
- [x] Death releases control and cannot be spoofed by the client.
- [x] The combat formula and its inputs are documented.

**Implemented:** Formula v1 is `max(minimum_damage, attacker.attack -
target.defense)`. `CombatStats` are server-owned resolved values (including
authoritative loadout effects); versioned `CombatRules` are persisted with the
zone and configure the minimum damage and friendly-fire/faction policy. Attack
effects include the server-resolved source for downstream loot, credit, and
audit consumers. See [security §Combat authority](../security.md#combat-authority).

---

## SIM-004 — Dynamic classes and Treat targeting (ADR 0022)

- **Priority:** P2
- **Status:** Proposed — ADR 0022 must be accepted or revised first
- **Depends on:** `SIM-003`, ADR 0022 accepted
- **References:** [ADR 0022](../adr/0022-dynamic-character-classes-and-combat-targeting.md)

**Why:** ADR 0022 proposes emergent classes from equipped items, learned skills,
and active features, plus a "Treat" value that drives AI targeting (for example,
prioritizing tanks). It is a design proposal, not an implementation contract.

**Scope**

- Resolve a character's effective role/class from server-owned loadout and
  learned skills at runtime; persist only the inputs, not a cached client claim.
- Define the Treat/threat model, decay, transfer on death/pull, and how it
  interacts with taunts, healing, and damage.
- Make AI target selection consume Treat through the same action/visibility
  rules as players.
- Expose role/utility information to clients without exposing hidden AI state.
- Provide content schemas for role tags and class-defining features.

**Acceptance criteria**

- [ ] ADR 0022 is Accepted with resolved open questions before code lands.
- [ ] A character's class is reproducible from persisted inputs alone.
- [ ] AI targeting tests cover tank priority, threat decay, and target loss.
- [ ] No client message can assert a class, role, or threat value.

---

## SIM-005 — Combat depth: status, mitigation, death, respawn

- **Priority:** P2
- **Status:** Done — timed effects, typed mitigation, respawn recovery, bounded audit, and reconnect/transfer policy documented
- **Depends on:** `SIM-003`
- **References:** `SIM-003`, `SIM-004`

**Scope**

- Status effects (stun, slow, damage-over-time, shields) with explicit stacking,
  duration, and cleanse rules.
- Resistances/mitigation and damage types, data-driven and versioned.
- Death state, respawn points, corpse/despawn policy, and penalty rules.
- Combat logging suitable for audit and abuse review, with private data
  redacted.

**Acceptance criteria**

- [x] Effects expire deterministically on tick boundaries and survive snapshot
      round trips when their policy says so.
- [x] Respawn cannot be triggered by a client-supplied location.
- [x] Stacking and mitigation rules are covered by table-driven tests.
- [x] Death/respawn behavior is documented for reconnect and zone transfer.

**Foundation:** `EntityRecord::stun_until` is server-owned and persists in
snapshots. `Zone::apply_stun` validates duration and target, never adds
durations, keeps the later expiry on reapplication, and `cleanse_stun` removes
only the timed stun so a permanent stunned profile remains. Expiry occurs on the
exclusive `advance_tick` boundary before AI or movement acts; pending movement is
revalidated during processing. Stunned actors are rejected by the shared action
pipeline, and public replication derives `stunned` from both the timed stun and
the static profile. Spawn/restore normalizes already-expired stuns. Tests cover
boundary expiry, stacking, cleanse, snapshot round trip, pending movement
rejection, and action rejection. `TimedSlow` is likewise server-owned,
snapshot-persistent, and tick-expiring. `Zone::apply_slow` rejects invalid
targets, dead actors, and out-of-range multipliers; reapplication keeps the
strongest multiplier and latest expiry without compounding. The effective
multiplier is applied inside authoritative movement validation. Nearby
observers receive only a public `slowed` flag; the exact factor is sent only in
the controlled actor's private movement state so local prediction can match the
server without granting authority. Tests cover stacking, cleanse, expiry,
snapshot recovery, private delivery, and rejection of movement that exceeds the
slowed budget.

`DamageType` (`physical`, `fire`, `frost`, `poison`) and flat `Resistances` are
versioned server-owned combat data. `CombatRules::resolve_typed_damage` applies
`defense` only to physical damage and the matching flat resistance to every
school; the existing physical path is unchanged when resistances are zero. No
action or wire body can select a damage type or resistance value; the trusted
attacker `CombatStats` selects its damage school. Timed periodic damage is
source-keyed, uses simulation ticks, refreshes same-source effects without
compounding, and is capped at eight concurrent sources. Shields are finite,
tick-expiring, capped at eight, and absorb damage after mitigation in persisted
application order. A bounded 512-entry redacted combat audit ring records
resolved source/target IDs, damage school, mitigation, shield absorption, and
health change; it is snapshot-persistent and never replicated to clients.
Entities with a server-authored `RespawnPolicy` recover at the configured safe
position after a positive tick delay. Death releases control and clears timed
effects; there is no lootable corpse or item penalty. The gateway rebinds an
already-connected principal after respawn and sends a private prediction reset.
Tests additionally verify that a DoT stops at its exclusive expiry tick and
that a persisted death schedule resumes after restart, restoring the server-set
position and character identity without restoring the old controller. The
reconnect policy keeps the persistent character and scheduled death/respawn state
in the zone; resume installs a private snapshot baseline and rebinds control only
if the actor is alive. A dead actor cannot travel. Once travel is implemented by
`SIM-014`, handoff must preserve the stable character identity and be idempotent;
respawn remains owned by the zone where death occurred, and prediction resets
from the destination's private snapshot. Handoff mechanics belong to `SIM-014`;
the bounded audit ring remains the current operator-visible record.

---

## SIM-006 — AI behaviors, schedules, and perception budgets

- **Priority:** P1
- **Status:** In progress — bounded guard primitives, typed behavior authoring, and content-to-runtime binding are implemented; scheduling and graph execution remain
- **Depends on:** `SIM-001`
- **References:** `crates/engine_sim/src/ai.rs`, `mind.rs`,
  [world simulation](../world-simulation.md)

**Why:** Every entity can have a mind and an AI controller exists, but behavior
is engine-local. Content cannot define patrols, schedules, aggro rules, or
groups without code changes.

**Scope**

- Content-defined behavior graphs or state machines with a bounded instruction
  set (patrol, guard, follow, flee, attack, interact, schedule).
- Perception (range, line of sight, faction) and group aggro sharing with
  explicit budgets.
- Daily/seasonal schedules tied to the world calendar.
- Runtime inspection: current behavior, target, last decision, and budget use.
- All AI actions go through the same validated action pipeline as players.

**Acceptance criteria**

- [ ] A content-only AI definition changes behavior without an engine fork.
- [ ] AI cannot exceed perception/action budgets or act while controlled by a
      player.
- [ ] Deterministic replay produces identical AI decisions for the same inputs.
- [ ] Behavior failures emit diagnostics instead of silently idling.

**Foundation:** `perceive_hostiles_bounded` provides server-supplied limits on
candidate records, line-of-sight checks, and returned targets, with counts and
completion/failure diagnostics. The zone wrapper iterates in stable entity-ID
order; results are sorted by distance and ID, but budget exhaustion returns only
the inspected prefix, not necessarily the globally nearest entities. A candidate
limit reached exactly at iterator end is conservatively reported as reached,
without reading ahead. Dead, non-AI-controlled, or factionless observers and
invalid observer geometry are rejected. Tests cover limits, replay, player
control, invalid ranges, and no candidate lookahead.

**Tick increment:** An explicitly installed server-owned `AiTickPolicy` makes
`advance_tick` (including the gateway's movement tick) evaluate the built-in
`behavior.guard`: perceive hostiles and attempt an attack on the nearest
discovered target through the same `apply_action` pipeline used by players.
Only current AI controllers are selected; player control is never overridden.
Controller records scanned, AI updates, per-observer candidates/LOS/targets,
and AI action attempts (including rejected actions) have deterministic caps.
The last tick's server-side report includes decisions, perception counts and
status, targets, and action rejections. Unknown behaviors are reported and do
not act. Faction reputation expiry uses the policy's explicit simulation epoch
and tick duration. Selection and perception inspect stable ID prefixes, so
low limits can starve later entities; this is not yet a fair scheduler.

This is opt-in: the running world must install the policy and restore it after
snapshot reload; the demo installs a bounded GuardChase policy and training
scenario. `max_actions` caps AI
attempts here, **not** all player plus AI actions: player actions are currently
applied immediately without a shared tick counter. The policy epoch must match
the authoritative reputation timebase. Existing unbounded perception/helper
APIs are unchanged. Content-defined behavior graphs, scheduling, group aggro,
fair scheduling, persistent inspection, and a shared action budget remain. AI
policy now accepts server-approved `AiBehaviorDefinition` bindings, allowing a
content behavior ID to select the bounded Guard or GuardChase primitive and its
perception range; a test verifies a custom ID and constrained range.
`engine_content` defines and validates typed behavior semantics and finite
positive ranges, while `lynx_world::demo::ai_behavior_bindings` maps a loaded
`ContentSet` into stable runtime bindings; tests verify authored primitive and
range values cross this seam without changing simulation code. GuardChase
uses the zone's cumulative path budget and per-agent retry state, then moves
through the same server-side collision and dynamic-obstacle checks as other
authoritative movement. `cargo test -p engine_sim
policy_binds_content_behavior_ids_to_bounded_guard_semantics` passes.
Only the Guard and GuardChase primitives exist, so content-only behavior graphs,
schedules, group aggro, fair scheduling, persistent inspection, and a shared
action budget remain open. Movement authority is implemented in `SIM-001`;
visual client reconciliation remains in `SIM-002`.

---

## SIM-007 — Navigation budgets and replan policy

- **Priority:** P1
- **Status:** Done — cumulative budgets, path diagnostics, replan policy, expiring blockers, shared nav bake, and an opt-in GuardChase consumer
- **Depends on:** `SIM-001`
- **References:** ADR 0010, `crates/engine_sim/src/nav.rs`

**Scope**

- Enforce cumulative per-zone/per-tick expansion and request limits; refuse
  excess requests without searching and reset allowances at the next tick.
- Define deterministic failure codes, nearest-reachable partial paths, and
  bounded exponential retry state; connect them to NPC navigation.
- Make dynamic obstacles use the same nav grid, with expiry/replan scheduling
  bounded by the tick budget.
- Expose a validated nav bake format shared by editor preview and runtime
  (`CNT-006`).
- Export bounded diagnostics for expansions, partial paths, failures, and
  budget exhaustion to `OPS-001`.

**Implemented:** `Zone::find_path` uses a cumulative `PathTickBudget` derived
from `TickBudget`; exhausted allowances reject requests without running A*.
`NavGrid::find_path_with_policy` can return the deterministic nearest reached
cell as an explicitly incomplete path, and `ReplanState`/`ReplanPolicy` provide
bounded exponential backoff and a consecutive-failure cap. Fixed-size
`PathDiagnostics` classify errors, count expansions and deferred work, and can
be sampled by the world metrics endpoint. `NavBake` round-trips the exact
versioned grid used for runtime path queries. Unit tests verify cumulative
accounting, reset and deferred semantics, deterministic partial paths, retry
limits, blocker expiry and extension, malformed bake rejection, and identical
paths before/after baking. Temporary blockers are stored separately from the
static nav bake, are active until their exclusive expiry tick, and affect both
endpoints and diagonal corner checks. Authoritative movement revalidates against
the same active blockers and resumes once they expire.

The opt-in `GuardChase` behavior requests one bounded path when needed, caches
and follows it using server-validated NPC movement, and uses per-agent
`ReplanState` backoff after unreachable/blocked routes. Expiring blockers reset
stale cached routes so the next AI tick can request a fresh one. The demo installs
the policy on fresh and restored zones and includes a persistent guard/raider
scenario; authored content activation remains in `SIM-006`.
Use `Zone::add_dynamic_obstacle` for tick-expiring temporary blockers and
`Zone::nav_mut` for authored/static cell changes.

**Acceptance criteria**

- [x] Budget exhaustion is deterministic, diagnosable, and never overruns the
      cumulative tick expansion/request limits.
- [x] A blocked path can degrade to an explicitly incomplete nearest-reachable
      path; callers can also choose fail-closed errors.
- [x] Path preview and runtime use the same validated, versioned `NavBake` data.
- [x] NPC path consumers honor bounded retry state and request replans after
      dynamic blockers expire without exceeding the tick budget.
- [x] Fixed-label pathfinding diagnostics are exported by the world metrics
      endpoint (the demo reports zero until its AI policy is enabled).

---

## SIM-008 — World chat and presence

- **Priority:** P1
- **Status:** In progress — authenticated zone chat/presence, bounded history, rate limiting, and reconnect tests are implemented; external moderation remains deferred
- **Depends on:** —
- **References:** `crates/game_api/src/chat.rs`, `social.rs`,
  [README Phase 3](../../README.md#phase-3--interaction-and-mind-system--in-progress),
  [security §Abuse prevention](../security.md#abuse-prevention)

**Why:** Players need persisted chat history, live delivery, and room presence
without moving world simulation authority into Nakama. Nakama's realtime room
provides those platform capabilities, while world admission and movement remain
owned by Lynx.

**Scope**

- Add gateway messages for sending, history, channel listing, and presence.
- Server-resolve sender identity, channel membership, zone scope, and rate
  limits; reject spoofed senders and unauthorized channels.
- Support zone, party, whisper, and system channels with bounded history and
  pagination.
- Redact private content from logs. Moderation/reporting and content-policy
  integration remain in `NET-009`/`CNT-008` and are not part of this increment.
- Replicate presence changes through the existing interest/session machinery.

**Acceptance criteria**

- [x] A client cannot send as another character or into a channel it has not
      joined.
- [x] History is bounded, paginated, and ordered deterministically.
- [x] Rate limits are enforced server-side. Mute/ignore policy is deferred to
      `NET-009`/`CNT-008`.
- [x] Chat survives reconnect for the retained window where policy allows.
- [x] Integration tests cover two sessions in one zone.

**Increment:** For account sessions with a verified character, the client joins
the persistent room named by `LYNX_CHAT_ROOM` (default `lynx-zone`) using the
authenticated Nakama token. Static `--demo-online` tokens stay on the gateway
fallback and are never sent to Nakama. The account client sends JSON content
through the realtime socket and reads persisted history from Nakama's
channel-history API. Recent-first Nakama pages are
reversed into display order and older pages are prepended with ID deduplication.
Nakama supplies sender identity, live message delivery, persistent history
with cursor pagination, and room-presence changes. This configured room is not
currently bound to Lynx world-zone admission. Without Nakama configuration, the
gateway fallback exposes one server-scoped `zone` channel, resolves
sender account and character from authenticated session claims, validates text,
retains the latest 500 messages in memory, and serves bounded cursor pages.
Accepted messages fan out only to currently connected sessions on that
gateway; `chat.presence` returns the connected account/character roster. Unknown
fields and unsupported channels fail closed. The runtime drains a bounded
per-session outbox and routes messages only to attached connections. History is
volatile across process restarts, and disconnected clients must request it
again. The gateway fallback caps sends at five messages per session per second
and presence at 100 entries. `NET-009` moderation/reporting and `CNT-008`
user-content policy remain unavailable; party/whisper/system routing,
mute/ignore, and durable retention remain open. The transport's existing
chat-family limiter is not an authorization or moderation substitute.

---

## SIM-009 — Parties and social graph

- **Priority:** P2
- **Status:** Blocked — `SIM-008`, `PST-006`
- **Depends on:** `SIM-008`, `PST-006`

**Scope**

- Party invite/accept/leave/kick, leader transfer, roles, and ready checks.
- Party chat, shared markers, loot rules, and a party finder with configurable
  eligibility (per the world-simulation guide).
- Friends/ignore lists and presence that survives reconnect.
- Bounded party size and abuse controls.

**Acceptance criteria**

- [ ] Membership changes are authorized and idempotent.
- [ ] Party state survives a reconnect and is cleaned up on logout/expiry.
- [ ] Cross-zone parties degrade to documented behavior until `SIM-014` lands.
- [ ] Invites cannot be used to spam or bypass ignore.

---

## SIM-010 — Trade and economy integrity

- **Priority:** P2
- **Status:** Blocked — `SIM-009`, `PST-002`, `PST-014`
- **Depends on:** `SIM-009`, `PST-002`, `PST-014`

**Scope**

- Two-phase player trade with explicit confirm, atomic swap, and cancellation.
- Anti-duplication checks for trades, mail, and reward claims; append-only
  economy ledger.
- Currency sources/sinks and reconciliation queries.
- Mail or offline delivery if the design adopts it; otherwise document the
  choice.

**Acceptance criteria**

- [ ] A failed or cancelled trade changes neither inventory.
- [ ] Replaying the same trade/reward request cannot duplicate value.
- [ ] Every value movement is traceable in the audit/ledger.
- [ ] Concurrent trades on the same items cannot double-spend.

---

## SIM-011 — Progression and quests

- **Priority:** P2
- **Status:** Proposed
- **Depends on:** `PST-001`, `CNT-005`

**Scope**

- XP/levels or an equivalent progression currency, with server-owned award
  rules and caps.
- Quest definitions, prerequisites, objectives, turn-in, and rewards as content.
- Idempotent reward grants and audit records.
- Client-facing progress state and localization keys.

**Acceptance criteria**

- [ ] Quest state and progression survive restart.
- [ ] Rewards cannot be claimed twice or granted by a client message.
- [ ] Content can add a quest without engine code.
- [ ] Migration/versioning behavior for changed quest definitions is documented.

---

## SIM-012 — Guilds and player factions

- **Priority:** P2
- **Status:** Proposed
- **Depends on:** `PST-003`, `SIM-009`

**Scope**

- Guild creation, ranks, invitations, treasury, and membership limits.
- Integrate with the existing faction governance/treasury/territory model rather
  than creating a parallel economy.
- Guild chat and territory claims through authorized calendar/faction actions.
- Succession, disbanding, and asset custody rules.

**Acceptance criteria**

- [ ] Guild actions pass the same authorization and transaction boundaries as
      NPC factions.
- [ ] Treasury and territory changes are audited and replay-safe.
- [ ] Disbanding cannot orphan or duplicate assets.
- [ ] Client views expose only permitted guild data.

---

## SIM-013 — Loot and encounter rewards

- **Priority:** P2
- **Status:** Blocked — `SIM-003`, `PST-014`
- **Depends on:** `SIM-003`, `PST-014`

**Scope**

- Loot tables, roll policies (need/greed/free-for-all), and encounter credit.
- Idempotent grants keyed by encounter/participant.
- Bounded drop budgets and anti-farming rules.
- Audit trail for rare/high-value grants.

**Acceptance criteria**

- [ ] A duplicate kill/reward request cannot grant twice.
- [ ] Loot policy is resolved server-side from party/encounter state.
- [ ] Drop tables validate at content load time.
- [ ] High-value grants are queryable by operators.

---

## SIM-014 — Additional zones and travel

- **Priority:** P2
- **Status:** In progress — authored-route player handoff and multi-zone lease foundations are implemented; durable receipts and runtime scheduling remain
- **Depends on:** `SIM-001`, `PST-005`

**Scope**

- Multiple authoritative zones with explicit handoff, capacity, and ownership.
- Route/gateway/portal validation against the authored atlas; reject arbitrary
  teleports.
- Cross-zone party/chat/presence behavior and travel cooldowns.
- Per-zone tick loops or a scheduler with fair budgets; document the choice.
- Zone-local persistence and recovery.

**Acceptance criteria**

- [x] Travel only follows validated routes and server-owned access rules.
- [x] A handoff cannot duplicate an entity or leave it in two zones.
- [ ] A zone crash/restart recovers independently without corrupting others.
- [ ] Cross-zone chat/party behavior is documented and tested.

**Foundation:** `WorldMapDefinition::route_kind` resolves only an authored
directed route (or a declared bidirectional reverse route) and returns no route
for arbitrary zone pairs. The atlas loader validates route endpoints against
placed zones. This prevents a future handoff caller from treating an arbitrary
destination as valid, but does not itself move entities. `ZoneAuthoritySet`
acquires independent per-zone leases in stable zone-ID order and attempts to
release earlier acquisitions if a later zone is unavailable. The runtime still
needs to connect this seam to multi-zone scheduling, enforce route-specific
access and travel cooldowns, write durable handoff receipts, and integrate
independent recovery.

`ZoneHandoffCoordinator` now validates both atlas placement and the authored
directed/bidirectional route before moving a persistent, living player entity.
The simulation handoff validates an unblocked, unoccupied destination cell,
preserves identity and compatible timed combat/action-cooldown state, clears the
old controller, and rolls back the destination spawn if source removal fails.
Request receipts are idempotent and bounded in-process; snapshots taken after a
completed transfer recover with the entity in exactly one zone. These receipts
are not yet durable, and saving the two zone snapshots is not yet one atomic
transaction, so crash-window recovery remains open work rather than a promised
guarantee.

`ZoneTickScheduler` defines the initial multi-zone scheduling policy: stable
round-robin order, one authoritative zone-tick quantum per zone in each batch,
and a rotating cursor when the global quantum budget is smaller than the active
zone set. The current single-zone `Service` loop does not yet consume this
scheduler; connecting zone ownership, gateways, session routing, and per-zone
persistence to it remains runtime integration work. It therefore provides a
testable fairness policy, not a claim that multiple zones are already served.

---

## SIM-015 — Calendar objectives and NPC participation

- **Priority:** P2
- **Status:** Blocked — `SIM-001`, `SIM-006`
- **Depends on:** `SIM-001`, `SIM-006`
- **References:** [world simulation §World calendar](../world-simulation.md#world-calendar-and-conflicts),
  ADR 0021

**Why:** The calendar lifecycle, enrollment, conflict resolution, and territory
changes are implemented. Objectives and NPC participation through the same
action rules are still design-only.

**Scope**

- Content-defined objectives and scoring rules for events.
- NPC sides that act through the normal action pipeline with budgets.
- Interrupt/suspend/resume semantics that cannot accidentally resolve.
- Editor overlays for scheduling and inspecting events (`CNT-016`).

**Acceptance criteria**

- [ ] Cancelling or suspending an event never applies its victory result.
- [ ] Duplicate resolution never duplicates rewards or territory changes.
- [ ] NPC participation obeys the same permissions, cooldowns, and range checks.
- [ ] Objective progress is visible to eligible participants only.

---

## SIM-016 — Faction and territory edge hardening

- **Priority:** P2
- **Status:** In progress — generated territory invariants and persistence recovery checks added; full reconciliation evidence remains
- **Depends on:** `PST-003`
- **References:** ADR 0020, [world simulation](../world-simulation.md#factions-guilds-and-wealth)

**Scope**

- Property-based tests for territory expiry, contest, ownership change, and
  history.
- Cancellation, rollback, and cross-aggregate transaction failure paths.
- Wealth/power invariants and reconciliation queries.
- Backfill/verification when moving from whole-world snapshots to normalized
  tables.

**Acceptance criteria**

- [x] Generated territory sequences prove eligible ownership changes under
      configured windows and power, while early failed captures are atomic.
- [x] Cancelled/failed calendar resolutions leave ownership and treasury
      unchanged; cancellation and rejection regressions are covered.
- [ ] Snapshot and normalized-table views agree after backfill/restart in the
      normalized faction/calendar persistence suite; live database evidence
      was unavailable in this environment.
- [x] History remains append-only, validated, and queryable by territory.

---

## SIM-017 — Minigame and panel expansion

- **Priority:** P3
- **Status:** Blocked — `CNT-005`
- **Depends on:** `CNT-005`
- **References:** ADR 0016, [content authoring §UI panels](../content-authoring.md#ui-panels)

**Scope**

- Additional concrete panels: inventory, terminal, dialogue, crafting.
- Content-defined minigames or a bounded minigame runtime beyond the
  combination lock.
- Per-principal budgets, lockouts, and anti-automation rules for new minigames.
- Panel capability/permission auditing.

**Acceptance criteria**

- [ ] New panels are content-only and use the common protocol.
- [ ] Progress is never replicated; only permitted results are.
- [ ] Abuse limits are enforced per principal, not per connection.
- [ ] Each minigame has deterministic tests.

---

## SIM-018 — Determinism and tick budget

- **Priority:** P1
- **Status:** In progress — release tick benchmark, runtime phase timings, replay regression, and CI p99 deadline gate implemented; broader workload scenarios remain
- **Depends on:** —
- **References:** [architecture §Simulation model](../architecture.md#simulation-model),
  `OPS-001`

**Why:** The simulation claims determinism and fair budgets, but movement,
combat depth, and AI will add work. Without a measured budget, the first
graphical clients will expose tick overruns as lag.

**Scope**

- Instrument tick phases (controllers, actions, effects, replication prep) and
  define per-phase budgets.
- Remove iteration-order nondeterminism (hash maps, unordered sets) from
  simulation-visible paths.
- Add seeded property tests/replays for multi-tick scenarios.
- Define overload behavior: shed cosmetic work before authoritative work, and
  never silently drop player actions.

**Overload policy:** Authoritative actions are validated and reported to their
requester; queue admission rejects excess movement with a correlated error
instead of dropping accepted input silently. AI controller scans, perception,
pathfinding, and replication queues have explicit caps and fixed diagnostics;
exhausted AI/path budgets defer or reject that work without overrunning the
authoritative tick. This simulation currently has no cosmetic tick work to shed.

**Acceptance criteria**

- [x] A release benchmark reports p50/p95/p99 tick duration and entity counts.
- [x] Replaying the same inputs produces byte-identical snapshots.
- [x] Overload behavior is documented and tested.
- [x] The CI benchmark fails when p99 exceeds the authoritative 50 ms tick
      deadline.

**Measured baseline:** `cargo bench -p engine_sim --bench tick` reported
100-entity p50/p95/p99 of 1.5/2.5/2.8 µs and 1,000-entity
13.5/26.0/38.6 µs on the local Windows release build. At 1,000 entities, phase
p99 was effects 29.2 µs, movement 0.2 µs, navigation 0.1 µs, AI 0.2 µs, and
statistics 4.8 µs. The benchmark also reports p99 for effects, movement, navigation, AI, and statistics phases. Runtime phase
measurements are diagnostics only and never enter snapshots or influence
simulation decisions. The CI gate is deliberately the 50 ms server tick deadline
rather than a machine-specific microbenchmark threshold; broader combat/AI
workload scenarios remain follow-up coverage.

---

## SIM-019 — Zone systems and shared utilities

- **Priority:** P2
- **Status:** Blocked — `SIM-014`, `PST-005`, `SIM-018`, `CNT-005`
- **Depends on:** `SIM-014`, `PST-005`, `SIM-018`, `CNT-005`
- **References:** [world simulation §Zone systems, work, and local economy](../world-simulation.md#zone-systems-work-and-local-economy),
  [content authoring](../content-authoring.md)

**Why:** Zones should be places with distinct, interdependent mechanics, not
only maps containing independent entities. Shared local infrastructure creates
cooperative goals and gives later jobs and trade systems meaningful outputs.

**Scope**

- Define a bounded, data-driven zone-system model with configurable inputs,
  outputs, consumers, failure behavior, update cadence, and offline policy.
- Implement a zone-scoped power network: generation, storage, distribution,
  demand, priority, outages, and recovery. Consumers such as doors, lights,
  pumps, and workstations use the same authoritative zone supply.
- Allow other zone mechanics (production, hazards, or local services) to use
  the same bounded system contracts without requiring every zone to enable them.
- Resolve player operation/repair through server-validated actions; clients may
  inspect replicated status but cannot set supply or consumer outcomes.
- Persist and version consequential system state, with deterministic replay,
  diagnostics, limits, and migration/quarantine behavior.
- Define behavior for essential services when no players are present and prevent
  unbounded simulation work or resource accumulation.

**Acceptance criteria**

- [ ] Two consumers in one zone compete for the same configured supply and
      priorities/outages produce deterministic documented results.
- [ ] A client cannot forge power, repair outcomes, or system state.
- [ ] Save/restore and replay preserve system state and reject incompatible
      versions with actionable diagnostics.
- [ ] Offline progression, resource caps, failure recovery, and tick budgets are
      tested, including a zone with no active players.
- [ ] Zones can opt into different system configurations without engine forks.

---

## SIM-020 — Zone work, dynamic markets, and faction contribution

- **Priority:** P2
- **Status:** Blocked — `SIM-019`, `SIM-010`, `SIM-011`, `SIM-014`, `PST-003`, `PST-014`
- **Depends on:** `SIM-019`, `SIM-010`, `SIM-011`, `SIM-014`, `PST-003`, `PST-014`
- **References:** [world simulation §Zone systems, work, and local economy](../world-simulation.md#zone-systems-work-and-local-economy),
  [SIM-010](#sim-010--trade-and-economy-integrity),
  [SIM-011](#sim-011--progression-and-quests)

**Why:** Players need repeatable, server-validated ways to keep zone systems
working and produce useful goods. Local supply and demand can then create
regional trade opportunities, while explicit accounting connects local success
to faction wealth without conflating the two.

**Scope**

- Add configurable work/contracts such as farming, repeatable local requests,
  courier deliveries over validated routes, and machinery or utility repair.
  Activities may use skills, credentials, faction policy, or contract limits;
  they need not be permanent character classes.
- Make work produce concrete outputs (goods, delivered stock, repaired capacity,
  or completed service) and use idempotent, auditable reward grants. Reuse
  quest/progression and trade contracts rather than introducing duplicate
  reward or inventory paths.
- Track per-zone configured stock, consumption, demand, and market orders; derive
  prices from bounded, smoothed inputs with floors/ceilings and explicit update
  cadence.
- Support import/export along valid routes, accounting for travel time,
  capacity, fees, risk/loss, and market refresh. Prevent guaranteed or
  repeatable value creation through arbitrage, stale prices, or duplicate
  deliveries.
- Define explicit transfers for configured zone revenue, taxes, upkeep, and
  losses into/out of faction treasury. Keep local economic health separate from
  faction balances and power; make every movement reconcilable and auditable.
- Provide player-visible stock/price information and enough history to make
  informed choices without promising fixed profits.

**Acceptance criteria**

- [ ] Work, delivery, and reward requests are server-authorized, bounded,
      idempotent, and cannot be repeated to mint value.
- [ ] Supply/demand changes affect prices within documented bounds and cadence;
      deterministic replay yields the same market state.
- [ ] Import/export requires a valid route and cannot double-spend stock or
      guarantee infinite profit under repeated transactions.
- [ ] Player, zone, and faction balances reconcile; faction contributions are
      explicit transfers and do not count value twice.
- [ ] Offline market/work progression, persistence, migration, and recovery are
      documented and tested.
- [ ] Tests cover shortages/surpluses, price limits, fees/loss, concurrent
      orders, failed deliveries, duplicate rewards, and faction accounting.
