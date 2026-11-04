# OPS-006 — CI Matrix and Live-Integration Coverage

**Status: Completed.** Hosted Linux, Windows, and macOS CI checks are green.
Linux provisions and migrates its PostgreSQL service and isolated integration
database; migration head validation is derived from migration files. The
headless online client smoke covers connect, Bevy presentation entities,
movement, and disconnect. CI also reports Rust/PostgreSQL job durations and
outcome history.

The Windows-only local account-screen smoke is available at
`scripts/client-graphical-smoke.bat`. It builds and launches the current login
window, captures a screenshot, and checks that it is not blank. It does not use
live credentials or test authenticated character creation/selection or gameplay
rendering.

Linux graphical CI automation is intentionally not part of OPS-006; it is
deferred as [OPS-016](../60-operations-release.md#ops-016--linux-graphical-client-smoke)
until a hosted Ubuntu graphics run can be validated.
