# Bevy client backlog

Scope: the graphical client that turns the headless world service into a playable
game — workspace layout, login, character selection, scene, input/camera,
rendering, prediction, UI, localization, packaging, and client-side tests.

**What exists today:** `clients/lynx_client` provides offline scenes and an online
WASD/chat loop with prediction, remote smoothing, resume and diagnostics.
Target interactions and the reference lockpick panel are playable, with
two-client combat/chat coverage. Graphical review, generic panels, and inventory
remain open.
[ADR 0001](../adr/0001-rust-and-bevy-version-policy.md) restricts Bevy to
client/editor crates. Phases 1, 2, 4, and 7 still require further client work.

**Related design:** [project guide §24](../lightweight_mmorpg_engine_project_guide.md#24-initial-vertical-slice-keep-it-small),
[platforms](../platforms.md), [networking](../networking.md),
[content authoring](../content-authoring.md),
[ADR 0001](../adr/0001-rust-and-bevy-version-policy.md),
[ADR 0015](../adr/0015-ui-panel-session-state.md).

---

## CLT-001 — Client workspace and crate layout

- **Priority:** P0
- **Status:** Done
- **Depends on:** —
- **References:** ADR 0001, [architecture §What is deliberately not here yet](../architecture.md#what-is-deliberately-not-here-yet)

**Why:** There is nothing to build a scene on. The crate boundary also matters:
`engine_core`, `engine_sim`, and `engine_content` must never depend on Bevy.

**Scope**

- Add client crates (for example `clients/lynx_client` for the app plus
  `crates/client_*` for reusable presentation/networking helpers).
- Pin Bevy 0.20 in `[workspace.dependencies]` as ADR 0001 requires; keep the
  dependency out of engine domain crates.
- Provide a headless feature/test mode so CI can build and test the client
  without a GPU or window.
- Wire the client into workspace fmt/clippy/test and document how to run it.
- Add an `--offline`/replay harness for UI work without a live server if that
  keeps iteration fast.

**Acceptance criteria**

- [x] `cargo build --workspace` and `cargo test --workspace` include the client.
- [x] A hosted CI run verifies the configured headless client build and test
      without a display/GPU.
- [x] No engine domain crate depends on Bevy.
- [x] The Bevy pin is recorded and an upgrade note exists.
- [x] Visually verify that `cargo run -p lynx_client` opens a window on a
      developer machine.

**Progress:** Bevy 0.20.0 is available on crates.io and pinned in the workspace.
The client provides a shared plugin and deterministic headless update test; see
[`clients/lynx_client/README.md`](../../clients/lynx_client/README.md) for run,
headless build/test, and upgrade instructions. On Windows, workspace build and
tests and the no-default-features client test passed. Ubuntu and
Windows CI jobs run explicit no-default-features headless client build and test
steps before workspace Clippy, so unrelated lint failures cannot suppress the
focused GPU-free verification. Hosted CI run
[#41](https://github.com/cosmosgc/Lynx/actions/runs/40806879047) passed those
headless client test/build steps on Ubuntu and Windows, although the full run
failed later at workspace Clippy and predates the current local changes.
The JSON offline presentation replay harness is documented in the client README.
Local workspace fmt, Clippy, build, and test checks passed for the prior
implementation. A developer confirmed that `cargo run -p lynx_client` opens a
window displaying the default offline placeholder scene.

---

## CLT-002 — Login and session bootstrap

- **Priority:** P0
- **Status:** In progress — secure Nakama groundwork and the account-screen server browser exist; live multi-server and credential isolation remain
- **Depends on:** `NET-001`, `CLT-001`
- **References:** [networking §Transport](../networking.md#transport)

**Scope**

- Server browser that supports multiple saved servers, each configured with an
  IP address or hostname and port.
- Show recent servers in connection history and let players pin servers for
  quick access.
- Provide login for the supported identity flow and remember credentials per
  server for fast reconnection. Saving credentials is opt-in and uses the
  operating system's secure credential store; never persist passwords in
  plaintext client configuration or logs.
- Allow multiple client processes to run at once, each with its own server
  connection; do not impose a client-wide single-instance lock. Respect the
  server's configured concurrency policy: per character or per account.
- Acquire a session, send `session.hello` with the negotiated handshake, and
  surface protocol/auth/capacity errors in the player's locale.
- Store tokens only as long as needed; never log them.
- Handle token expiry/refresh and a clear logout path.
- Distinguish "server unreachable", "auth failed", "version mismatch", and
  "world full" in the UI.
- Require `wss://` for remote servers; allow `ws://` only for trusted local
  networks. Public server-directory discovery is outside this initial scope.

**Acceptance criteria**

- [ ] Players can add and connect to multiple servers using an IP/hostname and
      port.
- [x] Recent connections appear in history, and pinned servers remain easy to
      find across restarts.
- [ ] Remembered credentials are isolated per server and stored only in the
      operating system's secure credential store.
- [ ] Multiple client instances connect according to the server's selected
      per-character or per-account session policy.
- [ ] A valid session reaches `world.ready` and renders the zone.
- [ ] Invalid/expired credentials show a localized, actionable error.
- [ ] A version mismatch is blocked before world entry with the supported range.
- [ ] Tokens do not appear in logs, crash reports, or screenshots.
- [ ] Logout/close releases the session promptly.

**Progress:** The development CLI supports device auth and email/password login
for accounts created through `account_portal/`. It prompts for passwords without
echo, stores account identity plus rotating refresh tokens in the OS credential
store, supports logout, and reaches the character HTTP API. Access tokens remain
in memory. The `--account` Bevy screen signs in to existing email accounts and
reuses the secure refresh token; new accounts use `account_portal/`. The server
browser/profiles/history are now wired into that screen: players can add a
server by name, host/IP, and port, select it, pin/unpin it, and see recent
successful connections. Profiles load from `LYNX_SERVER_BROWSER_PATH` or the
OS configuration directory and persist atomically without credential fields.
Pinned profiles sort first and history is unique and bounded. The selected
profile supplies both the character API and world WebSocket endpoints, with
`wss://`/`https://` required for remote hosts and plaintext restricted to
loopback and private-network addresses. Focused tests cover round-trip
persistence through a new parent directory, missing/corrupt/oversized documents,
temporary-file cleanup, pin/recent ordering, URL policy (including IPv6), and
secret-free serialization (7/7). The screen also has a pure selection/endpoint
test. Localized errors, per-world-profile credential isolation, live multi-server
reconnect, and world resume-token lifecycle remain open. Nakama credentials are
still scoped to the configured Nakama server rather than each world profile.
The server-browser UI itself has not been visually reviewed against a live
server, and no live multi-instance policy run is claimed. `NET-001` and
`CLT-001` remain prerequisites and are not assumed complete.

---

## CLT-003 — Character selection and creation

- **Priority:** P0
- **Status:** In progress — account screen and durable first-admission actor provisioning
- **Depends on:** `PST-001`, `CLT-002`
- **References:** `game_api::CharacterService`, `PST-001`

**Scope**

- List characters owned by the account, select one, and create a new one within
  the configured limits.
- Allow separate client instances to select different characters owned by the
  same account on the same server when its policy is per-character. The server
  may instead configure one connected session per account.
- Validate names and appearance choices client-side for UX, then re-validate on
  the server.
- Show creation/selection failures without leaking other accounts' data.
- If another instance already has the selected character connected, show an
  actionable conflict; never silently replace or disconnect that session.
- Enter the world with the selected character and resume the last safe position
  per persistence policy.

**Acceptance criteria**

- [x] Authenticated character list/create/select routes enforce account
      ownership; creation requires a retry-stable idempotency key.
- [x] The development CLI can list, create, and select characters, and tells
      the player to reuse the same creation key after an uncertain result.
- [x] The account screen lists, creates, and verifies characters; creation retries
      reuse a key for the lifetime of the screen (not across restarts).
- [x] Client-side name/key/ID validation matches server contracts.
- [x] First Nakama admission provisions a deterministic player actor and saves a
      snapshot before opening the session; snapshot retries and restart reuse it.
- [ ] A new account can create a character, enter through the Bevy screen, and
      restore the same actor after a real server restart.
- [ ] The client cannot select a character it does not own.
- [ ] Name/appearance validation matches server rules.
- [ ] The selected character is clearly identified in the UI.
- [ ] Two instances can play different characters from one account when the
      server uses per-character limits; per-account limits reject a second
      session without affecting the first.

The server derives actor IDs deterministically from character IDs and commits the
actor to the world snapshot before admission. If persistence fails, admission is
denied; retries reuse the actor already present in memory, or recreate the same
ID after a crash. The Bevy screen now hands the verified account session and
selected character to the online window. The online view supports movement, and
live Nakama/PostgreSQL plus process-restart onboarding remains to be exercised.

---

## CLT-004 — Scene, camera, and input abstraction

- **Priority:** P0
- **Status:** Done
- **Depends on:** `CLT-001`
- **References:** [project guide §24](../lightweight_mmorpg_engine_project_guide.md#24-initial-vertical-slice-keep-it-small),
  `QUA-017` (camera/input ADR)

**Scope**

- Load a zone/content bundle and render terrain, props, characters, and
  interactables.
- One camera mode for the slice, behind an abstraction that allows more modes
  later; support a 2D billboard alternative as the guide requires.
- Input mapping with rebinding-friendly actions (move, interact, chat,
  inventory, camera).
- Asset import/loading policy and placeholder assets for early work.
- Graceful handling of missing assets and content-version mismatches.

**Acceptance criteria**

- [x] A generated `mmorpg` project opens a playable scene with the demo zone.
- [x] Camera and input are replaceable without touching gameplay systems.
- [x] Missing/incompatible assets produce a clear error, not a panic.
- [x] The 2D billboard presentation path is selectable or documented as the
       fallback.

**Progress:** The offline client loads a project/content directory into a
presentation-only `ZoneScene`: bounded terrain plus deterministic placeholder
placement for props, characters, and interactables. With no content path, it
uses a built-in `zone.demo`; project roots automatically resolve their
`content/` directory. Content manifests are checked for engine compatibility,
safe paths, and SHA-256 hashes before parsing. Invalid or incompatible bundles
exit with a clear scene error; missing/unsafe/unsupported presentation assets
emit a diagnostic and use a placeholder. PNG portraits and GLB/GLTF models are
the supported asset policy; decoding remains future rendering work.

`InputBindings` maps platform-neutral controls to actions and accepts a JSON
rebinding file. `CameraController` selects `follow3d` or the selectable
`billboard2d` fallback (also cycled with C). The offline core’s ordered seams
let action and camera adapters be replaced independently without changing the
local movement or scene presentation systems. All of this remains local
presentation: authoritative movement, interaction outcomes, collision, and
persistence are server work.

Focused verification passed locally: `cargo test -p lynx_client
--no-default-features` (40 tests), `cargo test -p lynx_client` (54 tests), and
`cargo test -p engine_content` (48 tests). A freshly generated `mmorpg`
project loaded headlessly with `--content … --zone zone.starter --camera
billboard2d --describe` (256 terrain tiles, two characters/interactables), and
the default-feature client opened a Windows window titled `Lynx offline -
Taverna da Encruzilhada [zone.starter] source - camera billboard2d - 1
warning(s)`. The sole warning correctly identifies the generated source as
unversioned until a manifest is built.

---

## CLT-005 — Network client loop

- **Priority:** P0
- **Status:** Done — background reconnect, Nakama access-token refresh, and terminal recovery are implemented and tested
- **Depends on:** `CLT-001`, `CLT-002`
- **References:** [networking](../networking.md), `engine_net`

**Scope**

- Connect, perform the handshake, read/write framed envelopes on a background
  task, and feed snapshots/deltas into the presentation layer.
- Track server tick acknowledgements and send them on resume.
- Reconnect with exponential backoff and the rotated resume token; distinguish
  resumable from terminal failures.
- Bound queues and surface telemetry (RTT, loss, queue depth) in a debug overlay.
- Never block the render thread on network I/O.

**Acceptance criteria**

- [x] The client applies a full snapshot and subsequent deltas correctly.
- [x] A dropped connection reconnects and resumes within the server window.
- [x] Terminal errors (expired/revoked/conflict) show a clear path forward.
- [x] A stalled server does not freeze the client.
- [x] Debug telemetry exposes tick, movement round-trip time, and queue depth.

**Progress:** The online loop stores rotated resume tokens in memory, retries
transient failures with bounded exponential backoff, and captures the Tokio
runtime handle for retries from Bevy systems. Two consecutive resumes and
post-resume movement pass against an in-process server. The HUD reports queue
depths and movement round-trip time (including authority processing), not a
network-only RTT or measured packet loss. An in-process integration test verifies
rotated-token resume and movement after resume. A Windows local demo session was
also visually confirmed active, with tick 807, six entities,
accepted movement, zero reconnects, and empty transport queues (developer-provided
screenshot, 2026-11-04). This is static-token demo coverage, not Nakama account
login. Terminal failures show a category-specific explanation and recovery step
in the online HUD. A rejected Nakama access token first triggers one background
refresh; if its refresh credential is expired or revoked, the user is directed to
sign in again (`R` retries). A session conflict advises waiting for the other
session before retry, and a forbidden character directs the user back to character
selection. Other failures advise checking connectivity and pressing `R`. Unit
tests cover the recovery guidance. An in-process TCP proxy now injects a real
transport drop: the client notices it without an explicit
`reconnect()`, enters the automatic backoff path, resumes with the rotated
token, and keeps moving from the same authoritative baseline. Account refresh
now runs on a worker after the world edge rejects a saved Nakama access token.
The rotated refresh token is persisted to the OS credential store before the
client retries admission; expired/revoked refresh credentials retain an
actionable sign-in recovery path. A mock-edge integration test verifies the
refreshed bearer is used on retry without blocking the Bevy polling loop.

---

## CLT-006 — Entity presentation and interpolation

- **Priority:** P0
- **Status:** Done — client entity presentation delivered and visually reviewed; network-impairment tuning remains in `CLT-016`
- **Depends on:** `CLT-004`, `CLT-005`
- **References:** ADR 0012, `SIM-002`

**Scope**

- Spawn/despawn/update visuals from authoritative snapshots and deltas.
- Interpolate remote entities between server ticks; never extrapolate beyond a
  documented bound.
- Give visible entities a grounded marker/ring and a facing-direction indicator
  oriented from authoritative facing data. Use clear, distinct styling for the
  local player, other entities, selected targets, and interactable targets.
- Show nameplates and health bars using only server-authoritative public
  presentation data. Further status-effect indicators are tracked in
  [CLT-018](#clt-018--public-status-indicators).
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

**Acceptance criteria**

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
the separately scoped [CLT-018](#clt-018--public-status-indicators). They are
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

---

## CLT-018 — Public status indicators

- **Priority:** P2
- **Status:** In progress — safe stunned/slowed booleans, indicators, and deterministic demo fixture implemented; visual review remains in `CLT-020`
- **Depends on:** `CLT-006`
- **References:** [CLT-006](#clt-006--entity-presentation-and-interpolation), `SIM-002`

**Why:** Status effects need an explicit public-data and visual treatment. Keep
them separate from the delivered base entity presentation so hidden/private
simulation state is not exposed implicitly.

**Scope**

- Define which statuses are safe and useful to replicate to nearby observers;
  expose only approved public status identifiers, never internal modifiers.
- Complete the current `STUN` indicator and the new `SLOW` indicator, including
  reliable clearing/update behavior and a deterministic fixture that makes them
  visible.
- Add icon/label treatments for additional modeled public effects only when the
  simulation has authoritative semantics and content mappings for them.
- Validate visibility enter/leave, despawn, overlap/readability, and visual
  behavior in the Windows demo.

**Acceptance criteria**

- [ ] Public status contract is explicit, bounded, and covered by privacy tests.
- [ ] Status indicators appear/update/clear with authoritative state and entity
      visibility; no private movement/combat internals leak.
- [x] A deterministic visual fixture demonstrates statuses in the graphical
      client. User visual review is tracked separately in `CLT-020`.
- [ ] Additional effects are displayed only when modeled by authoritative
      simulation data; unsupported statuses fail closed.

**Implementation progress:** Public entity replication now includes optional
`stunned` and `slowed` booleans only. The timed slow's expiry tick and exact
movement multiplier are never broadcast to observers; the controlled actor's
session receives its exact factor for local movement prediction. `PublicEntity`
decodes the public flag optionally for compatibility, and the online Bevy
presentation shows a blue `SLOW` label while authoritative public state says the
entity is slowed, hiding it again when the status clears or the entity leaves
interest. Projection and privacy tests cover omission/default behavior and
ensure no private multiplier or slow component leaks. The built-in navigation
scenario keeps the non-AI raider stunned and slowed for graphical inspection;
the gateway still exposes only the public booleans. User visual review has been
moved to the future `CLT-020` task.

---

## CLT-007 — Local movement and prediction

- **Priority:** P0
- **Status:** In progress — WASD movement visually confirmed; latency tuning and rejection evidence remain
- **Depends on:** `SIM-001`, `SIM-002`, `CLT-005`
- **References:** `SIM-001`, `SIM-002`, [CLT-016](#clt-016--movement-latency-validation), [CLT-017](#clt-017--rejected-movement-and-desync-evidence)

**Scope**

- Sample input, assign sequence numbers, and send movement intents at a bounded
  rate.
- Predict locally, reconcile against server acknowledgements/corrections, and
  replay pending inputs.
- Snap/reset prediction on teleport, death, zone change, and reconnect.
- Tune smoothing so corrections are visible only when they matter.
- Keep an input log for reproducing desyncs.

**Acceptance criteria**

- [x] The client converges to the server position after corrections.

The following validation cannot be established by the current local tests/demo;
they are tracked as dependent follow-up tasks that require a suitable impairment
harness or controlled authoritative rejection fixture:
[CLT-016](#clt-016--movement-latency-validation) covers 100–200 ms simulated
latency, and [CLT-017](#clt-017--rejected-movement-and-desync-evidence) covers
server-rejected movement and sequence/tick desync evidence. Both depend on this
movement foundation (`CLT-007`); neither is a prerequisite for the current local
movement implementation.

**Progress:** WASD issues normalized, height-preserving requests at at most 20 Hz,
with one request in flight and no ambiguous-delivery replay. Chat composition
and focus loss suppress input. Server corrections drive the avatar and camera.
Default-feature tests verify held-key movement against the real server; tests
also cover correction/reset and bounded remote interpolation with teleport snap.
Verified locally: 60 default-feature client tests and 44 no-default-feature
tests. Windows visual confirmation additionally showed `last move accepted`,
server tick progression, a 66 ms movement round trip, and equal authoritative
and rendered coordinates while idle. This is a live local demo with a static
development token; it does not validate Nakama account admission. The localhost
demo does not inject latency or cause a server-side rejection, so those cases are
deferred to CLT-016/017 rather than claimed as visually verified here.

---

## CLT-008 — Interaction, inventory, and UI panels

- **Priority:** P1
- **Status:** In progress — server-authoritative actions, distinct generic/lockpick panels, and read-only owner inventory presentation are covered; inventory operations and content-defined widgets remain
- **Depends on:** `CLT-006`, `SIM-008`
- **References:** ADR 0015, [content authoring §UI panels](../content-authoring.md#ui-panels)

**Scope**

- Target selection and verb affordances derived from server-provided
  interactables.
- Render entity-provided panels from their namespaced schema, localized labels,
  and allowed actions.
- Inventory/container UI with server-validated moves, equip, and split/merge.
- UI session lifecycle: open/action/close, permission/range revalidation, and
  stale-panel handling.
- Keyboard/gamepad navigation for the common flows.

**Acceptance criteria**

- [x] Different entity types open different panels through one protocol.
- [x] A panel action rejected by the server does not change local state.
- [ ] Inventory moves match server results after reconnect.
- [x] Stale/closed panels cannot send actions for a different target.

**Progress (2026-11-05):** The online HUD lists supported, server-advertised
parameter-free verbs for the selected target. Number keys submit correlated
`action.request` intents; one interaction may be in flight, and server rejection
does not optimistically mutate world state. `ui.state` owns panel replacement and
closure; disconnect clears panel/hint state. X sends an idempotent `ui.close`.
The reference `ui.lockpick` panel accepts decimal guesses through `ui.action`,
shows server-provided comparison hints, attempts, and lockout tick, and closes
when the server confirms a solve. Submission checks the exact composed panel,
current target visibility, and advertised `play`; unsupported schemas have a
read-only title/schema fallback. Server error diagnostics are not displayed.

The demo barrel now exposes a read-only `ui.display` panel alongside the vault's
`ui.lockpick` minigame. A real-socket client test opens both schemas through the
same `ui.state` lifecycle and closes the display panel authoritatively. A focused
client test verifies a rejected panel action preserves the server-owned panel;
stale-target and closed-panel protections remain covered by client and gateway
tests. This does not yet provide content-authored localized widgets or inventory
operations.

Real-socket tests cover an out-of-range rejection, opening a door, approaching
and opening the vault, invalid/stale input rejection, solving from server hints,
and authoritative panel closure. A two-client test observes the same barrel
damage. Generic content-defined/localized widgets, inventory operations, gamepad
navigation, and graphical review remain open. No new
crate is needed for this slice: existing Bevy UI and the framed protocol suffice.

**Follow-up (2026-11-05):** User visually confirmed opening/using the vault and
guessing its code in the Windows online client (screenshot supplied in chat).
The server now automatically closes panels on the next tick when the provider
is outside interaction range, leaves interest, is disabled, or loses/changes its
UI schema. An unsolicited `ui.state` clears the client panel and hints. Tests
cover the inclusive distance boundary, single close notification, provider
removal, stale actions, and walking away/returning through real sockets.
The live HUD now has an explicit readable width and an embedded full Fira Mono
font for Portuguese glyphs. A separate, optional `bevy_immediate` 0.9 prototype
compares sidebar, dialog/keypad, and bottom-dock layouts using in-memory fixtures;
see the client README. It awaits user layout selection before promotion.

**Selected layout promoted:** The user chose the keypad dialog. The normal
window build now includes `bevy_immediate` and uses `lock_panel.rs` for the live
vault. Both `tests/client-online-demo` and account-world launch paths install it;
movement, chat, combat, telemetry, and reconnect remain in the same application.
Mouse digits/edit/submit/close share the existing code composer and authoritative
session API with keyboard shortcuts. Buttons gate pending requests, chat focus,
window focus, and panel identity. GPU-free integration tests drive pointer events
through the real keypad and socket-backed vault, including distance closure.
The standalone layout example is only a historical fixture, not the game demo.
The documented demo launchers were also smoke-run locally: the loopback server
listened, the windowed client reached `active` with advancing ticks, stayed up,
and logged no errors. That confirms the integrated window boots; keypad visual
review in the live window is still a user-facing follow-up. The user then
confirmed the live keypad works and requested window chrome: the dialog is now a
draggable `FloatingWindow` with a title bar, an OS-style **X** in the top-right
corner, remembered position across openings, and full-viewport clamping so it
cannot be dragged off-screen. The close button uses the same
idempotent server-owned `ui.close` path; pointer integration coverage exercises
it, then reopens the vault for the out-of-range closure check.

**Owner inventory (2026-11-07):** The world-state view now decodes session-private
`owner_only` replication only for the controlled actor, validates each inventory
structurally, ignores owner state for other owned entities, orders updates by
sequence then tick, and clears the inventory on disconnect and on every fresh
snapshot. Malformed owner state rejects the whole delta atomically, and private
inventory contents are redacted from diagnostics. The built-in demo character
starts with deterministic stackable and unique items. A `bevy_immediate` window
(**I** to toggle; **Escape** or the title-bar **X** to close) shows slot, derived
item name, quantity, and unique marker; it is suppressed during chat composition
and focus loss and is installed on both online launch paths. This is
presentation only: moves, equip, split/merge, and container transfers still need
a server-validated protocol, so the reconnect-inventory acceptance criterion
remains open.

---

## CLT-009 — Chat and presence UI

- **Priority:** P1
- **Status:** Done
- **Depends on:** —
- **References:** `SIM-008`, [security §Abuse prevention](../security.md#abuse-prevention)

This increment delivers the first one-zone chat and presence surface. It is
independent of the entity-panel and inventory work in `CLT-008`. Multi-client
exchange and history-page integration coverage live in `CLT-014`; additional
channels, unread indicators, localized chat metadata, and moderation controls
are deferred to `CLT-015`.

**Scope**

- One zone-channel feed and composer in the online client.
- Send/receive with server-resolved identity and safe errors.
- Connected-member presence roster.
- Bounded history scrollback and cursor-based previous-page loading.
- Static demo credentials stay on the world gateway path; Nakama chat uses
  Nakama account sessions only.

**Acceptance criteria**

- [x] The online client sends and displays messages with server-resolved identity.
- [x] Presence displays connected members for the selected zone room.
- [x] History is bounded, cursor-paged, ordered for display, and deduplicated.
- [x] The gateway rejects spoofed sender/channel identity; Nakama supplies the
      authenticated sender identity for account chat.

**Progress:** The online window shows recent room messages and connected
members, accepts `T` to compose and Enter to send, and uses PageUp/PageDown for
bounded history pages. Account sessions with a verified character use the
configured Nakama room, authenticated realtime socket, and persisted history
API. Static `--demo-online` sessions use the world gateway's in-memory zone
channel and never send their world token to Nakama. Nakama history pages merge
in chronological order with message-ID deduplication. The client keeps a
bounded 500-message scrollback and displays safe Portuguese errors without
rendering server diagnostics. The online demo now visibly echoes sent messages
and shows the connected demo character in presence. See the completion record
for validated behavior and deferred follow-up work.

**Visual validation (2026-11-04):**
- Screen: `lynx_client --demo-online`, active session at desktop size.
- Journey: sent two messages and confirmed both appeared with server-resolved
  character identity; the connected character appeared in the presence roster.
- Evidence: user-provided screenshot, not stored in the repository.
- Automated checks: `cargo check -p lynx_client` and
  `cargo clippy -p lynx_client --all-targets -- -D warnings` passed.
- Result: single-client gateway chat and self-presence passed. Multi-client and
  history-page integration checks moved to `CLT-014`.

---

## CLT-010 — Localization and accessibility

- **Priority:** P1
- **Status:** Blocked — `CLT-008`
- **Depends on:** `CLT-008`, `CLT-009`
- **References:** [internationalization](../internationalization.md), ADR 0011

**Scope**

- Load locale catalogs, negotiate the player's locale, and fall back to `pt-BR`.
- Render all player-facing text from message keys and parameters.
- Missing-key reporting in development and a release gate for zero missing keys.
- Text scaling, colorblind-safe indicators, remappable keys, and reduced motion.
- Locale-aware number/date/time formatting for the calendar.

**Acceptance criteria**

- [ ] No player-facing string is hardcoded in a client widget.
- [ ] `pt-BR` is the default; an alternate locale can be selected and persists.
- [ ] Missing keys are visible in development and fail the release check.
- [ ] Core flows are usable with keyboard only and at increased text size.

---

## CLT-011 — Settings, diagnostics, and recovery UX

- **Priority:** P2
- **Status:** Blocked — `CLT-005`
- **Depends on:** `CLT-005`, `CLT-007`
- **References:** [operations](../operations.md), `CLT-005`

**Scope**

- Graphics/audio/network settings with safe defaults and validation.
- Connection diagnostics: server, protocol version, RTT, packet loss, tick,
  reconnect count.
- Exportable logs/crash reports with secrets and private chat redacted.
- Recovery flows: reconnect, return to character select, quit safely.
- A "copy diagnostic report" action that operators can request.

**Acceptance criteria**

- [ ] Settings persist and invalid combinations are prevented.
- [ ] A diagnostic report contains versions and network state but no secrets.
- [ ] The user can recover from a terminal session error without restarting the
      process.
- [ ] Crash/exit paths do not corrupt local settings or assets.

---

## CLT-012 — Packaging and platform builds

- **Priority:** P2
- **Status:** Blocked — `OPS-005`
- **Depends on:** `OPS-005`, `CLT-004`
- **References:** [platforms](../platforms.md), `OPS-006`

**Scope**

- Windows x86-64 first, then Linux; macOS only after CI smoke coverage.
- Reproducible build stamp (engine/content/protocol versions) and checksums.
- Asset packaging/loading from a bundle or install directory.
- Installer/portable archive and an uninstall/cleanup path.
- Crash reporting and update-check hooks that respect privacy settings.

**Acceptance criteria**

- [ ] A clean machine installs and launches the client with no dev toolchain.
- [ ] The build stamp matches the server/content versions it targets.
- [ ] Assets load from the packaged layout, not a developer path.
- [ ] A documented uninstall leaves no unexpected user data.

---

## CLT-013 — Client performance profiling

- **Priority:** P2
- **Status:** Blocked — `CLT-006`
- **Depends on:** `CLT-006`, `SIM-018`
- **References:** [project guide §24](../lightweight_mmorpg_engine_project_guide.md#24-initial-vertical-slice-keep-it-small),
  `OPS-007`

**Scope**

- Measure frame time, draw calls, memory, load time, and network bytes on target
  hardware.
- Define budgets for the vertical slice and track regressions.
- Profile the worst case: crowded zone, many nameplates, open panels, chat.
- On-screen profiler and capture instructions for reports.
- Optimize only after measuring; document the baseline.

**Acceptance criteria**

- [ ] The slice holds the target frame rate on the reference machine.
- [ ] Load time and memory budgets are documented and measured.
- [ ] Network bytes per session are reported alongside server metrics.
- [ ] A regression beyond the budget fails a nightly check or is explicitly
      accepted.

---

## CLT-014 — Client integration and protocol tests

- **Priority:** P1
- **Status:** In progress — real-socket two-client gameplay/chat/resume coverage; account-service scenario remains
- **Depends on:** `CLT-005`, `QUA-014`, `CLT-009`
- **References:** `QUA-014`, `NET-004`

**Scope**

- Headless client harness that connects, authenticates, selects a character,
  moves, chats, and reconnects against a real or in-process server.
- Replay fixtures for snapshots/deltas and protocol errors.
- Simulated latency/loss and authoritative rejection fixtures that support
  [CLT-016](#clt-016--movement-latency-validation) and
  [CLT-017](#clt-017--rejected-movement-and-desync-evidence).
- UI smoke tests that do not require a GPU where possible.
- A small multi-client test proving consistent observation.
- Two independent online clients exchange zone chat and observe presence joins
  and leaves through the gateway path; Nakama room fan-out is an optional
  service-backed smoke.
- Exercise history-page cursor ordering and deduplication across reconnect.

**Acceptance criteria**

- [ ] CI runs a headless login → move → chat → reconnect scenario.
- [ ] Protocol fixtures catch accidental wire changes.
- [ ] Prediction/reconciliation tests run under simulated latency/loss.
- [x] Two clients observe the same authoritative state in one test.
- [x] Two clients exchange zone chat and observe presence changes without
      duplicate history entries after reconnect.

**Progress (2026-11-05):** `clients/lynx_client/tests/multiplayer.rs` starts the
real service and two independent WebSocket clients in one interest set. Both
observe the same server-applied barrel damage, exchange messages with distinct
server identities, see two connected members, resume one client, page history
without duplicates, and observe the other member leaving. This uses static demo
authentication; Nakama login/character creation and service-restart persistence
are still separate validation work. The suite runs in the existing GPU-free
client test command.

---

## CLT-015 — Expanded chat channels and moderation UI

- **Priority:** P2
- **Status:** Blocked — `SIM-009`, `NET-009`, `CLT-010`
- **Depends on:** `SIM-009`, `NET-009`, `CLT-010`, `CLT-009`
- **References:** `CLT-009`, [internationalization](../internationalization.md),
  `NET-009`

**Scope**

- Add channel navigation for the social channels supported by the server.
- Show unread counts for inactive channels.
- Localize channel labels and message timestamps.
- Add mute, ignore/block, and report controls with server-confirmed outcomes.

**Acceptance criteria**

- [ ] Only channels authorized for the selected character appear in navigation.
- [ ] Unread counts update on incoming messages and clear when read.
- [ ] Channel labels and timestamps follow the selected locale.
- [ ] Moderation controls display the server-confirmed state and cannot be
      bypassed by switching clients.

**Implementation note:** Chat moderation APIs and policy remain owned by
`NET-009`; this client task consumes those server-confirmed outcomes.

---

## CLT-016 — Movement latency validation

- **Priority:** P1
- **Status:** Blocked — `CLT-007`
- **Depends on:** `CLT-007`, `CLT-014`
- **References:** [CLT-007](#clt-007--local-movement-and-prediction), [CLT-014](#clt-014--client-integration-and-protocol-tests), `SIM-002`

**Why:** Localhost movement is responsive, but does not establish the feel of
movement under representative network delay. Keep this separate from the basic
movement implementation so the validation can use deterministic latency
injection and record an actual result. The current local test/demo setup does not
provide that latency injection; these criteria are not claimed as locally proven.

**Scope**

- Exercise online movement with deterministic 100 ms and 200 ms transport
  latency in a headless/in-process harness.
- Record input-to-authoritative-result timing, prediction error, correction
  magnitude, and user-visible responsiveness observations.
- Tune prediction/smoothing only from those measurements; do not weaken server
  movement authority to improve client feel.

**Acceptance criteria**

- [ ] Repeatable automated runs cover both 100 ms and 200 ms simulated latency.
- [ ] The client remains responsive without exceeding the bounded prediction
      contract, and converges to server state after each result.
- [ ] A Windows graphical run confirms the tuned behavior is acceptably smooth;
      record the environment and evidence location.

---

## CLT-017 — Rejected movement and desync evidence

- **Priority:** P1
- **Status:** Blocked — `CLT-007`
- **Depends on:** `CLT-007`, `CLT-014`
- **References:** [CLT-007](#clt-007--local-movement-and-prediction), [CLT-014](#clt-014--client-integration-and-protocol-tests), `SIM-001`, `SIM-002`

**Why:** The normal localhost demo currently exercises accepted movement and
cannot prove how the client handles a deliberate server rejection or a desync.
Test those paths with a controlled authoritative fixture instead of inferring
them from successful movement. The current local test/demo setup does not force
this server-side rejection or produce a desync report, so these criteria require
the future fixture and diagnostic work in this task.

**Scope**

- Add a deterministic server/test fixture that accepts a movement request into
  the queue but rejects it during authoritative processing (for example, a
  blocked destination or an over-budget endpoint).
- Verify the local prediction visibly returns to the authoritative position;
  server rejection must never be presented as accepted movement.
- Capture bounded desync evidence containing movement sequence, server tick,
  predicted position, and authoritative correction, with no credentials or
  private state.

**Acceptance criteria**

- [ ] A test drives a queued movement through server-side rejection and proves
      the authoritative position is unchanged.
- [ ] A graphical or deterministic presentation test proves the correction is
      visible and the final rendered position converges to authority.
- [ ] A desync diagnostic includes the input sequence and server tick (plus
      predicted/corrected positions) and is verified to redact credentials.

---

## CLT-019 — Online reconciliation visual validation

- **Priority:** P1
- **Status:** Icebox — graphical demo validation deferred
- **Depends on:** `CLT-007`, `CLT-016`, `CLT-017`
- **References:** `SIM-002`, [CLT-016](#clt-016--movement-latency-validation), [CLT-017](#clt-017--rejected-movement-and-desync-evidence)

**Why:** Headless reconciliation and online integration tests establish the
bounded contract, but the online demo has no built-in way to induce a correction.
Keep the visual acceptance criterion until a controlled correction fixture and
graphical validation environment are available.

**Scope**

- Run `lynx_client --demo-online` against a controlled authoritative correction
  using the fixture/harness from `CLT-016` or `CLT-017`.
- Observe and record presentation convergence (or snap for a large correction)
  while confirming presentation smoothing does not mutate authoritative state.
- Capture the environment and evidence location; document any remaining demo or
  diagnostic limitations.

**Acceptance criteria**

- [ ] The online graphical demo visibly converges to the authoritative position
      after a controlled private movement correction.
- [ ] Evidence confirms presentation smoothing leaves authoritative state
      unchanged, and records the environment and evidence location.

---

## CLT-020 — Public status visual validation

- **Priority:** P2
- **Status:** Icebox — user visual review deferred
- **Depends on:** `CLT-018`
- **References:** `SIM-005`, [CLT-018](#clt-018--public-status-indicators)

**Why:** Automated tests establish the public status contract and indicator
updates, but graphical overlap/readability and visual treatment require user
review. Keep that confirmation out of simulation completion criteria.

**Scope**

- Review the deterministic stunned/slowed status fixture in the Windows demo.
- Check visibility enter/leave, despawn, overlap, and label readability.
- Record the environment and any follow-up polish without changing simulation
  authority or exposing private status data.

**Acceptance criteria**

- [ ] User confirms the status indicators are visually clear in the fixture.
- [ ] Any requested visual polish is captured as a separate client task.
