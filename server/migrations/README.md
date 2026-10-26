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

```bash
./scripts/db-migrate.sh
./scripts/db-migrate.sh --status
```

The runner is idempotent: applied migrations are skipped, and a checksum
mismatch is an error.

## Current schema

| Migration | Contents |
|---|---|
| `0001_core.sql` | Accounts, characters, versioned components, inventory slots, property ownership, idempotency keys, audit events, and shared triggers. |
| `0002_content.sql` | Immutable content bundles, definition and locale indexes, and moderation history. |
| `0003_zone_state.sql` | Zone leases, snapshots, and the recovery event log. |
| `0004_factions.sql` | Faction catalog, role permissions, membership, treasury ledger and transfers, territory ownership and history, and derived views. |

## Audit retention

`lynx.audit_events`, `lynx.content_moderation_events`, `lynx.zone_events`,
`lynx.faction_treasury_entries`, `lynx.faction_treasury_transfers`, and
`lynx.faction_territory_history` reject `UPDATE` and `DELETE`. A retention job
may purge rows only inside a transaction that sets:

```sql
SET LOCAL lynx.allow_append_only_purge = 'on';
DELETE FROM lynx.audit_events WHERE occurred_at < now() - interval '365 days';
```

Record the purge itself in operational logs and never purge records under
active moderation review (ADR 0005, `docs/operations.md`).
