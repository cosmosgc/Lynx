# Nakama capabilities and boundaries

Lynx pins [Nakama 3.41.0](https://github.com/heroiclabs/nakama/releases/tag/v3.41.0)
as its identity and platform-service backend. This document distinguishes the
features the Nakama distribution provides from the features that Lynx currently
integrates. A Nakama Console page or API capability does **not** imply that the
game client or `game_api` exposes it.

The authoritative boundary is defined by [ADR 0002](adr/0002-backend-topology.md)
and [ADR 0018](adr/0018-nakama-adapter-seam-and-replacement-criteria.md): Nakama
does not own authoritative zones or durable world state.

## Capability matrix

| Nakama capability | Lynx ownership and current status | Notes |
|---|---|---|
| Device, email, social, custom, and platform authentication; sessions and refresh tokens | **Partially integrated** | `backend_nakama` validates a fresh bearer session at the world edge. The client has device and email CLI login with OS credential-store persistence and a `--logout` command. A login form and character-list UI remain pending. Static world tokens are development-only. |

| Account metadata and user profiles | **Partially integrated** | The world edge reads the authenticated account identity. Profile management is not exposed by the Lynx client or `game_api` adapter. |
| Friends, groups, chat, presence, status, notifications | **Planned, not integrated** | Nakama is the selected platform owner, but Lynx has no production adapter/client flow for these services yet. Do not use Nakama presence as authoritative zone presence. See [social and chat boundary](#social-and-chat-boundary). |
| Matchmaking and relayed multiplayer | **Planned, not integrated** | The Rust world service owns sessions, interest, replication, and authority. Nakama matchmaking may later form groups before a world admission. |
| Authoritative matches and runtime modules (Go, TypeScript, Lua) | **Not used for world authority** | They may later host isolated, low-stakes activities. They must not become the source of truth for zones, inventory, faction state, or world events. |
| Storage engine, leaderboards, and tournaments | **Not integrated for world state** | Lynx-owned PostgreSQL migrations and snapshots own durable simulation data. Any future Nakama storage use needs an explicit `game_api` seam, retention policy, and migration/export plan. |
| Console and Console API | **Available for local administration** | The development console binds only to `127.0.0.1:7351`. It is an administrator interface, not the player login experience. |
| Health endpoint and Prometheus metrics | **Health used; metrics opt-in** | `GET /healthcheck` is used by `lynx doctor --backend`. Set `LYNX_NAKAMA_METRICS_PORT` to expose Nakama Prometheus metrics; Lynx does not currently scrape, proxy, or alert on them. |

## Email login

For an account created through `account_portal/`, run `--email-login <email>` to
authenticate before listing or creating characters, or use
`--login-email <email> <character-id>` to authenticate and attempt world entry.
Configure `LYNX_NAKAMA_URL`,
`LYNX_NAKAMA_SERVER_KEY`, and, if needed, `LYNX_WORLD_API_URL` and
`LYNX_WORLD_URL` as for `--login`. The password is requested without terminal
echo only when needed; do not pass it as an argument. The OS credential store
holds the email identity and rotating refresh token, never the password. A saved
session is refreshed only for the matching email; an expired session prompts
again. Explicitly logging into a different email replaces the saved per-server
session after successful authentication. Existing device records continue to
work, and `--characters` and `--login` resume the currently saved account; if an
email session expires, use `--login-email` to authenticate again. `--logout`
revokes and removes the current saved session. There is no plaintext credential
fallback. Remote authentication requires HTTPS and does not use proxy settings.

Character selection verifies account ownership, but neither portal account
creation nor character selection provisions a live world actor. World admission
still requires a corresponding actor to exist on the server.

## Current request path

1. A client obtains a Nakama session through a supported Nakama authentication
   flow. `lynx_client_net::NakamaAuthClient` supports device and email acquisition,
   refresh, and per-session logout. The client stores account identity and the
   rotating refresh token in the OS credential store, refreshes before opening
   its window, and verifies the selected character through the authenticated
   Lynx HTTP API. It has no login/character-selection UI. It refuses remote
   plaintext HTTP and ambient HTTP(S) proxies so bearer and Basic credentials
   are not redirected.
2. For a fresh world admission, the client sends the Nakama bearer session in
   `Authorization` and exactly one selected Lynx character in
   `X-Lynx-Character`.
3. `lynx-world --auth-mode nakama` validates the bearer through Nakama, maps
   the trusted account to Lynx PostgreSQL data, and verifies character ownership.
4. The Rust gateway creates and controls the world session. Its local resume
   token is single-use, short-lived, and distinct from a Nakama session token.

The current login command is a transport integration seam, not a complete
first-time player flow. World admission requires a persisted Lynx character
owned by the authenticated Nakama account and an actor for that character in
the active world. The command creates a new ephemeral device identity but does
not create or list characters, so new accounts cannot normally finish admission
yet. Character provisioning and a stable, securely stored device identity are
still required for a usable player login flow.

See [networking](networking.md#world-edge-identity-admission) for admission and
revocation semantics, and [operations](operations.md#running-the-world-service)
for server configuration.

## Local operation

Nakama has a database schema independent of the `lynx` schema. After
PostgreSQL setup and before the first Nakama startup, run:

```text
scripts/run-nakama.ps1 -Migrate    # Windows PowerShell
scripts/run-nakama.sh --migrate   # Linux/macOS shell
```

Then run the corresponding start command without the migration option. The
scripts generate a gitignored `.lynx/nakama.local.yml` overlay containing the
database address and configured local secrets. Never commit or share it.

The repository's checked-in local configuration uses Nakama development
defaults for several cryptographic values. The startup warnings are expected
for a disposable loopback environment, but a production deployment must supply
unique, secret console signing, socket server, session encryption, refresh
encryption, and runtime HTTP keys through its deployment secret configuration.
Do not expose the Console, socket, or runtime HTTP APIs directly to the public
Internet without TLS, network controls, and production secret management.

Set `LYNX_NAKAMA_METRICS_PORT` to a nonzero port to enable Nakama's Prometheus
endpoint. It binds unauthenticated on all interfaces, so it must be protected
by a host firewall or private scrape network. See
[operations](operations.md#nakama-metrics) for the scrape configuration.

## Social and chat boundary

Nakama supports friends, groups, direct/group/room chat, presence, and
notifications, but Lynx does not yet expose them through `backend_nakama`.
This is intentional rather than an omitted HTTP call: `game_api::FriendState`
cannot represent Nakama's outgoing and incoming friend-invitation states, and
`game_api::ChatService::channels` requires generic channel discovery that
Nakama's public REST API does not provide. A quick adapter would lose invitation
state or invent channels, so it is not shipped. Evolve those contracts (and add
socket delivery where required) before integrating social/chat.

## References

- [Nakama commands](https://heroiclabs.com/docs/nakama/getting-started/commands/)
- [Nakama configuration](https://heroiclabs.com/docs/nakama/getting-started/configuration/)
- [Nakama Console](https://heroiclabs.com/docs/nakama/getting-started/console/)
- [PST-010 — Nakama adapter coverage](TODO/20-persistence-backend.md#pst-010--nakama-adapter-coverage)
