//! Read-only compatibility checks against the migration runner's history.
use postgres::Client;
use serde::Serialize;
use sha2::{Digest, Sha256};

const MIGRATIONS: &[(&str, &[u8])] = &[
    (
        "0001_core",
        include_bytes!("../../../server/migrations/0001_core.sql"),
    ),
    (
        "0002_content",
        include_bytes!("../../../server/migrations/0002_content.sql"),
    ),
    (
        "0003_zone_state",
        include_bytes!("../../../server/migrations/0003_zone_state.sql"),
    ),
    (
        "0004_factions",
        include_bytes!("../../../server/migrations/0004_factions.sql"),
    ),
    (
        "0005_world_events",
        include_bytes!("../../../server/migrations/0005_world_events.sql"),
    ),
    (
        "0006_world_snapshots",
        include_bytes!("../../../server/migrations/0006_world_snapshots.sql"),
    ),
    (
        "0007_zone_fencing",
        include_bytes!("../../../server/migrations/0007_zone_fencing.sql"),
    ),
    (
        "0008_inventory_revisions",
        include_bytes!("../../../server/migrations/0008_inventory_revisions.sql"),
    ),
    (
        "0009_inventory_transfer_ledger",
        include_bytes!("../../../server/migrations/0009_inventory_transfer_ledger.sql"),
    ),
    (
        "0010_snapshot_event_checkpoints",
        include_bytes!("../../../server/migrations/0010_snapshot_event_checkpoints.sql"),
    ),
    (
        "0011_snapshot_checkpoint_zone",
        include_bytes!("../../../server/migrations/0011_snapshot_checkpoint_zone.sql"),
    ),
    (
        "0012_recovery_quarantine",
        include_bytes!("../../../server/migrations/0012_recovery_quarantine.sql"),
    ),
    (
        "0013_fix_event_result_trigger",
        include_bytes!("../../../server/migrations/0013_fix_event_result_trigger.sql"),
    ),
];

/// Schema compatibility against the migrations embedded in this binary.
#[derive(Debug, Serialize)]
pub struct SchemaStatus {
    pub history_missing: bool,
    pub expected_head: &'static str,
    pub applied: Vec<&'static str>,
    pub pending: Vec<&'static str>,
    pub checksum_mismatches: Vec<&'static str>,
    pub unknown_count: usize,
}

impl SchemaStatus {
    pub fn compatible(&self) -> bool {
        !self.history_missing
            && self.pending.is_empty()
            && self.checksum_mismatches.is_empty()
            && self.unknown_count == 0
    }

    pub fn diagnostic(&self) -> String {
        format!(
            "PostgreSQL schema incompatible: history_missing={}, pending={:?}, checksum_mismatches={:?}, unsupported_migrations={}; back up and run scripts/db-migrate, or deploy a compatible binary; never edit applied migrations",
            self.history_missing, self.pending, self.checksum_mismatches, self.unknown_count
        )
    }
}

fn compare(history_missing: bool, rows: &[(String, String)]) -> SchemaStatus {
    let mut status = SchemaStatus {
        history_missing,
        expected_head: MIGRATIONS.last().unwrap().0,
        applied: vec![],
        pending: vec![],
        checksum_mismatches: vec![],
        unknown_count: 0,
    };
    for (version, bytes) in MIGRATIONS {
        if let Some((_, checksum)) = rows.iter().find(|(found, _)| found == version) {
            status.applied.push(version);
            if *checksum != checksum_hex(bytes) {
                status.checksum_mismatches.push(version);
            }
        } else {
            status.pending.push(version);
        }
    }
    status.unknown_count = rows
        .iter()
        .filter(|(version, _)| !MIGRATIONS.iter().any(|(known, _)| known == version))
        .count();
    status
}

fn checksum_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut output = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        write!(output, "{byte:02x}").expect("writing to a string cannot fail");
    }
    output
}

/// Inspect history without creating or changing any database object.
pub fn inspect(client: &mut Client) -> Result<SchemaStatus, &'static str> {
    let exists: bool = client
        .query_one(
            "SELECT to_regclass('lynx.schema_migrations') IS NOT NULL",
            &[],
        )
        .map_err(|_| "could not inspect PostgreSQL migration history")?
        .get(0);
    if !exists {
        return Ok(compare(true, &[]));
    }
    let rows = client
        .query("SELECT version, checksum FROM lynx.schema_migrations", &[])
        .map_err(
            |_| "could not read PostgreSQL migration history; check permissions and history format",
        )?;
    let rows = rows
        .iter()
        .map(|row| Ok((row.try_get(0)?, row.try_get(1)?)))
        .collect::<Result<Vec<(String, String)>, postgres::Error>>()
        .map_err(|_| "invalid PostgreSQL migration history format")?;
    Ok(compare(false, &rows))
}

/// Connect using the world PostgreSQL environment and inspect without writes.
pub fn inspect_env() -> Result<SchemaStatus, String> {
    let config = crate::persistence::postgres_config_from_env()
        .map_err(|_| "PostgreSQL configuration is missing or invalid".to_owned())?;
    let mut client = config
        .connect(postgres::NoTls)
        .map_err(|_| "could not connect to PostgreSQL".to_owned())?;
    inspect(&mut client).map_err(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_migrations_cover_the_repository_in_order() {
        let directory =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../server/migrations");
        let mut files: Vec<_> = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "sql"))
            .map(|path| path.file_stem().unwrap().to_str().unwrap().to_owned())
            .collect();
        files.sort();
        let embedded: Vec<_> = MIGRATIONS.iter().map(|(version, _)| *version).collect();
        assert_eq!(files, embedded, "embed every migration in filename order");
    }

    #[test]
    fn compatibility_is_exact_and_checks_checksums() {
        let mut rows: Vec<_> = MIGRATIONS
            .iter()
            .map(|(v, b)| ((*v).to_owned(), checksum_hex(b)))
            .collect();
        assert!(compare(false, &rows).compatible());
        assert!(!compare(true, &[]).compatible());
        rows.pop();
        assert_eq!(
            compare(false, &rows).pending,
            ["0013_fix_event_result_trigger"]
        );
        rows[0].1 = "bad".into();
        rows.push(("0007_future".into(), "secret".into()));
        let status = compare(false, &rows);
        assert_eq!(status.checksum_mismatches, ["0001_core"]);
        assert_eq!(status.unknown_count, 1);
        assert!(!status.diagnostic().contains("secret"));
    }

    #[test]
    fn missing_history_and_non_contiguous_history_are_actionable() {
        let missing = compare(true, &[]);
        assert!(missing.history_missing);
        assert_eq!(missing.pending.len(), MIGRATIONS.len());
        assert!(missing.diagnostic().contains("scripts/db-migrate"));

        let rows = vec![(MIGRATIONS[1].0.to_owned(), checksum_hex(MIGRATIONS[1].1))];
        let partial = compare(false, &rows);
        assert_eq!(partial.applied, ["0002_content"]);
        assert!(partial.pending.contains(&"0001_core"));
        assert!(!partial.compatible());
    }

    #[test]
    fn unknown_migration_is_rejected_even_with_complete_known_history() {
        let mut rows: Vec<_> = MIGRATIONS
            .iter()
            .map(|(version, bytes)| ((*version).to_owned(), checksum_hex(bytes)))
            .collect();
        rows.push(("0007_future".into(), "a".repeat(64)));
        let status = compare(false, &rows);
        assert_eq!(status.pending, [] as [&str; 0]);
        assert_eq!(status.checksum_mismatches, [] as [&str; 0]);
        assert_eq!(status.unknown_count, 1);
        assert!(!status.compatible());
    }
}
