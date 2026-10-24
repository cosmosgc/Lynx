# ADR 0016: Server-validated minigames

- **Status:** Accepted
- **Date:** 2026-10-24

## Context

Phase 4 requires one server-validated minigame behind the common UI protocol.
Content definitions already described a minigame contract (panel schema,
authority mode, rewards), and the gateway already stored the panel a validated
action opened. What was missing was a runtime that could accept player input
without handing authority to the client.

A naive implementation would send the answer to the client, let the client
report success, or accept `play` as a normal `action.request`. Each choice
breaks a different rule: a modified client could read the answer, claim a win,
or bypass the panel's range and permission checks. Deriving the answer from
content that the client downloads is equally unsafe. The simulation must own
the combination, the attempt budget, the lockout, and the unlock, while
`world_gateway` stays transport-agnostic and content-agnostic.

## Decision

- **Minigames are simulation components.** `engine_sim::minigame::Minigame` is
  attached to an `EntityRecord` with `with_minigame`. It stores the content
  definition, combination length, attempts per budget, retry cooldown in server
  ticks, a server-held seed, and per-principal progress.
- **The combination is derived, never stored per client.**
  `Minigame::secret_for(principal)` mixes the seed with the principal's stable
  ID. It is deterministic so a player keeps the same code across reconnects and
  lockouts, and it is never included in an action effect, replication batch, or
  owner snapshot. The seed must be server entropy, not a value shipped inside a
  client-visible content bundle.
- **Input uses the panel channel.** Guesses arrive as `ui.action` with
  `action: "play"` and `params.guess`. The gateway rejects a direct
  `action.request` with `Verb::Play` using `Unsupported`, so the server always
  resolves the provider from the session's open panel and re-validates range,
  visibility, and permission before the guess is compared.
- **The verb is first-class.** `Verb::Play` requires `ControlCapability::Interact`
  like other interaction verbs. It is routable only through a target that
  supports it, which keeps capability checks uniform.
- **Guesses are validated before they cost anything.** A guess must be exactly
  the combination length in decimal digits. Malformed input returns
  `InvalidRequest` without creating progress or consuming an attempt.
- **Hints reveal comparison, not the answer.** Each accepted guess returns one
  `exact`, `higher`, or `lower` hint per digit plus the remaining attempts.
  Solving sets the solved flag; a wrong guess decrements the budget.
- **Lockout is a server-tick rule.** Exhausting the budget sets `locked_until`
  to the current server tick plus the configured cooldown. Further guesses return
  `Cooldown` until the tick passes; progress then resets to a fresh budget. The
  action pipeline uses the zone tick, not a client-supplied timestamp.
- **Solving unlocks the provider.** A solved challenge clears the door's lock,
  emits `MinigameSolved`, `DoorUnlocked`, and `UiClosed`, and lets the gateway
  clear the session panel. The lock state already replicates, so other players
  observe the unlock without learning the combination.
- **Progress is server-only.** `Minigame` is not part of `EntitySnapshot` or the
  interest set. Only the door lock state and the provider's interaction surface
  are replicated.
- **Attempts are per principal.** One player cannot spend another player's
  attempts or force a shared lockout. A second player at the same vault has an
  independent code, budget, and progress.

## Consequences

- A modified client can neither read the combination nor decide the outcome; it
  can only submit guesses and receive hints.
- The server can be probed for hints, so the attempt budget and lockout are the
  anti-brute-force control. The defaults are deliberately small.
- Minigame progress lives in memory and resets with the zone. Durable progress
  is future work if a minigame must survive a process restart.
- `world_gateway` still does not load content definitions, so it does not
  cross-check the panel action name against the loaded `MinigameDefinition`.
  That check belongs to a future content runtime; today the simulation rejects
  unsupported verbs and targets.
- The reference minigame is a combination lock. The component shape supports
  other challenge types, but each new type needs its own authority review before
  it can mutate world state.
