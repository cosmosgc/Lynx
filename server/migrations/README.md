# Database migrations

Migrations are plain PostgreSQL SQL, applied in filename order by
`scripts/db-migrate.ps1` or `scripts/db-migrate.sh`.

## Rules

- One file per change, named `NNNN_lower_snake_case.sql`, with a
  monotonically increasing four-digit `NNNN`.
- Never edit an applied migration. The runner stores its SHA-256 and refuses to
  continue when a file changes.
- A migration runs inside one transaction, together with its
  `lynx.schema_migrations` row and a transaction-scoped advisory lock. Do not
  add `BEGIN`, `COMMIT`, or `ROLLBACK` to a file.
- Destructive changes need a backup step and a documented rollback path
  (ADR 0005). Additive migrations are preferred.
- The `lynx` schema is owned by the application role and created by
  `scripts/setup-postgres.*`. Migrations do not create it.
- Tables use `lynx.`-qualified names explicitly; the application role's
  `search_path` is not relied on, because Nakama shares the database and uses
  `public`.

## Applying

```powershell
./scripts/db-migrate.ps1
./scripts/db-migrate.ps1 -Status
```

## Upgrade order and rollback

1. Take and verify a database backup before a production upgrade.
2. Apply the migrations with `scripts/db-migrate.*` and confirm the reported
   head/status.
3. Start the new binary. A PostgreSQL-backed world service refuses startup when
   migration history is missing, pending, newer than the binary, or has a
   checksum mismatch. `lynx doctor --backend` inspects the same history without
   changing it.
4. If rolling back the binary, first verify that the older binary supports the
   migrated schema and that any writes made by the new binary remain compatible.
   Restore the backup when that cannot be guaranteed; do not delete migration
   history or edit an applied migration to force startup.

Schema checks prevent an unsupported binary/schema pairing; they do not make
database migrations reversible or provide automatic data rollback.

```bash
./scripts/db-migrate.sh
./scripts/db-migrate.sh --status
```

The runner is idempotent: applied migrations are skipped. Before applying any
pending migration it checks all local applied files for checksum mismatches and
aborts if any have changed, including when a pending file sorts earlier.
`--status` / `-Status` reports mismatches without applying migrations. Do not
change migration files or run another migration process concurrently: preflight
and individual migration transactions do not share a single lock.

## Upgrades and compatibility

Stop writers and take a verified database backup before upgrading. Run the
new release's migration runner, then start its world binary. The binary embeds
the exact migration files and refuses missing history, pending or unknown
migrations, and checksum mismatches before loading or writing a world (including
`--fresh`). `lynx doctor --backend` reports schema history without applying
migrations; configure the same `LYNX_PG_*` variables as the world service.
Unknown migrations are refused; no forward compatibility is currently declared.
If rollback is needed, stop the service and restore the pre-upgrade backup before
starting the old binary: an old binary cannot safely use a newer schema. Never
edit applied migration files to resolve a mismatch.

## Current schema

| Migration | Contents |
|---|---|
| `0001_core.sql` | Accounts, characters, versioned components, inventory slots, property ownership, idempotency keys, audit events, and shared triggers. |
| `0002_content.sql` | Immutable content bundles, definition and locale indexes, and moderation history. |
| `0003_zone_state.sql` | Zone leases, snapshots, and the recovery event log. |
| `0004_factions.sql` | Faction catalog, role permissions, membership, treasury ledger and transfers, territory ownership and history, and derived views. |
| `0005_world_events.sql` | Calendar events, bounded conflict sides, append-only lifecycle transitions, enrollment, conflict results, consistency triggers, and calendar views. |
| `0006_world_snapshots.sql` | One resumable whole-world snapshot per world: serialized zone, calendar, and faction state with a payload hash. |
| `0007_zone_fencing.sql` | Persistent fencing generations for single-writer zone leases. |
| `0008_inventory_revisions.sql` | Optimistic aggregate versions for inventory snapshots. |
| `0009_inventory_transfer_ledger.sql` | Append-only transfer receipts and idempotency keys. |
| `0010_snapshot_event_checkpoints.sql` | Pins durable snapshots to their exact replay-log checkpoint. |
| `0011_snapshot_checkpoint_zone.sql` | Binds zone-local checkpoint sequences to their owning zone. |

## Audit retention

`lynx.audit_events`, `lynx.content_moderation_events`, `lynx.zone_events`,
`lynx.faction_treasury_entries`, `lynx.faction_treasury_transfers`,
`lynx.faction_territory_history`, `lynx.world_event_transitions`,
`lynx.world_event_enrollments`, `lynx.world_event_results`, and
`lynx.world_event_result_sides` reject `UPDATE` and `DELETE`. A retention job
may purge rows only inside a transaction that sets:

```sql
SET LOCAL lynx.allow_append_only_purge = 'on';
DELETE FROM lynx.audit_events WHERE occurred_at < now() - interval '365 days';
```

Record the purge itself in operational logs and never purge records under
active moderation review (ADR 0005, `docs/operations.md`).
