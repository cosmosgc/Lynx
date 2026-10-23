# ADR 0001: Rust and Bevy version policy

- **Status:** Accepted
- **Date:** 2026-10-23

## Context

Lynx needs a stable, reproducible build across contributors and CI. Bevy evolves
quickly, and engine-internal APIs are not a compatibility surface we can freeze
for game projects.

## Decision

- Pin a single Rust toolchain in `rust-toolchain.toml`. The current pin is
  **1.99.0**, edition 2024.
- Pin Bevy in `[workspace.dependencies]`. The current target is **Bevy 0.20**.
  Bevy is introduced only in client/editor crates, never in `engine_core`,
  `engine_sim`, `engine_content`, or `game_api`.
- `engine_core` and `engine_sim` compile without Bevy, a window, a GPU, or a
  database. They are the compatibility surface game projects build against.
- Engine releases use semantic versioning. A Bevy minor upgrade is an engine
  minor release at minimum and requires a migration note in `CHANGELOG.md`.
- Game projects declare the engine version they target in their manifest; the
  CLI refuses to scaffold a project against an incompatible engine version.
- Upgrade Bevy in a dedicated commit with `cargo test --workspace` and a visual
  smoke test, never mixed with gameplay changes.

## Consequences

- Contributors install one toolchain; CI and local builds match.
- Engine domain code is testable headlessly and quickly.
- Rendering features cannot leak into simulation logic without an explicit
  boundary review.
- Bevy upgrades require a planned migration window rather than silent drift.
