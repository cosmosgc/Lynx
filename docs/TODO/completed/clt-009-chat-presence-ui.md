# CLT-009 — Chat and presence UI

**Status: Completed (2026-11-04).** The Bevy online client delivers one
server-authoritative zone chat surface with a message feed, composer, bounded
history paging, safe errors, and connected-member presence.

For account sessions with a verified character, chat uses Nakama's
authenticated realtime room and persistent history API. Static
`--demo-online` sessions use the gateway fallback and never send their
world-only bearer token to Nakama. The gateway resolves sender identity from
session claims and enforces the zone channel. The client retains at most 500
messages and deduplicates pages by message ID.

**Validation:** On 2026-11-04, the user supplied a desktop screenshot showing
an active demo session, two server-echoed messages under the resolved character
identity, and the connected character in the presence roster. `cargo check -p
lynx_client` and `cargo clippy -p lynx_client --all-targets -- -D warnings`
passed. The screenshot was not copied into the repository.

**Deferred:** Multi-client exchange, presence join/leave, and history paging
across reconnect are tracked in [CLT-014](../40-bevy-client.md#clt-014--client-integration-and-protocol-tests).
Channel navigation, unread indicators, localized chat metadata, and moderation
controls are tracked in [CLT-015](../40-bevy-client.md#clt-015--expanded-chat-channels-and-moderation-ui).
