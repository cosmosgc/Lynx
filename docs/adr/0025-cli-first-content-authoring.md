# ADR 0025: CLI-first content authoring

- **Status:** Accepted
- **Date:** 2026-11-12

## Decision

Build content authoring as a CLI-first workflow. A future desktop editor is a
standalone application that calls the same project, schema, and bundle
validation modules as `lynx`; it does not embed an editor into the player
client. The CLI remains usable in CI and headless environments, and it is the
reference implementation for validation and publish checks.

## Considered options

- **In-client editor:** Rejected because creator tools would inherit player
  client lifecycle and graphics requirements.
- **Standalone editor before CLI contracts:** Rejected because it would create
  a second validation path and make CI harder to reproduce.

## Consequences

- Editor work can begin after project and CLI contracts stabilize, while
  creators can automate validation before the visual editor exists.
- Editor UI state and undo history stay outside runtime and simulation crates.
