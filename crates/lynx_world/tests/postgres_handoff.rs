//! PostgreSQL integration coverage for durable multi-zone handoff recovery.
//!
//! The test is restricted to `LYNX_PG_TEST_DATABASE`, a dedicated scratch
//! database migrated to head. It skips on offline workstations unless
//! `LYNX_REQUIRE_POSTGRES_TEST=1` is set.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use backend_nakama::postgres_pool::{PostgresPool, pool as postgres_pool};
use engine_content::{DisplayText, MapPosition, MapZone, RouteKind, WorldMapDefinition, ZoneRoute};
use engine_core::i18n::MessageKey;
use engine_core::id::{DefinitionId, IdKind, StableId};
use engine_core::inventory::UniformItemRules;
use engine_core::math::Vec3;
use engine_core::schema::{PersistencePolicy, SchemaVersion};
use engine_core::time::Tick;
use engine_sim::entity::EntityRecord;
use engine_sim::mind::Mind;
use engine_sim::nav::NavGrid;
use engine_sim::snapshot::ZoneSnapshot;
use engine_sim::zone::{Zone, ZoneConfig};
use lynx_world::auth::{Authenticator, digest};
use lynx_world::authority::ZoneAuthoritySet;
use lynx_world::persistence::{
    PostgresResumeSessionStore, PostgresZoneLeaseStore, PostgresZoneSnapshotStore, ZoneLeaseStore,
    ZoneSnapshotStore,
};
use lynx_world::travel::{PlayerHandoffRequest, ZoneHandoffCoordinator, ZoneHandoffStore};
use postgres::{Config, NoTls};
use world_gateway::{ResumeSessionRecord, ResumeSessionStore, TokenDigest};

static DATABASE_TEST_LOCK: Mutex<()> = Mutex::new(());
static ID_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct HandoffRowsGuard {
    config: Config,
    zone_ids: [String; 2],
    request_keys: Vec<String>,
    session_ids: Vec<String>,
    trigger_name: String,
    function_name: String,
}

impl Drop for HandoffRowsGuard {
    fn drop(&mut self) {
        let Ok(mut client) = self.config.connect(NoTls) else {
            eprintln!("PostgreSQL handoff integration-test cleanup connection failed");
            return;
        };
        let trigger_cleanup = format!(
            "DROP TRIGGER IF EXISTS {} ON lynx.zone_handoffs; DROP FUNCTION IF EXISTS lynx.{}();",
            self.trigger_name, self.function_name
        );
        if client.batch_execute(&trigger_cleanup).is_err() {
            eprintln!("PostgreSQL handoff failure trigger could not be cleaned");
        }
        let Ok(mut transaction) = client.transaction() else {
            eprintln!("PostgreSQL handoff integration-test cleanup transaction failed");
            return;
        };
        for request_key in &self.request_keys {
            if transaction
                .execute(
                    "DELETE FROM lynx.zone_handoffs WHERE request_key = $1",
                    &[request_key],
                )
                .is_err()
            {
                eprintln!("PostgreSQL handoff test rows could not be cleaned");
                return;
            }
        }
        for session_id in &self.session_ids {
            if transaction
                .execute(
                    "DELETE FROM lynx.resume_sessions WHERE session_id = $1",
                    &[session_id],
                )
                .is_err()
            {
                eprintln!("PostgreSQL session test rows could not be cleaned");
                return;
            }
        }
        for zone_id in &self.zone_ids {
            for table in [
                "lynx.zone_events",
                "lynx.zone_snapshots",
                "lynx.zone_leases",
                "lynx.recovery_quarantine",
            ] {
                let statement = format!("DELETE FROM {table} WHERE zone_id = $1");
                if transaction.execute(&statement, &[zone_id]).is_err() {
                    eprintln!("PostgreSQL handoff zone test rows could not be cleaned");
                    return;
                }
            }
        }
        if transaction.commit().is_err() {
            eprintln!("PostgreSQL handoff integration-test cleanup failed");
        }
    }
}

fn test_config() -> anyhow::Result<Option<Config>> {
    dotenvy::dotenv().ok();
    let database = match std::env::var("LYNX_PG_TEST_DATABASE") {
        Ok(database) if !database.trim().is_empty() => database,
        _ if std::env::var("LYNX_REQUIRE_POSTGRES_TEST").as_deref() == Ok("1") => {
            anyhow::bail!("LYNX_REQUIRE_POSTGRES_TEST=1 but LYNX_PG_TEST_DATABASE is missing")
        }
        _ => {
            eprintln!("skipping PostgreSQL handoff test: LYNX_PG_TEST_DATABASE is not configured");
            return Ok(None);
        }
    };
    let user = std::env::var("LYNX_PG_USER").context("set LYNX_PG_USER")?;
    let password = std::env::var("LYNX_PG_PASSWORD").context("set LYNX_PG_PASSWORD")?;
    let host = std::env::var("LYNX_PG_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned());
    let port = std::env::var("LYNX_PG_PORT")
        .unwrap_or_else(|_| "5432".to_owned())
        .parse::<u16>()?;
    let mut config = Config::new();
    config
        .host(&host)
        .port(port)
        .dbname(&database)
        .user(&user)
        .password(&password)
        .application_name("lynx-postgres-handoff-integration-test")
        .connect_timeout(Duration::from_secs(5))
        .options(
            "-c statement_timeout=5000 -c lock_timeout=2000 -c idle_in_transaction_session_timeout=5000",
        );
    Ok(Some(config))
}

fn zone_id() -> String {
    let millis = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX);
    let sequence = ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    StableId::from_parts(IdKind::Zone, millis, u128::from(sequence)).to_string()
}

fn make_zone(definition: DefinitionId, instance: StableId) -> anyhow::Result<Zone> {
    let config = ZoneConfig::new(definition, instance, Vec3::ZERO, Vec3::new(20.0, 4.0, 20.0))?;
    let nav = NavGrid::new(20, 20, 1.0, Vec3::ZERO).context("creating test navigation grid")?;
    Ok(Zone::new(config, nav, Arc::new(UniformItemRules::new(99)))?)
}

fn make_map(source: DefinitionId, destination: DefinitionId) -> anyhow::Result<WorldMapDefinition> {
    Ok(WorldMapDefinition {
        id: DefinitionId::new("map.postgres_handoff_test")?,
        version: SchemaVersion::new(1, 0),
        display: DisplayText {
            name: MessageKey::parse("map.postgres_handoff_test.name")?,
            description: MessageKey::parse("map.postgres_handoff_test.description")?,
        },
        zones: vec![
            MapZone {
                zone: source.clone(),
                position: MapPosition { x: 0, y: 0 },
            },
            MapZone {
                zone: destination.clone(),
                position: MapPosition { x: 1, y: 0 },
            },
        ],
        routes: vec![ZoneRoute {
            from: source,
            to: destination,
            bidirectional: false,
            kind: RouteKind::Gateway,
        }],
    })
}

fn bound_snapshot_store(
    pool: PostgresPool,
    authorities: &ZoneAuthoritySet,
    zone_ids: [&str; 2],
) -> anyhow::Result<PostgresZoneSnapshotStore> {
    let store = PostgresZoneSnapshotStore::with_pool(pool)?;
    for zone_id in zone_ids {
        let authority = authorities
            .get(zone_id)
            .context("zone authority was not acquired")?;
        store.bind_zone_lease(zone_id, authority.owner(), authority.fencing_token())?;
    }
    Ok(store)
}

fn restore_snapshot(snapshot: &ZoneSnapshot) -> anyhow::Result<Zone> {
    let nav = NavGrid::new(20, 20, 1.0, Vec3::ZERO).context("creating recovery navigation grid")?;
    Ok(Zone::restore(
        snapshot,
        nav,
        Arc::new(UniformItemRules::new(99)),
    )?)
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one isolated PostgreSQL scenario checks atomic handoff, receipt, route, and independent zone recovery"
)]
fn postgres_handoff_commits_both_zones_receipt_and_resume_route_for_recovery() -> anyhow::Result<()>
{
    let _lock = DATABASE_TEST_LOCK.lock().unwrap();
    let Some(config) = test_config()? else {
        return Ok(());
    };

    let source_zone_id = zone_id();
    let destination_zone_id = zone_id();
    let sequence = ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .saturating_add(u128::from(sequence));
    let request_key = format!("pg-handoff-{suffix}");
    let failing_request_key = format!("pg-handoff-fail-{suffix}");
    let session_id = format!("pg-handoff-session-{suffix}");
    let failing_session_id = format!("pg-handoff-fail-session-{suffix}");
    let owner = format!("pg-handoff-owner-{suffix}");
    let trigger_name = format!("fail_handoff_receipt_{suffix}");
    let function_name = format!("fail_handoff_receipt_{suffix}");
    let _cleanup = HandoffRowsGuard {
        config: config.clone(),
        zone_ids: [source_zone_id.clone(), destination_zone_id.clone()],
        request_keys: vec![request_key.clone(), failing_request_key.clone()],
        session_ids: vec![session_id.clone(), failing_session_id.clone()],
        trigger_name: trigger_name.clone(),
        function_name: function_name.clone(),
    };

    let pool = postgres_pool(config.clone(), 8, Duration::from_secs(3));
    let lease_store: Arc<dyn ZoneLeaseStore> =
        Arc::new(PostgresZoneLeaseStore::with_pool(pool.clone())?);
    let authorities = ZoneAuthoritySet::acquire(
        &lease_store,
        [&source_zone_id, &destination_zone_id],
        &owner,
        Duration::from_secs(30),
    )?
    .context("both isolated test zone leases must be available")?;
    let snapshot_store = bound_snapshot_store(
        pool.clone(),
        &authorities,
        [&source_zone_id, &destination_zone_id],
    )?;
    let resume_store = PostgresResumeSessionStore::with_pool(pool.clone())?;

    let grant = Authenticator::new(&[digest(&session_id)])?
        .grants()
        .next()
        .context("test authenticator must issue a character grant")?
        .clone();
    let source_instance = StableId::parse(&source_zone_id)?;
    let destination_instance = StableId::parse(&destination_zone_id)?;
    let source_definition = DefinitionId::new("zone.postgres_handoff_source")?;
    let destination_definition = DefinitionId::new("zone.postgres_handoff_destination")?;
    let actor = StableId::from_parts(IdKind::Entity, 1, grant.principal.random_bits());
    let mut source = make_zone(source_definition.clone(), source_instance)?;
    let mut destination = make_zone(destination_definition.clone(), destination_instance)?;
    source.spawn(
        EntityRecord::new(
            actor,
            DefinitionId::new("character.postgres_handoff_player")?,
            Vec3::new(1.5, 0.0, 1.5),
        )
        .with_health(100)
        .with_mind(Mind::player(grant.principal, "unbound"))
        .with_persistence(PersistencePolicy::PersistentEntity),
    )?;
    let failing_grant = Authenticator::new(&[digest(&failing_session_id)])?
        .grants()
        .next()
        .context("test authenticator must issue a second character grant")?
        .clone();
    let failing_actor =
        StableId::from_parts(IdKind::Entity, 1, failing_grant.principal.random_bits());
    source.spawn(
        EntityRecord::new(
            failing_actor,
            DefinitionId::new("character.postgres_handoff_player")?,
            Vec3::new(3.5, 0.0, 1.5),
        )
        .with_health(100)
        .with_mind(Mind::player(failing_grant.principal, "unbound"))
        .with_persistence(PersistencePolicy::PersistentEntity),
    )?;

    let original_token = TokenDigest::of("postgres-handoff-original-token");
    resume_store.create(&ResumeSessionRecord {
        session_id,
        zone_id: source_zone_id.clone(),
        account_id: grant.account.to_string(),
        principal_id: grant.principal.to_string(),
        actor_id: actor.to_string(),
        staff: false,
        token_digest: original_token,
        expires_at_ms: None,
        connected: true,
        generation: 0,
        revoked: false,
    })?;
    let failing_token = TokenDigest::of("postgres-handoff-failing-token");
    resume_store.create(&ResumeSessionRecord {
        session_id: failing_session_id,
        zone_id: source_zone_id.clone(),
        account_id: failing_grant.account.to_string(),
        principal_id: failing_grant.principal.to_string(),
        actor_id: failing_actor.to_string(),
        staff: false,
        token_digest: failing_token,
        expires_at_ms: None,
        connected: true,
        generation: 0,
        revoked: false,
    })?;

    let atlas = make_map(source_definition, destination_definition)?;
    let arrival = Vec3::new(6.5, 0.0, 6.5);
    let request = PlayerHandoffRequest::new(grant.principal, arrival, request_key.clone());
    let mut coordinator = ZoneHandoffCoordinator::default();
    let receipt = coordinator.handoff_player_durable(
        &atlas,
        &authorities,
        &mut source,
        &mut destination,
        &request,
        &snapshot_store,
    )?;

    anyhow::ensure!(
        snapshot_store.handoff_receipt(&request_key)? == Some(receipt.clone()),
        "the transaction must persist its idempotency receipt"
    );
    anyhow::ensure!(
        resume_store
            .lookup_token(original_token)?
            .is_some_and(|record| record.zone_id == destination_zone_id),
        "the durable resume route must move with the handoff"
    );

    // Fail on the final receipt insert, after both snapshot writes and the
    // resume-route update have run in the transaction. The trigger is scoped
    // to this unique request key and is always removed by the cleanup guard.
    let before_failed_source = snapshot_store
        .load_zone(&source_zone_id)?
        .context("the successful handoff must leave a source checkpoint")?;
    let before_failed_destination = snapshot_store
        .load_zone(&destination_zone_id)?
        .context("the successful handoff must leave a destination checkpoint")?;
    let mut ddl = config.connect(NoTls)?;
    ddl.batch_execute(&format!(
        "CREATE FUNCTION lynx.{function_name}() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN IF NEW.request_key = '{failing_request_key}' THEN \
         RAISE EXCEPTION 'injected handoff receipt failure'; END IF; RETURN NEW; END; $$; \
         CREATE TRIGGER {trigger_name} BEFORE INSERT ON lynx.zone_handoffs \
         FOR EACH ROW EXECUTE FUNCTION lynx.{function_name}();"
    ))?;
    let failing_request = PlayerHandoffRequest::new(
        failing_grant.principal,
        Vec3::new(10.5, 0.0, 10.5),
        failing_request_key.clone(),
    );
    anyhow::ensure!(
        coordinator
            .handoff_player_durable(
                &atlas,
                &authorities,
                &mut source,
                &mut destination,
                &failing_request,
                &snapshot_store,
            )
            .is_err(),
        "a failure on the receipt insert must abort the durable handoff"
    );
    anyhow::ensure!(
        source.entity(failing_actor).is_some() && destination.entity(failing_actor).is_none(),
        "the failed durable handoff must not mutate either live zone"
    );
    anyhow::ensure!(
        snapshot_store.load_zone(&source_zone_id)? == Some(before_failed_source),
        "a failed handoff must roll back its staged source checkpoint"
    );
    anyhow::ensure!(
        snapshot_store.load_zone(&destination_zone_id)? == Some(before_failed_destination),
        "a failed handoff must roll back its staged destination checkpoint"
    );
    anyhow::ensure!(
        resume_store
            .lookup_token(failing_token)?
            .is_some_and(|record| record.zone_id == source_zone_id),
        "a failed handoff must leave the durable resume route at the source"
    );
    anyhow::ensure!(
        snapshot_store
            .handoff_receipt(&failing_request_key)?
            .is_none(),
        "a failed handoff must not persist a receipt"
    );

    // Release and reacquire both leases under a new owner before recovery, as a
    // replacement service process would after the previous owner shuts down.
    anyhow::ensure!(
        authorities.release_all()?,
        "the original owner must release both zone leases"
    );
    let restarted_owner = format!("pg-handoff-restart-{suffix}");
    let restarted_authorities = ZoneAuthoritySet::acquire(
        &lease_store,
        [&source_zone_id, &destination_zone_id],
        &restarted_owner,
        Duration::from_secs(30),
    )?
    .context("replacement service must acquire both released zone leases")?;
    let restarted_snapshot_store = bound_snapshot_store(
        pool.clone(),
        &restarted_authorities,
        [&source_zone_id, &destination_zone_id],
    )?;
    let destination_authority = restarted_authorities
        .get(&destination_zone_id)
        .context("replacement destination authority must be held")?;
    let now_ms = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(i64::MAX - 60_000);
    let expires_at_ms = now_ms.saturating_add(60_000);
    anyhow::ensure!(
        resume_store.detach_stale_connected(
            &destination_zone_id,
            destination_authority.owner(),
            destination_authority.fencing_token(),
            expires_at_ms,
        )? == 1,
        "replacement startup must detach the transferred session under its destination lease"
    );
    let rotated_token = TokenDigest::of("postgres-handoff-restarted-token");
    let resumed_record = resume_store
        .consume_and_rotate(original_token, rotated_token, now_ms)?
        .context("the destination-routed session must resume after restart")?;
    anyhow::ensure!(
        resumed_record.zone_id == destination_zone_id
            && resumed_record.connected
            && resumed_record.token_digest == rotated_token,
        "restart resume must retain the destination route and rotate the token"
    );

    let source_snapshot = restarted_snapshot_store
        .load_zone(&source_zone_id)?
        .context("handoff transaction must persist the source snapshot")?;
    let destination_snapshot = restarted_snapshot_store
        .load_zone(&destination_zone_id)?
        .context("handoff transaction must persist the destination snapshot")?;
    anyhow::ensure!(
        source_snapshot.tick == Tick::ZERO && destination_snapshot.tick == Tick::ZERO,
        "both durable zone checkpoints must load at their committed ticks"
    );

    // Rebuild each zone independently, as separate authorities would after a
    // process restart. The character must exist only in the destination.
    let mut recovered_source = restore_snapshot(&source_snapshot)?;
    let mut recovered_destination = restore_snapshot(&destination_snapshot)?;
    anyhow::ensure!(
        recovered_source.entity(actor).is_none(),
        "the recovered source zone must not contain the transferred actor"
    );
    let recovered_actor = recovered_destination
        .entity(actor)
        .context("the recovered destination must contain the transferred actor")?;
    anyhow::ensure!(
        recovered_actor.position() == arrival,
        "the recovered destination must preserve the safe arrival position"
    );

    // A retry after a simulated restart resolves through the durable receipt
    // instead of transferring a second time.
    let mut recovered_coordinator = ZoneHandoffCoordinator::default();
    let retried = recovered_coordinator.handoff_player_durable(
        &atlas,
        &restarted_authorities,
        &mut recovered_source,
        &mut recovered_destination,
        &request,
        &restarted_snapshot_store,
    )?;
    anyhow::ensure!(
        retried == receipt,
        "durable retries return the same receipt"
    );
    anyhow::ensure!(
        recovered_destination.entity(actor).is_some() && recovered_source.entity(actor).is_none(),
        "retry must leave exactly one actor in the destination"
    );
    Ok(())
}
