# Platforms

The supported matrix is deliberately small. A target is listed as supported only
after its build, tests, and smoke test run in CI.

| Target | Status | Notes |
|---|---|---|
| Windows 10/11 x86-64 | **Development supported** | Primary developer platform; native Nakama and PostgreSQL releases available |
| Linux x86-64 | Build supported | Tests run headlessly; Nakama release available |
| macOS x86-64 / arm64 | Build supported | Nakama release available; not yet smoke-tested in CI |
| WebAssembly / browser | Not supported | Needs storage, threading, and asset-loading design |
| Mobile / handheld | Not supported | Needs input, memory, and UI work |
| Consoles | Not supported | No plan before desktop is stable |

## Engine vs. client vs. server

- **Engine domain crates** are platform-neutral Rust. They build for every
  tier-1 Rust target and are tested on the host.
- **Server binaries** are headless and must build without a GPU or window
  system. Server deployments use Linux by default; Windows is supported for
  development.
- **Clients** require Bevy's supported graphics backends. The client matrix
  follows the earliest of: Bevy support, our smoke tests, and our ability to
  package and update the build.
- **Admin tools** target desktop platforms only.

## Minimum hardware (initial hypothesis)

These are budgets to measure, not promises:

- Client: 4 logical cores, 8 GB RAM, GPU with Vulkan 1.2 / D3D12 / Metal
  support, 1280×720.
- Server: 2 vCPU, 2 GB RAM per world process for a small zone, plus PostgreSQL.
- Network: 100 ms RTT playable for movement; 250 ms treated as degraded.

## Browser considerations (future)

Browser support needs decisions on asset loading (no filesystem), storage
(IndexedDB vs. server-only), threading (SharedArrayBuffer and COOP/COEP),
audio, and transport (WebSocket only). Do not add browser support by feature-
gating renderer code alone.

## Platform release gates

Every supported platform must pass:

1. `cargo build --workspace` and `cargo test --workspace`.
2. Client smoke test: launch, connect, render one zone, move, interact, quit.
3. Content bundle load with version and hash verification.
4. Graceful handling of server disconnect and reconnect.
5. Package, install, and uninstall without leftovers.

Until then, the platform is experimental and the README must say so.
