# CLT-001 — Client workspace and crate layout

**Status: Done.** All acceptance criteria below are checked complete in the source task.

## Original task scope

- Add client crates (for example `clients/lynx_client` for the app plus
  `crates/client_*` for reusable presentation/networking helpers).
- Pin Bevy 0.20 in `[workspace.dependencies]` as ADR 0001 requires; keep the
  dependency out of engine domain crates.
- Provide a headless feature/test mode so CI can build and test the client
  without a GPU or window.
- Wire the client into workspace fmt/clippy/test and document how to run it.
- Add an `--offline`/replay harness for UI work without a live server if that
  keeps iteration fast.

## Acceptance evidence

- [x] `cargo build --workspace` and `cargo test --workspace` include the client.
- [x] A hosted CI run verifies the configured headless client build and test
      without a display/GPU.
- [x] No engine domain crate depends on Bevy.
- [x] The Bevy pin is recorded and an upgrade note exists.
- [x] Visually verify that `cargo run -p lynx_client` opens a window on a
      developer machine.

**Progress:** Bevy 0.20.0 is available on crates.io and pinned in the workspace.
The client provides a shared plugin and deterministic headless update test; see
[`clients/lynx_client/README.md`](../../../clients/lynx_client/README.md) for run,
headless build/test, and upgrade instructions. On Windows, workspace build and
tests and the no-default-features client test passed. Ubuntu and
Windows CI jobs run explicit no-default-features headless client build and test
steps before workspace Clippy, so unrelated lint failures cannot suppress the
focused GPU-free verification. Hosted CI run
[#41](https://github.com/cosmosgc/Lynx/actions/runs/40806879047) passed those
headless client test/build steps on Ubuntu and Windows, although the full run
failed later at workspace Clippy and predates the current local changes.
The JSON offline presentation replay harness is documented in the client README.
Local workspace fmt, Clippy, build, and test checks passed for the prior
implementation. A developer confirmed that `cargo run -p lynx_client` opens a
window displaying the default offline placeholder scene.

## Source

See [`40-bevy-client.md`](../40-bevy-client.md) for the full task history and implementation notes.
