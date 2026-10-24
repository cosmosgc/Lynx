# Lynx

A lightweight, data-driven multiplayer game engine and reference MMORPG built
with Rust, Bevy, and Heroic Labs Nakama.

Lynx is two deliverables in one repository:

1. **The engine/toolkit** — authoritative simulation, content pipeline,
   networking adapters, UI and camera modules, world editor, and operations
   tooling.
2. **The reference game** — a small, playable MMORPG that exercises the engine
   and keeps its architecture honest.

The project guide lives at
[`docs/lightweight_mmorpg_engine_project_guide.md`](docs/lightweight_mmorpg_engine_project_guide.md).
Architecture decisions are recorded in [`docs/adr/`](docs/adr/), and the
localization model is described in
[`docs/internationalization.md`](docs/internationalization.md).

## Status

Early implementation. The repository currently contains the Phase 0/1
foundation: pinned toolchain, workspace layout, architecture decision records,
stable entity IDs, versioned component schemas, faction and inventory domain
rules, a headless authoritative simulation with action validation, and the
`lynx` project CLI. Bevy client crates arrive in the next phase.

Brazilian Portuguese (`pt-BR`) is the default language for generated projects,
templates, and player-facing content. See
[`docs/internationalization.md`](docs/internationalization.md).

Nothing in this repository should be treated as production-ready yet. See
[`docs/platforms.md`](docs/platforms.md) for the supported matrix, which is
intentionally small until each target is tested in CI.

## Quick start

Prerequisites: Rust 1.99 (installed automatically by `rustup` through
`rust-toolchain.toml`). No Docker is required at any point.

```bash
# Build and test the engine workspace
cargo test --workspace

# Create and check a new game project
cargo run -p project_cli -- new my-world --template minimal --output ../my-world
cargo run -p project_cli -- doctor --project ../my-world
```

The CLI is also exposed as `lynx` when installed with
`cargo install --path tools/project_cli`.

Generated projects include a Brazilian Portuguese catalog and default to
`pt-BR`; additional locales are opt-in translations.

Nakama and PostgreSQL are optional for engine development and required only
for multiplayer work. The native, non-Docker setup is documented in
[`docs/getting-started.md`](docs/getting-started.md).

## Repository layout

```text
crates/engine_core      Stable IDs, schemas, factions, inventory, audit, errors
crates/engine_sim       Headless authoritative zone simulation and action pipeline
crates/engine_content   Content definitions, manifests, validation, versioning
crates/engine_net       Protocol envelopes, replication categories, adapter traits
crates/backend_nakama   Native Nakama HTTP adapter (no Docker required)
crates/game_api         Backend service interfaces and gameplay DTOs
tools/project_cli       `lynx` project scaffolding, doctor, and validation
server/                 Native Nakama config, SQL migrations, run scripts
content/                Canonical schemas and example content
templates/              Generated-project templates
docs/                   Architecture, operations, security, localization, ADRs
```

## Design rules

- The server is authoritative; clients send requests, never outcomes.
- Gameplay is composed from versioned data, not inheritance trees.
- Every persistent entity has a stable ID and an explicit persistence policy.
- Every entity may have a mind; a player is one controller among many.
- Content and protocol versions are explicit and checked at boundaries.
- Player-facing text is localized; Brazilian Portuguese is the default language.
- Tools, diagnostics, and recovery paths are part of the product.
- Docker is optional. Native operation is the supported baseline.

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md). Commit early and often; keep the
workspace building and tested at every commit.

## License

MIT. See [`LICENSE`](LICENSE).
