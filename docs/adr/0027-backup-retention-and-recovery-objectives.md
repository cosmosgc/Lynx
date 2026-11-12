# ADR 0027: Backup retention and recovery objectives

- **Status:** Accepted
- **Date:** 2026-11-12

## Decision

The initial production target is a 15-minute recovery point objective and a
1-hour recovery time objective for PostgreSQL-backed character, inventory,
world-snapshot, and audit data. Require nightly full backups, continuous WAL
archiving where supported, encrypted off-host storage, and at least 35 days of
daily full-backup retention. Run and record a restore drill quarterly. These
are service objectives and policy requirements; they are not considered met
until the deployment exercises and measures them.

## Consequences

- Backup jobs, WAL continuity, encryption, access, and retention need separate
  monitoring and ownership.
- Recovery validation includes append-only audit continuity, not only process
  readiness or snapshot loading.
- A deployment that cannot meet the objectives must record a scoped exception
  before production launch.
