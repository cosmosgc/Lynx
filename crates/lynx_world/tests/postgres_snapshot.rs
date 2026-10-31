//! PostgreSQL snapshot-store integration tests.
//!
//! These tests need a migrated local database and the same `LYNX_PG_*`
//! variables the scripts use (a repository-root `.env` is picked up). They
//! skip cleanly when no database is configured, so `cargo test --workspace`
//! still passes on an offline workstation or in CI without PostgreSQL. Set
//! `LYNX_REQUIRE_POSTGRES_TEST=1` to fail instead of skipping when unconfigured.

use std::time::{SystemTime, UNIX_EPOCH};

use engine_core::id::{DefinitionId, IdKind, StableId};
use engine_core::math::Vec3;
use engine_core::schema::PersistencePolicy;
use engine_core::time::{DurationMs, Millis};
use engine_sim::entity::EntityRecord;
use lynx_world::auth::{Authenticator, Grant, digest};
use lynx_world::demo;
use lynx_world::persistence::{
    PostgresSnapshotStore, PostgresZoneLeaseStore, SnapshotStore, SnapshotVerification,
    WorldSnapshot, WorldSnapshotInputs, ZoneEventInput, replay_world_snapshot_events,
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

fn assert_current_lease_renews(
    store: &PostgresZoneLeaseStore,
    zone: &str,
    token: i64,
) -> anyhow::Result<()> {
    let renewed = store
        .renew(zone, "owner-a", token, std::time::Duration::from_secs(20))?
        .expect("the current owner can renew its live lease");
    assert_eq!(renewed.fencing_token, token);
    assert!(
        store
            .renew(
                zone,
                "owner-a",
                token + 1,
                std::time::Duration::from_secs(20),
            )?
            .is_none(),
        "a stale fencing token cannot renew a lease"
    );
    Ok(())
}

fn assert_snapshot_event_replays(
    store: &PostgresZoneLeaseStore,
    zone: &str,
    world_id: &str,
    snapshot: &WorldSnapshot,
) -> anyhow::Result<()> {
    let recorded = store.events_after(zone, -1)?;
    assert_eq!(recorded[0].kind, "world.snapshot");
    assert_eq!(
        replay_world_snapshot_events(&recorded, world_id)?,
        Some(snapshot.clone()),
        "the event committed with the snapshot reconstructs its live state"
    );
    Ok(())
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
    assert_current_lease_renews(&store, &zone, first.fencing_token)?;
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
    snapshots.save_at_tick(&snapshot, 1)?;
    assert_snapshot_event_replays(&store, &zone, &world_id, &snapshot)?;

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
        1
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
        2
    );
    let events = store.events_after(&zone, -1)?;
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert!(store.release(&zone, "owner-b", second.fencing_token)?);
    snapshots.delete()?;
    Ok(())
}

#[test]
#[allow(clippy::too_many_lines)] // One isolated fixture covers checkpoint verification end to end.
fn fenced_recovery_rejects_missing_checkpoint_rows_and_events() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    if std::env::var("LYNX_PG_PASSWORD").is_err() {
        anyhow::ensure!(
            std::env::var_os("LYNX_REQUIRE_POSTGRES_TEST").is_none(),
            "PostgreSQL test required but not configured"
        );
        eprintln!("skipping PostgreSQL checkpoint test: database is not configured");
        return Ok(());
    }

    let zone = test_zone_id();
    let world_id = test_world_id();
    let leases = PostgresZoneLeaseStore::from_env()?;
    let lease = leases
        .acquire(&zone, "checkpoint-test", std::time::Duration::from_secs(30))?
        .expect("isolated test zone lease is available");
    let store = PostgresSnapshotStore::from_env(&world_id)?;
    store.bind_zone_lease(&zone, "checkpoint-test", lease.fencing_token)?;
    let mut snapshot = authored_snapshot()?;
    snapshot.zone.tick.0 = 3;
    let config = lynx_world::persistence::postgres_config_from_env()?;
    let mut client = config.connect(postgres::NoTls)?;
    let verifier = PostgresSnapshotStore::from_env(&world_id)?;
    assert_eq!(
        verifier.verify_recovery(&zone)?,
        SnapshotVerification::Absent
    );
    store.save_at_tick(&snapshot, 3)?;
    assert_eq!(store.load()?, Some(snapshot.clone()));
    assert_eq!(
        verifier.verify_recovery(&zone)?,
        SnapshotVerification::Verified {
            event_sequence: 0,
            event_count: 1,
            saved_at_ms: snapshot.saved_at_ms,
            tick: 3,
        }
    );

    let checkpoint_sequence: i64 = client
        .query_one(
            "SELECT event_sequence FROM lynx.world_snapshots WHERE world_id = $1",
            &[&world_id],
        )?
        .get(0);
    client.execute(
        "UPDATE lynx.world_snapshots SET event_sequence = $2 WHERE world_id = $1",
        &[&world_id, &(checkpoint_sequence + 1)],
    )?;
    let stale_checkpoint = store.load().unwrap_err();
    assert!(
        stale_checkpoint
            .to_string()
            .contains("checkpoint is missing or stale")
    );
    assert!(verifier.verify_recovery(&zone).is_err());
    client.execute(
        "UPDATE lynx.world_snapshots SET event_sequence = $2 WHERE world_id = $1",
        &[&world_id, &checkpoint_sequence],
    )?;

    assert!(store.delete()?);
    let missing_row = store.load().unwrap_err();
    assert!(
        missing_row
            .to_string()
            .contains("checkpoint row is missing")
    );
    assert!(verifier.verify_recovery(&zone).is_err());

    store.save_at_tick(&snapshot, 3)?;
    let mut purge = client.transaction()?;
    purge.batch_execute("SET LOCAL lynx.allow_append_only_purge = 'on'")?;
    purge.execute(
        "DELETE FROM lynx.zone_events WHERE zone_id = $1 AND kind = 'world.snapshot' AND payload->>'world_id' = $2",
        &[&zone, &world_id],
    )?;
    purge.commit()?;
    let missing_event = store.load().unwrap_err();
    assert!(
        missing_event
            .to_string()
            .contains("checkpoint is missing or stale")
    );
    assert!(verifier.verify_recovery(&zone).is_err());

    store.delete()?;
    assert!(leases.release(&zone, "checkpoint-test", lease.fencing_token)?);
    client.execute("DELETE FROM lynx.zone_leases WHERE zone_id = $1", &[&zone])?;

    let legacy_zone = test_zone_id();
    let legacy_world = test_world_id();
    let legacy_lease = leases
        .acquire(
            &legacy_zone,
            "legacy-checkpoint-test",
            std::time::Duration::from_secs(30),
        )?
        .expect("isolated legacy test zone lease is available");
    let legacy_store = PostgresSnapshotStore::from_env(&legacy_world)?;
    legacy_store.save(&snapshot)?;
    legacy_store.bind_zone_lease(
        &legacy_zone,
        "legacy-checkpoint-test",
        legacy_lease.fencing_token,
    )?;
    assert_eq!(
        legacy_store.verify_recovery(&legacy_zone)?,
        SnapshotVerification::LegacyUncheckpointed {
            saved_at_ms: snapshot.saved_at_ms,
            tick: 3,
        }
    );
    assert_eq!(legacy_store.load()?, Some(snapshot));
    legacy_store.delete()?;
    assert!(leases.release(
        &legacy_zone,
        "legacy-checkpoint-test",
        legacy_lease.fencing_token,
    )?);
    client.execute(
        "DELETE FROM lynx.zone_leases WHERE zone_id = $1",
        &[&legacy_zone],
    )?;
    Ok(())
}

#[test]
fn postgres_load_replays_entity_deltas_after_the_pinned_checkpoint() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    if std::env::var("LYNX_PG_PASSWORD").is_err() {
        anyhow::ensure!(
            std::env::var_os("LYNX_REQUIRE_POSTGRES_TEST").is_none(),
            "PostgreSQL test required but not configured"
        );
        eprintln!("skipping PostgreSQL entity-delta test: database is not configured");
        return Ok(());
    }

    let zone = test_zone_id();
    let world_id = test_world_id();
    let leases = PostgresZoneLeaseStore::from_env()?;
    let lease = leases
        .acquire(
            &zone,
            "entity-delta-test",
            std::time::Duration::from_secs(30),
        )?
        .expect("isolated test zone lease is available");
    let store = PostgresSnapshotStore::from_env(&world_id)?;
    store.bind_zone_lease(&zone, "entity-delta-test", lease.fencing_token)?;
    let mut snapshot = authored_snapshot()?;
    snapshot.zone.tick.0 = 3;
    store.save_at_tick(&snapshot, 3)?;

    let entity = EntityRecord::new(
        StableId::from_parts(IdKind::Entity, 77, 1),
        DefinitionId::new("entity.replay-test")?,
        Vec3::new(2.0, 0.0, 3.0),
    )
    .with_persistence(PersistencePolicy::PersistentEntity);
    let actor_id = entity.id.to_string();
    let payload = serde_json::json!({ "entity": entity });
    let config = lynx_world::persistence::postgres_config_from_env()?;
    let mut client = config.connect(postgres::NoTls)?;
    client.execute(
        "INSERT INTO lynx.zone_events (zone_id, sequence, tick, occurred_at, kind, actor_id, payload, schema_version) VALUES ($1, 1, 4, clock_timestamp(), 'zone.entity.upsert', $2, $3, 1)",
        &[&zone, &actor_id, &payload],
    )?;

    let mut expected = snapshot.clone();
    expected
        .zone
        .entities
        .push(serde_json::from_value(payload["entity"].clone())?);
    expected.zone.entities.sort_by_key(|value| value.id);
    expected.zone.tick.0 = 4;
    assert_eq!(store.load()?, Some(expected));
    assert_eq!(
        store.verify_recovery(&zone)?,
        SnapshotVerification::Verified {
            event_sequence: 0,
            event_count: 2,
            saved_at_ms: snapshot.saved_at_ms,
            tick: 4,
        }
    );

    let impossible_entity = EntityRecord::new(
        StableId::from_parts(IdKind::Entity, 77, 2),
        DefinitionId::new("entity.replay-test")?,
        Vec3::new(10_000.0, 0.0, 10_000.0),
    )
    .with_persistence(PersistencePolicy::PersistentEntity);
    let impossible_actor = impossible_entity.id.to_string();
    let impossible_payload = serde_json::json!({ "entity": impossible_entity });
    client.execute(
        "INSERT INTO lynx.zone_events (zone_id, sequence, tick, occurred_at, kind, actor_id, payload, schema_version) VALUES ($1, 2, 5, clock_timestamp(), 'zone.entity.upsert', $2, $3, 1)",
        &[&zone, &impossible_actor, &impossible_payload],
    )?;
    assert!(store.load()?.is_some(), "reducer returns the event state");
    let error = startup::load(Some(&store), false, &characters(1), 1_000).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("failed to restore the demo zone")
    );
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
        .acquire(&zone, "owner-a", std::time::Duration::from_millis(500))?
        .unwrap();
    assert!(
        store
            .acquire(&zone, "owner-b", std::time::Duration::from_secs(10))?
            .is_none(),
        "an unexpired lease cannot be taken over"
    );
    std::thread::sleep(std::time::Duration::from_millis(700));
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
