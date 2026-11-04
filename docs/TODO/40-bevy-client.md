# Bevy client backlog

Scope: the graphical client that turns the headless world service into a playable
game — workspace layout, login, character selection, scene, input/camera,
rendering, prediction, UI, localization, packaging, and client-side tests.

**What exists today:** `clients/lynx_client` provides offline scenes and an online
WASD/chat loop with prediction, remote smoothing, resume and diagnostics.
Graphical gameplay validation and interaction/inventory remain open.
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
- **Status:** In progress — secure Nakama credential/session groundwork exists; server browser and world-session lifecycle remain
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
- [ ] Recent connections appear in history, and pinned servers remain easy to
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
reuses the secure refresh token; new accounts use `account_portal/`. The server browser/profiles/history, localized
errors, and world resume-token lifecycle remain open. The new `server_browser`
module provides validated IP/DNS profiles, stable per-server keys, pin ordering,
and a bounded recent list, with strict JSON serialization containing no
credential fields. It now loads and saves a bounded profile file through a
sibling temporary file, rejecting malformed and oversized documents. The
persistence API is not yet wired to a UI or OS-specific config path; remote
transport still requires WSS. Focused model tests pass (3/3), but the new file
I/O path has not been tested. `NET-001` and `CLT-001` remain prerequisites and
are not assumed complete.

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
- **Status:** In progress — background loop, rotated-token resume and diagnostics implemented; account lifecycle and failure UX remain
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
- [ ] Terminal errors (expired/revoked/conflict) show a clear path forward.
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
login. Automatic-drop injection, account refresh, and richer terminal-error UX
remain open.

---

## CLT-006 — Entity presentation and interpolation

- **Priority:** P0
- **Status:** In progress — Windows online rendering visually confirmed; motion quality and interpolation remain open
- **Depends on:** `CLT-004`, `CLT-005`
- **References:** ADR 0012, `SIM-002`

**Scope**

- Spawn/despawn/update visuals from authoritative snapshots and deltas.
- Interpolate remote entities between server ticks; never extrapolate beyond a
  documented bound.
- Present facing, animation state, nameplates, health/status, and interaction
  affordances from server data only.
- Handle visibility enter/leave and entity caps without popping or leaking.
- Provide a debug view of authoritative vs. rendered position.

**Acceptance criteria**

- [ ] Remote movement looks smooth at the target tick rate.
- [x] A teleport/correction does not cause unbounded extrapolation.
- [x] Despawned entities disappear immediately and do not linger in UI.
- [x] Rendered state never feeds back into authoritative decisions.

**Visual validation (2026-11-04):** The Windows local demo rendered the active
online world, six blue entity markers, and the authoritative/rendered position
overlay. The user confirmed movement and packet exchange work, but motion is less
smooth than expected. Therefore this verifies the online graphical path only;
the remote-smoothness acceptance criterion stays open for measured tuning and
another visual check. Screenshot was supplied in chat and is not stored in the
repository.

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

Remaining validation-only criteria are tracked in dependent follow-up tasks:
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
- **Status:** Blocked — `CLT-006`, `SIM-008`
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

- [ ] Different entity types open different panels through one protocol.
- [ ] A panel action rejected by the server does not change local state.
- [ ] Inventory moves match server results after reconnect.
- [ ] Stale/closed panels cannot send actions for a different target.

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
- **Status:** Blocked — `CLT-005`, `QUA-014`
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
- [ ] Two clients observe the same authoritative state in one test.
- [ ] Two clients exchange zone chat and observe presence changes without
      duplicate history entries after reconnect.

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
injection and record an actual result.

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
them from successful movement.

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
