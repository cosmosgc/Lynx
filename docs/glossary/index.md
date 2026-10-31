# Project glossary

This is a plain-language dictionary for technical terms that appear in Lynx
documentation and code. Each topic explains the idea, why it matters in this
project, and where to read more. These pages are introductions, not replacements
for the detailed design or API references.

## Topics

| Topic | In a few words |
| --- | --- |
| [Atomic variables](atomic-variables.md) | Small shared values that threads can read or update safely. |
| [Backpressure](backpressure.md) | Slowing or refusing incoming work when a service is at capacity. |
| [Content bundles and versions](content-bundles.md) | The packaged game data a world runs with, and how it is versioned. |
| [Entity-component system (ECS)](ecs.md) | A way to represent game objects as entities with data components. |
| [Fencing tokens](fencing-tokens.md) | Increasing ownership numbers that stop an old zone process from writing after takeover. |
| [Handshake and version negotiation](handshake.md) | How client and server agree on protocol compatibility when connecting. |
| [Interest management](interest-management.md) | Sending each player only the world updates they need to see. |
| [Idempotency](idempotency.md) | Making a retried request have its effect only once. |
| [MCP](mcp.md) | A standard way for AI tools to call explicitly provided project tools. |
| [RON](ron.md) | The human-readable data format used for authored game content. |
| [Prometheus text format](prometheus-text-format.md) | The line-based format used to expose metrics for scraping. |
| [Rate limits](rate-limits.md) | Server-side caps that keep clients from overwhelming actions or services. |
| [Replication, snapshots, and deltas](replication.md) | How the server sends current state and later changes to clients. |
| [Server authority](server-authority.md) | The server decides the real game state; clients send requests, not outcomes. |
| [Snapshots and event logs](snapshots-and-event-logs.md) | Saved state and the ordered records used to recover it. |
| [Stable IDs](stable-ids.md) | Persistent identifiers that remain meaningful across restarts. |
| [Ticks](ticks.md) | Discrete simulation steps used to advance the world. |
| [Transactions](transactions.md) | A group of data changes that all succeed together or are rolled back. |
| [Zones and territories](zones-and-territories.md) | The difference between a simulation/map area and its political ownership. |

## Adding a topic

Create one Markdown file per topic, explain unfamiliar terms as they arise, use
examples from Lynx where possible, and link to the authoritative project docs or
code. Add the page to this index.
