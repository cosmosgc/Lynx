# `lynx-load`

Run a repeatable local load against the real world service composition and the
production headless WebSocket client:

```text
cargo run --release -p lynx_load_harness -- --clients 8 --duration-seconds 30 --operations-per-second 100 --scenario movement
```

The demo world supports up to 32 generated static-auth clients. Scenarios are
`idle`, `movement`, `chat`, `action`, and `reconnect`. The command prints one
CSV row with offered operations, observed reply samples, operation rate,
application-envelope counts and bytes, p50/p95/p99 response latency, and
authority tick/failure counters and duration. It also reports queue-full events,
queue high-water marks/capacities, replication enqueue bytes, and snapshot/delta
WebSocket write counts and completed bytes. The test harness shares the world's
metrics registry with its WebSocket transport so these transport values describe
the measured run. The harness reports its process resident memory at start,
peak, and end, plus first/last
non-empty p95 response-latency windows. `--sample-interval-seconds` controls
the default five-second sampling interval. Pass `--samples-csv <path>` to write
the per-window resident-memory and latency series. Memory includes both the
in-process server and its clients; it is not a server-only measurement.
Application and replication bytes exclude WebSocket framing, TLS, and TCP/IP
overhead. Actions use the player's own entity and can be rejected by game rules;
the benchmark measures the server path, not successful
gameplay outcomes.

The harness records samples for runs up to one hour, which can reveal resident
memory growth and response-latency drift across the run. It does not inject
database latency or enforce a regression budget. Use `engine_sim`'s tick
benchmark for isolated simulation latency; use `lynx-load` to include the
WebSocket transport and multiple authenticated sessions.

See the [recorded local baseline](../../docs/benchmarks/load-baseline-2026-11-12.md)
and the [nightly/manual workflow](../../.github/workflows/load-benchmark.yml)
for scenario artifacts from the hosted Ubuntu runner.
