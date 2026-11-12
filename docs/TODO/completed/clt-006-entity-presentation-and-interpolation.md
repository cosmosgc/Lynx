# CLT-006 — Entity presentation and interpolation

**Status: Done.** The backlog records this outcome: client entity presentation delivered and visually reviewed; network-impairment tuning remains in `CLT-016`. All acceptance criteria below are checked complete.

## Original task scope

- Spawn/despawn/update visuals from authoritative snapshots and deltas.
- Interpolate remote entities between server ticks; never extrapolate beyond a
  documented bound.
- Give visible entities a grounded marker/ring and a facing-direction indicator
  oriented from authoritative facing data. Use clear, distinct styling for the
  local player, other entities, selected targets, and interactable targets.
- Show nameplates and health bars using only server-authoritative public
  presentation data. Further status-effect indicators are tracked in
  [CLT-018](../40-bevy-client.md#clt-018--public-status-indicators).
- Show a contextual interaction prompt/affordance only for actions exposed by
  the server for that entity. Executing interactions and panels remain in
  `CLT-008`.
- Handle visibility enter/leave and entity caps without popping or leaking.
- Provide a debug view of authoritative vs. rendered position.

**Presentation direction:** Use familiar tab-target MMORPG conventions (the
user cited FFXIV): a ground ring/marker under entities, a ground-facing cue,
readable nameplates, health display, and a contextual interaction
indicator. Create Lynx's own visual language rather than copying game assets.
The client `PublicEntity` projection contains prototype, position, yaw, optional
public health, interaction availability, and public stunned/slowed flags. Names
currently derive from prototype IDs; localized identity and broader status
effects have separate follow-up work. Never infer hidden/private state client-side.

## Acceptance evidence

- [x] Remote movement looks smooth at the target tick rate for the current local demo.
- [x] A teleport/correction does not cause unbounded extrapolation.
- [x] Despawned entities disappear immediately and do not linger in UI.
- [x] Rendered state never feeds back into authoritative decisions.
- [x] Visible entities have a ground marker and facing indicator matching
      authoritative position and yaw.
- [x] Nameplates display server-authoritative public identity and disappear or
      update with the entity.
- [x] Health bars display server-authoritative public health and disappear or
      update with the entity.
- [x] Interaction affordances reflect only server-exposed actions; invoking
      them is handled separately by `CLT-008`.
- [x] Target selection has distinct styling, supports cycling/clearing, and
      clears when the target leaves the public interest set or connection fails.

**Implementation progress:** The online sprite renderer now adds ground rings
and facing ticks to entities, with cyan for the local actor and gold for other
visible entities. Child visuals follow the rendered entity position and are
removed with their parent. Facing uses replicated yaw (0 degrees = +Z, 90 = +X).
Accepted player/NPC movement updates yaw on the authority; stationary and
rejected movement preserve it. Client tests cover projection and replicated
facing after movement, plus entity cleanup. The user visually approved the
Windows demo markers: the local actor is cyan, five remote entities are gold,
and each marker has a ground ring/facing tick (2026-11-04). Screenshot was
supplied in chat and is not stored in the repository.

**Health display progress:** Client snapshots/deltas now retain the gateway's
optional public hit points. Green health bars above entity markers update from
that data and follow/despawn with the entity. Missing health or a nonpositive
maximum hides the bar; the displayed fraction is clamped without altering
authority. Replication update/removal and fraction bounds have test coverage.
The user visually confirmed bars on the player and one entity in the Windows
demo (2026-11-04). Nameplates and status effects remain separate unfinished
work.

**Interaction affordance progress:** A small gold marker now appears above an
entity only when its server-projected interaction surface is enabled and
advertises at least one verb. It follows and despawns with the entity; removal,
empty verbs, or disabled state hides it. Automated projection/availability tests
cover the gate. The user visually confirmed the gold square above interactable
entities in the Windows demo (2026-11-04). The marker is informational only and
grants no authority; interaction execution remains in `CLT-008`. Richer
verb-specific prompts remain future polish.

**Nameplate progress:** Online entities now show a small text label above their
marker, derived from the server-replicated prototype ID (for example,
`character.demo_guard` renders as “Demo Guard”). This provides a readable
fallback without accepting arbitrary display text. It is not yet the authored,
localized display name: the public entity contract currently lacks that field
and the client does not resolve localization keys here. The user visually
confirmed readable labels in the Windows demo (“player” and entity names
including crate, owner crate, door, vault, and barrel) (2026-11-04). The current
prototype-based nameplate criterion is complete; localized authored display
names remain future polish, not a blocker for this criterion.

Initial stunned-state replication and a `STUN` client tag are groundwork for
the separately scoped [CLT-018](../40-bevy-client.md#clt-018--public-status-indicators). They are
not a CLT-006 acceptance criterion; the current demo lacks a visual fixture.

**Target selection progress:** Tab cycles visible non-local entities in stable
ID order; Shift+Tab cycles backwards and Escape clears selection. An orange-red
outer ring follows the selected entity. Selection clears on loss of active
connection or target visibility and is suppressed during chat entry/focus loss.
This is local UI selection, not interaction authorization. Cycle/wrap and ring
visibility have automated coverage; the user confirmed the Windows demo target
selection works as intended (2026-11-04). Screenshot was supplied in chat and is
not stored in the repository.

**Visual validation (2026-11-04):** The Windows local demo rendered the active
online world, six blue entity markers, and the authoritative/rendered position
overlay. The user confirmed movement and packet exchange, then clarified that
the movement feels smooth enough and approved this criterion. This is qualitative
Windows localhost validation, not a simulated-latency/jitter benchmark; such
network-condition validation remains tracked by CLT-016. Screenshot was supplied
in chat and is not stored in the repository.

## Source

See [`40-bevy-client.md`](../40-bevy-client.md) for the full task history and implementation notes.
