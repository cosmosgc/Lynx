# ADR 0003: Native Nakama and PostgreSQL operation

- **Status:** Accepted
- **Date:** 2026-10-23

## Context

The project guide requires a tested native, non-Docker development and
deployment path. Docker is convenient but cannot be the only supported route.

## Decision

- **PostgreSQL 17** is the baseline database. Production uses a dedicated
  database and a least-privilege role that owns only the Lynx schema.
- **Nakama 3.41.0** is pinned. Development runs the official Windows, Linux, or
  macOS release binary as a foreground process or OS service.
- Repository scripts live in `scripts/`:
  - `setup-postgres.ps1` / `setup-postgres.sh` create the role, database, and
    extensions, and are safe to re-run.
  - `run-nakama.ps1` / `run-nakama.sh` start Nakama with `server/nakama/local.yml`;
    their `-Migrate` / `--migrate` forms apply Nakama's internal schema before
    first start and before a Nakama upgrade.
  - `db-migrate.ps1` / `db-migrate.sh` apply versioned SQL migrations.
- Configuration comes from a checked-in non-secret template plus environment
  variables. Real secrets are never committed.
- Docker Compose files, if added, are convenience wrappers around the same
  images, ports, and migrations, and are tested separately.
- The setup is verified by `lynx doctor --backend` and a smoke test that logs in
  a device account and reads the account record.

## Consequences

- Contributors with a local PostgreSQL installation need no container runtime.
- Windows is a first-class development platform because it is the primary
  developer OS for this project.
- Documentation must track exact Nakama flags and migration behavior per pinned
  release.
