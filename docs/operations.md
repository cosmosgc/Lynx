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
`scripts/*.sh` entry points and the `lynx-world` binary load that file without
overriding variables already present in the process environment. They accept
simple `KEY=VALUE` entries and matching quotes, but do not perform shell
expansion. Keep `.env` local and untracked; do not use it as a production
secret store.

`LYNX_PG_PASSWORD` is the application database role's password.
`setup-postgres.*` separately connects as `LYNX_PG_SUPERUSER` (default
`postgres`), using libpq authentication such as peer auth, a pgpass file, or
`PGPASSWORD`; it does not use the application password for that connection.
Use `LYNX_PG_MAINTENANCE_DB` to choose the database used for setup operations.

`run-nakama.*` locates the binary using `NAKAMA_BIN` (default `nakama`) and
reads console credentials, console signing key, socket server key, session and
refresh encryption keys, runtime HTTP key, optional Prometheus port, and log
level from the corresponding `LYNX_NAKAMA_*` environment variables.
`server/nakama/local.yml` contains non-secret development settings and
deliberately has no database address. The script renders `.lynx/nakama.local.yml`
with the database URL and optional overrides, then starts Nakama with both
configs via repeated `--config` arguments. `.lynx/` is gitignored; the overlay
contains credentials and must not be shared. The PowerShell script writes it
using the current directory ACL; the Bash script restricts its permissions with
`umask 077`.

All five Nakama keys must be independent, high-entropy, single-line values in
any persistent or networked environment. Changing the console signing or
session/refresh keys invalidates the relevant issued tokens; every node in a
cluster must use the same values. `LYNX_NAKAMA_SERVER_KEY` is the public client
server key and the Basic-auth key named `LYNX_NAKAMA_HTTP_KEY` by `lynx-world`;
it is distinct from the private `LYNX_NAKAMA_RUNTIME_HTTP_KEY` used for Nakama
runtime invocation. The former variable is retained for backwards-compatible
world-service configuration.

Nakama maintains its own schema history (`migration_info`) separately from the
Lynx `lynx.schema_migrations` history. On first setup, and before starting a
new pinned Nakama release, run `scripts/run-nakama.ps1 -Migrate` on Windows or
`scripts/run-nakama.sh --migrate` on Unix-like hosts. The root
`nakama-migrate.bat` helper does the same for the configured local Windows
binary. Run `db-migrate.*` separately for Lynx-owned migrations. See
[Nakama capabilities and boundaries](nakama.md) for feature ownership.

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

`lynx-world` defaults to `--auth-mode demo`, which accepts only SHA-256 hashes
of high-entropy bearer tokens. This is loopback development mode, not a
production login. Create a hash without writing the token to a file or shell
history:

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
| `--admin-bind` | `127.0.0.1:8091` | unauthenticated health and metrics listener |
| `--tick-ms` | `50` | authoritative tick interval, 5–1000 ms |
| `--resume-ttl-ms` | `30000` | reconnect window |
| `--view-radius-m` | `60` | server-chosen interest radius |
| `--max-sessions` | `1024` | connected plus retained sessions |
| `--session-policy` | `per-character` | retained-session uniqueness: `per-character` or `per-account` |
| `--auth-mode` | `demo` | `demo` static development tokens, or `nakama` production identity admission |
| `--staff` | off | mark every demo grant as staff; enables calendar lifecycle commands (loopback development only) |
| `--token-hash` | — | repeat once per demo character |
| `--state-file <path>` | off | write durable world snapshots to this JSON file and load it at startup |
| `--state-postgres` | off | store durable world snapshots in PostgreSQL instead; reads `LYNX_PG_HOST`, `LYNX_PG_PORT`, `LYNX_PG_DATABASE`, `LYNX_PG_USER`, `LYNX_PG_PASSWORD` (mutually exclusive with `--state-file`) |
| `--fresh` | off | ignore any saved snapshot, rebuild the authored demo, and overwrite it at startup; requires `--state-file` or `--state-postgres` (development reset) |
| `--snapshot-ms <ms>` | 10000 | how often the configured snapshot store is rewritten (must be at least the tick interval) |

For production authentication, use `--auth-mode nakama`, set
`LYNX_NAKAMA_URL` (and optional `LYNX_NAKAMA_HTTP_KEY`) plus the normal
`LYNX_PG_*` variables, and provision a zone snapshot whose live actors already
correspond to the characters allowed to enter it. The service opens separate
PostgreSQL connections for the trusted identity mapping and character lookup.
Clients send the Nakama bearer session in `Authorization` and exactly one
`X-Lynx-Character: chr_…` request header. The header does not establish
ownership; the service validates it against the account returned by Nakama.
Do not use `--staff` in Nakama mode. The service refuses it because production
staff roles are not part of this admission path.

Nakama validation occurs on every fresh WebSocket admission. Resume tokens are
local, single-use secrets and do not recontact Nakama; keep
`--resume-ttl-ms` short (30 seconds by default) to bound the period in which an
upstream revocation is not observed. A selected database character without one
live actor is refused; world population is not performed by admission.

At startup the service also publishes one open faction conflict over the demo
zone. Player sessions may enroll through `calendar.command` and inspect the
calendar with `calendar.query`; lifecycle transitions (schedule, activate,
resolve, …) are accepted only from `--staff` sessions. Applied commands are
committed to the in-memory backend together with the faction snapshot.

With `--state-file` or `--state-postgres` configured, the service loads that
snapshot at startup — zone entities, links, calendar, and faction state —
writes a baseline snapshot before the authority loop begins, rewrites it on
the snapshot interval and once more at shutdown, and resumes the saved tick.
Snapshots follow each entity's explicit persistence policy; sessions, resume
tokens, and minigame replication stay in memory as before. Without a store,
state is in memory and a restart logs everyone out and resets the zone.

With `--state-postgres`, the whole-world snapshot lives in
`lynx.world_snapshots` (one row per world, written atomically by an upsert, so
a failed write keeps the previous world) and the connection comes from the
same `LYNX_PG_*` variables the setup scripts use. Plain TCP only: keep the
database on loopback or a trusted private network and terminate TLS in front
of it; a remote, unencrypted database is not supported.

A snapshot is a whole-world overwrite, so falling back to the authored demo
would otherwise mean deleting the snapshot by hand. `--fresh` does that
explicitly: it requires `--state-file` or `--state-postgres`, ignores the
existing snapshot (even a corrupt one), rebuilds the demo from authored
content, and writes that pristine world over it before serving. If that
overwrite fails, startup aborts rather than serving a world the old snapshot
would replace on the next restart. Use it to reset a development world after
play testing:

```text
lynx-world serve --state-file world.json --fresh --token-hash <sha256-hex>
lynx-world serve --state-postgres --fresh --token-hash <sha256-hex>
```

The reset affects durable state only. Held connections, resume tokens, and
pending minigame seeds are process-local and already disappear on restart. Fresh
demo minigame seeds are drawn from server entropy on every build, which is not
a player-visible state change.

The process logs through `RUST_LOG` (`info` by default) and shuts down on
`Ctrl+C`: readiness turns false, the listener stops accepting, a final world
snapshot (when configured) is written, connections close, and the process
exits.

Health checks:

| Check | Endpoint / command |
|---|---|
| Nakama liveness | `GET /healthcheck` |
| World liveness | `GET /healthz` |
| World readiness | `GET /readyz` |
| Nakama connectivity | `lynx doctor --backend` (Nakama health only, not a database check) |
| Content validity | `lynx validate --project <dir>` |
| Content compatibility | `lynx content diff --base <manifest> --next <manifest>` |
| Metrics | `GET /metrics` |

Readiness is stricter than liveness: the current binary becomes ready after the
zone is built or restored and before it accepts connections, and turns not-ready
as soon as shutdown begins. A configured snapshot must load; a failed baseline
write aborts startup only with `--fresh`, otherwise it logs a warning. Migration execution, ongoing database health,
authored content loading, and event replay are not readiness checks.

## Metrics and logs

The service emits tracing logs; the comprehensive structured fields and
correlation plumbing below are targets, not a guaranteed current log schema
(`OPS-013`). Never log tokens, passwords, or full private chat.

The admin listener serves `/healthz`, `/readyz`, and `/metrics` independently
of the gameplay listener. These endpoints are unauthenticated and have no TLS;
keep the default loopback bind or restrict a private admin interface with
network policy and a trusted TLS proxy. Local users able to connect can read
aggregate operational data. The gameplay listener retains these routes for
compatibility. Do not expose either listener to untrusted networks without
appropriate controls; demo bearer authentication is not production auth.

Prometheus scrape example (use a private target reachable by Prometheus):

```yaml
scrape_configs:
  - job_name: lynx-world
    static_configs:
      - targets: ['127.0.0.1:8091']
```

Exported metrics are process-local and reset on restart. No client-controlled
strings or identities are labels. Rate-limit class and code labels come from
fixed enums. The single demo zone is exported without a zone label.
The endpoint uses Prometheus text format 0.0.4 on both listeners. Transport
counter samples are encoded with `prometheus-client` from a scrape-time snapshot;
the authority metrics retain their existing collector. Sample ordering is not
part of the endpoint contract.

### Nakama metrics

Set `LYNX_NAKAMA_METRICS_PORT` to a nonzero port to enable Nakama's Prometheus
endpoint (for example `7354`), then scrape `http://<nakama-host>:<port>/metrics`.
The default is disabled. Nakama exposes this listener unauthenticated on all
interfaces and has no separate metrics bind-address setting: only enable it
behind a host firewall, private network, or similarly restrictive network
policy. Lynx currently does not proxy, scrape, or alert on these metrics.

Run the read-only local smoke check from PowerShell after Nakama is already
running:

```powershell
pwsh -NoProfile -File .\scripts\test-nakama.ps1
```

It runs the startup script's `-Check` preflight (which does not start Nakama or
write the overlay), verifies configured security keys against the existing
gitignored overlay without displaying their values, checks `GET /healthcheck`,
and checks `GET /metrics` only when `LYNX_NAKAMA_METRICS_PORT` is nonzero. If
the binary is installed outside `PATH`, pass its path explicitly, for example:

```powershell
pwsh -NoProfile -File .\scripts\test-nakama.ps1 -NakamaBin 'D:\Development\MMORPG\Nakama\nakama.exe'
```

The smoke check never starts, stops, or restarts Nakama. A metrics check after
changing the port requires a separately authorized Nakama restart; the script
only checks the currently running listener.

```yaml
scrape_configs:
  - job_name: nakama
    static_configs:
      - targets: ['127.0.0.1:7354']
```

| Metric | Meaning |
|---|---|
| `lynx_world_ticks_total` | attempted ticks |
| `lynx_world_tick_failures_total` | ticks returning an authority error |
| `lynx_world_tick_duration_seconds_total` | total time executing ticks, seconds |
| `lynx_world_tick_overruns_total` | ticks whose execution exceeded the interval; not scheduler deadline misses |
| `lynx_world_tick_deadline_misses_total` | scheduled ticks that began after their configured deadline; distinct from execution overruns |
| `lynx_world_entities` | current demo-zone entities, refreshed on authority work |
| `lynx_world_connections` | attached connections, refreshed on authority work |
| `lynx_world_inputs_total` | envelopes delivered to authority, not action throughput |
| `lynx_world_retained_sessions` | sessions including attached and disconnected resumable sessions |
| `lynx_world_action_requests_total` | `action.request` envelopes delivered to the authority, including repeated requests |
| `lynx_world_action_rejections_total{code}` | authority action rejection replies, including gameplay rejections, by fixed `ErrorCode`; excludes transport rate limits |
| `lynx_world_resume_attempts_total{outcome}` | authority resume admissions, classified `success` or `failure`; excludes failures before authority admission |
| `lynx_world_outbound_queue_full_total` | failed enqueues due to a full per-connection outbound queue |
| `lynx_world_admission_rejections_total{reason}` | rejected WebSocket upgrades by bounded capacity/draining reason |
| `lynx_world_snapshot_writes_total{outcome}` | configured snapshot store write attempts, including startup and shutdown, by `success` or `failure`; no-store calls are excluded |
| `lynx_world_snapshot_write_duration_seconds` | histogram of time in configured store `save` calls, including successful and failed writes at startup, periodically, and at shutdown; excludes snapshot capture and no-store calls. Cumulative buckets: 0.001, 0.01, 0.1, 1, 10, +Inf seconds |
| `lynx_world_rate_limit_rejections_total{class,code}` | rate-limit rejections by bounded class and error code |
| `lynx_world_actions_applied_total` | newly applied actions; cached idempotent replies are excluded |
| `lynx_world_action_authority_duration_seconds` | histogram of time spent handling action requests inside the authority; not end-to-end client latency. Cumulative buckets: 0.001, 0.01, 0.1, 1, 10, +Inf seconds |
| `lynx_world_resume_failures_total{code}` | failed resume admissions by bounded `ErrorCode`; unknown and expired tokens share the gateway's `session_expired` category |
| `lynx_world_session_attaches_total` / `lynx_world_session_detaches_total` | authority connection attach and detach events, including resume attach |
| `lynx_world_session_expired_closes_total` | connections closed after the attached session disappeared or expired; not disconnected-window expiry sweeps |
| `lynx_world_outbound_queue_depth` / `lynx_world_outbound_queue_capacity` | summed frame depth/capacity sampled during authority work; not a continuous high-water mark |
| `lynx_world_replication_enqueued_bytes_total` | encoded snapshot/delta bytes accepted by the outbound queue; not confirmed socket delivery bytes |
| `lynx_world_ai_ticks_total` / `lynx_world_ai_decisions_total` | enabled AI ticks and evaluated controller decisions |
| `lynx_world_ai_budget_exhaustions_total{budget}` | AI controller-scan, update, perception and action limits using fixed budget labels |
| `lynx_world_content_version_info{state,version}` | content integration state (`absent`, `configured`, or `loaded`) and version when available. The current world service reports `state="absent"`; it does not load a content bundle |
| `lynx_backend_operations_total{adapter}` / `lynx_backend_operation_errors_total{adapter}` | Nakama and PostgreSQL adapter operation counts/errors; emitted in Nakama auth mode with fixed adapter labels. Errors include validation/domain failures |
| `lynx_backend_operation_duration_seconds{adapter}` | backend operation duration including PostgreSQL lock waits and failures; adapter snapshots are individually coherent, not atomic as a group |

Starting alert examples (tune against staging; not tested production rules):
`increase(lynx_world_tick_overruns_total[5m]) > 0`,
`sum(rate(lynx_world_rate_limit_rejections_total[5m])) > 1`,
`increase(lynx_world_snapshot_writes_total{outcome="failure"}[5m]) > 0`, and
`up{job="lynx-world"} == 0` for five minutes. Monitor readiness separately.

Unsupported or incomplete requested families (do not interpret their absence as zero):

- Tick duration distributions.
- A separate count of disconnected reconnect windows and resume-window expiry sweeps.
- End-to-end action latency and action rejections before authority delivery (authority latency, applied actions, and transport rate limits are measured separately).
- Per-session replication delivery counts, queue high-water marks, and bytes confirmed written to sockets.
- Pathfinding expansion/budget metrics are not wired into the world runtime. `NavGrid::find_path_observed` is opt-in; the current service has no pathfinding call site to observe.
- AI error counters; current AI diagnostics cover decisions and bounded-work limits only.
- Backend metrics are emitted only in Nakama auth mode and combine database domain/validation errors with infrastructure errors. There are no per-query or pool metrics.
- Snapshot load failures; write metrics above do not identify the store or error cause and do not include snapshot capture time.
- Localization fallback/missing-key counts are not wired to the service. `CatalogSet::resolve_with_diagnostics` is opt-in and the world service currently does not resolve localized catalogs.

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
- The production server is intended to configure whether this uniqueness limit
  is per character or per account. The current demo and gateway enforce the
  per-character policy; account-wide enforcement is not implemented yet.
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

1. Build and test the exact artifact. Review the changelog, migration notes,
   README progress and counts, relevant docs and links, and backlog gap register
   against the release (`OPS-014`; this review is not yet an enforced gate).
   Run `python scripts/test_check_markdown_links.py` and
   `python scripts/check_markdown_links.py`; manually review links outside the
   checker's documented coverage in `QUA-009`.
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
