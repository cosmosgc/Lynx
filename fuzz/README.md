# Lynx parser fuzzing

The fuzz crate is an independent workspace because `cargo-fuzz` uses nightly
Rust and libFuzzer's sanitizer runtime. The seven targets exercise envelope
and length-prefixed frame decoding, RON content definitions, JSON content
manifests, authoritative zone snapshots, the protocol handshake payload, and
server-authoritative action requests.

Install nightly Rust, `cargo-fuzz`, and a C++ toolchain on Linux, macOS, or WSL.
`cargo-fuzz` does not support native Windows. From the repository root:

```sh
cargo install cargo-fuzz --locked
cargo +nightly fuzz build --fuzz-dir fuzz
cargo +nightly fuzz run --fuzz-dir fuzz envelope_decode -- \
  -max_total_time=60 -timeout=2 -max_len=1048576 -rss_limit_mb=512
```

Replace `envelope_decode` with any target listed in `fuzz/Cargo.toml`. The
nightly GitHub workflow builds every target and runs each for 30 seconds with a
2-second per-input timeout, 1 MiB input cap, and 512 MiB RSS cap. Its cache
persists each target's evolving corpus. Keep crash artifacts, minimize them
with `cargo +nightly fuzz tmin`, then add a deterministic regression case to
the owning crate before removing the crash from triage.

Fuzz input is untrusted. Do not seed corpora with real tokens, user content,
production snapshots, or other private data.
