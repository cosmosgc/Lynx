# ADR 0030: Player property ownership

- **Status:** Accepted
- **Date:** 2026-11-12

## Decision

Player-owned property and leases are out of scope for the initial playable
vertical slice. Map placements and authored world structures remain owned by
versioned game content, not by a player, character, account, or faction. The
server will not expose player placement, transfer, rental, or seizure actions
until a later ADR defines the owner identity, lease duration, renewal/default,
transfer, placement limits, persistence, and moderation rules together.

## Consequences

- The editor may place and configure authored structures but cannot publish
  player-owned property.
- No durable player-property schema or lease service should be added as a
  side effect of the content editor.
- A future housing feature must make its ownership and recovery model explicit
  before implementation and must preserve server authority over placement.
