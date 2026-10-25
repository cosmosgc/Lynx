# Operations

## Environments

| Environment | Purpose | Data |
|---|---|---|
| local | development, tests | disposable |
| staging | publish validation, load tests | synthetic, refreshable |
| production | live service | backed up, audited |

Configuration is environment-specific. Secrets come from environment variables
or an OS secret store, never from the repository.

For native PostgreSQL/Nakama development, copy `.env.example` to `.env` in the
repository root and fill in local values. The `scripts/*.ps1` and
`scripts/*.sh` entry points load that file without overriding variables
already present in the process environment. They accept simple `KEY=VALUE`
entries and matching quotes, but do not perform shell expansion. Keep `.env`
local and untracked; do not use it as a production secret store.

`LYNX_PG_PASSWORD` is the application database role's password.
`setup-postgres.*` separately connects as `LYNX_PG_SUPERUSER` (default
`postgres`), using libpq authentication such as peer auth, a pgpass file, or
`PGPASSWORD`; it does not use the application password for that connection.
Use `LYNX_PG_MAINTENANCE_DB` to choose the database used for setup operations.

`run-nakama.*` locates the binary using `NAKAMA_BIN` (default `nakama`) and
reads console credentials, server key, and log level from the corresponding
`LYNX_NAKAMA_*` environment variables. `server/nakama/local.yml` contains
non-secret development settings and deliberately has no database address. The
script renders `.lynx/nakama.local.yml` with the database URL and optional
overrides, then starts Nakama with both configs via repeated `--config`
arguments. `.lynx/` is gitignored; the overlay contains credentials and must
not be shared. The PowerShell script writes it using the current directory ACL;
the Bash script restricts its permissions with `umask 077`.

The `lynx` CLI does not load `.env`. For `lynx doctor --backend`, set
`LYNX_NAKAMA_URL` in the process environment (or use `NAKAMA_URL` /
`NAKAMA_BASE_URL`); otherwise it checks the default `http://127.0.0.1:7350`.

## Native service layout

- `lynx-world` — authoritative world service. It owns one in-memory demo zone,
  serves `/world`, `/healthz`, and `/readyz`, and runs the tick loop.
- `nakama` — authentication, social, chat, matchmaking.
- `postgresql` — durable state.

Run each as an OS service in production with restart-on-failure, resource
limits, and log rotation. Docker Compose may mirror this layout for
convenience; it is not the reference path.

## Running the world service

`lynx-world` accepts only SHA-256 hashes of high-entropy bearer tokens. Create a
hash without writing the token to a file or shell history:

```bash
printf '%s' "$LYNX_DEMO_TOKEN" | cargo run -p lynx-world -- token-hash
cargo run -p lynx-world -- serve \
  --bind 127.0.0.1:8090 \
  --token-hash <sha256-hex>
```

Each configured hash creates one demo character. Tokens may also be supplied
comma- or whitespace-separated through `LYNX_WORLD_TOKEN_HASHES`. Relevant
flags:

| Flag | Default | Meaning |
|---|---|---|
| `--bind` | `127.0.0.1:8090` | listen address; loopback by default |
| `--tick-ms` | `50` | authoritative tick interval, 5–1000 ms |
| `--resume-ttl-ms` | `30000` | reconnect window |
| `--view-radius-m` | `60` | server-chosen interest radius |
| `--max-sessions` | `1024` | connected plus retained sessions |
| `--token-hash` | — | repeat once per demo character |

The process logs through `RUST_LOG` (`info` by default) and shuts down on
`Ctrl+C`: readiness turns false, the listener stops accepting, connections
close, and the process exits. State is in memory, so a restart logs everyone
out and resets the zone.

Health checks:

| Check | Endpoint / command |
|---|---|
| Nakama liveness | `GET /healthcheck` |
| World liveness | `GET /healthz` |
| World readiness | `GET /readyz` |
| Database connectivity | `lynx doctor --backend` |
| Content validity | `lynx validate --project <dir>` |
| Content compatibility | `lynx content diff --base <manifest> --next <manifest>` |
| Metrics | Prometheus text endpoint (planned) |

Readiness is stricter than liveness: the current binary becomes ready after the
zone is built and before it accepts connections, and turns not-ready as soon as
shutdown begins. Durable migrations, content loading, and zone recovery will
join that gate when they exist.

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
  The binary sends the current token once per admission in `world.token`; treat
  that control message as sensitive.
- Static bearer tokens are also secrets, even though the service stores only
  their SHA-256 hashes. Use high-entropy values, distribute them through the OS
  secret store, and regenerate the hash when a token is retired. Hashed
  configuration does not make a weak token safe from offline guessing.
- A character has at most one connected session. Opening a second connection
  with the same static token returns `Conflict` until the first detaches or its
  resume window expires; do not work around it by duplicating hashes.
- A client that cannot drain its bounded outbound queue is detached instead of
  slowing the authoritative tick. Alert on repeated detaches; they usually mean
  a stuck client or a network path with insufficient throughput.
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
