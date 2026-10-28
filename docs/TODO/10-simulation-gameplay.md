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
- **Status:** In progress — bounded displacement validation foundation only
- **Depends on:** —
- **References:** [architecture §Simulation model](../architecture.md#simulation-model),
  [networking §Reconnect and recovery](../networking.md#reconnect-and-recovery),
  ADR 0010, `crates/engine_sim/src/nav.rs`

**Why:** There is no movement verb. `EntityRecord` has a transform and the
engine has navigation primitives, but a client cannot ask to move, and the
server has no per-tick movement budget to validate. Every client milestone
depends on this contract.

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

- [ ] A headless test proves a client cannot cross a wall, exceed the per-tick
      speed budget, move while dead/stunned, or move an entity it does not
      control.
- [ ] Two sessions observe the same positions for the same tick sequence.
- [ ] Movement is deterministic across repeated runs with identical inputs.
- [ ] A slow or spamming client cannot stall the authority loop.
- [ ] Protocol and tick-budget documentation is updated.

**Foundation:** `NavGrid::can_step` checks a single adjacent cell transition,
including blocked endpoints, bounds, and both diagonal corner cells; A* uses
the same rule. `NavGrid::allows_displacement` adds a finite horizontal position
and distance check against a caller-supplied speed and elapsed time, and permits
only same-cell or adjacent-cell moves. Diagonals conservatively require both
corner cells open. Its budget inputs must come from trusted server state; it is
not itself a per-tick budget or an authority boundary. It does not check zone
bounds, occupancy, or fine collision, expose requests, or mutate transforms.
Authority checks, tick advancement, budget enforcement, replication, and the
acceptance tests remain open.

---

## SIM-002 — Client prediction and reconciliation contract

- **Priority:** P0
- **Status:** Blocked — `SIM-001`
- **Depends on:** `SIM-001`, `CLT-005`
- **References:** ADR 0014, `SIM-001`

**Why:** A graphical client cannot wait a full round trip for every step, but
prediction must never become client authority. The server needs to acknowledge
input sequences and send bounded corrections the client can replay.

**Scope**

- Define input sequence numbers, server acknowledgements, and correction
  payloads (authoritative position, velocity, tick, last processed input).
- Keep a bounded client replay buffer; replay unacknowledged inputs after a
  correction and smooth visual error without changing simulation state.
- Define behavior for teleports, zone changes, deaths, and reconnect snapshots,
  which reset the prediction history.
- Document thresholds for "accept small error" versus "snap".

**Acceptance criteria**

- [ ] A simulated-latency/loss harness shows bounded divergence under normal
      movement and recovery after a forced correction.
- [ ] Corrections never grant the client authority over position, speed, or
      collision.
- [ ] Reconnect and zone-transfer paths reset prediction state explicitly.
- [ ] The contract is documented in `docs/networking.md`.

---

## SIM-003 — Server-derived combat resolution

- **Priority:** P0
- **Status:** Ready
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

- [ ] A request carrying an inflated `damage` field is rejected as an unknown
      field or ignored; damage matches the actor's server-side stats.
- [ ] Out-of-range, dead-actor, dead-target, and cooldown cases are tested.
- [ ] Death releases control and cannot be spoofed by the client.
- [ ] The combat formula and its inputs are documented.

**Interim behavior:** An `attack` accepts no parameters and applies one hit
point, clamped to the target's remaining health. Dead targets are rejected
without a second death event. This is a temporary server rule, not the planned
content-driven combat formula.

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
- **Status:** Blocked — `SIM-003`
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

- [ ] Effects expire deterministically on tick boundaries and survive snapshot
      round trips when their policy says so.
- [ ] Respawn cannot be triggered by a client-supplied location.
- [ ] Stacking and mitigation rules are covered by table-driven tests.
- [ ] Death/respawn behavior is documented for reconnect and zone transfer.

---

## SIM-006 — AI behaviors, schedules, and perception budgets

- **Priority:** P1
- **Status:** Ready
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

---

## SIM-007 — Navigation budgets and replan policy

- **Priority:** P1
- **Status:** Ready
- **Depends on:** `SIM-001`
- **References:** ADR 0010, `crates/engine_sim/src/nav.rs`

**Scope**

- Formalize expansion budgets, replan cadence, partial-path behavior, and
  failure codes.
- Handle moving obstacles, doors, and dynamic blockers without unbounded
  replanning.
- Expose a nav preview/bake format for the editor (`CNT-006`).
- Add metrics for expansions, budget exhaustion, and path failures (`OPS-001`).

**Acceptance criteria**

- [ ] Budget exhaustion is deterministic, diagnosable, and never overruns the
      tick budget.
- [ ] A blocked path degrades to a documented behavior rather than a freeze.
- [ ] Path preview and runtime use the same nav data.

---

## SIM-008 — World chat and presence

- **Priority:** P1
- **Status:** Ready
- **Depends on:** —
- **References:** `crates/game_api/src/chat.rs`, `social.rs`,
  [README Phase 3](../../README.md#phase-3--interaction-and-mind-system--in-progress),
  [security §Abuse prevention](../security.md#abuse-prevention)

**Why:** `ChatService` and `SocialService` exist in `game_api` and the in-memory
backend, but `world_gateway` does not route chat. Phase 3 cannot close without
it, and the client needs a real channel to render.

**Scope**

- Add gateway messages for sending, history, channel listing, and presence.
- Server-resolve sender identity, channel membership, zone scope, ignore/mute
  state, and rate limits; reject spoofed senders and unauthorized channels.
- Support zone, party, whisper, and system channels with bounded history and
  pagination.
- Hook moderation/reporting (`NET-009`) and redact private content from logs.
- Replicate presence changes through the existing interest/session machinery.

**Acceptance criteria**

- [ ] A client cannot send as another character or into a channel it has not
      joined.
- [ ] History is bounded, paginated, and ordered deterministically.
- [ ] Mute/ignore and rate limits are enforced server-side.
- [ ] Chat survives reconnect for the retained window where policy allows.
- [ ] Integration tests cover two sessions in one zone.

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
- **Status:** Blocked — `SIM-001`, `PST-005`
- **Depends on:** `SIM-001`, `PST-005`

**Scope**

- Multiple authoritative zones with explicit handoff, capacity, and ownership.
- Route/gateway/portal validation against the authored atlas; reject arbitrary
  teleports.
- Cross-zone party/chat/presence behavior and travel cooldowns.
- Per-zone tick loops or a scheduler with fair budgets; document the choice.
- Zone-local persistence and recovery.

**Acceptance criteria**

- [ ] Travel only follows validated routes and server-owned access rules.
- [ ] A handoff cannot duplicate an entity or leave it in two zones.
- [ ] A zone crash/restart recovers independently without corrupting others.
- [ ] Cross-zone chat/party behavior is documented and tested.

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
- **Status:** Blocked — `PST-003`
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

- [ ] Every eligible territory can change hands under configured rules.
- [ ] Cancelled/failed events leave ownership and treasury unchanged.
- [ ] Snapshot and normalized-table views agree after backfill.
- [ ] History remains append-only and queryable.

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
- **Status:** Ready
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

**Acceptance criteria**

- [ ] A benchmark reports p50/p95/p99 tick duration and entity counts.
- [ ] Replaying the same inputs produces byte-identical snapshots.
- [ ] Overload behavior is documented and tested.
- [ ] Budget regressions fail CI or a nightly job.
