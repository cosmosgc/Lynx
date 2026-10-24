# ADR 0010: Navigation representation and pathfinding budget

- **Status:** Accepted (Phase 1 scope)
- **Date:** 2026-10-23

## Context

Zone navigation must be authorable, server-validated, and bounded so a crowd of
AI agents cannot starve simulation ticks.

## Decision

- Phase 1 uses a **uniform cost grid** per zone, authored alongside the map:
  width, height, cell size, blocked cells (static), and optional per-cell cost
  multipliers. Diagonal movement is allowed only when both orthogonal
  neighbours are open.
- Grids are the canonical navigation data until terrain complexity justifies a
  navmesh. The zone format reserves a `navmesh` variant so the change is not a
  breaking content migration.
- Pathfinding is A* with deterministic tie-breaking. Each zone owns a
  `PathBudget` per tick: maximum expansions, maximum queued requests, and a
  maximum path length. Requests exceeding budget return
  `PathError::BudgetExhausted`; they are retried with lower priority rather
  than blocking the tick.
- Dynamic obstacles are represented by temporarily blocked cells with an
  expiry tick. Stuck detection triggers a bounded replan with exponential
  backoff.
- AI agents never move themselves. They request a path, then emit movement
  actions validated by the server against the same collider and bounds rules as
  players.
- Pathfinding and navigation state are observable: current path, goal, last
  error, expansions used, and replan count per agent.

## Consequences

- Navigation is cheap to author and to validate in tests.
- Very large or multi-level zones will need a navmesh variant; the interface is
  designed for that replacement.
- Budget exhaustion is a normal, observable outcome rather than a tick stall.
