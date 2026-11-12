# PST-003 — Normalized faction and calendar persistence

**Status: Done.** All acceptance criteria below are checked complete in the source task.

## Original task scope

- Implement the `FactionStore` and `CalendarStore` seams against the existing
  tables.
- Keep calendar/faction paired updates in one transaction, including territory
  changes, enrollments, resolutions, and treasury entries.
- Backfill from an existing whole-world snapshot and verify equality before
  switching the authority.
- Decide whether the whole-world snapshot remains a cache/backup or is retired
  for these aggregates; document it.
- Preserve append-only history and audit triggers.

## Acceptance evidence

- [x] A restart restores factions, memberships, treasury, territory, events,
      enrollments, and results from normalized tables.
- [x] A failed cross-aggregate update changes neither aggregate.
- [x] Snapshot and normalized views agree after backfill.
- [x] Append-only tables still reject update/delete outside the purge path.
- [x] Operator queries can inspect current state and history.

**Current implementation state:** `PostgresFactionCalendarStore` maps the
paired aggregates to migrations 0004/0005, validates and fences paired writes,
and preserves immutable histories by accepting only matching prefixes plus new
rows. `lynx-world --state-postgres` backfills an absent normalized pair from the
replayed snapshot, requires an existing pair to exactly match it, and commits
calendar commands through the paired store. `--fresh` uses the distinct fenced
purge-and-write path. The whole-world snapshot remains the recovery source and
backup mirror for these aggregates in this increment; normalized state becomes
runtime authority only after startup verifies equality. Existing
`faction_relations` rows are not represented by `FactionSnapshot` and must be
preserved untouched; they cannot be backfilled from snapshots. Ordinary
replacement must append only matching history suffixes and reject rewrites or
truncation. A fresh migrated scratch database passed the paired PostgreSQL
integration test and complete `backend_nakama`/`lynx-world` suite, including
restart, rollback, fencing, reset, and append-only checks.

## Source

See [`20-persistence-backend.md`](../20-persistence-backend.md) for the full task history and implementation notes.
