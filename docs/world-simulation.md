# Living world, factions, and game updates

This document connects Lynx's zone, faction, content, persistence, and editor
designs into one world-simulation contract. It describes intended capabilities,
not features that are already implemented. The implementation roadmap in the
[project guide](lightweight_mmorpg_engine_project_guide.md) remains the source
of delivery status.

**Current implementation:** authored world-map atlases place stable zone IDs on
map cells, declare explicit routes, and validate duplicate, missing, and
orphaned references (ADR 0019). Faction governance, membership, an append-only
treasury ledger, inspectable power, and per-zone territory ownership with
expiry, contest state, and history are implemented in the faction domain and
persisted through a versioned snapshot (ADR 0020). A durable world calendar now
implements the event lifecycle, faction-checked enrollment, and one bounded
conflict per event whose deterministic result can change territory ownership
([ADR 0021](adr/0021-world-calendar-and-bounded-conflicts.md)). Map-editor
overlays and player-facing event objectives remain design-only.

## Design goals

- A creator can build and operate a complete MMORPG world without forking the
  engine for common systems such as zones, factions, territory, scheduled
  events, and player grouping.
- The same authoritative rules apply to player-governed and NPC-governed
  factions. Differences come from permissions, policy, and controllers, not
  separate incompatible game systems.
- The world can change over time through scheduled, interruptible events whose
  outcomes have durable, explainable effects.
- A game can adopt compatible engine improvements through a supported engine
  upgrade, while retaining control over its content, configuration, and rollout.
- Shared services such as party finder and server-validated teleport are
  reusable engine capabilities, configured by each game rather than reimplemented
  in each game.

## World map and zones

The authored world map is an atlas of stable-ID zones and the routes between
them. A zone is a content and simulation boundary; it is not necessarily a
server process, shard, or faction territory. A territory is ownership state
associated with one or more zones, so capturing a zone never rewrites its
geometry, identity, authored lore, or content bundle.

The map definition owns static facts: zone IDs, bounds, map coordinates,
environment and lore references, spawn and navigation data, and explicit
routes or transfer points. The live world owns dynamic facts: current
controller/owner, contest state, active event, and the history of ownership
changes. Use stable IDs and explicit cross-zone routes so maps can be moved or
expanded without making coordinates or display names authoritative.

Every persistent, player-facing zone must participate in the territory model:
it is owned by a faction or explicitly unclaimed, and can be captured under
the game's configured rules. Do not hard-code permanent immunity for a
player-facing zone. Temporary protection, cooldowns, prerequisites, and
new-player safeguards must be visible rules with defined expiry. Technical
instances such as private housing interiors may inherit territory policy
without becoming separate strategic territories.

The editor must provide a world-map view as well as per-zone editing. Creators
need to create, rename, connect, reposition, and organize zones; edit transfer
routes and ownership policy; inspect territory and event overlays; and preview
travel and capture boundaries. Validation must catch duplicate IDs, missing
routes, invalid endpoints, orphaned territory records, and broken references
between maps, zones, lore, factions, and events. Publishing a map change must
show a diff and must not silently transfer live ownership.

## Factions, guilds, and wealth

Use one faction domain model for player-founded groups, guilds, and NPC
polities. A guild is a player-governed faction, not a parallel entity type
with separate territory or economy mechanics. Each faction has a stable ID,
localized display/lore references, a governance/controller policy, members or
agents, relations, treasury/economic ledger, and optional territory. Player
versus NPC behavior is determined by its controllers and permissions: NPC
factions can claim, own, defend, lose, and contest territory through the same
validated operations available to player factions.

The simulation resolves faction actions on the server. Membership, leadership,
recruitment, diplomacy, treasury transfers, territory changes, and event
participation are permission-checked and audited. A creator can configure
governance rules (such as who may declare a conflict or spend treasury) without
changing engine code. Player factions can use the same capabilities as NPC
factions, subject to their configured game rules and abuse controls.

Faction wealth is durable economic state, not a client-supplied score. Define a
game-specific, inspectable power calculation from eligible treasury and
economic inputs, with explicit valuation, update cadence, and caps or
diminishing returns. The derived power can affect configured strategic
capabilities, such as logistics, upkeep, defenses, or event strength; it must
not directly grant arbitrary combat outcomes. Record which inputs and rule
version produced a result, test that wealth changes produce the documented
effect, and account for spending, losses, and transfers so wealth is not
counted twice. This keeps both the inputs and consequences understandable to
players and operators.

Faction-to-faction alignment and individual reputation continue to follow
[ADR 0009](adr/0009-faction-alignment-and-reputation.md). Alignment answers
who is friendly or hostile; territory ownership, membership, and wealth are
separate concepts and must not be inferred from alignment.

## World calendar and conflicts

Provide an in-game world calendar and queryable event feed backed by durable
event records, not only a process-local timer. Creators and authorized faction
governance can schedule events with a stable ID, world-clock start and end,
affected zones or territories, participating factions, eligibility and
enrollment rules, objectives, cancellation policy, and a versioned resolution
rule. The client renders dates in the player's locale and time zone; the server
stores and compares an unambiguous timestamp and declares the world-clock
policy.

An event has an explicit lifecycle: `Draft`, `Scheduled`, `Open`, `Active`,
`Suspended`, `Cancelled`, and `Resolved`. Only valid, audited transitions are
accepted. The calendar shows scheduled and current events, affected territories,
participating sides, enrollment state, and changes or cancellation. A conflict
can be stopped by an authorized operator or by its configured conditions. A
cancelled or suspended conflict must not accidentally resolve or award
territory. The rules define whether a resumed event keeps its state or must be
rescheduled.

Players can join an eligible side through a server-validated enrollment action.
The event defines whether joining requires faction membership, permits allies
or mercenaries, has capacity or role limits, and when enrollment closes.
Participation is bound to an authenticated character and event; it cannot
transfer a side or submit a result. Combat and objectives use the ordinary
authoritative action pipeline.

Resolution is deterministic for its recorded inputs and idempotent under
retries. A successful result is committed with its event status, audit record,
and affected territory ownership changes in one transaction, then replicated
to relevant zones and calendar clients. Competing conflicts affecting the same
territory must use explicit serialization or conflict rules to prevent
overlapping results. Keep a result record containing the rule version,
participants or eligible aggregates, outcome, and ownership changes so an
operator can explain and recover it. Cancellation, restart, delayed execution,
and partial failure must never produce duplicate rewards or ownership flips.

### Implemented calendar and conflict rules

`engine_core::world_event` provides the durable aggregate and its contracts:

- A `WorldEvent` uses an `event.*` definition ID, a title and optional
  description, an absolute `EventSchedule`, one `zone.*` target territory, and
  exactly two sides in the `side.*` namespace. One side is the attacker and one
  is the defender, and each side names exactly one `faction.*` participant.
- The lifecycle is `Draft -> Scheduled -> Open -> Active`; `Active` may
  `Suspended` and back; cancellation is accepted from any non-terminal state
  subject to `CancellationPolicy`; only `Active -> Resolved` applies a result.
  Every transition is an append-only, time-ordered `EventTransition` with a
  validated actor, and suspension or cancellation requires a reason.
- Scheduling requires the defender to own the target and reserves the target
  against another non-terminal event. Activation marks the territory contested.
  Suspension, cancellation, and defender victory clear the contest. Resuming
  requires that the target still belongs to the defender.
- `EnrollmentRules` decide whether faction membership is required, whether
  unaffiliated participants are allowed, and the per-side capacity. The
  authoritative `FactionWorld` supplies the participant's faction; membership
  and side choices are validated before an immutable `EventEnrollment` is
  recorded, and a participant cannot change sides.
- `ConflictRule` records a version, the scored input (`participants` or
  `faction_power`), and a minimum per side. Resolution compares the two scores;
  a tie goes to the defender. The result records both sides' inputs and scores
  plus the exact rule snapshot.
- An attacker victory captures the target through the trusted
  `FactionWorld::award_territory` path, which rechecks the recorded defender and
  the configured protection and cooldown windows. A defender victory records no
  territory change. Resolution requires the scheduled end time, and replaying a
  resolved event returns the original result.
- `WorldCalendar` and the faction world are cloned before mutation. The calendar
  status, result record, and territory capture commit together, or neither
  aggregate changes.
- `CalendarSnapshot` is the versioned persistence format. Recovery rebuilds the
  transition chain, requires a result for every resolved event, checks
  enrollment against side membership, and cross-validates the calendar against a
  `FactionSnapshot` so two persisted aggregates cannot disagree.
- `game_api::CalendarService` exposes public events, enrollment, and results to
  authenticated accounts and hides drafts. `CalendarStore` is the trusted
  load/replace seam; the in-memory adapter cross-checks both snapshots. SQL
  migration `0005_world_events.sql` stores the same facts with append-only
  transitions, enrollment, and result tables plus deferred lifecycle, side,
  enrollment, and result checks.

## Engine features and game upgrades

Engine releases should make shared capabilities available to every compatible
game that updates its engine dependency/runtime; game creators should not need
to copy feature code or maintain an engine fork. Each game project declares its
engine compatibility range and content version. New engine capabilities expose
stable APIs and data-driven configuration, and generated projects receive
upgrade notes and validation for compatibility.

Availability does not mean silently changing every live game's behavior.
Games deliberately choose and deploy an engine release through their normal
staging and rollout process. Compatible fixes and opt-in features can be
adopted without rewriting world content; changes that affect persisted state,
protocols, or gameplay semantics require explicit migration, version checks,
and a rollback path. Feature flags or game configuration may control behavior
where it is appropriate, but must not be a substitute for versioning or
authorization. Clients and servers must agree on supported protocol and
content versions before a session enters the world.

The publishing pipeline therefore treats engine code/runtime upgrades and
content-bundle updates as related but distinct artifacts: validate both,
display both diffs, stage them together when required, track adoption, and
restore a known-compatible pair on rollback. See
[content authoring](content-authoring.md) and [ADR 0017](adr/0017-content-manifest-versioning.md).

Examples of reusable additions include a party finder with configurable
eligibility, roles, and destination rules, and teleport actions that validate
the actor, destination, access, cooldown, and cross-zone route on the server.
They should use shared social and zone-transfer contracts, not client-side
shortcuts or game-specific engine forks.

## Delivery and acceptance checks

Build this capability in layers rather than implying the entire world model
exists in the initial playable slice:

1. Establish stable world-map and zone IDs, explicit routes, and map validation.
2. Extend the shared faction model with governance, treasury, membership, and
   transactional territory ownership.
3. Add a durable calendar/event lifecycle and one bounded faction conflict.
   *Implemented in ADR 0021.*
4. Add player enrollment, NPC participation through the same action rules, and
   deterministic territory-changing resolution. *Implemented in ADR 0021.*
5. Add map-editor overlays and calendar/faction inspection, then exercise
   engine and content upgrades through staging and rollback.

Before release, test at least these invariants: unowned zones can be claimed
under configured rules; every eligible territory can change hands; guild and
NPC faction actions pass through the same authorization and transaction
boundaries; wealth changes affect only documented power inputs; cancelling an
event never applies its victory result; duplicate resolution never duplicates
rewards or ownership changes; and a compatible engine update preserves the
game's content and persistent state or presents an explicit migration failure.
