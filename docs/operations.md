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
Deployments that mount secrets as files can set `LYNX_PG_PASSWORD_FILE` instead;
the service reads the file contents and removes the final line ending. A
non-empty direct value and its `_FILE` form cannot both be set; blank template
values are treated as unset so mounted secrets work with a copied `.env` file.
The setup scripts also accept
`LYNX_PG_MIGRATION_PASSWORD_FILE`; `run-nakama.*` accepts `_FILE` forms for the
console password, console signing key, socket server key, session and refresh
encryption keys, and runtime HTTP key. `lynx-world` accepts file forms for
`LYNX_NAKAMA_HTTP_KEY` and `LYNX_WORLD_TOKEN_HASHES`. In every case the file
path replaces the literal secret in the service's configuration environment.
The Rust service reads secret files directly. The shell setup helpers pass
PostgreSQL values to `psql` through its required environment interface, and the
Nakama runner writes its values into `.lynx/nakama.local.yml`; keep mounted
files and that generated overlay readable only by the service account.
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

On Windows, `BAT\run-server.bat` and `run-multiplayer-server.bat` launch with
`--auth-mode nakama`; the multiplayer launcher uses a separate world snapshot.
Both require the repository-root `.env` to contain `LYNX_NAKAMA_URL` and valid
`LYNX_PG_*` settings. The batch launchers no longer prompt for static demo
tokens. To run the loopback-only demo mode manually, use the commands below.

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

### Public TLS deployment

The world binary has no TLS support. For a public endpoint, terminate HTTPS/WSS
at a maintained reverse proxy and keep `lynx-world --bind 127.0.0.1:8090` on
loopback. [`deploy/nginx/lynx-world.conf`](../deploy/nginx/lynx-world.conf) is a
reference Nginx server configuration for `/world`; replace its hostname and
certificate paths, install it in the appropriate Nginx context, then run
`nginx -t` before reload. It preserves `Authorization`, uses WebSocket upgrade,
and bounds headers/timeouts. Restrict access to the upstream so external peers
cannot bypass TLS or authentication controls. Do not publish the unauthenticated
admin listener; retain its loopback default.

The service ignores `Forwarded`, `X-Forwarded-For`, and `X-Real-IP`; behind the
proxy, IP admission limits see the proxy's address. Do not enable trust in
client-supplied forwarding headers. See [networking](networking.md#tls-termination-and-proxy-trust-boundary)
for the complete trust boundary and HSTS/redirect guidance. TLS termination
does not replace Nakama authentication: production must still use
`--auth-mode nakama`.

### Reference deployment topology and service lifecycle

The current production shape is one active `lynx-world` process for the demo
world, a TLS reverse proxy, private PostgreSQL, Nakama identity, and separate
backup, metrics, and log collection. The world binary has no TLS support. Keep
its gameplay listener and unauthenticated admin listener on loopback behind
the proxy and local scrape agent. Expose only TCP 443 to players, routing the
world hostname to Lynx and the account/realtime hostname to Nakama's public API
and socket endpoints. Permit the world host to reach PostgreSQL and Nakama over
their private service networks; do not expose PostgreSQL, Nakama Console,
runtime HTTP, or metrics endpoints to the public internet. Upload encrypted
backups to a separate failure domain.

```text
Players ── TLS/WSS :443 ──> reverse proxy ── loopback :8090 ──> lynx-world
   └──── auth/realtime TLS ───────┘          └── private :7350 ──> Nakama
                                           scrape agent ─ loopback :8091
lynx-world ── private TLS ──> PostgreSQL and Nakama
encrypted backup target <──── PostgreSQL
```

This deployment is single-world and single-active-authority. Starting another
world process against the same PostgreSQL world and zone does not add capacity;
the lease intentionally rejects a second authority. Multi-zone scheduling and
gateway routing are not yet wired into the runtime. Scale the host only after
measured load data and keep the database, backups, and telemetry in separate
failure domains where the deployment supports it.

The repository includes a hardened `systemd` unit at
[`deploy/systemd/lynx-world.service`](../deploy/systemd/lynx-world.service).
Install the release binary at `/usr/local/bin/lynx-world`, create the `lynx`
service account and `/var/lib/lynx-world`, and provide the required
`LYNX_PG_*` and `LYNX_NAKAMA_*` variables through
`/etc/lynx-world/lynx-world.env`. Restrict that file to root (`0600`); use the
deployment's secret injection facility where one is available. The unit binds
both listeners to loopback and runs as an unprivileged account. Install it with
`systemctl daemon-reload`, enable it with `systemctl enable --now lynx-world`,
and inspect `systemctl status lynx-world` plus the journal. The process handles
SIGTERM by marking readiness false, stopping admission, and completing its
configured final snapshot before exit; keep `TimeoutStopSec` longer than the
longest expected snapshot write.

For Windows installations that run the service under the Windows Service
Control Manager, [`deploy/windows/LynxWorldService.xml`](../deploy/windows/LynxWorldService.xml)
is a WinSW wrapper configuration. Place the pinned, trusted WinSW release,
`LynxWorldService.xml`, and `lynx-world.exe` in one protected directory, and
make the `LYNX_PG_*` and `LYNX_NAKAMA_*` variables available to the service
process without storing production credentials in the repository. From an
elevated PowerShell session, install/start with
`.\LynxWorldService.exe install` and `.\LynxWorldService.exe start`; use
the matching `stop` command for planned shutdown. WinSW sends Ctrl-C and waits
up to the configured stop timeout, allowing the service's graceful snapshot
path to run ([WinSW stop-timeout behavior](https://github.com/winsw/winsw/blob/v3/docs/xml-config-file.md#stoptimeout)).
Windows is the primary development target; Linux x86-64 remains the supported
server deployment target.

Deployment checklist:

- Apply database migrations with the separate migration role; run the service
  with only the least-privilege runtime role.
- Verify PostgreSQL TLS and Nakama reachability from the service host.
- Confirm the proxy forwards WebSocket upgrades and authorization headers for
  both Lynx and Nakama; restrict direct access to the loopback gameplay listener.
- Keep the admin listener private to the local scrape agent or a trusted
  management network. It has no authentication.
- Allow only intended ingress ports in host and cloud firewalls; PostgreSQL
  must remain private.
- Check `/readyz` and `/metrics` locally before enabling player traffic, then
  verify the external WSS route through the proxy.
- Before a planned restart, drain player traffic, stop the service, confirm a
  successful final snapshot and clean exit, then start it and verify readiness.
- Confirm backups are encrypted, off-host, and covered by the restore drill in
  [backups and recovery](#backups-and-recovery).
- Keep staging and production configuration differences explicit: hostnames,
  TLS certificates, database/Nakama endpoints, credentials, retention, and
  resource limits. Never use `--fresh` in staging or production.

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
| `--outbound-queue` | `64` | frames queued per connection before the client is detached as too slow, 8–256 |
| `--session-policy` | `per-character` | retained-session uniqueness: `per-character` or `per-account` |
| `--auth-mode` | `demo` | `demo` static development tokens, or `nakama` production identity admission |
| `--staff` | off | mark every demo grant as staff; enables calendar lifecycle commands (loopback development only) |
| `--token-hash` | — | repeat once per demo character |
| `--state-file <path>` | off | write durable world snapshots to this JSON file and load it at startup |
| `--state-postgres` | off | store durable world snapshots in PostgreSQL instead; reads `LYNX_PG_HOST`, `LYNX_PG_PORT`, `LYNX_PG_DATABASE`, `LYNX_PG_USER`, `LYNX_PG_PASSWORD` (mutually exclusive with `--state-file`) |
| `--world-map <definition-id>` | off | serve the placed zones from this map in `--content-bundle`; requires `--state-postgres`, acquires a lease per zone, and checkpoints them independently |
| `--fresh` | off | ignore any saved snapshot, rebuild the authored demo, and overwrite it at startup; requires `--state-file` or `--state-postgres` (development reset) |
| `--snapshot-ms <ms>` | 10000 | how often the configured snapshot store is rewritten (must be at least the tick interval) |
| `--lease-ttl-ms <ms>` | 30000 | zone lease lifetime with `--state-postgres`, 1000–600000 ms and at least three tick intervals; the holder renews every third of it |

For Nakama authentication, use `--auth-mode nakama`, set `LYNX_NAKAMA_URL` (and
optional `LYNX_NAKAMA_HTTP_KEY`) plus the normal `LYNX_PG_*` variables, and
configure a durable world snapshot with `--state-file` or `--state-postgres`.
Nakama mode refuses to start without snapshot storage because newly admitted
characters are provisioned into the live zone and the snapshot is committed
before a session is opened. The service opens separate PostgreSQL connections
for the trusted identity mapping and character lookup.
Clients send the Nakama bearer session in `Authorization` and exactly one
`X-Lynx-Character: chr_…` request header. The header does not establish
ownership; the service validates it against the account returned by Nakama.
Do not use `--staff` in Nakama mode. The service refuses it because production
staff roles are not part of this admission path.

To serve an authored map, provide its content bundle and PostgreSQL state:

```text
lynx-world serve --world-map world.main --content-bundle ./content --state-postgres --token-hash <sha256-hex>
```

The process acquires every placed zone lease before restoring zone checkpoints.
Clients connect to `GET /world/<zone-instance-id>`; the ID is stable for a
map/zone-definition pair. `GET /world` remains available: authenticated
characters with a persisted actor are routed to that actor's zone, while new
characters are admitted only to the lexically first stable zone ID. Resume
tokens are looked up without consumption and routed by their persisted zone
ID; an explicitly wrong
zone route is rejected before token rotation. Zone IDs are printed at startup.
Migration `0018_zone_scoped_resume_sessions` revokes durable resume sessions
created before zone ownership was recorded; clients must authenticate again
after applying it. Each zone uses its authored player capacity and geometry.
This mode currently serves
zone-owned simulation state only;
world calendar/faction coordination, typed behavior for authored entity
placements, and live travel
handoff are not composed into it yet. Do not treat cross-zone travel as enabled
until those pieces are wired and the PostgreSQL recovery acceptance is run.

Nakama validation occurs on every fresh WebSocket admission. Resume tokens are
local, single-use secrets and do not recontact Nakama; keep
`--resume-ttl-ms` short (30 seconds by default) to bound the period in which an
upstream revocation is not observed. A selected database character without one
live actor is refused; world population is not performed by admission.

At startup the service also publishes one open faction conflict over the demo
zone. Player sessions may enroll through `calendar.command` and inspect the
calendar with `calendar.query`; lifecycle transitions (schedule, activate,
resolve, …) are accepted only from `--staff` sessions. Applied commands are
committed atomically with the faction snapshot; PostgreSQL mode uses the
normalized paired store, while local modes remain in memory.

With `--state-file` or `--state-postgres` configured, the service loads that
snapshot at startup — zone entities, links, calendar, and faction state —
writes a baseline snapshot before the authority loop begins, rewrites it on
the snapshot interval and once more at shutdown, and resumes the saved tick.
Snapshots follow each entity's explicit persistence policy. In PostgreSQL mode,
resume-session metadata is durable (only token digests are stored); a detached
session can resume within its configured window after restart, while sockets
and interest state are rebuilt. File and memory modes retain process-local
sessions. Without a durable store, a restart logs everyone out and resets the
zone.

With `--state-postgres`, the whole-world snapshot lives in
`lynx.world_snapshots` (one row per world, written atomically by an upsert, so
a failed write keeps the previous world) and the connection comes from the
same `LYNX_PG_*` variables the setup scripts use. Each socket connection
attempt is limited to five seconds; the three-attempt initial-connect budget is
bounded. PostgreSQL statement, lock, and idle-transaction
timeouts are configured to five, two, and five seconds respectively. Initial
connection attempts use a bounded three-attempt backoff. TCP user timeout and
keepalive probes are also configured to detect dead connections. Queries are not
automatically retried because a lost response can make a write outcome
ambiguous. PostgreSQL connections use verified TLS by default, including remote
hosts; certificates are checked against the platform's native trust store.
For a private CA, set `LYNX_PG_TLS_CA_CERT` to a PEM certificate bundle. Hostname
verification remains enabled. Remote plaintext connections are not supported.
For isolated local development only, set
`LYNX_PG_ALLOW_INSECURE_LOOPBACK=true` to permit plaintext when every configured
host resolves exclusively to loopback addresses; leave it unset in deployed
environments. Without that opt-in, loopback connections also require verified
TLS.
World-state stores share a bounded eight-connection pool with a two-second
acquisition deadline; identity and character stores share a separate pool with
the same limits. New physical pool connections use the bounded three-attempt
connect budget, and pool shutdown closes idle sockets outside the pool lock.
Pool exhaustion returns a sanitized error rather than waiting indefinitely.
PostgreSQL work is synchronous and runs inside Tokio's blocking section, so a
PostgreSQL-backed world service must use a multi-thread Tokio runtime; the
binary's default `#[tokio::main]` runtime qualifies. A current-thread runtime is
safe only for in-memory stores.
The setup and migration scripts use libpq and apply the same TLS policy
automatically: remote hosts use `verify-full`, with `LYNX_PG_TLS_CA_CERT` mapped
to libpq's root-certificate setting. Plaintext is enabled for scripts only when
the configured hosts are loopback and the explicit development opt-in is set.
For production, configure `LYNX_PG_USER`/`LYNX_PG_PASSWORD` as the runtime role
and `LYNX_PG_MIGRATION_USER`/`LYNX_PG_MIGRATION_PASSWORD` as a separate schema
owner used only by setup and migration scripts. The setup script grants the
runtime role DML on Lynx tables and sequence usage, while future migration-owned
tables receive the same defaults; do not pass migration credentials to the
world service.

### Secret rotation

Inject database and Nakama secrets through the deployment environment or an OS
secret store, not command-line arguments. The service reads them at startup;
rotate by staging the replacement secret, rolling/restarting instances so new
connections use it, checking readiness and sanitized error metrics, then
revoking the old credential. Keep migration-role credentials separate and
rotate them independently. For zero-downtime PostgreSQL rotation, provision a
second least-privilege runtime login with the same grants, switch the secret and
roll the world instances, then revoke the former login. Existing database pool
connections are not a live credential-refresh mechanism.

Nakama server and runtime keys come from their `LYNX_NAKAMA_*` environment
variables. Coordinate key changes across every Nakama node; changing session or
refresh encryption keys invalidates affected sessions. For development bearer
tokens, `LYNX_WORLD_TOKEN_HASHES` can temporarily contain both old and new
digests during rollout; distribute the replacement token out-of-band, then
remove the old digest and restart. Never place raw tokens or passwords in shell
history, process arguments, logs, or shared `.env` files.

With `--state-postgres`, the service also takes a single-writer lease on the
demo zone in `lynx.zone_leases` before it loads the snapshot. A second process
refuses to start while an unexpired lease is held. The lease carries a
monotonically increasing fencing token; every snapshot upsert and event append
checks the owner, token, and expiry in the same transaction as the write, so a
process that lost the lease is rejected with `conflict` even if it has not yet
noticed. A replacement may take over an expired lease after the previous holder
crashes; fenced startup verifies the pinned checkpoint/event history and replays
entity, link, cooldown, transfer-receipt, and calendar/faction deltas. Each
successful checkpoint save prunes every earlier event in the zone, so the log is
bounded by one snapshot interval. The
holder renews every third of `--lease-ttl-ms`; if renewal reports a takeover, or
keeps failing past the recorded expiry, the process turns not-ready and shuts
down instead of serving stale authority. The event log records `world.started`,
`world.snapshot`, and `world.stopped`. The file-backed store and the memory-only
mode are unchanged and take no lease: without a shared database there is nothing
to fence.

Run a read-only, point-in-time check of the PostgreSQL checkpoint/event pair
before a planned restart or during incident triage:

```text
lynx-world verify-snapshot --world-id demo --zone-id <zone-id>
```

The command does not acquire a lease or modify rows. `VERIFIED` means the
snapshot row matches its pinned checkpoint event and ordered replay;
`UNVERIFIED` means a legacy snapshot has no matching checkpoint event; `ABSENT`
means neither snapshot nor checkpoint exists. Errors (including sequence gaps
and row/event mismatches) return a nonzero exit.

The same command also performs a dry-run zone restore against the replayed
state and prints a `RECOVERY` line plus one `QUARANTINE` line per entry that
would be isolated. It then reads the durable `lynx.recovery_quarantine` report
and prints `DURABLE_QUARANTINE` / `STORED_QUARANTINE` lines. A quarantined entry
is not part of the recovered world; inspect its entity, section, code, and
message before deciding whether to repair or accept the loss. Startup also
writes the report before serving and fails if it cannot be stored.

Startup logs `world recovery completed` with `elapsed_ms`, or
`world recovery exceeded the documented target` when the measured time passes
60 seconds. The reference recovery dataset (1,000 persistent entities, 5,000
upserts, and 400 removals) is replayed by
`recovery_reference_dataset_completes_within_the_documented_target`, which
enforces that target. The ignored
`recovery_production_scale_meets_the_target` check replays 55,001 events five
times; run it with `cargo test -p lynx-world --lib
recovery_production_scale_meets_the_target -- --ignored --nocapture` to record
headroom on the deployment host. Recovery now replays entity, link, cooldown,
transfer-receipt, calendar/faction, action/link idempotency-cache, and counter
deltas. Each checkpoint save deletes earlier events for the zone
under the append-only purge guard, so the retained log is one snapshot interval
and `--snapshot-interval-ms` controls that boundary.

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
| Bounded per-session replication diagnostics | `GET /diagnostics/replication` |

Readiness is stricter than liveness: the current binary becomes ready after the
zone is built or restored and before it accepts connections, and turns not-ready
as soon as shutdown begins. A configured snapshot must load; a failed baseline
write aborts startup only with `--fresh`, otherwise it logs a warning. With
`--state-postgres`, losing the zone lease also turns readiness false and stops
the process. Migration execution, ongoing database health, authored content
loading, and event replay are not readiness checks.

## Metrics and logs

See the [error and operator diagnostic catalog](error-catalog.md) for stable
error codes, retry behavior, and handling guidance. Keep server diagnostics out
of player-facing text and metric labels.

The service emits structured tracing events for authentication denials,
transport rate limits, authority rejections, and principal-session revocation.
The deployment collector still needs to extract and retain these fields as
specified in the security event policy below (`NET-013`, `OPS-013`). Never log
tokens, passwords, or full private chat.

The admin listener serves `/healthz`, `/readyz`, `/metrics`, and
`/diagnostics/replication` independently of the gameplay listener. These
endpoints are unauthenticated and have no TLS; keep the default loopback bind or
restrict a private admin interface with network policy and a trusted TLS proxy.
Replication diagnostics retain at most 256 recently active opaque session IDs;
they include per-session queue and frame/byte counts, but no account, character,
IP, or credential data. Session IDs are not Prometheus labels. Local users able
to connect can read this operational data. The gameplay listener retains the
health, readiness, and metrics routes for compatibility. Do not expose either
listener to untrusted networks without appropriate controls; demo bearer
authentication is not production auth.

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

To verify metrics on an already-running instance whose metrics listener uses a
non-default port, pass its exact endpoint explicitly; this is read-only and
does not change configuration or process state:

```powershell
pwsh -NoProfile -File .\scripts\test-nakama.ps1 `
  -NakamaBin 'D:\Development\MMORPG\Nakama\nakama.exe' `
  -HealthUrl 'http://127.0.0.1:7350' `
  -MetricsUrl 'http://127.0.0.1:7354/metrics'
```

The smoke check never starts, stops, or restarts Nakama. A metrics check after
changing the port requires a separately authorized Nakama restart; the script
only checks the currently running listener. Ubuntu CI runs ShellCheck over all
operations shell scripts, including `scripts/run-nakama.sh`, independently of
the Rust test job.

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
| `lynx_world_zone_lease_held` | `1` while this process holds the PostgreSQL zone lease, `0` otherwise; stays `0` without `--state-postgres` |
| `lynx_world_zone_lease_renewals_total{outcome}` | lease renewal attempts by `success` or `failure`; a takeover or an expiry-driven shutdown counts as a failure |
| `lynx_world_rate_limit_rejections_total{class,code}` | rate-limit rejections by bounded class and error code |
| `lynx_world_actions_applied_total` | newly applied actions; cached idempotent replies are excluded |
| `lynx_world_action_authority_duration_seconds` | histogram of time spent handling action requests inside the authority; not end-to-end client latency. Cumulative buckets: 0.001, 0.01, 0.1, 1, 10, +Inf seconds |
| `lynx_world_resume_failures_total{code}` | failed resume admissions by bounded `ErrorCode`; unknown and expired tokens share the gateway's `session_expired` category |
| `lynx_world_session_attaches_total` / `lynx_world_session_detaches_total` | authority connection attach and detach events, including resume attach |
| `lynx_world_session_expired_closes_total` | connections closed after the attached session disappeared or expired; not disconnected-window expiry sweeps |
| `lynx_world_outbound_queue_depth` / `lynx_world_outbound_queue_capacity` | summed frame depth/capacity sampled during authority work |
| `lynx_world_outbound_queue_depth_high_water` | highest aggregate frame depth observed at authority-work samples during this process |
| `lynx_world_outbound_queue_session_depth_high_water` | highest depth observed in any one connection queue during this process; aggregate only, not labeled by session |
| `lynx_world_replication_enqueued_bytes_total` | encoded snapshot/delta bytes accepted by outbound queues; not confirmed socket delivery bytes |
| `lynx_world_replication_socket_write_frames_total{kind,outcome}` | snapshot/delta frames whose WebSocket send completed or failed; labels are fixed to two kinds and two outcomes |
| `lynx_world_replication_socket_written_bytes_total{kind}` | encoded snapshot/delta bytes for completed local WebSocket sends; not client acknowledgements |
| `lynx_world_ai_ticks_total` / `lynx_world_ai_decisions_total` | enabled AI ticks and evaluated controller decisions |
| `lynx_world_ai_budget_exhaustions_total{budget}` | AI controller-scan, update, perception, action, group-aggro-share, and behavior-step limits using fixed budget labels |
| `lynx_world_path_requests_total`, `lynx_world_path_expansions_total`, `lynx_world_path_failures_total`, `lynx_world_path_budget_exhaustions_total`, `lynx_world_path_partial_results_total`, `lynx_world_path_deferred_requests_total` | cumulative path diagnostics sampled from the zone; the demo GuardChase scenario generates path requests and expansions during normal ticks. |
| `lynx_world_content_version_info{state,version}` | content integration state (`absent`, `configured`, or `loaded`) and version when available. `--content-bundle <PATH>` reports `loaded` only after manifest, engine requirement, definition metadata, locale paths, and file hashes pass; without the option it reports `absent` |
| `lynx_backend_operations_total{adapter}` / `lynx_backend_operation_errors_total{adapter}` | Nakama and PostgreSQL adapter operation counts/errors; emitted in Nakama auth mode with fixed adapter labels. Errors include validation/domain failures |
| `lynx_backend_operation_duration_seconds{adapter}` | backend operation duration including PostgreSQL lock waits and failures; adapter snapshots are individually coherent, not atomic as a group |

Starting alert examples (tune against staging; not tested production rules):
`increase(lynx_world_tick_overruns_total[5m]) > 0`,
`sum(rate(lynx_world_rate_limit_rejections_total[5m])) > 1`,
`increase(lynx_world_snapshot_writes_total{outcome="failure"}[5m]) > 0`, and
`up{job="lynx-world"} == 0` for five minutes. Monitor readiness separately.

For session abuse, separate routine reconnect volume from suspicious failures
and throttling. These examples use only bounded metric labels and should alert
on sustained patterns; tune their windows and thresholds with staging traffic:

```promql
# Repeated invalid/expired resume attempts, excluding successful reconnects.
sum(increase(lynx_world_resume_failures_total[5m])) > 20

# Resume-token guessing is reaching the independent per-IP attempt budget.
sum(increase(lynx_world_rate_limit_rejections_total{class="resume_attempt"}[5m])) > 0

# Failed identity credentials are being throttled.
sum(increase(lynx_world_rate_limit_rejections_total{class="failed_auth"}[5m])) > 0
```

The resume failure counter is authority-side and excludes failures rejected
before authority admission. Rate-limit counters cover those transport-level
rejections. Neither metric contains IP addresses or session identifiers.

### Dashboards, alert rules, and SLOs

Grafana provisioning files and the versioned world dashboard are in
[`deploy/grafana`](../deploy/grafana). They use the `lynx-prometheus`
datasource UID and provision without dashboard edits. Prometheus rules are in
[`deploy/prometheus/lynx-world.rules.yml`](../deploy/prometheus/lynx-world.rules.yml);
every alert carries a runbook link and uses a pending interval to avoid
page-worthy transitions during brief restarts. Counter `increase()` handles
process counter resets. `LynxWorldMetricsTargetUnavailable` handles sustained
scrape failure separately. These rules still need promtool validation and a
staging exercise before operators rely on their thresholds.

| Alert | Initial threshold | Response |
| --- | --- | --- |
| `LynxWorldTickDeadlinesMissed` | Any deadline miss in 5 minutes, sustained for 2 minutes | [Tick deadline misses](#tick-deadline-misses) |
| `LynxWorldOutboundQueueNearCapacity` | Process high-water reaches 90% of capacity for 5 minutes | [Replication queue pressure](#replication-queue-pressure) |
| `LynxWorldSessionExpirySpike` | 10 expiry closes in 10 minutes, sustained for 2 minutes | [Session expiry](#session-expiry) |
| `LynxWorldAuthenticationRejections` | Any failed-auth throttle or 10 failed resumes in 5 minutes | [Authentication and resume failures](#authentication-and-resume-failures) |
| `LynxWorldPostgresOperationErrors` | Any PostgreSQL adapter error in 5 minutes, sustained for 2 minutes | [PostgreSQL operation errors](#postgresql-operation-errors) |
| `LynxWorldBackupFailure` | External backup exporter reports a failure in 15 minutes | [Backup failures](#backup-failures) |
| `LynxWorldMetricsTargetUnavailable` | Scrape target down for 5 minutes | [Metrics target unavailable](#metrics-target-unavailable) |

The backup alert requires the deployment backup job to expose the counter
`lynx_backup_failures_total`; Lynx does not currently run or instrument backup
jobs. The session alert observes attached connections closed after session
expiry, not retained-window expiry sweeps. PostgreSQL adapter metrics exist only
in Nakama authentication mode.

Proposed initial service objectives are 99.9% monthly Prometheus scrape
availability for the world metrics target, fewer than 0.1% scheduled ticks
missing their deadline in each rolling 30-day window, and recovery from a
durable checkpoint within 60 seconds. Scrape availability is an operations
proxy, not player-visible availability; current tick telemetry has no duration
histogram, so the deadline objective is the available latency guardrail. The
60-second recovery objective is exercised by the reference replay test. These
objectives and alert thresholds need staging review against the chosen topology
before becoming production SLOs.

#### Tick deadline misses

Check scheduler load and tick duration logs, then compare the alert window with
deployments and host CPU pressure. Execution overruns and scheduler deadline
misses are distinct metrics.

#### Replication queue pressure

Inspect slow or disconnected peers and outbound socket write outcomes. The
high-water metric is process-local and aggregate; it does not identify a
session. Do not raise queue limits without measuring memory and bandwidth.

#### Session expiry

Check reconnect rates, network interruptions, and resume TTL configuration.
This counter records closes of attached sessions that disappeared or expired;
it does not count every disconnected resume window that expires.

#### Authentication and resume failures

Review bounded rate-limit classes, stable error codes, and the correlated
security audit events. Treat sudden failed-credential throttling as possible
abuse; do not log or copy submitted tokens into incident notes.

#### PostgreSQL operation errors

Check database availability, migration state, pool connectivity, and the
restricted operator logs. Adapter errors combine domain/validation and
infrastructure failures; this signal does not identify a failing query.

#### Backup failures

Check the external backup job's exit status, checksum, destination access, and
last known good backup. Preserve the last verified backup while investigating.
The current repository has no backup exporter or scheduled backup job.

#### Metrics target unavailable

Check the process health and readiness endpoints, deployment/restart timeline,
admin listener bind, and Prometheus target configuration. A scrape outage is
not by itself proof that gameplay is unavailable.

### Security event logs

The world process emits structured tracing fields to stdout. Preserve these
event names and fields in the deployment's log collector:

| Event | Safe correlation fields |
| --- | --- |
| `world_admission_denied` | `source_ip`, stable `reason`, and selected `character` when known |
| `world_resume_admission_denied` | `source_ip` and stable `reason` |
| `world_rate_limit_rejected` | `source_ip`, fixed `class` (`handshake`, `failed_auth`, `resume_attempt`, `connection_churn`, or an authenticated message family), and stable `reason`; `session` after admission |
| `world_authority_rejected` | `source_ip`, server-issued `session`, server-resolved `actor`, request `correlation_id`, and stable `reason` |
| `world_action_processed` | `service`, `zone`, `connection`, server-issued `session` and `actor`, `message_kind`, wire `correlation_id`, optional `parent_correlation_id`, `outcome`, `duration_ms`, and stable `reason` on non-applied outcomes |
| `world_principal_sessions_revoked` | server-verified `principal` and count of revoked sessions |
| `world session detached` / `world session expired` | `connection` and `session` |

Do not add bearer tokens, resume tokens, request bodies, chat text, or backend
error details to these records. IP addresses and stable player/session IDs are
sensitive operational data: restrict log access to incident responders and
security operators, encrypt collector transport and storage, and retain
searchable records for 90 days before purge. The world process writes stdout
only; it does not provide a durable log store or collector. Deployments must
configure the collector, field extraction, retention, and access controls.

`world_action_processed` uses stable field names on both applied and non-applied
actions. Applied actions are emitted at `info`; rejected, failed, unknown, and
no-result actions are emitted at `warn` and include a stable reason code where
available. These events are not sampled. The action body and private fields are
never logged. For durable lookup, `zone.action-results.replace` events store the
bounded `action_correlations` object beside `action_results`; its keys are
simulation idempotency request IDs and its values are the wire correlation IDs.
Older readers ignore this additive JSON field and still replay the action
results. Find the durable request ID for a wire ID, then inspect that result in
the same event payload:

```sql
SELECT event.sequence, event.tick, event.kind, mapping.key AS request_id,
       event.payload->'action_results' AS action_results
FROM lynx.zone_events AS event
CROSS JOIN LATERAL jsonb_each_text(
    COALESCE(event.payload->'action_correlations', '{}'::jsonb)
) AS mapping(key, correlation_id)
WHERE event.kind = 'zone.action-results.replace'
  AND mapping.correlation_id = 'trace-action-41'
ORDER BY event.sequence;
```

Use the service's structured stdout logs for latency investigations. Lynx does
not currently export distributed traces; retain `correlation_id` and
`parent_correlation_id` in the collector so log searches can join wire,
authority, and durable event records.

The PromQL rules above are candidate alerts, not exercised production rules.
Validate their thresholds against staging traffic, confirm alert routing, and
record false-positive tuning before relying on them for incident response.

Unsupported or incomplete requested families (do not interpret their absence as zero):

- Tick duration distributions.
- A separate count of disconnected reconnect windows and resume-window expiry sweeps.
- End-to-end action latency and action rejections before authority delivery (authority latency, applied actions, and transport rate limits are measured separately).
- Per-session replication delivery counts and bytes; queue depth is currently
  exposed only as an aggregate maximum to avoid unbounded session-ID labels.
- Pathfinding expansion, failure, partial-result, and budget counters are sampled from zone diagnostics. The demo GuardChase runtime exercises `Zone::find_path`; these counters represent calls actually made by the current runtime.
- AI error counters; current AI diagnostics cover decisions and bounded-work limits only.
- Backend metrics are emitted only in Nakama auth mode and combine database domain/validation errors with infrastructure errors. There are no per-query or pool metrics.
- Snapshot load failures; write metrics above do not identify the store or error cause and do not include snapshot capture time.
- Localization fallback/missing-key counts are unsupported by the current world service: it sends typed message keys and does not resolve localized catalogs. `CatalogSet::resolve_with_diagnostics` remains opt-in for a future server-side catalog consumer; no localization metric is emitted.

## Player inventory value ledger

Migration `0016_player_inventory_value_ledger.sql` establishes an opening
balance for existing player inventories, then records every trusted inventory
snapshot replacement and idempotent transfer as signed per-item quantities.
For transfers, both owners' entries and the inventory update commit in one
transaction. This ledger does not assign prices; no player currency balance is
defined yet (ADR 0032).

Operators can investigate movements by account, character, correlation ID, and
time range. Run this query using the restricted operator database role:

```sql
SELECT event.occurred_at, account.account_id, entry.owner_character_id,
       entry.item_definition, entry.quantity_delta, entry.before_quantity,
       entry.after_quantity, event.source, event.actor_kind, event.actor_id,
       event.reason, event.correlation_id
FROM lynx.inventory_value_entries AS entry
JOIN lynx.inventory_value_events AS event USING (event_key)
JOIN lynx.characters AS character
  ON character.character_id = entry.owner_character_id
JOIN lynx.accounts AS account ON account.account_id = character.account_id
WHERE account.account_id = 'account-id-or-null'
  AND entry.owner_character_id = 'chr-id-or-null'
  AND event.correlation_id = 'correlation-id-or-null'
  AND event.occurred_at >= '2026-11-01T00:00:00Z'
  AND event.occurred_at <  '2026-12-01T00:00:00Z'
ORDER BY event.occurred_at, entry.entry_id;
```

Replace any `*-or-null` filter with `TRUE` when that dimension is not needed.
The query intentionally omits Nakama user IDs and character names. Reconcile a
character through `PostgresInventoryStore::reconcile_character`; its `differences`
list reports current per-item quantities that do not equal opening balances
plus signed ledger entries. Ledger rows reject updates and deletes outside the
documented append-only retention path.

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
- A client that cannot drain its bounded outbound queue (configured by
  `--outbound-queue`, 8–256 frames, default 64) is detached
  instead of slowing the authoritative tick. Overflow does not coalesce or
  replay queued deltas in place. The client must reconnect/resume and replace
  replicated state from the authoritative snapshot sent on admission before
  applying later deltas. Alert on repeated detaches; they usually mean a stuck
  client or a network path with insufficient throughput.
- A zone restart invalidates in-memory sessions. Announce expected restarts and
  expect players to log in again until durable session storage exists.
- Debugging a stuck character starts with its session: connected vs retained,
  principal, actor, and last acknowledged tick. Do not "fix" it by granting a
  client-supplied actor or session ID.

## Backups and recovery

- Nightly full PostgreSQL backup plus continuous WAL archiving where supported.
- Backups are encrypted, access-controlled, and stored outside the host.
- Retain daily full backups and their required WAL chain for at least 35 days;
  delete only after a newer backup is checksummed and restore-verified.
- Restore drill monthly and before destructive migrations.
- Recovery objectives for durable character, inventory, faction/calendar, audit,
  and world-snapshot data: RPO 15 minutes with continuous WAL archiving enabled;
  RTO 1 hour to restore service. Zone recovery after a process restart remains
  within 60 seconds from snapshot plus event log.
- Retain audit records according to policy; never let retention delete records
  under active moderation review.

### Automated scratch restore smoke

CI runs a bounded logical-backup restore smoke on its disposable PostgreSQL 17
service. To repeat it locally on Linux, macOS, or WSL, first migrate an isolated
loopback database named `lynx_backup_test_*` (or use the CI-only name `lynx_ci`),
build `lynx-world`, and run:

```bash
LYNX_PG_DATABASE=lynx_backup_test_local \
LYNX_BACKUP_TEST_DATABASE=lynx_backup_test_local \
bash scripts/backup-restore-smoke.sh target/debug/lynx-world
```

The script refuses other database names and non-loopback hosts. It writes a
non-secret character and audit marker, starts the service once to create a real
world snapshot, takes a custom-format dump and checksum in a private temporary
directory, restores only to a uniquely named `lynx_restore_check_*` database,
compares migration/data/trigger continuity, exercises the restored audit
append-only trigger, verifies the snapshot/event checkpoint, and starts the
service against the restored database before dropping the scratch database.
CI treats any failure as a failed job. This is a smoke test, not an encrypted,
off-host production backup; it must never be pointed at production data.

### Manual logical restore drill (PostgreSQL)

This operator drill makes a custom-format logical dump of the configured Lynx
database, restores it into a newly-created scratch database, and checks that
representative durable data and the append-only audit trigger are present. Run it
from a trusted operator host with PostgreSQL client tools (`pg_dump`, `pg_restore`,
`createdb`, `dropdb`, and `psql`) installed. Set `LYNX_PG_*` as for the service
(or populate the repository-root `.env`); for `pg_dump`/`psql`,
`LYNX_PG_PASSWORD` is consumed through `PGPASSWORD`, not a command-line argument.
Database creation and cleanup use the configured libpq superuser authentication
(`LYNX_PG_SUPERUSER`, default `postgres`), as in `setup-postgres.*`. Do not run
this against a production host without an approved change window and
scratch-database capacity.

PowerShell example (each invocation is explicit; the generated database name is
restricted to the `lynx_restore_check_` prefix):

```powershell
$ErrorActionPreference = 'Stop'
$scratch = 'lynx_restore_check_' + [guid]::NewGuid().ToString('N')
$maintenance = if ($env:LYNX_PG_MAINTENANCE_DB) { $env:LYNX_PG_MAINTENANCE_DB } else { 'postgres' }
$archive = Join-Path (Resolve-Path .) ('backups/lynx-' + (Get-Date -Format 'yyyyMMdd-HHmmss') + '.dump')
New-Item -ItemType Directory -Force (Split-Path $archive) | Out-Null
$env:PGPASSWORD = $env:LYNX_PG_PASSWORD
pg_dump --host $env:LYNX_PG_HOST --port $env:LYNX_PG_PORT --username $env:LYNX_PG_USER --dbname $env:LYNX_PG_DATABASE --format custom --file $archive --verbose
if ($LASTEXITCODE -ne 0) { throw 'pg_dump failed' }
Remove-Item Env:PGPASSWORD -ErrorAction SilentlyContinue
try {
    createdb --host $env:LYNX_PG_HOST --port $env:LYNX_PG_PORT --username $env:LYNX_PG_SUPERUSER --maintenance-db $maintenance --owner $env:LYNX_PG_USER --template template0 $scratch
    if ($LASTEXITCODE -ne 0) { throw 'createdb failed' }
    $env:PGPASSWORD = $env:LYNX_PG_PASSWORD
    pg_restore --host $env:LYNX_PG_HOST --port $env:LYNX_PG_PORT --username $env:LYNX_PG_USER --dbname $scratch --no-owner --no-privileges --exit-on-error --verbose $archive
    if ($LASTEXITCODE -ne 0) { throw 'pg_restore failed' }
    $env:PGPASSWORD = $env:LYNX_PG_PASSWORD
    psql --host $env:LYNX_PG_HOST --port $env:LYNX_PG_PORT --username $env:LYNX_PG_USER --dbname $scratch --no-psqlrc --set ON_ERROR_STOP=1 --command "SELECT 'characters=' || count(*) FROM lynx.characters; SELECT 'audit_events=' || count(*) FROM lynx.audit_events; SELECT 'world_snapshots=' || count(*) FROM lynx.world_snapshots; SELECT 'audit_append_only_trigger=' || count(*) FROM pg_trigger WHERE tgrelid = 'lynx.audit_events'::regclass AND tgname = 'audit_events_append_only' AND NOT tgisinternal;"
    if ($LASTEXITCODE -ne 0) { throw 'restore verification query failed' }
} finally {
    # Drop only the uniquely generated scratch DB, never LYNX_PG_DATABASE.
    Remove-Item Env:PGPASSWORD -ErrorAction SilentlyContinue
    dropdb --host $env:LYNX_PG_HOST --port $env:LYNX_PG_PORT --username $env:LYNX_PG_SUPERUSER --maintenance-db $maintenance --if-exists $scratch
    if ($LASTEXITCODE -ne 0) { Write-Warning "Could not remove scratch database '$scratch'; remove it manually after confirming its name." }
}
```

The displayed row counts are evidence to record and compare with the source
before the drill; they are not expected to be nonzero in an empty development
database. Require `audit_append_only_trigger=1`. Repeat that query for every
append-only table introduced by later migrations, and test the application
against restored staging data before declaring a production recovery drill
passed. Never use `--clean` or point restore tools at the configured source
database. Keep the archive encrypted with restrictive filesystem permissions,
copy it off-host before counting it as a backup, and retain a checksum alongside
it. A failed dump/restore/query is a failed drill; the command exits nonzero
except that cleanup failure emits a warning and needs operator follow-up.

This is a manual logical-dump procedure, not a scheduled backup or a verified
production recovery: WAL archiving, encrypted off-host copies, retention
enforcement, alert routing, restore duration, and the stated RPO/RTO still require
an exercised target-environment policy. The world snapshot is in
`lynx.world_snapshots` when `--state-postgres` is used; for `--state-file`, protect
and copy that JSON file separately. The existing world-service readiness
endpoint is not a database or character/audit continuity check.

## Deploy and rollback

1. Build and test the exact artifact. Review README progress, relevant docs, and
   the backlog gap register against the release (`OPS-014`). CI checks README
   crate/ADR/document counts and local Markdown links; the tagged release
   workflow requires a versioned changelog and migration entry. Run
   `python scripts/test_check_markdown_links.py` and
   `python scripts/check_markdown_links.py`; manually review links outside the
   checker's documented coverage in `QUA-009`.
2. Apply forward-compatible migrations.
3. Deploy to staging; run smoke and load tests.
4. Publish content as an immutable versioned bundle, including validated
   localization catalogs for every declared locale.
5. Roll out to one zone, monitor errors and tick health.
6. Promote to remaining zones.
7. Roll back the content bundle and its matching snapshot version together;
   startup rejects a mismatched pair. Roll back code if the fault is in the
   runtime. Definition-level data migration and automated pair restoration are
   still required before content rollback is a complete recovery procedure.

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
