# Local 16-client movement sample — 2026-11-12

This one-minute release run exercised the local movement workload at twice the
client count of the previous sample. All clients and the authority ran in one
process on localhost. It does not represent deployment network overhead,
database latency, or target-scale capacity.

## Command

```powershell
cargo run --release -p lynx_load_harness -- --clients 16 --duration-seconds 60 --operations-per-second 100 --scenario movement --sample-interval-seconds 5 --samples-csv target/soak-smoke/movement-16c-100ops-60s-samples.csv
```

## Summary

```csv
scenario,clients,duration_s,target_ops_per_s,offered_ops,accepted_ops,queue_rejections,pending_replies,reply_samples,achieved_ops_per_s,outbound_envelopes,inbound_envelopes,outbound_bytes,inbound_bytes,reply_p50_ns,reply_p95_ns,reply_p99_ns,ticks,tick_failures,tick_duration_s,tick_mean_ms,tick_overruns,server_queue_full_events,queue_depth_high_water_frames,session_queue_depth_high_water_frames,queue_capacity_total_frames,session_queue_capacity_frames,replication_enqueued_bytes,snapshot_socket_write_successes,snapshot_socket_write_failures,delta_socket_write_successes,delta_socket_write_failures,snapshot_socket_written_bytes,delta_socket_written_bytes,rss_start_bytes,rss_peak_bytes,rss_end_bytes,rss_growth_bytes,first_window_reply_samples,first_window_p95_ns,last_window_reply_samples,last_window_p95_ns
movement,16,60.007,100,6000,6000,0,2,5998,99.99,6000,12043,824544,3444792,30069100,31291000,32876600,991,0,0.539782500,0.5447,0,0,8,3,1024,64,2717920,0,0,5997,0,0,2717920,21594112,40280064,40280064,18685952,498,31004700,493,31367900
```

The run accepted all 6,000 offered operations without queue rejection, tick
failure, tick overrun, or server queue-full event. Mean tick work was 0.5447 ms;
reply p95 was 31.29 ms overall and 31.00/31.37 ms in the first/last five-second
windows. Delta socket writes completed for 5,997 frames totaling 2,717,920
bytes, with no write failures. Combined server/client RSS grew by 17.8 MiB and
did not visibly plateau in one minute. These observations show no local
authority or transport pressure at this workload; they neither isolate memory
ownership nor establish a production bottleneck or regression budget.

Per-window RSS and reply p95 samples were written to
`target/soak-smoke/movement-16c-100ops-60s-samples.csv` for this local run.
