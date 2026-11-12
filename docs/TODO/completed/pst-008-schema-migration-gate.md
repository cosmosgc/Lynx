# PST-008 — Schema version checks and migration integration

**Status: Done.** The world binary embeds repository migrations and checks
applied versions, checksums, pending entries, and unknown/newer entries before
opening PostgreSQL-backed world state. `lynx doctor --backend` reports schema
status through read-only inspection. The migration runner checks checksums
before applying pending files and is covered with both mocked and hosted
PostgreSQL tests; CI applies migrations twice and verifies the second run is a
no-op.

The preflight is not locked across an entire migration run, so operators must
avoid concurrent edits or migration processes. Upgrade order and rollback notes
remain in the operations documentation. See the original acceptance checklist
in [`20-persistence-backend.md`](../20-persistence-backend.md).
