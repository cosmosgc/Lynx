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
| Device, email, social, custom, and platform authentication; sessions and refresh tokens | **Partially integrated** | `backend_nakama` validates a fresh bearer session at the world edge. The Bevy account screen, server browser, secure credential storage, and refresh groundwork exist; live account/character entry validation remains. Static world tokens are development-only. |

| Account metadata and user profiles | **Partially integrated** | The world edge reads the authenticated account identity. Profile management is not exposed by the Lynx client or `game_api` adapter. |
| Character inventory | **Lynx PostgreSQL, authenticated read path integrated** | `GET /characters/{character}/inventory` validates the Nakama bearer session and character ownership before reading the Lynx inventory store. Nakama storage is not authoritative; mutations remain behind trusted world persistence. |
| Factions, territories, and world-event calendar | **Lynx PostgreSQL, authenticated read views integrated** | `NakamaPostgresBackend` implements `FactionService` and `CalendarService` over the normalized faction/calendar store. Reads require a valid Nakama session; mutations remain authoritative world results. Draft events are not public. |
| Friends, groups, parties, chat, and social presence | **Friend/group management and room/direct/group chat routing are integrated; live validation remains** | The authenticated world API, `CharacterApiClient`, and account screen expose friend and group management. Authenticated chat destination discovery is limited to mutual friends and current group memberships; the client joins those Nakama channels on demand. Party chat, zone admission binding, and live contract validation remain. Static `--demo-online` remains a separate development fallback. Nakama social presence is not authoritative Lynx zone presence. |
| Matchmaking and relayed multiplayer | **Planned, not integrated** | Nakama can own party formation and matchmaking; the Rust world service still owns zone admission, sessions, interest, replication, and gameplay authority. |
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

Character selection verifies account ownership. On first Nakama world admission,
the server now creates a player actor with a deterministic entity ID and persists
the world snapshot before opening the session. Nakama mode requires durable
snapshot storage (`--state-file` or `--state-postgres`); a failed snapshot write
fails admission and a retry reuses the same actor.

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
4. The authority loop ensures the character's deterministic actor exists and
   writes a durable snapshot before the gateway creates and controls the session.
   Its local resume token is single-use, short-lived, and distinct from a Nakama
   session token.

The `--account` Bevy screen supports email sign-in, listing/creating/selecting
characters, then handing the selected character to the world client. The older
CLI commands remain development seams. Character creation still goes through
the authenticated Lynx API; world actor IDs are deterministic from character
IDs and their records are stored in the world snapshot before fresh admission.

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

The client joins a configured Nakama room over the authenticated realtime
socket. It also requests account-authorized direct and group destinations from
`GET /social/chat/channels` and joins those targets on demand. Nakama derives
sender identity, persists messages, serves cursor history, and reports room
presence. Set `LYNX_NAKAMA_URL` and optionally `LYNX_CHAT_ROOM`; the default
room name is `lynx-zone`. The configured room is not proof of admission to the
matching Lynx world zone. World admission, movement, and authoritative presence
remain owned by the Lynx world service. Party chat and zone-admission binding
remain open.

Friend requests and group REST operations use `game_api` contracts that preserve
incoming/outgoing invitations, join-request roles, and membership state. The
player HTTP API exposes the account-scoped friend contract with the Nakama
bearer session: `GET /social/friends`, `POST /social/friends/request`,
`/social/friends/accept`, `/social/friends/dismiss`, `/social/friends/remove`,
and `POST /social/blocks/block` or `/social/blocks/unblock`. Mutation requests
carry `{"account_id":"..."}`; the server derives the caller from the session.
`lynx_client_net::CharacterApiClient` maps those routes to typed friend data and
operations. The Bevy account screen lists relationships, sends requests, and
offers accept, dismiss, remove, block, and unblock controls.

Group endpoints expose `GET /social/groups` for the caller's memberships,
`GET /social/groups/discover` for bounded filtered discovery, `POST
/social/groups` for creation, and `POST /social/groups/{id}/join` or `/leave`
for membership changes. `GET /social/groups/{id}/members` reads bounded roster
pages. The member mutation endpoints are
`POST /social/groups/{id}/members/add`, `/kick`, `/promote`, and `/demote`; each
accepts an `account_ids` array capped at 100. The account screen provides group discovery,
open-group creation, join or join-request, membership listing, leave, roster
inspection, join-request approval/decline, member add/remove, and role
promotion/demotion. The UI exposes roster mutation controls to owners and
administrators; Nakama remains the final authorization check for each operation.

The
`RealtimeChatService` discovers direct-message targets only for mutual friends
and group targets only for current memberships; join, history, send, and
presence use the shared `RealtimeChatSession` contract. Transient party
creation, membership, and join-request moderation use the separate
`RealtimePartyService`/`RealtimePartySession` contract so party socket lifetime
remains explicit. Nakama still authorizes each join and mutation.

The current account screen now exposes friend/group discovery and management;
realtime clients still join only the configured room. Live Nakama contract validation requires a configured test
service and accounts. Moderation/content policy remains tracked by
`NET-009`/`CNT-008`.
Lynx administration is separate: it uses
dedicated admin authentication and the Lynx audit log rather than player
Nakama sessions.

## References

- [Nakama real-time chat](https://heroiclabs.com/docs/nakama/concepts/chat/)
- [Nakama commands](https://heroiclabs.com/docs/nakama/getting-started/commands/)
- [Nakama configuration](https://heroiclabs.com/docs/nakama/getting-started/configuration/)
- [Nakama Console](https://heroiclabs.com/docs/nakama/getting-started/console/)
- [PST-010 — Nakama adapter coverage](TODO/20-persistence-backend.md#pst-010--nakama-adapter-coverage)
