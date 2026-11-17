# 3D client frame pacing — 2026-11-12

Measured the local online demo on Windows using the NVIDIA GeForce RTX 3060
Vulkan adapter. The display reports 120 Hz. Scene geometry, shadows, camera,
default window size, and presentation settings were unchanged between runs.
Each run excluded five seconds of startup and sampled ten seconds of real
frame intervals while connected to the local world server. The scene was idle.

| Configuration | Sampled frames | Mean FPS | Mean frame time | p95 frame time |
| --- | ---: | ---: | ---: | ---: |
| Original unoptimized debug build, Bevy multithreading disabled | 251 | 25.08 | 39.88 ms | 47.65 ms |
| Optimized `demo` profile, Bevy multithreading enabled | 1200 | 120.00 | 8.33 ms | 8.99 ms |
| Same optimized configuration, repeat | 1200 | 120.01 | 8.33 ms | 8.94 ms |

The original configuration reproduced the reported 25 FPS. The optimized
configuration sustains the display's 120 Hz pacing. These are VSync/presentation
paced measurements, **not uncapped renderer throughput**. They do not establish
maximum scene capacity, performance while moving, or results on other machines.
The build and threading changes were measured together; this comparison does
not attribute a separate improvement to each.

## Changes

- The `demo` Cargo profile optimizes workspace code at level 2 and dependency
  code at level 3, retaining development checks without release LTO.
- The client enables Bevy's `multi_threaded` feature, including its parallel
  scheduler and supported render pipelining.
- The demo launcher builds and runs `target/demo` by default, with explicit
  `debug` and `release` alternatives.
- The client displays smoothed FPS and frame time, refreshed twice per second.

## Reproduce

```powershell
BAT\run-demo-3d.bat -BenchmarkSeconds 10
```

This builds the selected profile, starts a disposable loopback world, runs the
measurement, prints a `FRAME_BENCHMARK` summary, exits the client, and cleans up
its server. Logs remain under `%TEMP%/lynx-3d-client-*.stderr.log`.

Use `-Profile debug` or `-Profile release` for future comparisons. The current
debug configuration also enables multithreading; the original serial baseline
above was captured before enabling it. Ordinary interactive launches omit
`-BenchmarkSeconds` and do not exit automatically.
