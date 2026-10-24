# ADR 0002: Backend and authoritative simulation topology

- **Status:** Accepted
- **Date:** 2026-10-23

## Context

Nakama provides authentication, sessions, social features, chat, groups,
matchmaking, and an authoritative-match runtime. It does not provide a
general-purpose persistent world database, and its runtime is not the best home
for every simulation workload.

## Decision

- **Nakama 3.41.x** owns authentication, sessions, account metadata, friends,
  groups, chat, presence, and matchmaking.
- Authoritative zone simulation runs in a **separate Rust service**
  (`engine_sim`) that embeds the same domain crates as the client. It is not
  embedded in Nakama matches.
- Clients authenticate with Nakama, then present the session token to the world
  gateway. The gateway validates the token with Nakama and owns the player's
  world session.
- Nakama's authoritative-match feature may be used later for isolated,
  low-stakes activities (for example a minigame), but never becomes the source
  of truth for world state.
- All backend access goes through the interfaces in `game_api`
  (`AuthService`, `SessionService`, `SocialService`, `ChatService`,
  `WorldGateway`, `CharacterService`, `InventoryService`, `AdminService`).
  Gameplay code never imports a Nakama type.
- Start with a single world service process hosting multiple zones. Split zones
  across processes only when measurements demand it; keep the zone boundary
  explicit from day one.

## Consequences

- Simulation can be tested, profiled, and scaled without Nakama's runtime.
- Nakama upgrades cannot change gameplay semantics.
- The world gateway must implement its own session, presence, and interest
  management on top of Nakama identity.
- Running two services is an operational cost, offset by clear authority and
  simpler debugging.
