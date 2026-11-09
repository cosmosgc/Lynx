//! PostgreSQL snapshot-store integration tests.
//!
//! These tests need a migrated local database and the same `LYNX_PG_*`
//! variables the scripts use (a repository-root `.env` is picked up). They
//! skip cleanly when no database is configured, so `cargo test --workspace`
//! still passes on an offline workstation or in CI without PostgreSQL. Set
//! `LYNX_REQUIRE_POSTGRES_TEST=1` to fail instead of skipping when unconfigured.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use backend_nakama::postgres_pool::pool as postgres_pool;
use engine_core::id::{DefinitionId, IdKind, StableId};
use engine_core::inventory::TransferReceipt;
use engine_core::math::Vec3;
use engine_core::schema::PersistencePolicy;
use engine_core::time::{DurationMs, Millis, Tick};
use engine_sim::action::{ActionOutcome, ActionResult, Verb};
use engine_sim::entity::EntityRecord;
use engine_sim::link::{EntityLink, LinkEvent};
use lynx_world::auth::{Authenticator, Grant, digest};
use lynx_world::demo;
use lynx_world::persistence::{
    EVENT_CALENDAR_FACTIONS_REPLACE, EVENT_COOLDOWNS_REPLACE, EVENT_ENTITY_UPSERT,
    EVENT_LINK_COOLDOWNS_REPLACE, EVENT_LINKS_REPLACE, EVENT_TRANSFERS_APPEND,
    PostgresSnapshotStore, PostgresZoneLeaseStore, SnapshotStore, SnapshotVerification,
    WorldSnapshot, WorldSnapshotInputs, ZoneEventInput, replay_world_snapshot_events,
};
use lynx_world::startup;
use world_gateway::{GatewayConfig, SystemResumeTokens, WorldGateway};
use world_server::WorldServer;

#[test]
fn configured_but_unreachable_database_fails_loudly() {
    // Reserve and release an ephemeral port so the connection attempt is
    // refused instead of hanging. This test does not need a database and never
    // skips: a configured-but-unreachable store must surface an error.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let mut config = postgres::Config::new();
    config
        .host("127.0.0.1")
        .port(port)
        .dbname("lynx")
        .user("lynx")
        .password("unreachable-sentinel-secret");

    let error = PostgresSnapshotStore::connect(&config, test_world_id()).unwrap_err();
    let rendered = format!("{error:?} {error}");
    assert_eq!(error.code(), engine_core::error::ErrorCode::Internal);
    assert!(
        !rendered.contains("unreachable-sentinel-secret"),
        "connection errors must not echo the password"
    );
}

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

#[test]
fn postgres_zone_store_releases_pool_leases_and_bounds_exhaustion_wait() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    if std::env::var("LYNX_PG_PASSWORD").is_err() {
        anyhow::ensure!(
            std::env::var_os("LYNX_REQUIRE_POSTGRES_TEST").is_none(),
            "PostgreSQL test required but not configured"
        );
        return Ok(());
    }
    let config = lynx_world::persistence::postgres_config_from_env()?;
    let timeout = Duration::from_millis(40);
    let pool = postgres_pool(config, 1, timeout);
    let store = PostgresZoneLeaseStore::with_pool(pool.clone())?;

    // Constructor schema inspection acquired and returned its connection; the
    // same pool can be leased by another consumer and then by the store again.
    let held = pool
        .acquire()
        .map_err(|_| anyhow::anyhow!("pool lease failed"))?;
    let started = Instant::now();
    let error = store
        .events_after("zone_00000000000000000000000000", -1)
        .expect_err("an exhausted pool must return its acquisition timeout");
    let elapsed = started.elapsed();
    assert!(elapsed >= timeout.saturating_sub(Duration::from_millis(10)));
    assert!(elapsed < Duration::from_millis(500));
    assert!(!error.to_string().contains("password"));

    drop(held);
    assert_eq!(
        store
            .events_after("zone_00000000000000000000000000", -1)?
            .len(),
        0
    );
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

static TEST_ID_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn test_run_id() -> u128 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let process = u128::from(std::process::id());
    let sequence =
        u128::from(TEST_ID_SEQUENCE.fetch_add(1, Ordering::Relaxed) & u64::from(u32::MAX));
    (nanos << 64) | (process << 32) | sequence
}

fn test_world_id() -> String {
    format!("test-{}", test_run_id())
}

fn test_zone_id() -> String {
    const BASE32: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut value = test_run_id();
    let mut encoded = [b'0'; 26];
    for digit in encoded.iter_mut().rev() {
        let index = usize::try_from(value & 0x1f).expect("base32 digit fits in usize");
        *digit = BASE32[index];
        value >>= 5;
    }
    format!(
        "zone_{}",
        String::from_utf8(encoded.to_vec()).expect("base32 test ID is valid UTF-8")
    )
}

#[derive(Default)]
struct PostgresTestScope {
    world_ids: Vec<String>,
    zone_ids: Vec<String>,
    cleaned: bool,
}

impl PostgresTestScope {
    fn world_id(&mut self) -> String {
        let id = test_world_id();
        self.world_ids.push(id.clone());
        id
    }

    fn zone_id(&mut self) -> String {
        let id = test_zone_id();
        self.zone_ids.push(id.clone());
        id
    }

    fn cleanup(&mut self) -> anyhow::Result<()> {
        if self.cleaned {
            return Ok(());
        }
        if self.world_ids.is_empty() && self.zone_ids.is_empty() {
            self.cleaned = true;
            return Ok(());
        }

        let config = lynx_world::persistence::postgres_config_from_env()?;
        let mut client = config
            .connect(postgres::NoTls)
            .map_err(|_| anyhow::anyhow!("PostgreSQL test cleanup connection failed"))?;
        let mut transaction = client.transaction()?;
        transaction.batch_execute("SET LOCAL lynx.allow_append_only_purge = 'on'")?;
        for zone_id in &self.zone_ids {
            transaction.execute(
                "DELETE FROM lynx.zone_events WHERE zone_id = $1",
                &[zone_id],
            )?;
            transaction.execute(
                "DELETE FROM lynx.zone_snapshots WHERE zone_id = $1",
                &[zone_id],
            )?;
            transaction.execute(
                "DELETE FROM lynx.zone_leases WHERE zone_id = $1",
                &[zone_id],
            )?;
            transaction.execute(
                "DELETE FROM lynx.recovery_quarantine WHERE zone_id = $1",
                &[zone_id],
            )?;
        }
        for world_id in &self.world_ids {
            transaction.execute(
                "DELETE FROM lynx.world_snapshots WHERE world_id = $1",
                &[world_id],
            )?;
            transaction.execute(
                "DELETE FROM lynx.recovery_quarantine WHERE world_id = $1",
                &[world_id],
            )?;
        }
        transaction.commit()?;
        self.cleaned = true;
        Ok(())
    }

    fn finish(mut self) -> anyhow::Result<()> {
        self.cleanup()
    }
}

impl Drop for PostgresTestScope {
    fn drop(&mut self) {
        if !self.cleaned && self.cleanup().is_err() {
            eprintln!("PostgreSQL integration-test row cleanup failed");
        }
    }
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

fn open_bound_store(
    world_id: &str,
    zone: &str,
    owner: &str,
    token: i64,
) -> anyhow::Result<PostgresSnapshotStore> {
    let store = PostgresSnapshotStore::from_env(world_id)?;
    store.bind_zone_lease(zone, owner, token)?;
    Ok(store)
}

fn assert_event_batch_appends_contiguously(
    store: &PostgresZoneLeaseStore,
    zone: &str,
    token: i64,
) -> anyhow::Result<()> {
    let first_payload = serde_json::json!({ "entity": "first" });
    let second_payload = serde_json::json!({ "entity": "second" });
    let batch = [
        ZoneEventInput {
            tick: 4,
            kind: "test.batch",
            actor_id: None,
            payload: &first_payload,
            schema_version: 1,
        },
        ZoneEventInput {
            tick: 4,
            kind: "test.batch",
            actor_id: None,
            payload: &second_payload,
            schema_version: 1,
        },
    ];
    assert_eq!(store.append_events(zone, "owner-b", token, &batch)?, [3, 4]);
    assert_eq!(
        store
            .events_after(zone, -1)?
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3, 4]
    );
    Ok(())
}

#[test]
#[allow(clippy::panic)] // A caught panic proves the scope guard cleans up during unwinding.
fn postgres_test_scope_cleans_rows_during_unwind() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    if std::env::var("LYNX_PG_PASSWORD").is_err() {
        anyhow::ensure!(
            std::env::var_os("LYNX_REQUIRE_POSTGRES_TEST").is_none(),
            "PostgreSQL test required but not configured"
        );
        eprintln!("skipping PostgreSQL cleanup test: LYNX_PG_PASSWORD is not configured");
        return Ok(());
    }

    let mut scope = PostgresTestScope::default();
    let world_id = scope.world_id();
    let zone_id = scope.zone_id();
    let check_world_id = world_id.clone();
    let check_zone_id = zone_id.clone();
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _scope = scope;
        let leases = PostgresZoneLeaseStore::from_env()
            .expect("lease store connects for the configured PostgreSQL test");
        let lease = leases
            .acquire(&zone_id, "cleanup-unwind-test", Duration::from_secs(30))
            .expect("the lease store acquires the isolated zone")
            .expect("the isolated test zone lease is available");
        let store = PostgresSnapshotStore::from_env(&world_id)
            .expect("snapshot store connects for the configured PostgreSQL test");
        store
            .bind_zone_lease(&zone_id, "cleanup-unwind-test", lease.fencing_token)
            .expect("the snapshot store binds to the isolated lease");
        let mut snapshot = authored_snapshot().expect("the authored fixture is valid");
        snapshot.zone.tick.0 = 1;
        store
            .save_at_tick(&snapshot, 1)
            .expect("the fixture snapshot is persisted before unwinding");
        panic!("simulate an assertion failure after writing PostgreSQL test rows");
    }));
    assert!(
        unwind.is_err(),
        "the test fixture must unwind after inserting its rows"
    );

    let config = lynx_world::persistence::postgres_config_from_env()?;
    let mut client = config.connect(postgres::NoTls)?;
    let leftovers: bool = client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM lynx.world_snapshots WHERE world_id = $1) OR EXISTS (SELECT 1 FROM lynx.zone_events WHERE zone_id = $2) OR EXISTS (SELECT 1 FROM lynx.zone_leases WHERE zone_id = $2)",
            &[&check_world_id, &check_zone_id],
        )?
        .get(0);
    assert!(
        !leftovers,
        "the scope guard must delete snapshot, event, and lease rows during unwind"
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
    let mut scope = PostgresTestScope::default();
    let zone = scope.zone_id();
    let store = PostgresZoneLeaseStore::from_env()?;
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
    let world_id = scope.world_id();
    let snapshots = PostgresSnapshotStore::from_env(&world_id)?;
    snapshots.bind_zone_lease(&zone, "owner-a", first.fencing_token)?;
    let snapshot = authored_snapshot()?;
    let mut snapshot = snapshot;
    snapshot.zone.tick.0 = 1;
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
    assert_event_batch_appends_contiguously(&store, &zone, second.fencing_token)?;
    let events = store.events_after(&zone, -1)?;
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3, 4]
    );
    assert!(store.release(&zone, "owner-b", second.fencing_token)?);
    snapshots.delete()?;
    scope.finish()?;
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

    let mut scope = PostgresTestScope::default();
    let zone = scope.zone_id();
    let world_id = scope.world_id();
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

    let legacy_zone = scope.zone_id();
    let legacy_world = scope.world_id();
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
    scope.finish()?;
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

    let mut scope = PostgresTestScope::default();
    let zone = scope.zone_id();
    let world_id = scope.world_id();
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
    let impossible_id = impossible_entity.id;
    let world = startup::load(Some(&store), false, &characters(1), 1_000)
        .expect("an impossible entity is quarantined instead of failing startup");
    assert_eq!(world.quarantined.len(), 1);
    assert_eq!(world.quarantined[0].entity, Some(impossible_id));
    assert_eq!(world.quarantined[0].section, "entity");
    assert!(
        world.zone.entity(impossible_id).is_none(),
        "the impossible entity is not adopted"
    );

    store.record_quarantine(&zone, &world.quarantined)?;
    let stored = store.quarantine_records(&zone)?;
    assert_eq!(stored, world.quarantined, "the report is durable");
    scope.finish()?;
    Ok(())
}

#[test]
#[allow(clippy::too_many_lines)] // One isolated fixture proves multi-tick recovery and retention end to end.
fn postgres_recovers_multi_tick_aggregates_and_prunes_replayed_history() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    if std::env::var("LYNX_PG_PASSWORD").is_err() {
        anyhow::ensure!(
            std::env::var_os("LYNX_REQUIRE_POSTGRES_TEST").is_none(),
            "PostgreSQL test required but not configured"
        );
        eprintln!("skipping PostgreSQL multi-tick recovery test: database is not configured");
        return Ok(());
    }

    let mut scope = PostgresTestScope::default();
    let zone = scope.zone_id();
    let world_id = scope.world_id();
    let owner = "multi-tick-recovery-test";
    let leases = PostgresZoneLeaseStore::from_env()?;
    let lease = leases
        .acquire(&zone, owner, std::time::Duration::from_secs(60))?
        .expect("isolated test zone lease is available");
    let store = open_bound_store(&world_id, &zone, owner, lease.fencing_token)?;

    let mut expected = authored_snapshot()?;
    expected.zone.tick.0 = 10;
    store.save_at_tick(&expected, 10)?;

    // Tick 11: move an entity, add a link, and arm an action cooldown.
    let source = expected.zone.entities[0].id;
    let target = expected.zone.entities[1].id;
    let mut moved = expected.zone.entities[0].clone();
    moved.transform.position.x += 0.5;
    expected.zone.entities[0] = moved.clone();
    expected.zone.links.push(EntityLink::new(
        DefinitionId::new("link.recovery-test")?,
        source,
        target,
        [LinkEvent::Activated],
    ));
    expected
        .zone
        .cooldowns
        .push(((source, Verb::Inspect, None), Tick(12)));

    let moved_actor = moved.id.to_string();
    let entity_payload = serde_json::json!({ "entity": moved });
    let links_payload = serde_json::json!({ "links": expected.zone.links });
    let cooldowns_payload = serde_json::json!({ "cooldowns": expected.zone.cooldowns });
    let tick_eleven = [
        ZoneEventInput {
            tick: 11,
            kind: EVENT_ENTITY_UPSERT,
            actor_id: Some(moved_actor.as_str()),
            payload: &entity_payload,
            schema_version: 1,
        },
        ZoneEventInput {
            tick: 11,
            kind: EVENT_LINKS_REPLACE,
            actor_id: None,
            payload: &links_payload,
            schema_version: 1,
        },
        ZoneEventInput {
            tick: 11,
            kind: EVENT_COOLDOWNS_REPLACE,
            actor_id: None,
            payload: &cooldowns_payload,
            schema_version: 1,
        },
    ];
    leases.append_events(&zone, owner, lease.fencing_token, &tick_eleven)?;

    // Tick 12: add an entity, arm a link cooldown, append a transfer receipt,
    // and replace the paired calendar/faction aggregates.
    let added = EntityRecord::new(
        StableId::from_parts(IdKind::Entity, 98, 1),
        DefinitionId::new("entity.recovery-test")?,
        Vec3::new(2.0, 0.0, 3.0),
    )
    .with_persistence(PersistencePolicy::PersistentEntity);
    expected.zone.entities.push(added.clone());
    expected.zone.entities.sort_by_key(|entity| entity.id);
    expected
        .zone
        .link_cooldowns
        .push(((source, target, LinkEvent::Activated), Tick(14)));
    let receipt = TransferReceipt {
        key: "multi-tick-transfer".to_owned(),
        source,
        source_slot: 0,
        destination: target,
        definition: DefinitionId::new("item.coin")?,
        quantity: 1,
        destination_slots: vec![0],
    };
    expected.zone.transfers.push(receipt.clone());
    expected
        .zone
        .transfers
        .sort_by(|left, right| left.key.cmp(&right.key));
    expected.zone.action_results.push((
        "multi-tick-action-cache".to_owned(),
        ActionResult {
            request_id: "multi-tick-action-cache".to_owned(),
            actor: source,
            verb: Verb::Inspect,
            server_tick: Tick(12),
            outcome: ActionOutcome::Applied {
                effects: Vec::new(),
            },
        },
    ));
    expected
        .zone
        .link_results
        .push(("multi-tick-link-cache".to_owned(), Vec::new()));
    expected.zone.applied_actions = 1;
    expected.zone.link_activations = 1;
    let mut factions = demo::build_faction_world(50_000)?;
    let calendar = demo::build_calendar(&mut factions, 50_000)?;
    expected.calendar = Some(calendar.snapshot());
    expected.factions = Some(factions.snapshot());
    expected.zone.tick.0 = 12;

    let added_actor = added.id.to_string();
    let added_payload = serde_json::json!({ "entity": added });
    let link_cooldowns_payload =
        serde_json::json!({ "link_cooldowns": expected.zone.link_cooldowns });
    let transfers_payload = serde_json::json!({ "transfers": [receipt] });
    let calendar_payload = serde_json::json!({
        "calendar": expected.calendar,
        "factions": expected.factions,
    });
    let action_results_payload =
        serde_json::json!({ "action_results": expected.zone.action_results });
    let link_results_payload = serde_json::json!({ "link_results": expected.zone.link_results });
    let counters_payload = serde_json::json!({
        "applied_actions": expected.zone.applied_actions,
        "link_activations": expected.zone.link_activations,
    });
    let tick_twelve = [
        ZoneEventInput {
            tick: 12,
            kind: EVENT_ENTITY_UPSERT,
            actor_id: Some(added_actor.as_str()),
            payload: &added_payload,
            schema_version: 1,
        },
        ZoneEventInput {
            tick: 12,
            kind: EVENT_LINK_COOLDOWNS_REPLACE,
            actor_id: None,
            payload: &link_cooldowns_payload,
            schema_version: 1,
        },
        ZoneEventInput {
            tick: 12,
            kind: EVENT_TRANSFERS_APPEND,
            actor_id: None,
            payload: &transfers_payload,
            schema_version: 1,
        },
        ZoneEventInput {
            tick: 12,
            kind: EVENT_CALENDAR_FACTIONS_REPLACE,
            actor_id: None,
            payload: &calendar_payload,
            schema_version: 1,
        },
        ZoneEventInput {
            tick: 12,
            kind: lynx_world::persistence::EVENT_ACTION_RESULTS_REPLACE,
            actor_id: None,
            payload: &action_results_payload,
            schema_version: 1,
        },
        ZoneEventInput {
            tick: 12,
            kind: lynx_world::persistence::EVENT_LINK_RESULTS_REPLACE,
            actor_id: None,
            payload: &link_results_payload,
            schema_version: 1,
        },
        ZoneEventInput {
            tick: 12,
            kind: lynx_world::persistence::EVENT_COUNTERS_REPLACE,
            actor_id: None,
            payload: &counters_payload,
            schema_version: 1,
        },
    ];
    leases.append_events(&zone, owner, lease.fencing_token, &tick_twelve)?;

    // Simulate a crash: drop the connection-bound store and recover through a
    // fresh store with the still-held lease.
    drop(store);
    let recovered_store = open_bound_store(&world_id, &zone, owner, lease.fencing_token)?;
    assert_eq!(
        recovered_store.load()?,
        Some(expected.clone()),
        "multi-tick entity, link, cooldown, transfer, calendar, and faction state survives the restart"
    );

    // A new checkpoint covers every earlier event, so retention keeps only the
    // checkpoint and recovery still returns the same world.
    recovered_store.save_at_tick(&expected, 12)?;
    let retained = leases.events_after(&zone, -1)?;
    assert_eq!(retained.len(), 1, "events before the checkpoint are pruned");
    assert_eq!(retained[0].kind, "world.snapshot");
    let checkpoint_sequence = retained[0].sequence;
    assert!(checkpoint_sequence > 0);
    assert_eq!(
        recovered_store.verify_recovery(&zone)?,
        SnapshotVerification::Verified {
            event_sequence: checkpoint_sequence,
            event_count: 1,
            saved_at_ms: expected.saved_at_ms,
            tick: 12,
        }
    );
    assert_eq!(recovered_store.load()?, Some(expected.clone()));

    // Deltas after the retained checkpoint still replay.
    let mut after = expected;
    after.zone.entities[0].transform.position.x += 0.25;
    after.zone.tick.0 = 13;
    let after_payload = serde_json::json!({ "entity": after.zone.entities[0] });
    let after_actor = after.zone.entities[0].id.to_string();
    leases.append_events(
        &zone,
        owner,
        lease.fencing_token,
        &[ZoneEventInput {
            tick: 13,
            kind: EVENT_ENTITY_UPSERT,
            actor_id: Some(after_actor.as_str()),
            payload: &after_payload,
            schema_version: 1,
        }],
    )?;
    drop(recovered_store);

    let final_store = open_bound_store(&world_id, &zone, owner, lease.fencing_token)?;
    assert_eq!(
        final_store.load()?,
        Some(after),
        "a delta after a pruned checkpoint still replays"
    );
    let events = leases.events_after(&zone, -1)?;
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].sequence, checkpoint_sequence);
    assert_eq!(events[1].kind, EVENT_ENTITY_UPSERT);

    final_store.delete()?;
    assert!(leases.release(&zone, owner, lease.fencing_token)?);
    scope.finish()?;
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
    let mut scope = PostgresTestScope::default();
    let zone = scope.zone_id();
    let store = PostgresZoneLeaseStore::from_env()?;
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
    scope.finish()?;
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

    let mut scope = PostgresTestScope::default();
    let world_id = scope.world_id();
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
    scope.finish()?;
    Ok(())
}
