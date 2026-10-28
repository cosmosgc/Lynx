# Roadmap and sequencing

This roadmap turns the phase scope in
[project guide §23](../lightweight_mmorpg_engine_project_guide.md#23-suggested-implementation-roadmap)
into delivery milestones with concrete exit criteria and task IDs. It does not
replace the phase roadmap; it answers "what do we pick up next, and what must be
true before the next milestone starts?"

## Planning principles

1. **Prove the loop before broadening it.** A graphical client that can log in,
   move, chat, and reconnect is worth more than another server-only subsystem.
2. **Risk first.** Authority bugs, authentication gaps, and unrecoverable
   persistence fail late and expensively. They come before content breadth.
3. **Thin vertical slices.** Each milestone should be demonstrable end to end,
   not a pile of crate-local features waiting for integration.
4. **One authoritative path.** Clients send intent; the server validates and
   resolves. No milestone may introduce a client-side shortcut that later needs
   to be removed.
5. **Everything reversible.** Persisted state, content, and protocol changes
   need a migration and a rollback story before they ship.

## Milestones at a glance

| Milestone | Official phase | Goal | Exit criteria |
| --- | --- | --- | --- |
| **M0 — Truth and safety** | Phase 2 hardening | Remove authority holes and make the docs honest before adding a client | No implemented verb takes an outcome from the request; production auth path is proven; rate limits exist; all gates and docs agree with the code |
| **M1 — Client foundation** | Phases 1–3 | Two graphical clients share one authoritative zone | Two clients log in, select characters, see each other move, chat, and interact; reconnect works; Phase 1 and 2 exit criteria met |
| **M2 — Vertical slice** | Phases 4 and 7 | A small group can play a sustained session | One polished zone with movement, chat, inventory, interactions, simple AI, persistence, packaging, and recovery from disconnect/server restart |
| **M3 — Creator pipeline** | Phase 5 | A creator builds a zone without touching engine source | Editor can place zones/entities/links, validate, preview, and publish; MCP read-only tools and docs exist; Phase 5 exit criteria met |
| **M4 — Live operations** | Phase 6 | An operator can run and change a live world safely | Metrics, backups/restore drills, staging publish, rollout/rollback, and admin inspection are exercised; Phase 6 exit criteria met |
| **M5 — Expand and stabilize** | Phase 8 | Breadth without losing the guarantees | Economy, progression, quests, parties, guilds, more zones, moderation, load testing, and additional validated platforms |

Phase 0 is complete. Phase 1 and Phase 2 are held open only by client and
persistence work; the server-side criteria are already covered by headless tests.

## Waves

Waves are ordered by dependency, not by calendar. A wave may run in parallel
across domains when its tasks do not touch the same seam.

### Wave 0 — M0: truth and safety

| Task | Domain | Why it is here |
| --- | --- | --- |
| `SIM-003` | Simulation | Replace temporary fixed attack damage with server-owned combat rules. |
| `NET-001` | Networking | The world edge must validate a real session and character ownership. |
| `NET-002` | Networking | Add per-IP, per-session, and per-action budgets before exposing a client. |
| `NET-012` | Networking | Handshake timeouts, connection caps, and origin policy. |
| `PST-007` | Persistence | TLS, timeouts, pooling, and retry for the PostgreSQL adapter. |
| `PST-008` | Persistence | Make migrations repeatable and observable from the binary/CLI. |
| `QUA-006` | Quality | Prove authority, replay, and rate-limit failures with tests. |
| `QUA-009` | Quality | Sweep stale docs (snapshot seam, ADR counts, test counts). |
| `OPS-008` | Operations | Dependency and license audit in CI. |

**Exit:** authority tests demonstrate that forged outcomes are rejected; a
Nakama-backed session reaches the world gateway; the docs no longer describe
superseded architecture; `fmt`, Clippy, and tests pass.

### Wave 1 — M1: client foundation

| Task | Domain | Depends on |
| --- | --- | --- |
| `SIM-001` | Simulation | — |
| `SIM-002` | Simulation | `SIM-001` |
| `SIM-008` | Simulation | — |
| `CLT-001` | Client | — |
| `CLT-002` | Client | `NET-001` |
| `CLT-003` | Client | `PST-001` |
| `CLT-004` | Client | `CLT-001` |
| `CLT-005` | Client | `CLT-001`, `CLT-002` |
| `CLT-006` | Client | `CLT-004`, `CLT-005` |
| `CLT-007` | Client | `SIM-001`, `SIM-002`, `CLT-005` |
| `CLT-008` | Client | `CLT-006`, `SIM-008` |
| `PST-001` | Persistence | — |
| `QUA-001` | Quality | `SIM-001`, `SIM-003` |
| `QUA-014` | Quality | `CLT-005` |

**Exit:** two graphical clients observe consistent state; movement is
server-validated and reconciled; chat and inventory panels work; a reconnect
restores the session. This closes Phase 1, Phase 2, and the headless parts of
Phase 3.

### Wave 2 — M2: vertical slice

| Task | Domain | Depends on |
| --- | --- | --- |
| `CLT-009` | Client | `SIM-008` |
| `CLT-010` | Client | `CLT-008` |
| `CLT-011` | Client | `CLT-005` |
| `CLT-012` | Client | `OPS-005` |
| `CLT-013` | Client | `CLT-006` |
| `CLT-014` | Client | `CLT-005`, `QUA-014` |
| `SIM-014` | Simulation | `SIM-001`, `PST-005` |
| `PST-002` | Persistence | `PST-001` |
| `PST-006` | Persistence | `PST-001`, `NET-001` |
| `NET-006` | Networking | `CLT-005` |
| `OPS-001` | Operations | — |
| `OPS-003` | Operations | `PST-009` |
| `OPS-005` | Operations | `CLT-012` |
| `QUA-004` | Quality | `PST-001` |

**Exit:** a small group plays for a sustained session; disconnects resume;
server restart restores the world and characters; a backup restores a known
state; packaged builds exist. This closes Phase 7.

### Wave 3 — M3: creator pipeline

| Task | Domain | Depends on |
| --- | --- | --- |
| `CNT-001` | Content | `CLT-001` |
| `CNT-002` | Content | `CNT-001`, `SIM-014` |
| `CNT-003` | Content | `CNT-001` |
| `CNT-004` | Content | `CNT-003` |
| `CNT-005` | Content | `CNT-001` |
| `CNT-006` | Content | `CNT-002`, `SIM-007` |
| `CNT-009` | Content | `CNT-011` |
| `CNT-011` | Content | — |
| `CNT-012` | Content | `CLT-009`, `SIM-008` |
| `CNT-013` | Content | `CNT-005` |
| `CNT-014` | Content | `CNT-011` |
| `CNT-015` | Content | `CLT-010` |
| `CNT-016` | Content | `CNT-002`, `SIM-015` |

**Exit:** a creator makes a small zone and its entities, validates it, previews
it, and publishes it without editing engine source. This closes Phase 5.

### Wave 4 — M4: live operations

| Task | Domain | Depends on |
| --- | --- | --- |
| `OPS-002` | Operations | `OPS-001` |
| `OPS-004` | Operations | `CNT-014`, `OPS-005` |
| `OPS-006` | Operations | `OPS-005` |
| `OPS-007` | Operations | `QUA-005` |
| `OPS-009` | Operations | `OPS-003` |
| `OPS-010` | Operations | `OPS-004` |
| `OPS-011` | Operations | `OPS-001` |
| `OPS-012` | Operations | `NET-010` |
| `OPS-013` | Operations | `OPS-001` |
| `OPS-014` | Operations | — |
| `NET-008` | Networking | `NET-001` |
| `NET-009` | Networking | `SIM-008`, `CNT-008` |
| `PST-003` | Persistence | `PST-001`, `PST-005` |
| `PST-005` | Persistence | `PST-004` |
| `PST-009` | Persistence | `PST-007` |
| `PST-010` | Persistence | `PST-001`, `PST-002` |
| `PST-013` | Persistence | `CNT-014` |
| `PST-014` | Persistence | `PST-002` |

**Exit:** an operator inspects a live zone, applies a safe content change, and
rolls it back with an audit trail. This closes Phase 6.

### Wave 5 — M5: expand and stabilize

| Task | Domain | Depends on |
| --- | --- | --- |
| `SIM-004` | Simulation | ADR 0022 accepted, `SIM-003` |
| `SIM-005` | Simulation | `SIM-003` |
| `SIM-006` | Simulation | `SIM-001` |
| `SIM-007` | Simulation | `SIM-001` |
| `SIM-009` | Simulation | `SIM-008`, `PST-006` |
| `SIM-010` | Simulation | `SIM-009`, `PST-002`, `PST-014` |
| `SIM-011` | Simulation | `PST-001`, `CNT-005` |
| `SIM-012` | Simulation | `PST-003`, `SIM-009` |
| `SIM-013` | Simulation | `SIM-003`, `PST-014` |
| `SIM-015` | Simulation | `SIM-001`, `SIM-006` |
| `SIM-016` | Simulation | `PST-003` |
| `SIM-017` | Simulation | `CNT-005` |
| `PST-011` | Persistence | `PST-014` |
| `PST-012` | Persistence | `SIM-014`, `NET-005` |
| `NET-005` | Networking | `SIM-014`, `PST-004` |
| `NET-007` | Networking | `NET-002`, `SIM-001`, `SIM-003` |
| `NET-011` | Networking | `SIM-001` |
| `CNT-007` | Content | `SIM-006` |
| `CNT-008` | Content | `CNT-014`, `NET-009` |
| `CNT-010` | Content | `CNT-009`, `OPS-004` |
| `CNT-017` | Content | `CNT-005` |

**Exit:** the Phase 8 systems named in the project guide are present and tested,
load targets are measured, and additional platforms are supported only after
automated validation.

## Critical path

```text
NET-001 ─┐
         ├─> CLT-002 ─> CLT-005 ─> CLT-007 ─> CLT-008 ─> CLT-009
CLT-001 ─┘              │             ▲
                        │             │
SIM-001 ─> SIM-002 ─────┘             │
                                      │
PST-001 ─> CLT-003 ───────────────────┘

CLT-008/009 ─> M1 ─> CLT-012/013 + PST-002/006 + OPS-003/005 ─> M2
```

`SIM-001` and `CLT-001` start immediately and in parallel. `NET-001` and
`PST-001` are independent of the client shell but block the first real login and
character selection. Anything not on this path may slip without delaying the
first playable build.

## Decision gates

| Gate | Question | Owner | Unblocks |
| --- | --- | --- | --- |
| **D1 — Auth** | Validate Nakama sessions at the world edge, or introduce a separate identity service? | Networking + backend | `NET-001`, `CLT-002` |
| **D2 — Client shell** | Bevy client crate layout, camera/input abstraction, and asset formats. Write the missing ADRs. | Client + architecture | `CLT-001`, `CLT-004`, `QUA-017` |
| **D3 — Combat model** | Accept, revise, or reject ADR 0022 (dynamic classes and Treat targeting). | Combat + AI + persistence | `SIM-004` |
| **D4 — Persistence authority** | Normalized per-aggregate tables plus an event log, or whole-world snapshots with replay? | Persistence | `PST-003`, `PST-005` |
| **D5 — Editor scope** | In-engine Bevy editor, standalone tool, or CLI-first authoring? | Content + client | `CNT-001`–`CNT-007` |
| **D6 — User content** | Ownership, moderation, publishing, and revocation policy before any public upload. | Security + content | `CNT-008`, `NET-009` |
| **D7 — Multi-zone** | When does one authority loop stop being enough, and how are zones handed off? | Simulation + networking | `SIM-014`, `NET-005`, `PST-012` |
| **D8 — Platforms** | Which targets get CI smoke tests and support promises? | Operations | `OPS-006`, `OPS-005` |

## Risks

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Client work reveals protocol gaps late | M1 slips; server work churns | Build a headless protocol harness (`QUA-014`) before the renderer; keep `CLT-005` independent of rendering. |
| Movement prediction and server validation diverge | Rubber-banding, cheating, or both | Define the contract in `SIM-001`/`SIM-002` first; test with simulated latency and loss. |
| Persistence grows two competing sources of truth | Data loss, unrecoverable restarts | Decide D4 before `PST-003`; keep snapshot + event replay deterministic and tested. |
| Auth is deferred until "after the client" | Rework of session and character ownership | `NET-001` in Wave 0; `PST-001` before character selection. |
| Docs drift from code | Wrong decisions, duplicated work | `QUA-009` sweep and the gap register in the index; update docs in the same PR. |
| Editor scope explodes | Creator milestone stalls | Deliver inspector/placement first; behavior graph and user content stay later. |
| Content schema churn breaks saved worlds | Player loss | `PST-013` version pinning; `CNT-014` migrations with dry run and rollback. |
| No load/abuse testing until launch | Surprises under real traffic | `QUA-005`/`OPS-007` harness early; `NET-007` abuse checks once movement/combat exist. |
| Static-token deployment outlives development | Security exposure | Make static mode explicitly dev-only and refuse it outside loopback in `NET-001`. |

## How to change this roadmap

Re-order waves when a dependency changes, but keep the milestone exit criteria
intact until they are demonstrably met. If a milestone is split, add a new
milestone rather than weakening the existing one. Record scope changes here and
in the affected domain backlog in the same commit.
