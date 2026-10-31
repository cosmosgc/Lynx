//! Live schema gate checks, run only against an explicitly prepared scratch database.

use lynx_world::persistence::{PostgresSnapshotStore, postgres_config_from_env};

#[test]
fn prepared_schema_is_rejected() -> anyhow::Result<()> {
    let Ok(scenario) = std::env::var("LYNX_TEST_SCHEMA_SCENARIO") else {
        return Ok(());
    };
    let config = postgres_config_from_env()?;
    let mut client = config.connect(postgres::NoTls)?;
    let status = lynx_world::schema::inspect(&mut client).map_err(anyhow::Error::msg)?;
    assert!(!status.compatible());
    let error = PostgresSnapshotStore::connect(&config, "schema-test").unwrap_err();
    let message = error.to_string();
    match scenario.as_str() {
        "missing" => {
            assert!(status.history_missing);
            assert_eq!(status.applied, [] as [&str; 0]);
            assert_eq!(status.pending.last(), Some(&status.expected_head));
            assert!(message.contains("scripts/db-migrate"), "{message}");
        }
        "newer" => {
            assert_eq!(status.unknown_count, 1);
            assert!(message.contains("unsupported_migrations=1"), "{message}");
        }
        "checksum" => {
            assert_eq!(status.checksum_mismatches, ["0001_core"]);
            assert!(message.contains("0001_core"), "{message}");
        }
        _ => anyhow::bail!("unknown schema test scenario"),
    }
    Ok(())
}
