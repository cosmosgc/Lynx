# Lynx client foundation

`cargo run -p lynx_client` starts the built-in local demo zone on Windows (and
Linux with X11). It renders bounded terrain, props, characters, and
interactables as 2D placeholder sprites. WASD moves the local blue avatar; it
is a presentation preview only, not server-authoritative movement, collision,
or interaction resolution.

## Offline scene, camera, and input

Pass a generated project or its `content/` directory to preview authored scene
data. The loader recognizes a project root automatically, selects `zone.demo`
or `zone.starter` by default, and `--describe` prints the resolved scene before
the window opens:

```text
cargo run -p project_cli -- new my-world --template mmorpg --output ../my-world
cargo run -p lynx_client -- --content ../my-world --zone zone.starter --describe
```

Use `--camera follow3d` (the default) or `--camera billboard2d`; `3d`, `2d`,
`follow`, and `billboard` are accepted aliases. Press `C` in the offline window
to cycle modes. The present renderer uses camera-relative 2D sprites for both
modes; `billboard2d` is the selectable wider-scale fallback for clients that
cannot present the follow view.

Inputs resolve from platform-neutral `Control` values into `Action` values.
Defaults are WASD for movement, E for interact, Enter for chat, I for inventory,
and C for camera. A JSON file can replace bindings, for example
`{"bindings":{"W":"MoveForward","E":"Interact"}}`:

```text
cargo run -p lynx_client -- --bindings path/to/bindings.json
```

`OfflineFoundationPlugin` composes `OfflineCorePlugin`,
`DefaultActionPlugin`, and `FollowCameraPlugin`. Projects can replace action
resolution or camera systems independently in `OfflineSet::Actions` and
`OfflineSet::Camera`, while keeping movement and scene presentation unchanged.
The ordered sets are input, input filtering, actions, local preview movement,
camera, then presentation. `OfflineInputEnabled` can discard supplied input
while a UI has focus; the window adapter also clears controls when the primary
window loses focus. These facilities are local presentation seams, never
gameplay commands.

Content with `manifest.json` is rejected cleanly if the engine requirement,
file path, or SHA-256 hash is invalid. A missing manifest is reported as an
unversioned-source warning. Missing, unsafe, or unsupported presentation assets
produce a visible placeholder and a scene warning instead of panicking. The
current policy accepts PNG billboard portraits and GLB/GLTF models; verified
assets are recorded but not decoded by the placeholder renderer yet.

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

The development CLI remembers account identity and rotating refresh tokens per
Nakama server in the operating system's secure credential store. Passwords are
prompted without echo when required and are never saved. A server browser,
connection history, and pins remain planned. Remote world servers
must use `wss://`; plain `ws://` is for trusted local networks only. Public server
discovery is not specified.

## Development-only login and online demo

Run `cargo run -p lynx_client -- --account` for the in-client account screen.
Click a field or use Tab, enter an existing email and password, and sign in;
matching saved sessions are resumed from the OS credential store. Create new
email accounts through `account_portal/` first. The password is masked and is
not saved. Refresh the list after creating a character, then click a character
to verify ownership, then click **Enter world**. The Nakama world server creates
the character's deterministic player actor and persists it to the configured
world snapshot before admitting the session. The online presentation remains
read-only; movement and interactions are not implemented yet. Creation retries
keep the same idempotency key while this screen remains open; do not close it
while the result is uncertain. The default command still launches the offline
demo.

The client loads the repository-root `.env` when present; variables already set
in the process environment take precedence. For Nakama device login, configure
`LYNX_NAKAMA_URL` (for example,
`http://127.0.0.1:7350`) and the public client server key in
`LYNX_NAKAMA_SERVER_KEY`. The device ID and rotating
refresh token are stored in the OS credential manager (Windows Credential
Manager, macOS Keychain, or Linux Secret Service); the access token stays in
memory. If that secure store is unavailable, login fails closed without a
plaintext fallback. Existing stored devices refresh their session before the
client opens the window. The client verifies character ownership through
`LYNX_WORLD_API_URL` (default `http://127.0.0.1:8090`) before sending the access
token and selected character to the WebSocket gateway. Set `LYNX_WORLD_URL` for
the gateway (default `ws://127.0.0.1:8090/world`). Nakama HTTP permits plaintext
only to loopback; remote Nakama and world endpoints require HTTPS/WSS. The
Nakama server key is public client configuration, but must not be confused with
the private runtime HTTP key.

Onboarding uses the default-feature executable. To sign into the email/password
account created in `account_portal/`, use:

```text
cargo run -p lynx_client -- --email-login player@example.com
cargo run -p lynx_client -- --characters
cargo run -p lynx_client -- --create-character "Hero" my-first-hero-2026
cargo run -p lynx_client -- --login-email player@example.com <character-id>
```

`--email-login` authenticates and saves the account before listing/creating
characters; `--login-email` authenticates/resumes that account and attempts world
entry for the selected character. The password is prompted interactively without
terminal echo. The device-auth flow remains available separately. Character
commands use whichever Nakama account is currently saved for the server.

Choose a unique, non-secret intent key for each creation (1–128 ASCII letters,
digits, underscores or hyphens). Keep it and the exact name until the outcome is
known. If a request fails or the process stops, **repeat the same command and
key**, including after restarting; never choose a new key for that retry. A new
creation intent needs a new key, even if reusing a name. The server binds the key
to the request and account; changing the name with the same key conflicts. The
key is supplied explicitly, not generated or saved by the client. Names must have
1–32 characters with no control characters or surrounding whitespace.

The Nakama server provisions an actor on first admission and requires durable
world snapshot storage (`--state-file` or `--state-postgres`). It writes the
deterministically identified actor before creating the session, so a failed or
retried admission cannot create duplicates. `cargo run -p lynx_client -- --logout`
revokes the saved Nakama session and removes its OS credential entry. Startup
errors are coarse and printed without response bodies or credentials.

### Static-token demo

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

`validate_server_url` exposes the same URL policy before connecting. For fresh
Nakama admission, callers may supply the selected character in `Config.character`;
the transport sends it as a sensitive `X-Lynx-Character` header alongside the
bearer. Omit it for the static demo or a gateway resume token. The server checks
ownership. HTTP 401, 403, 429 and 503 map to coarse authentication, character,
rate-limit and unavailable failures, without retaining response details; 503
does not distinguish capacity from identity failure. Neither field is included
in `Config` debug output. The explicit `--login` development mode below now
acquires a Nakama device session, but the reusable transport API remains a seam
rather than a login UI or account manager. The HTTP auth client supports refresh
and per-session logout, but the application does not yet expose those operations
in its UI. The command-line device flow uses the OS credential store, with no
plaintext substitute. The application does not yet
retain or rotate world resume tokens. Closing the transport detaches the session,
which may remain resumable.

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
