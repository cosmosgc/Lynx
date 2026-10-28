# Icebox and research

These are deliberately **not scheduled**. They are kept here so promising ideas
do not get lost, with the trigger that would justify pulling them forward. A
proposal moves out of the icebox only when the trigger is met and an ADR or
scoping task exists.

| ID | Idea | Trigger to revisit | First step |
| --- | --- | --- | --- |
| ICE-001 | WebAssembly scripting/behavior runtime | A creator needs behavior beyond the bounded data-driven set, and a sandbox design is accepted | ADR 0008 review: capabilities, fuel limits, determinism, host ABI |
| ICE-002 | UDP with a reliable layer | WebSocket latency/head-of-line blocking measurably hurts the vertical slice | Transport spike behind the existing envelope; compare loss/latency under load |
| ICE-003 | Browser/Wasm client | Desktop slice is stable and browser demand justifies the storage/threading/asset work | Feasibility report covering IndexedDB, SharedArrayBuffer/COOP/COEP, audio, asset loading |
| ICE-004 | Mobile/handheld client | Desktop input/UI and performance budgets are stable | Input/UI/memory study; no renderer feature-gating shortcuts |
| ICE-005 | Console support | Desktop platforms are CI-smoked and business terms exist | Platform certification and controller/UI requirements |
| ICE-006 | Player housing/property | Ownership, lease, placement, and moderation rules are accepted (`QUA-017`) | ADR + bounded placement prototype in one zone |
| ICE-007 | Unrestricted public user-content publishing | Ownership/moderation/revocation (`CNT-008`) is live and audited | Threat model + staged rollout with quotas and review |
| ICE-008 | Cross-region / multi-datacenter placement | Single-region capacity or latency targets are exceeded | Measure `OPS-015`; design world routing and data residency |
| ICE-009 | Third-party engine extension/plugin API | Multiple games duplicate engine features or maintain forks | Define stable extension points, versioning, and sandbox/trust rules |
| ICE-010 | Voice chat | Text chat and moderation are mature and demand is explicit | Privacy, moderation, bandwidth, and platform policy study |

## Notes per item

### ICE-001 — WebAssembly scripting

ADR 0008 names Wasm as the planned escape hatch for advanced creator behavior,
but it needs a sandbox review before adoption. Requirements: deterministic
execution for replay, fuel/time limits, no ambient filesystem/network access,
a narrow host ABI, and the same authority checks as native actions. Do not let
scripts mutate state outside the validated action pipeline.

### ICE-002 — UDP transport

Gameplay messages are transport-independent by design. A UDP/reliable-layer
spike must preserve framing semantics, ordering guarantees where required, and
the session/resume model. Measure before committing: WebSocket may be entirely
adequate for the target zone sizes.

### ICE-003 — Browser client

The platforms document is explicit: browser support needs storage, threading,
asset-loading, audio, and transport decisions. Do not feature-gate renderer code
and call it supported. A browser build must pass the same authority, reconnect,
and content-version checks as desktop.

### ICE-004 — Mobile and handheld

Mobile is post-desktop. It requires input, memory, UI, packaging, and
certification work that would distract from the vertical slice. Revisit only
after the desktop client is packaged and smoke-tested.

### ICE-005 — Console support

Consoles are not planned before desktop is stable. They add certification,
controller/UI requirements, and platform business terms. Revisit only after the
desktop platform matrix is CI-smoked.

### ICE-006 — Housing and property

The security doc already requires bounds, collision, budget, ownership, and zone
rules for placement. Housing is a large content/moderation surface; prove it with
bounded examples after the editor and ownership model exist.

### ICE-007 — Public user content

This is a policy and abuse problem before it is a technical one. It depends on
`CNT-008` (ownership/moderation/revocation), `NET-009` (moderation pipeline), and
an accepted publishing policy. Start with private/allowlisted content, not open
uploads.

### ICE-008 — Multi-datacenter

Multi-zone routing (`NET-005`) and world scoping (`PST-012`) come first. Cross-
region placement adds data residency, latency, consistency, and operational
complexity that should be justified by measured demand.

### ICE-009 — Extension/plugin API

The world-simulation guide explicitly wants games to adopt engine capabilities
without forks. Before a plugin API, stabilize the data/content formats,
versioning, and upgrade workflow (`OPS-010`). An extension API without those is
just a second, less-tested engine.

### ICE-010 — Voice chat

Not in the current roadmap. If pursued, treat it as a separate media service with
its own privacy, moderation, consent, and bandwidth rules; do not route audio
through the authoritative simulation.
