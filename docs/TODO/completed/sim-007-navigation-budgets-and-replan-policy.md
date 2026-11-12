# SIM-007 — Navigation budgets and replan policy

**Status: Done.** The backlog records this outcome: cumulative budgets, path diagnostics, replan policy, expiring blockers, shared nav bake, and an opt-in GuardChase consumer. All acceptance criteria below are checked complete.

## Original task scope

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

## Acceptance evidence

- [x] Budget exhaustion is deterministic, diagnosable, and never overruns the
      cumulative tick expansion/request limits.
- [x] A blocked path can degrade to an explicitly incomplete nearest-reachable
      path; callers can also choose fail-closed errors.
- [x] Path preview and runtime use the same validated, versioned `NavBake` data.
- [x] NPC path consumers honor bounded retry state and request replans after
      dynamic blockers expire without exceeding the tick budget.
- [x] Fixed-label pathfinding diagnostics are exported by the world metrics
      endpoint (the demo reports zero until its AI policy is enabled).

## Source

See [`10-simulation-gameplay.md`](../10-simulation-gameplay.md) for the full task history and implementation notes.
