# Lynx client foundation

`cargo run -p lynx_client` starts a windowed, offline 2D placeholder on Windows
(and on Linux with X11). WASD moves the blue square across a marked ground; a
top-down camera follows it. E, Enter, I, and C map to interact, chat, inventory,
and camera actions but have no behavior yet. Bindings can be changed through
the `InputBindings` resource; there is no rebinding UI.

This is not the demo zone or a billboard character implementation. Movement
is a fixed distance per update, so its speed depends on frame rate. There is no
content loading, collision, UI, or gameplay authority. The 2D camera follows the rig's target rather than its 3D offset.
A developer has confirmed that the default scene opens in a window on Windows.

For offline experiments, `OfflineFoundationPlugin` composes `OfflineCorePlugin`
with `DefaultActionPlugin` and `FollowCameraPlugin`. An app can instead install
the core and provide its own action and camera systems in `OfflineSet::Actions`
and `OfflineSet::Camera`; the ordered sets run input, input filtering, actions, placeholder
movement, camera, then presentation. Alternative action systems must update or
clear `ActiveActions` and `ActionTransitions` each update. The default resolver
reports per-update press/release edges after combining bindings, so releasing one
of two controls bound to the same action does not release the action. Rebinding
a held control releases its old action and presses its new one. These edges are
local presentation state, not queued input or gameplay commands. This is a
testable local seam, not the final camera/input architecture or an online
gameplay interface. The asset import and camera/input decisions remain gated by
QUA-017.

The offline core also exposes `OfflineInputEnabled` (default true). Set it in
or before `OfflineSet::Input` to discard supplied controls before action
resolution, for example when an experimental UI captures input. The default
resolver emits releases on the first disabled update. This gate does not
automatically observe UI state; a held keyboard key is supplied again when
re-enabled. Separately, the window keyboard adapter clears controls when the
primary window is unfocused or missing, releasing actions on that update. It
supplies any keys still reported held when focus returns. Replay ignores window
focus and continues consuming frames while the gate is disabled rather
than postponing them. Custom action adapters must honor the filtered controls
themselves. This affects only offline presentation, not online server state.

## Planned server browser

The client is intended to provide a server browser, similar to those in Valve
games. Players will be able to add multiple servers by IP address or hostname
and port, connect to a server, and revisit servers from connection history or a
pinned list.

Multiple client instances will be supported. Each instance has its own server
connection, so several instances may connect to the same server and account
subject to that server's session policy. Servers may choose whether to limit
concurrent sessions per character or per account. Under the per-character
policy, different characters on the same account can be played from separate
instances; under the per-account policy, only one character from that account
can be connected at a time. A duplicate connection receives an error rather
than disconnecting or replacing the existing session. This is a planned flow,
not a feature of the current demo; the current gateway policy is per-character.

Login credentials will be remembered per server for faster connections. Saving
them must be opt-in and use the operating system's secure credential store;
passwords must not be written in plaintext to client configuration or logs.
Remote servers must use `wss://`; plain `ws://` is for trusted local networks
only. This browser and credential storage are planned features and are not
implemented yet. The initial scope is user-added servers, history, and pins;
public server discovery is not specified.

## Development-only online demo

Start the demo world server with a configured static token hash (see the server
documentation), then set `LYNX_DEMO_TOKEN` in the client process environment to
the matching plaintext token and run `cargo run -p lynx_client -- --demo-online`.
Optionally set `LYNX_DEMO_URL`; it defaults to `ws://127.0.0.1:8090/world`.
Do not put tokens on the command line or in committed files. Environment values
can still be visible to processes or diagnostics; use only disposable development
credentials. Use local `ws://` only on a trusted network; remote use requires
`wss://` with a trusted certificate. This is not production authentication.

The static-token demo maps each configured token to one fixed demo character.
It does not yet support logging in to an account and selecting different
characters across multiple client instances.

The window title shows connection status. Blue squares represent the server's
bounded public interest set; the camera follows the authoritative player position.
Online mode is **read-only**: WASD and other offline controls do not send movement
or affect the displayed world. There is no reconnect, resume, prediction display,
interaction, or gameplay UI. Disconnects and errors clear displayed entities;
restart to reconnect. The handshake uses this workspace's package version for
engine and content, matching the built-in demo server.

## Headless networking

The separate `lynx_client_net` crate provides a background WebSocket transport;
it is also used by the explicit online window mode. Start `Client::connect` inside a
live Tokio runtime with a `Config` containing the `/world` URL, bearer token,
client `Handshake`, and last acknowledged tick. Poll `status` and `try_recv`,
and submit `engine_net::Envelope` values with `try_send`. The outgoing and
incoming queues each hold 64 bounded frames. Outgoing saturation returns `Full`;
incoming saturation disconnects with `IncomingQueueFull`. Dropping or closing
the client aborts I/O without flushing. The application owns reconnection,
resume-token storage, tick acknowledgements, and interpretation of world data.
`world.token` envelopes contain bearer secrets: do not log them. Transport
failures are deliberately coarse and do not include URLs or credentials.

The demo server's static bearer tokens are development authentication, not a
production identity system. Use local `ws://` only on trusted networks; `wss://`
uses WebPKI roots. Configure compatible handshake versions explicitly. Run
`cargo test -p lynx_client_net` for the headless transport tests.

For a display- and GPU-independent build and test, run:

```text
cargo build -p lynx_client --no-default-features
cargo test -p lynx_client --no-default-features
```

Without arguments, the no-default-features executable advances the shared
application by one update and exits. Use `--replay` to apply an offline
presentation fixture as described below. Workspace tests also exercise the
headless application, even with the default window feature enabled.

## Offline presentation replay

Drive the placeholder scene without a server using a JSON fixture. Each entry in
`frames` represents one client update; `pressed` names controls (`W`, `A`, `S`,
`D`, `E`, `Enter`, `I`, or `C`). For example:

```json
{"frames":[{"pressed":["W"]},{"pressed":["W","D"]},{}]}
```

With the default window feature, run `cargo run -p lynx_client -- --replay
path/to/fixture.json` to view the scripted sequence. For GPU/display-independent
inspection, run `cargo run -p lynx_client --no-default-features -- --replay
path/to/fixture.json`; this prints the final placeholder position. Fixtures
supply only local presentation inputs: they are not server snapshots and never
decide authoritative gameplay state. The fixture parser rejects unknown fields
and control names. Replay ends after its last frame and leaves controls released.

Bevy is pinned to 0.20.0 at the workspace boundary. Per ADR 0001, upgrade it in
a dedicated change, run workspace tests and a visual smoke test, and add a
migration note to `CHANGELOG.md` for a minor upgrade.
