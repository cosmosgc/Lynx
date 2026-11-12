# Local load baseline — 2026-11-12

This is a repeatability reference for the in-process world service and headless
WebSocket clients. The service and clients run on the same host; this does not
represent a deployed topology or include WebSocket framing, TLS, or TCP/IP
overhead. Database latency is not injected.

## Environment

- OS: Windows 11 Home, build 26300, x64
- CPU: AMD Ryzen 7 5700G with Radeon Graphics, 8 cores / 16 logical processors
- Installed memory: 31.9 GiB
- Rust: `rustc 1.99.0 (b940084d7 2026-09-28)`
- Cargo: `cargo 1.99.0 (5f94df478 2026-08-27)`
- Profile: release (`lto = "thin"`, one codegen unit)
- Source revision: `40339cb` (working tree contained uncommitted changes)

## Command and raw output

```powershell
cargo run --release -p lynx_load_harness -- --clients 8 --duration-seconds 10 --operations-per-second 100 --scenario movement
```

```csv
scenario,clients,duration_s,target_ops_per_s,offered_ops,accepted_ops,queue_rejections,pending_replies,reply_samples,achieved_ops_per_s,outbound_envelopes,inbound_envelopes,outbound_bytes,inbound_bytes,reply_p50_ns,reply_p95_ns,reply_p99_ns,ticks,tick_failures
movement,8,10.006,100,1000,1000,0,2,998,99.94,1000,2014,135772,586436,30125800,31073200,31540100,164,0
```

The configured dispatch rate was met within the run's wall-clock timing. There
were no queue rejections or authority tick failures. Two replies remained
pending when sampling stopped; 998 reply latencies were measured. This baseline
is descriptive only and does not define a regression budget. For scheduled
comparisons, the [nightly/manual load workflow](../../.github/workflows/load-benchmark.yml)
uploads five scenario rows and runner metadata as a 90-day artifact.
