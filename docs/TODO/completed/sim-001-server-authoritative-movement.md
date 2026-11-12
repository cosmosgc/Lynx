# SIM-001 — Server-authoritative movement

**Status: Done.** The backlog records this outcome: queued, collision-checked, rate-limited movement authority. All acceptance criteria below are checked complete.

## Original task scope

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

## Acceptance evidence

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

## Source

See [`10-simulation-gameplay.md`](../10-simulation-gameplay.md) for the full task history and implementation notes.
