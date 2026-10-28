# Lynx client foundation

`cargo run -p lynx_client` starts a windowed, offline 2D placeholder on Windows
(and on Linux with X11). WASD moves the blue square across a marked ground; a
top-down camera follows it. E, Enter, I, and C map to interact, chat, inventory,
and camera actions but have no behavior yet. Bindings can be changed through
the `InputBindings` resource; there is no rebinding UI.

This is not the demo zone or a billboard character implementation. Movement
is a fixed distance per update, so its speed depends on frame rate. There is no
content loading, collision, UI, or gameplay authority. The 2D camera follows the rig's target rather than its 3D offset.
Visual verification on Windows is still required.

## Development-only online demo

Start the demo world server with a configured static token hash (see the server
documentation), then set `LYNX_DEMO_TOKEN` in the client process environment to
the matching plaintext token and run `cargo run -p lynx_client -- --demo-online`.
Optionally set `LYNX_DEMO_URL`; it defaults to `ws://127.0.0.1:8090/world`.
Do not put tokens on the command line or in committed files. Environment values
can still be visible to processes or diagnostics; use only disposable development
credentials. Use local `ws://` only on a trusted network; remote use requires
`wss://` with a trusted certificate. This is not production authentication.

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

The no-default-features executable advances the shared application by one
update and exits; it is not an offline gameplay or replay harness. Workspace
tests also exercise the headless application, even with the default window
feature enabled.

Bevy is pinned to 0.20.0 at the workspace boundary. Per ADR 0001, upgrade it in
a dedicated change, run workspace tests and a visual smoke test, and add a
migration note to `CHANGELOG.md` for a minor upgrade.
