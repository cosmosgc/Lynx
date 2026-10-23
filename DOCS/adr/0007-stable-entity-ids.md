# ADR 0007: Stable entity identifiers

- **Status:** Accepted
- **Date:** 2026-10-23

## Context

Bevy `Entity` values are transient, reused, and meaningful only inside one
process. Database identity, network references, links, ownership, and audit
records must survive restarts and must not collide across zones or servers.

## Decision

- Durable identity is `StableId`: a 128-bit value rendered as a prefixed
  Crockford Base32 string, for example `ent_01J8Z3K4Q7Y2N5V6B8C0D1E2F3`.
- The 128 bits are a 48-bit millisecond timestamp followed by 80 bits of
  randomness. IDs are monotonic within a process: generating two IDs in the
  same millisecond increments the random component instead of re-reading the
  clock.
- A kind prefix identifies the namespace: `ent` (world entity), `chr`
  (character), `zone` (zone instance), `prop` (property), `itm` (item
  instance). Parsing rejects a prefix that does not match the expected kind.
- Authored definitions use `DefinitionId`: a validated lowercase slug such as
  `door.wooden` or `item.torch`. Definitions are stable, human-writable keys;
  they are not entity instances.
- Clients may propose a request ID, but never a durable entity ID. The server
  creates entity IDs.
- IDs are never reused, including after deletion. Tombstones and audit records
  reference the original ID.
- Tests inject a deterministic generator; production uses the system
  generator. No code path reads the clock directly to create an ID.

## Consequences

- IDs sort usefully by creation time and remain unique without coordination.
- Prefixes make logs, traces, and database rows self-describing.
- The 26-character form is longer than a UUID but copy-pasteable and
  case-insensitive on parse.
