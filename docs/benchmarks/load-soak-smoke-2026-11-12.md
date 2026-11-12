# Load and replication sampler smoke — 2026-11-12

This five-second run verifies that `lynx-load` reports per-run tick counters,
queue state, replication enqueue totals, socket write counters, and per-window
RSS/reply latency. It is a wiring smoke check, not a performance baseline or a
leak-detection soak.

## Command

```powershell
cargo run --release -p lynx_load_harness -- --clients 2 --duration-seconds 5 --operations-per-second 10 --scenario movement --sample-interval-seconds 1 --samples-csv target/soak-smoke/replication-movement-samples.csv
```

## Summary output

```csv
scenario,clients,duration_s,target_ops_per_s,offered_ops,accepted_ops,queue_rejections,pending_replies,reply_samples,achieved_ops_per_s,outbound_envelopes,inbound_envelopes,outbound_bytes,inbound_bytes,reply_p50_ns,reply_p95_ns,reply_p99_ns,ticks,tick_failures,tick_duration_s,tick_mean_ms,tick_overruns,server_queue_full_events,queue_depth_high_water_frames,session_queue_depth_high_water_frames,queue_capacity_total_frames,session_queue_capacity_frames,replication_enqueued_bytes,snapshot_socket_write_successes,snapshot_socket_write_failures,delta_socket_write_successes,delta_socket_write_failures,snapshot_socket_written_bytes,delta_socket_written_bytes,rss_start_bytes,rss_peak_bytes,rss_end_bytes,rss_growth_bytes,first_window_reply_samples,first_window_p95_ns,last_window_reply_samples,last_window_p95_ns
movement,2,5.001,10,50,50,0,0,50,10.00,50,106,6714,35555,30442200,31842500,44899800,83,0,0.011347700,0.1367,0,0,3,3,128,64,23660,0,0,50,0,0,23660,13918208,14868480,14868480,950272,10,30563000,9,30536700
```

## Per-window samples

```csv
elapsed_ms,rss_bytes,reply_samples,reply_p95_ns
0,13918208,0,0
1013,14483456,10,30563000
2026,14557184,10,44899800
3038,14647296,11,32115600
4052,14761984,10,31715800
5000,14868480,9,30536700
```

The Windows release run used an in-process world and two headless clients.
RSS includes both server and client allocations. It grew by 0.9 MiB across the
sampled interval, which is too short to distinguish normal allocator behavior
from a leak. The run observed 50 successful delta writes (23,660 bytes), queue
high-water 3/64 frames per session, zero full-queue events, and 0.1367 ms mean
tick work. It establishes that the shared metrics path works under light load;
it does not identify a bottleneck or establish a capacity limit.

See the [recorded local baseline](load-baseline-2026-11-12.md) for the machine
and release profile. Application and replication byte counts exclude
WebSocket framing, TLS, and TCP/IP overhead.
