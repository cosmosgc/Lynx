# ADR 0006: Content, configuration, and protocol formats

- **Status:** Accepted
- **Date:** 2026-10-23

## Context

The engine handles authored content, human-edited configuration, and
machine-to-machine protocol messages. One format for everything would either
make authoring painful or make parsing fragile.

## Decision

One canonical format per category:

| Category | Format | Extensions |
|---|---|---|
| Content definitions (items, entities, zones, behaviors, UI) | RON | `.ron` |
| Runtime configuration and project manifests | TOML | `.toml` |
| Network protocol messages | JSON | — |
| Persistence rows and event records | PostgreSQL columns + JSONB payloads | — |

- Content bundles include a manifest with a content version, schema versions,
  SHA-256 hashes, and engine compatibility constraints. `engine_content`
  computes and verifies these.
- Protocol messages are versioned independently (`PROTOCOL_VERSION`). A client
  whose major version differs is rejected at handshake; a minor mismatch is
  reported and handled by the feature negotiation table.
- Content and protocol schemas have explicit versions. Adding an optional field
  is a minor change; removing, renaming, or retyping a field is a major change
  and requires a migration.
- RON definitions are validated against the schemas in `content/schemas/`
  before they are loaded. Invalid content fails the build, not the runtime.
- Binary serialization is not used for protocol v1. Optimize after measuring,
  and keep the JSON schema as the reference.

## Consequences

- Authors get comments and expressive RON; operators get familiar TOML; network
  debugging stays readable.
- Content validation is a required CI step and a required publishing step.
- Changing formats later is an engine-level migration, not a per-project fork.
