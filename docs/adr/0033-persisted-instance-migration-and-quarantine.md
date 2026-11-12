# ADR 0033: Persisted-instance migration and quarantine

- **Status:** Accepted
- **Date:** 2026-11-12

Persisted instances are migrated as part of a reviewed content transition from
one exact source manifest hash to one exact target manifest hash. The migration
plan targets stable prototype IDs and component keys, declares source and
target schema versions, and contains only deterministic data operations with
preconditions; it cannot execute code or choose filesystem paths. The source
world snapshot supplies the source content version, while each
`ComponentInstance` supplies its own component schema version. Engine-owned
typed `EntityRecord` fields remain governed by the world snapshot schema and
are not rewritten by content migrations.

Startup first builds a dry-run candidate from a copy of the snapshot. It
validates every migrated component against the target registry and every
prototype against the target bundle. A failed or missing transition quarantines
the complete entity with its exact serialized state and a stable diagnostic;
the live world is never partially mutated. Links that cannot be restored after
entity quarantine are quarantined with their endpoint IDs. The migration report
lists migrated and quarantined records before the candidate is committed.

Applying a candidate atomically stores the new snapshot, quarantine records,
the report, and the previous compatible snapshot as one migration checkpoint.
File-backed stores use an atomic checkpoint replacement; PostgreSQL stores use
one transaction under the zone lease. Startup accepts sessions only after
this commit succeeds. A rollback before the new world accepts gameplay restores
the previous snapshot and its content identity as one pair. Once authoritative
gameplay has been written under the new version, the old checkpoint is retained
for recovery and audit, but rollback must use a reviewed reverse migration or a
restore drill; blindly restoring the old snapshot could discard new gameplay.

## Considered options

- **Rewrite typed entity fields with content plans:** Rejected because those
  fields belong to the engine snapshot schema, not authored component schemas.
- **Drop incompatible entities during restore:** Rejected because it loses
  recoverable player state without preserving evidence or an operator repair
  path.
- **Commit each entity as it migrates:** Rejected because a crash could expose
  a mixed-version world. Migration and quarantine must commit as one checkpoint.
- **Allow rollback to replace a world after new gameplay has been accepted:**
  Rejected because that would silently lose mutations made under the new
  content version.

## Consequences

- Dry-run and apply use the same validator, so reports predict the exact
  migration candidate.
- Quarantine must retain the complete entity payload, not only an error string.
- Snapshot stores need an atomic migration-checkpoint operation and a rollback
  operation that verifies the active content identity.
- The migration transition is distinct from authored-file rollback in ADR 0031.
