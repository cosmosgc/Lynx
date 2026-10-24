# Operations

## Environments

| Environment | Purpose | Data |
|---|---|---|
| local | development, tests | disposable |
| staging | publish validation, load tests | synthetic, refreshable |
| production | live service | backed up, audited |

Configuration is environment-specific. Secrets come from environment variables
or an OS secret store, never from the repository.

## Native service layout

- `lynx-world` — authoritative world service (not yet implemented as a binary;
  `world_gateway` is the library that will compose it).
- `nakama` — authentication, social, chat, matchmaking.
- `postgresql` — durable state.

Run each as an OS service in production with restart-on-failure, resource
limits, and log rotation. Docker Compose may mirror this layout for
convenience; it is not the reference path.

## Health checks

| Check | Endpoint / command |
|---|---|
| Nakama liveness | `GET /healthcheck` |
| Database connectivity | `lynx doctor --backend` |
| Content validity | `lynx validate --project <dir>` |
| Zone simulation | world service `/healthz` (planned) |
| Metrics | Prometheus text endpoint (planned) |

Readiness is stricter than liveness: a world service is ready only after
migrations, content load, and zone recovery complete.

## Metrics and logs

Structured logs include timestamp, level, service, zone ID, entity ID where
safe, correlation ID, action, outcome, and duration. Never log tokens,
passwords, or full private chat.

Minimum metrics:

- Tick duration, missed deadlines, entity count per zone.
- Connected sessions, retained reconnect windows, and zone population.
- Action throughput, rejection counts by `ErrorCode`.
- Replication bytes and queue depth, plus per-session delivery counts.
- Resume attempts by outcome (`resumed`, `expired`, `conflict`) and expired
  windows swept. Alert on a spike in `SessionExpired` or `Conflict`, which
  usually means token loss or duplicate clients.
- Pathfinding expansions, budget exhaustion, AI errors.
- Database latency and errors.
- Content version per zone.
- Missing localization keys by locale (should be zero in release).

## Session operations

- The session cap counts connected and retained sessions. Raise it only with a
  matching memory and bandwidth budget; reconnect windows are not free.
- Resume TTL is a gameplay tradeoff: long enough to survive a flaky link, short
  enough that an abandoned character does not hold a slot or stay in the world
  indefinitely. The default is 30 seconds.
- Resume tokens are secrets. Do not log envelopes that contain them, do not
  include them in error details, and rotate any log pipeline that captured one.
- A zone restart invalidates in-memory sessions. Announce expected restarts and
  expect players to log in again until durable session storage exists.
- Debugging a stuck character starts with its session: connected vs retained,
  principal, actor, and last acknowledged tick. Do not "fix" it by granting a
  client-supplied actor or session ID.

## Backups and recovery

- Nightly full PostgreSQL backup plus continuous WAL archiving where supported.
- Backups are encrypted, access-controlled, and stored outside the host.
- Restore drill monthly and before destructive migrations.
- Recovery objectives: RPO 15 minutes, RTO 1 hour for production; zone recovery
  after a process restart within 60 seconds from snapshot plus event log.
- Retain audit records according to policy; never let retention delete records
  under active moderation review.

## Deploy and rollback

1. Build and test the exact artifact.
2. Apply forward-compatible migrations.
3. Deploy to staging; run smoke and load tests.
4. Publish content as an immutable versioned bundle, including validated
   localization catalogs for every declared locale.
5. Roll out to one zone, monitor errors and tick health.
6. Promote to remaining zones.
7. Roll back the content bundle first; roll back code if the fault is in the
   runtime. Destructive migrations require a tested down-migration or restore.

## Incident response

- Mark the incident in the admin console with a note and correlation ID.
- Prefer read-only diagnosis before mutating live state.
- Capture logs, metrics, content version, and affected entity IDs.
- Apply the smallest reversible mitigation; record every admin action.
- After resolution, write a short postmortem with timeline, cause, and a
  follow-up task list.

## Upgrades

- Pin versions in `Cargo.toml`, `rust-toolchain.toml`, and the Nakama run
  scripts.
- Read release notes for Nakama and Bevy before upgrading.
- Upgrade PostgreSQL only through a tested dump/restore or `pg_upgrade` plan.
- Keep one previous compatible release artifact for rollback.
