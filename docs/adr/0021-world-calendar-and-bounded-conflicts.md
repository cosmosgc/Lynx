# ADR 0021: World calendar, enrollment, and bounded conflicts

- **Status:** Accepted
- **Date:** 2026-10-26

## Context

The faction domain records durable governance, treasury, and territory
ownership (ADR 0020), but the world had no durable way to schedule events or to
change territory through a conflict. A process-local timer would lose
scheduled events on restart, and a mutable "current conflict" flag would make
cancellation, retries, and recovery ambiguous.

The world-simulation design requires scheduled, interruptible events whose
results have explainable durable effects, player and NPC participation through
the same server-validated rules, and deterministic, idempotent resolution that
cannot award a territory twice.

## Decision

- **One calendar aggregate.** `engine_core::world_event::WorldCalendar` owns
  event records, lifecycle transitions, enrollment, and final conflict results.
  Events use `event.*` definition IDs, sides use `side.*`, and targets use zone
  IDs. Events are server state and never rewrite authored zone content.
- **Explicit, audited lifecycle.** The accepted states are `Draft`,
  `Scheduled`, `Open`, `Active`, `Suspended`, `Cancelled`, and `Resolved`. The
  only transitions are creation to draft, draft to scheduled, scheduled to open,
  open to active, active to suspended, suspended to active, active to resolved,
  and cancellation from any non-terminal state. Cancellation policy can restrict
  cancellation to the pre-start window. Every transition appends an
  `EventTransition` with a validated actor, monotonic time, and a reason for
  suspension or cancellation.
- **Bounded conflict shape.** Each event targets one territory and has exactly
  two sides: one attacker and one defender, each naming exactly one faction.
  The two sides cannot share a faction. A non-terminal event reserves its target
  so two conflicts cannot resolve against the same territory out of order.
- **Scheduling and activation are checked against faction state.** Scheduling
  requires the defender to currently own the target. Activation requires the
  scheduled window, marks the territory contested, and rechecks ownership.
  Suspension clears the contest; resuming requires the target to still belong to
  the defender. Cancellation clears the contest and never applies a result.
- **Enrollment reads authoritative membership.** `EnrollmentRules` decide
  whether faction membership is required, whether unaffiliated participants are
  allowed, and per-side capacity. The participant's faction comes from
  `FactionWorld`, never from client input. Enrollment is immutable: replaying
  the same side returns the original record, and switching sides is a conflict.
  NPC and player participants use the same enrollment and resolution rules.
- **Deterministic resolution.** `ConflictRule` records a rule version, a scored
  metric (`participants` or `faction_power`), and a minimum per side. Resolution
  requires the scheduled end time, recomputes both scores from authoritative
  state, and keeps both inputs and the rule snapshot in `ConflictResolution`.
  A tie goes to the defender because the defender retains what it already owns.
- **Territory changes use the trusted faction path.** An attacker victory calls
  `FactionWorld::award_territory`, which rechecks the recorded defender, respects
  protection and cooldown windows, clears the contest, and appends normal
  territory history. A defender victory records no territory change. The result
  stores the capture proof, so a missing or mismatched change fails validation.
- **Atomic and idempotent.** Resolution clones the calendar event and the faction
  world before applying the result. Status, result, and ownership change either
  commit together or leave both aggregates unchanged. Replaying a resolved event
  returns the original result without appending another transition or change.
- **Validating recovery.** `CalendarSnapshot` is the versioned persistence
  format. Recovery rejects newer major versions, duplicate events, transition
  or enrollment keys, non-contiguous history, status/history disagreement,
  enrollment that violates its side's rules or capacity, a resolved event
  without a result, and a result whose sides or capture do not match the event.
  `WorldCalendar::validate_against` additionally requires faction and territory
  references to exist, active events to contest their target, non-active
  reserving events not to contest it, and attacker captures to appear in
  faction history.
- **Adapters and SQL.** `game_api::CalendarService` exposes public events,
  enrollment, and results to authenticated accounts; drafts are hidden.
  `CalendarStore` is the trusted load/replace seam. The in-memory adapter stores
  the whole `WorldCalendar` and cross-validates both directions when either
  snapshot is replaced. `0005_world_events.sql` adds normalized event, side,
  transition, enrollment, result, and result-side tables. Deferred constraint
  triggers enforce the lifecycle chain, exactly two sides, side-consistent
  enrollment, a result exactly when the event is resolved, participant counts,
  winning-side outcomes, and append-only history.

## Consequences

- Calendar state survives restarts and can be inspected and explained, because
  every status change and result is durable.
- Conflicts cannot capture a territory twice, cannot resolve before their
  scheduled end, cannot change sides after enrollment, and cannot award a
  territory the defender no longer owns.
- A tie never steals territory, and an attacker victory reuses the same
  protection and cooldown rules as ordinary captures.
- Calendar and faction snapshots cannot be loaded into a mutually inconsistent
  world; recovery fails loudly instead of silently preferring one aggregate.
- `CalendarService` is read-only. Mutating calendar commands are authoritative
  simulation actions: `engine_sim::calendar` validates participant enrollment
  and staff-gated lifecycle operations against the calendar and faction world,
  deduplicates requests, and expects the caller to persist both snapshots
  together. Routing these commands through backend traits and the world gateway
  remains future work.
- Event objectives, rewards, and map-editor calendar overlays remain future
  work. The current conflict is intentionally one bounded attacker-versus-
  defender contest over one territory.
