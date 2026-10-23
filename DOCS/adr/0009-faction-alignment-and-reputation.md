# ADR 0009: Faction alignment and reputation resolution

- **Status:** Accepted
- **Date:** 2026-10-23

## Context

Factions drive perception, targeting, dialogue, and assistance. If alignment
rules live in client content or are recomputed differently in different
systems, NPCs and players disagree about who is hostile.

## Decision

- Alignment is a signed integer in **-100..=100**, clamped at every boundary.
- Classification uses configurable thresholds with defaults:
  - **Enemy:** `<= -34`
  - **Neutral:** `-33..=33`
  - **Friend:** `>= 34`
- Faction definitions and faction-to-faction relations are database-backed
  records with stable IDs and schema versions. Content bundles may mirror them
  for tooling, but the server database is authoritative.
- Relations are **directional**: `FactionRelation { source, target, points }`.
  A convenience API writes both directions when a designer wants symmetry.
- Effective alignment for an observer/target pair is:
  `clamp(base(source_faction -> target_faction) + character_override + temporary_modifiers)`.
  Character overrides are keyed by `(character, target_faction)` and replace
  nothing: they are additive, so a quest reward can improve an individual's
  standing without rewriting global politics.
- Multiple temporary modifiers stack additively, are ordered deterministically,
  and may expire. Expired modifiers are ignored.
- The server resolves effective alignment. Clients receive the resulting class
  and only the detail their permissions allow.
- Every reputation change writes an audit event with actor, reason, before,
  after, and correlation ID.
- Changes take effect immediately for perception, targeting, and assistance in
  the same tick after the mutation is applied.

## Consequences

- Designers can tune politics without a code deployment.
- Additive overrides can push a friendly faction to neutral for one character,
  which is intentional and testable.
- Directionality is explicit, so asymmetric factions are possible without a
  schema change.
