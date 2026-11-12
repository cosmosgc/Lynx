# ADR 0024: Camera and input abstraction

- **Status:** Accepted
- **Date:** 2026-11-12

## Decision

Client controls produce semantic game actions before they reach presentation
or networking code. User bindings map physical keys or buttons to those
actions and remain client configuration; server authority validates the
resulting gameplay requests independently. Camera behavior is selected through
the client camera-mode interface, with the current `follow3d` and `billboard2d`
modes as independent implementations. No camera or device-specific input type
may cross into `engine_core`, `engine_sim`, `game_api`, or wire schemas.

## Consequences

- Rebinding and adding a camera mode do not change the protocol or authoritative
  simulation.
- Camera modes may share semantic movement intent but keep their projection,
  framing, and presentation settings separate.
- The Bevy client owns device events and translates them into the stable client
  action vocabulary.
