# Contributing to Lynx

## Ground rules

- Keep the workspace building and tested at every commit.
- Prefer small, focused commits with a clear subject line.
- Never commit credentials, tokens, database dumps, or generated build output.
- Record architecture-changing decisions as an ADR in `docs/adr/` before or
  alongside the change.
- Treat content, network input, and AI tool output as untrusted data.

## Development setup

```bash
rustup show                     # should select the pinned 1.99.0 toolchain
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

Tests must not require Docker, a running Nakama server, or a network
connection. Integration tests that need external services belong behind an
explicit `--ignored` test or a script in `scripts/`, and must document their
prerequisites.

## Code style

- `rustfmt` with the repository configuration; CI enforces `cargo fmt --check`.
- Clippy runs with `-D warnings`.
- `unsafe` is forbidden at workspace level. If a dependency requires it, keep
  the unsafe boundary in that dependency and document why.
- Domain code in `engine_core` and `engine_sim` must stay platform-neutral and
  must not depend on Bevy, a windowing system, or a database driver.
- Public types are `Debug`; errors carry a stable machine-readable code plus a
  human-readable message.

## Authority and validation

- Every new gameplay action goes through the server action pipeline. Add tests
  proving that invalid distance, permission, state, and cooldown requests are
  rejected.
- New persistent state needs a schema version, a persistence policy, a
  migration path, and a recovery story.
- New content types need schema validation plus a fixture used by tests.

## Tests

- Unit tests live next to the code they test.
- Cross-crate behavior belongs in `tests/` in the crate that owns the
  behavior.
- Prefer deterministic tests: inject IDs, ticks, and clocks rather than reading
  the system clock.
- Property-style checks are welcome for invariants such as item conservation.

## Commit and review checklist

1. `cargo fmt --all`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace`
4. Documentation updated when behavior or setup changes
5. ADR added/updated when a boundary or version policy changes
