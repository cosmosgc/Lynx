# ADR 0012: Interest management and replication audiences

- **Status:** Accepted
- **Date:** 2026-10-24

## Context

A zone can hold more entities than any client can display or should know about.
Broadcasting the whole zone would leak hidden objects and private property,
waste bandwidth, and make crowded areas scale with total entity count instead
of visible entity count. Transport-level filtering is not enough: the decision
of what a session may know is a gameplay and privacy rule, so it belongs in
engine code that can be tested without a socket.

Replication categories already declare a delivery guarantee and a default
audience, but nothing enforced that audience. Session-directed events also had
no way to name their recipient, so a gateway could not separate two sessions
fed from the same batch.

## Decision

- **Interest selection runs in `engine_net`** and is platform-neutral. It takes
  an observer position and an authoritative entity index; it does not own a
  transport, a zone, or a clock.
- **Entry and exit radii differ.** An entity enters at the observer's view
  radius and leaves at radius plus a configurable hysteresis margin (default
  12.5%). This prevents appear/disappear churn for entities pacing a boundary.
  Distances are horizontal.
- **Visibility is explicit.** Public, hidden (owner or staff), owner-only, and
  staff-only classes are checked before distance. Hidden state never becomes
  visible merely because it is close.
- **The nearest entities win under a cap.** Each observer is capped at 512
  entities by default; ties break on stable entity ID so results are
  deterministic across processes.
- **One interest set per session.** The manager retains the previous set, so a
  movement update produces added/removed deltas rather than a full resend.
  Disconnecting a session drops its set.
- **Filtering is fail-closed.** Session-directed updates must name their
  session. Owner-only and property-scoped updates must resolve to an owner or
  an authorized principal. Admin updates require a staff session. Missing
  entities or sessions are dropped, never promoted to a zone broadcast.
- **Filtered batches still collapse superseded state snapshots** per entity, so
  a batch that survives interest selection does not resend stale positions.
- **The gateway, not the client, supplies the observer and audience.** Clients
  cannot choose a larger view radius, claim staff, or name another session.

## Consequences

- Interest policy can be unit-tested deterministically and reused by any
  transport or gateway implementation.
- Sending a full-zone snapshot requires deliberately constructing one; the
  default path is per-recipient filtering.
- Hidden entities and property are protected by engine code rather than by
  client cooperation.
- Each session costs one stored set plus one refresh scan per tick. If scans
  become the bottleneck, a spatial index can replace the linear scan without
  changing the filtering rules or wire format.
- Party, quest, and ownership exceptions will need explicit visibility or
  authorization entries; silent special cases are not allowed.
