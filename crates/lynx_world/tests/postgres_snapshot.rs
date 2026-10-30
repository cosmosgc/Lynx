//! PostgreSQL snapshot-store integration tests.
//!
//! These tests need a migrated local database and the same `LYNX_PG_*`
//! variables the scripts use (a repository-root `.env` is picked up). They
//! skip cleanly when no database is configured, so `cargo test --workspace`
//! still passes on an offline workstation or in CI without PostgreSQL. Set
//! `LYNX_REQUIRE_POSTGRES_TEST=1` to fail instead of skipping when unconfigured.

use std::time::{SystemTime, UNIX_EPOCH};

use engine_core::time::{DurationMs, Millis};
use lynx_world::auth::{Authenticator, Grant, digest};
use lynx_world::demo;
use lynx_world::persistence::{
    PostgresSnapshotStore, PostgresZoneLeaseStore, SnapshotStore, WorldSnapshot,
    WorldSnapshotInputs, ZoneEventInput,
};
use lynx_world::startup;
use world_gateway::{GatewayConfig, SystemResumeTokens, WorldGateway};
use world_server::WorldServer;

#[test]
fn schema_inspection_does_not_change_history() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    if std::env::var("LYNX_PG_PASSWORD").is_err() {
        anyhow::ensure!(
            std::env::var_os("LYNX_REQUIRE_POSTGRES_TEST").is_none(),
            "PostgreSQL test required but not configured"
        );
        return Ok(());
    }
    let config = lynx_world::persistence::postgres_config_from_env()?;
    let mut client = config
        .connect(postgres::NoTls)
        .map_err(|_| anyhow::anyhow!("PostgreSQL connection failed"))?;
    let history = |client: &mut postgres::Client| -> anyhow::Result<Vec<String>> {
        Ok(client.query("SELECT version || '|' || checksum || '|' || applied_at::text FROM lynx.schema_migrations ORDER BY version", &[])?.iter().map(|r| r.get(0)).collect())
    };
    let before = history(&mut client)?;
    assert!(
        lynx_world::schema::inspect(&mut client)
            .map_err(anyhow::Error::msg)?
            .compatible()
    );
    assert_eq!(before, history(&mut client)?);
    Ok(())
}

fn characters(count: usize) -> Vec<Grant> {
    let hashes: Vec<[u8; 32]> = (0..count)
        .map(|index| digest(&format!("postgres-token-{index}")))
        .collect();
    Authenticator::new(&hashes)
        .unwrap()
        .grants()
        .cloned()
        .collect()
}

fn test_world_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("test-{nanos}")
}

fn test_zone_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("zone_{nanos:026}")
}

fn authored_snapshot() -> anyhow::Result<WorldSnapshot> {
    let grants = characters(1);
    let world = startup::load(None, true, &grants, 1_000)?;
    Ok(WorldSnapshot::capture(
        &WorldSnapshotInputs {
            zone: world.zone.snapshot(),
            calendar_factions: Some((world.calendar.snapshot(), world.factions.snapshot())),
        },
        1_000,
    ))
}

#[test]
fn postgres_zone_lease_takeover_fences_old_owner_writes() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    if std::env::var("LYNX_PG_PASSWORD").is_err() {
        anyhow::ensure!(
            std::env::var_os("LYNX_REQUIRE_POSTGRES_TEST").is_none(),
            "PostgreSQL test required but not configured"
        );
        eprintln!("skipping PostgreSQL zone lease test: LYNX_PG_PASSWORD is not configured");
        return Ok(());
    }
    let store = PostgresZoneLeaseStore::from_env()?;
    let zone = test_zone_id();
    let first = store
        .acquire(&zone, "owner-a", std::time::Duration::from_secs(10))?
        .unwrap();
    assert!(
        store
            .acquire(&zone, "owner-b", std::time::Duration::from_secs(10))?
            .is_none(),
        "an unexpired lease has one owner"
    );

    // A snapshot written under the live lease is accepted.
    let world_id = test_world_id();
    let snapshots = PostgresSnapshotStore::from_env(&world_id)?;
    snapshots.bind_zone_lease(&zone, "owner-a", first.fencing_token)?;
    let snapshot = authored_snapshot()?;
    snapshots.save(&snapshot)?;

    // A clean handoff still increments the generation.
    assert!(store.release(&zone, "owner-a", first.fencing_token)?);
    let second = store
        .acquire(&zone, "owner-b", std::time::Duration::from_secs(10))?
        .unwrap();
    assert!(second.fencing_token > first.fencing_token);

    // Both durable write paths reject the superseded owner.
    let stale_snapshot = snapshots.save(&snapshot).unwrap_err();
    assert_eq!(
        stale_snapshot.code(),
        engine_core::error::ErrorCode::Conflict
    );
    let payload = serde_json::json!({"value": 1});
    let stale_event = store
        .append_event(
            &zone,
            "owner-a",
            first.fencing_token,
            ZoneEventInput {
                tick: 1,
                kind: "test",
                actor_id: None,
                payload: &payload,
                schema_version: 1,
            },
        )
        .unwrap_err();
    assert_eq!(stale_event.code(), engine_core::error::ErrorCode::Conflict);

    assert_eq!(
        store.append_event(
            &zone,
            "owner-b",
            second.fencing_token,
            ZoneEventInput {
                tick: 2,
                kind: "test",
                actor_id: None,
                payload: &payload,
                schema_version: 1,
            },
        )?,
        0
    );
    assert_eq!(
        store.append_event(
            &zone,
            "owner-b",
            second.fencing_token,
            ZoneEventInput {
                tick: 3,
                kind: "test",
                actor_id: None,
                payload: &payload,
                schema_version: 1,
            },
        )?,
        1
    );
    let events = store.events_after(&zone, -1)?;
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert!(store.release(&zone, "owner-b", second.fencing_token)?);
    snapshots.delete()?;
    Ok(())
}

#[test]
fn postgres_zone_lease_expiry_allows_takeover() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    if std::env::var("LYNX_PG_PASSWORD").is_err() {
        anyhow::ensure!(
            std::env::var_os("LYNX_REQUIRE_POSTGRES_TEST").is_none(),
            "PostgreSQL test required but not configured"
        );
        eprintln!("skipping PostgreSQL zone lease test: LYNX_PG_PASSWORD is not configured");
        return Ok(());
    }
    let store = PostgresZoneLeaseStore::from_env()?;
    let zone = test_zone_id();
    let first = store
        .acquire(&zone, "owner-a", std::time::Duration::from_millis(25))?
        .unwrap();
    assert!(
        store
            .acquire(&zone, "owner-b", std::time::Duration::from_secs(10))?
            .is_none(),
        "an unexpired lease cannot be taken over"
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let second = store
        .acquire(&zone, "owner-b", std::time::Duration::from_secs(10))?
        .unwrap();
    assert!(second.fencing_token > first.fencing_token);
    assert!(store.release(&zone, "owner-b", second.fencing_token)?);
    Ok(())
}

#[test]
fn postgres_store_round_trips_and_fresh_start_resets_it() -> anyhow::Result<()> {
    // Integration tests do not inherit the scripts' environment loader.
    dotenvy::dotenv().ok();
    if std::env::var("LYNX_PG_PASSWORD").is_err() {
        anyhow::ensure!(
            std::env::var_os("LYNX_REQUIRE_POSTGRES_TEST").is_none(),
            "PostgreSQL test required but LYNX_PG_PASSWORD is not configured"
        );
        eprintln!("skipping PostgreSQL test: LYNX_PG_PASSWORD is not configured");
        return Ok(());
    }

    let world_id = test_world_id();
    let store = PostgresSnapshotStore::from_env(&world_id)?;
    assert_eq!(store.world_id(), world_id);
    assert!(
        store.load()?.is_none(),
        "a new world id starts without a snapshot"
    );

    // Build and play a world, then save the whole aggregate.
    let grants = characters(2);
    let now_ms = 1_000_000;
    let mut factions = demo::build_faction_world(now_ms)?;
    let calendar = demo::build_calendar(&mut factions, now_ms)?;
    let mut gateway = WorldGateway::new(
        demo::build_zone(&grants)?,
        demo::handshake()?,
        GatewayConfig::new(DurationMs::new(30_000)),
        SystemResumeTokens::new(),
    )?;
    gateway = gateway.with_calendar(calendar, factions)?;
    for _ in 0..3 {
        gateway.tick(Millis::new(0))?;
    }
    let server = WorldServer::new(gateway);
    let (calendar_snapshot, faction_snapshot) = server
        .gateway()
        .calendar_stage()
        .expect("the demo owns a calendar")
        .snapshots();
    let played = WorldSnapshot::capture(
        &WorldSnapshotInputs {
            zone: server.gateway().zone().snapshot(),
            calendar_factions: Some((calendar_snapshot, faction_snapshot)),
        },
        now_ms,
    );
    store.save(&played)?;

    let loaded = store.load()?.expect("the saved world loads");
    assert_eq!(
        loaded, played,
        "zone, calendar, and faction state survive the database round trip"
    );
    assert_eq!(loaded.zone.tick.0, 3);

    // `--fresh` builds the authored world and ignores the played row.
    let reset = startup::load(Some(&store), true, &grants, now_ms)?;
    assert_eq!(reset.zone.tick().0, 0);

    // Main writes that authored world back through the same store.
    let authored = WorldSnapshot::capture(
        &WorldSnapshotInputs {
            zone: reset.zone.snapshot(),
            calendar_factions: Some((reset.calendar.snapshot(), reset.factions.snapshot())),
        },
        now_ms + 1,
    );
    store.save(&authored)?;
    let reloaded = store.load()?.expect("the reset world loads");
    assert_eq!(reloaded.zone.tick.0, 0, "the played tick is gone");
    assert_eq!(
        reloaded.zone.entities.len(),
        authored.zone.entities.len(),
        "the authored placement replaced the played one"
    );

    let deleted = store.delete()?;
    assert!(deleted, "the test world owned a row to delete");
    assert!(store.load()?.is_none(), "a deleted snapshot is absent");
    Ok(())
}
