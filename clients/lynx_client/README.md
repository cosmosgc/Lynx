# Lynx client foundation

`cargo run -p lynx_client` starts a windowed, offline 2D placeholder on Windows
(and on Linux with X11). WASD moves the blue square across a marked ground; a
top-down camera follows it. E, Enter, I, and C map to interact, chat, inventory,
and camera actions but have no behavior yet. Bindings can be changed through
the `InputBindings` resource; there is no rebinding UI.

This is not the demo zone or a billboard character implementation. Movement
is a fixed distance per update, so its speed depends on frame rate. There is no
content loading, collision, UI, server connection, authentication, or gameplay
authority. The 2D camera follows the rig's target rather than its 3D offset.
Visual verification on Windows is still required.

For a display- and GPU-independent build and test, run:

```text
cargo build -p lynx_client --no-default-features
cargo test -p lynx_client --no-default-features
```

The no-default-features executable advances the shared application by one
update and exits; it is not an offline gameplay or replay harness. Workspace
tests also exercise the headless application, even with the default window
feature enabled.

Bevy is pinned to 0.20.0 at the workspace boundary. Per ADR 0001, upgrade it in
a dedicated change, run workspace tests and a visual smoke test, and add a
migration note to `CHANGELOG.md` for a minor upgrade.
