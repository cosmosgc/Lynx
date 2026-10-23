# ADR 0004: Authoritative simulation boundary

- **Status:** Accepted
- **Date:** 2026-10-23

## Context

The Bevy client ECS is convenient for presentation, prediction, and input, but
it runs on a machine the player controls. Treating it as authoritative would
allow item, position, and combat forgery.

## Decision

- The client ECS contains presentation, input, prediction, interpolation, and
  local-only entities. It never owns authoritative state.
- `engine_sim` owns authoritative zone state: entity lifetime, transforms,
  health, inventories, links, minds, and action outcomes.
- Deterministic domain types (`StableId`, components, faction math, action
  validation, pathfinding) live in `engine_core`/`engine_sim` and are shared by
  the client and server. Sharing types does not share authority.
- Clients send `ActionRequest` values. The server resolves the stable target,
  checks distance, visibility, permissions, state, cooldowns, and costs, then
  applies or rejects the action with a structured error.
- AI controllers use the same action pipeline as players. There is no
  privileged "NPC shortcut" that bypasses validation.
- Prediction is optional and presentation-only until reconciled by a server
  snapshot. A rejected prediction must roll back without granting rewards.

## Consequences

- Every gameplay feature needs a server-side validation test.
- Client and server can share data definitions without sharing mutable state.
- Cheating becomes a validation problem with a single choke point rather than
  an audit of every client system.
