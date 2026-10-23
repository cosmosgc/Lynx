# Security and moderation

## Threat model

Trusted: the world service process, the database, and the operator's admin
session.

Untrusted: clients, client input, client clocks, client content claims, player
chat, user-authored content, MCP tool metadata and results, and external tool
servers.

## Authority invariants

- A client cannot grant itself items, currency, experience, admin roles, or
  control of an entity.
- Every action is validated server-side for actor control, distance,
  visibility, permissions, state, cooldown, and cost.
- Inventory and economy mutations are transactional and idempotent. Replayed
  requests cannot duplicate or destroy items.
- Stable IDs are server-assigned. Client-supplied entity IDs are rejected.
- Content bundles are hashed and version-checked; the client cannot authorize
  gameplay from local content.

## Authentication and sessions

- Player authentication is delegated to Nakama. The world gateway validates the
  session token on connect and on token refresh.
- Admin authentication is separate from player authentication and uses its own
  role model. Optional MFA or identity-provider integration is required before
  production admin exposure.
- Admin sessions are short-lived and revocable. Every admin operation is
  authorized server-side, regardless of what the UI shows.

## Admin roles (initial)

| Role | Capabilities |
|---|---|
| `observer` | read-only inspection, live maps, metrics |
| `moderator` | mute, kick, quarantine, report resolution |
| `operator` | zone restart, safe content publish, rollback |
| `engineer` | diagnostics, feature flags, configuration |
| `owner` | role management, break-glass, destructive actions |

Destructive or economy-affecting actions require a stronger role, explicit
confirmation, a captured reason, and an audit record. Read-only is the default
mode for every admin client.

## Audit log

- Append-only. Ordinary admins cannot update or delete entries.
- Records actor, role, action, target, before/after summary, reason,
  correlation ID, timestamp, and source address.
- Secrets, tokens, and unnecessary private chat data are redacted before write.
- Writes happen in the same transaction as the state change, or the change is
  rejected.

## Content and user-generated content

- User content is data, not code. It is schema-validated, size-bounded, and
  scanned for disallowed asset types before publishing.
- Ownership, visibility, moderation status, and revision history are required
  metadata.
- Publishing is explicit and reversible. Revocation removes content from new
  sessions without deleting the audit trail.
- Placement and property changes are validated for bounds, collision, budget,
  ownership, and zone rules.

## AI and MCP tools

- MCP support is opt-in and disabled by default.
- Tools are workspace-scoped and least-privilege. File writes are confined to
  the project; shell execution uses an allowlist or explicit approval.
- Destructive, privileged, publishing, and production actions require approval
  and display the exact command and diff.
- Tool outputs are untrusted input: validate paths, reject traversal, cap
  output size, and never execute returned shell text automatically.
- Credentials are never exposed to tools or written to logs. Audit AI-initiated
  changes with the tool name, inputs, and outcome.

## Abuse prevention

- Rate limits per session, per IP, and per action type.
- Payload size, nesting depth, and string length caps.
- Chat and content reporting, blocking, muting, and moderation queues.
- Anti-duplication checks for trades, mail, and reward claims.
- Backups are verified by restore drills, not by job success alone.
- Dependencies are audited and updated; build artifacts are reproducible where
  practical.

## Reporting a vulnerability

Do not open a public issue for a vulnerability. Contact the maintainers
privately with reproduction steps, affected version, and impact. See
`SECURITY.md` (to be added before any public deployment).
