# SIM-003 — Server-derived combat resolution

**Status: Done.** All acceptance criteria below are checked complete in the source task.

## Original task scope

- Remove `damage` from the accepted request parameters (or ignore it) and derive
  damage from the actor's server-owned stats/loadout and the target's defenses.
- Validate range, line of sight, target liveness, control capability, cooldown,
  and friendly-fire/faction rules before applying damage.
- Resolve death, controller release, and loot/credit hooks through the existing
  effect pipeline. Do not let the caller choose the result.
- Keep the formula data-driven and versioned so content can tune it without an
  engine fork.
- Emit `Damaged`/`EntityDied` effects and audit-relevant events.

## Acceptance evidence

- [x] A request carrying an inflated `damage` field is rejected as an unknown
      field or ignored; damage matches the actor's server-side stats.
- [x] Out-of-range, dead-actor, dead-target, and cooldown cases are tested.
- [x] Death releases control and cannot be spoofed by the client.
- [x] The combat formula and its inputs are documented.

**Implemented:** Formula v1 is `max(minimum_damage, attacker.attack -
target.defense)`. `CombatStats` are server-owned resolved values (including
authoritative loadout effects); versioned `CombatRules` are persisted with the
zone and configure the minimum damage and friendly-fire/faction policy. Attack
effects include the server-resolved source for downstream loot, credit, and
audit consumers. See [security §Authority invariants](../../security.md#authority-invariants).

## Source

See [`10-simulation-gameplay.md`](../10-simulation-gameplay.md) for the full task history and implementation notes.
