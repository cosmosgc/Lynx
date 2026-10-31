//! PostgreSQL integration coverage for the normalized faction/calendar store.
//!
//! These tests are deliberately restricted to `LYNX_PG_TEST_DATABASE`; they
//! never resolve the application database name from `LYNX_PG_DATABASE`.
//! Because the normalized aggregates are global and their histories are
//! append-only, point this at a dedicated scratch database migrated to head.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, ensure};
use backend_nakama::PostgresFactionCalendarStore;
use lynx_world::demo;
use postgres::{Client, Config, NoTls};

fn test_config() -> anyhow::Result<Option<Config>> {
    dotenvy::dotenv().ok();
    let database = match std::env::var("LYNX_PG_TEST_DATABASE") {
        Ok(database) if !database.trim().is_empty() => database,
        _ if std::env::var("LYNX_REQUIRE_POSTGRES_TEST").as_deref() == Ok("1") => {
            anyhow::bail!(
                "LYNX_REQUIRE_POSTGRES_TEST=1 but LYNX_PG_TEST_DATABASE is missing; set it to a dedicated scratch database migrated to head"
            )
        }
        _ => {
            eprintln!(
                "skipping PostgreSQL faction/calendar test: LYNX_PG_TEST_DATABASE is not configured"
            );
            return Ok(None);
        }
    };

    let user = std::env::var("LYNX_PG_USER")
        .context("set LYNX_PG_USER for the dedicated PostgreSQL scratch database")?;
    let password = std::env::var("LYNX_PG_PASSWORD")
        .context("set LYNX_PG_PASSWORD for the dedicated PostgreSQL scratch database")?;
    let host = std::env::var("LYNX_PG_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned());
    let port = std::env::var("LYNX_PG_PORT")
        .unwrap_or_else(|_| "5432".to_owned())
        .parse::<u16>()
        .context("LYNX_PG_PORT must be a valid port")?;

    let mut config = Config::new();
    config
        .host(&host)
        .port(port)
        .dbname(&database)
        .user(&user)
        .password(&password)
        .application_name("lynx-faction-calendar-integration-test")
        .connect_timeout(std::time::Duration::from_secs(5));
    Ok(Some(config))
}

fn connect(config: &Config) -> anyhow::Result<Client> {
    config
        .connect(NoTls)
        .context("could not connect to LYNX_PG_TEST_DATABASE")
}

struct FailureTriggerGuard {
    config: Config,
    trigger: String,
    function: String,
}

struct ScratchStateGuard {
    config: Config,
    zone_id: String,
    owner: String,
    token: i64,
}

impl Drop for ScratchStateGuard {
    fn drop(&mut self) {
        if let Ok(store) = PostgresFactionCalendarStore::connect(&self.config) {
            let _ = store.bind_zone_lease(&self.zone_id, &self.owner, self.token);
            let _ = store.reset_world_state(
                &engine_core::world_event::CalendarSnapshot::default(),
                &engine_core::faction::FactionSnapshot::default(),
            );
        }
        if let Ok(mut client) = connect(&self.config) {
            let _ = client.execute(
                "DELETE FROM lynx.zone_leases WHERE zone_id = $1 AND owner = $2 AND fencing_token = $3",
                &[&self.zone_id, &self.owner, &self.token],
            );
        }
    }
}

impl Drop for FailureTriggerGuard {
    fn drop(&mut self) {
        if let Ok(mut client) = connect(&self.config) {
            let _ = client.batch_execute(&format!(
                "DROP TRIGGER IF EXISTS {} ON lynx.world_events; DROP FUNCTION IF EXISTS lynx.{}()",
                self.trigger, self.function
            ));
        }
    }
}

fn install_world_event_failure_trigger(config: &Config) -> anyhow::Result<FailureTriggerGuard> {
    let suffix = unique_suffix();
    let function = format!("pst003_test_fail_{suffix}");
    let trigger = format!("pst003_test_fail_{suffix}");
    let mut client = connect(config)?;
    client.batch_execute(&format!(
        "CREATE FUNCTION lynx.{function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'PST-003 injected failure'; END; $$; CREATE TRIGGER {trigger} BEFORE UPDATE ON lynx.world_events FOR EACH ROW EXECUTE FUNCTION lynx.{function}()"
    ))?;
    Ok(FailureTriggerGuard {
        config: config.clone(),
        trigger,
        function,
    })
}

fn unique_suffix() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos()
        .to_string()
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "the isolated integration scenario deliberately verifies one complete lifecycle"
)]
fn normalized_faction_calendar_roundtrip_is_atomic_and_fenced() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let Some(config) = test_config()? else {
        return Ok(());
    };

    let mut client = connect(&config)?;
    let state_exists: bool = client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM lynx.factions) OR EXISTS (SELECT 1 FROM lynx.faction_territories) OR EXISTS (SELECT 1 FROM lynx.faction_territory_history) OR EXISTS (SELECT 1 FROM lynx.faction_treasury_entries) OR EXISTS (SELECT 1 FROM lynx.faction_treasury_transfers) OR EXISTS (SELECT 1 FROM lynx.world_events) OR EXISTS (SELECT 1 FROM lynx.world_event_transitions) OR EXISTS (SELECT 1 FROM lynx.world_event_enrollments) OR EXISTS (SELECT 1 FROM lynx.world_event_results)",
            &[],
        )?
        .get(0);
    ensure!(
        !state_exists,
        "LYNX_PG_TEST_DATABASE must be a dedicated scratch database with empty normalized faction/calendar state; do not clear append-only history"
    );

    // A lone normalized row must not be interpreted as an absent world. Keep
    // it short-lived and remove only this non-history row after the assertion.
    let partial_id = format!("faction.partial{}", unique_suffix());
    client.execute(
        "INSERT INTO lynx.factions (faction_id, display_name, controller, max_members, content_version_major, content_version_minor, power_version_major, power_version_minor) VALUES ($1, 'Partial fixture', 'system', 1, 1, 0, 1, 0)",
        &[&partial_id],
    )?;
    let partial_store = PostgresFactionCalendarStore::connect(&config)
        .map_err(|error| anyhow::anyhow!("creating faction/calendar store: {error}"))?;
    ensure!(
        partial_store.load_world_state().is_err(),
        "partial normalized state must fail closed"
    );
    client.execute(
        "DELETE FROM lynx.factions WHERE faction_id = $1",
        &[&partial_id],
    )?;
    drop(partial_store);

    // Construct the authored pair using public world helpers, not hand-built
    // database rows or private codec details.
    let now_ms = 1_000_000;
    let mut factions = demo::build_faction_world(now_ms)?;
    let mut calendar = demo::build_calendar(&mut factions, now_ms)?;
    let calendar_snapshot = calendar.snapshot();
    let faction_snapshot = factions.snapshot();
    let suffix = unique_suffix();
    let zone_id = format!("zone_{suffix:0>26}");
    let owner = "faction-calendar-integration-test";
    let lease = client.query_one(
        "INSERT INTO lynx.zone_leases (zone_id, owner, expires_at) VALUES ($1, $2, clock_timestamp() + interval '60 seconds') RETURNING fencing_token",
        &[&zone_id, &owner],
    )?;
    let token: i64 = lease.get(0);
    let store = PostgresFactionCalendarStore::connect(&config)
        .map_err(|error| anyhow::anyhow!("creating faction/calendar store: {error}"))?;
    let unbound = PostgresFactionCalendarStore::connect(&config)
        .map_err(|error| anyhow::anyhow!("creating unbound store: {error}"))?;
    ensure!(
        matches!(
            unbound.replace_world_state(&calendar_snapshot, &faction_snapshot),
            Err(game_api::ApiError::Conflict(_))
        ),
        "normalized writes require a bound lease"
    );
    store
        .bind_zone_lease(&zone_id, owner, token)
        .map_err(|error| anyhow::anyhow!("binding test lease: {error}"))?;
    let _scratch_guard = ScratchStateGuard {
        config: config.clone(),
        zone_id: zone_id.clone(),
        owner: owner.to_owned(),
        token,
    };

    store
        .replace_world_state(&calendar_snapshot, &faction_snapshot)
        .map_err(|error| anyhow::anyhow!("writing normalized authored state: {error}"))?;
    ensure!(
        store
            .verify_world_state(&calendar_snapshot, &faction_snapshot)
            .map_err(|error| anyhow::anyhow!("verifying normalized authored state: {error}"))?,
        "normalized state verifies against the authored pair"
    );
    let history_key = &faction_snapshot.territory_history[0].key;
    ensure!(
        client
            .execute(
                "UPDATE lynx.faction_territory_history SET action = action WHERE change_key = $1",
                &[history_key],
            )
            .is_err(),
        "append-only territory history rejects direct updates"
    );
    ensure!(
        client
            .execute(
                "DELETE FROM lynx.faction_territory_history WHERE change_key = $1",
                &[history_key],
            )
            .is_err(),
        "append-only territory history rejects direct deletes"
    );

    // A structurally invalid pair must roll back without changing either
    // aggregate. Default calendar state cannot describe this authored faction
    // world and is rejected by public snapshot validation.
    let invalid_calendar = engine_core::world_event::CalendarSnapshot::default();
    ensure!(
        store
            .replace_world_state(&invalid_calendar, &faction_snapshot)
            .is_err(),
        "mismatched paired state must be rejected"
    );
    let mut rewritten_history = faction_snapshot.clone();
    rewritten_history.territory_history[0].key = "claim-demo-rewritten".into();
    ensure!(
        store
            .replace_world_state(&calendar_snapshot, &rewritten_history)
            .is_err(),
        "an ordinary replacement must reject rewritten append-only territory history"
    );
    ensure!(
        store
            .load_world_state()
            .map_err(|error| anyhow::anyhow!("loading after rejected pair: {error}"))?
            == Some((calendar_snapshot.clone(), faction_snapshot.clone())),
        "a rejected paired write leaves both aggregates unchanged"
    );

    let event = engine_core::id::DefinitionId::new("event.assalto_praca")?;
    let attacker = engine_core::StableId::from_parts(engine_core::IdKind::Character, 1, 1);
    calendar.enroll(
        &factions,
        &event,
        attacker,
        &engine_core::id::DefinitionId::new("side.assaltantes")?,
        engine_core::time::Millis::new(now_ms + 1),
    )?;
    calendar.activate(
        &mut factions,
        &event,
        engine_core::StableId::from_parts(engine_core::IdKind::Character, 2, 1),
        engine_core::time::Millis::new(now_ms + 60_000),
    )?;
    calendar.resolve(
        &mut factions,
        &event,
        engine_core::StableId::from_parts(engine_core::IdKind::Character, 2, 1),
        engine_core::time::Millis::new(now_ms + 120_000),
    )?;
    let resolved_calendar = calendar.snapshot();
    let resolved_factions = factions.snapshot();
    store
        .replace_world_state(&resolved_calendar, &resolved_factions)
        .map_err(|error| anyhow::anyhow!("persisting enrollment and resolution: {error}"))?;
    ensure!(
        store
            .load_world_state()
            .map_err(|error| anyhow::anyhow!("loading resolution state: {error}"))?
            == Some((resolved_calendar.clone(), resolved_factions.clone())),
        "enrollment, result sides, resolution, and territory capture round-trip together"
    );

    let failure_guard = install_world_event_failure_trigger(&config)?;
    let mut failed_calendar = resolved_calendar.clone();
    failed_calendar.events[0]
        .title
        .push_str(" (must roll back)");
    let mut failed_factions = resolved_factions.clone();
    failed_factions.factions[0]
        .definition
        .display_name
        .push_str(" (must roll back)");
    ensure!(
        store
            .replace_world_state(&failed_calendar, &failed_factions)
            .is_err(),
        "an injected database failure rejects the paired update"
    );
    ensure!(
        store
            .load_world_state()
            .map_err(|error| anyhow::anyhow!("loading after injected failure: {error}"))?
            == Some((resolved_calendar.clone(), resolved_factions.clone())),
        "failure after faction updates rolls back the whole pair"
    );
    drop(failure_guard);

    // A distinct connection models process restart and must decode normalized
    // rows independently of the writer's in-memory state.
    drop(store);
    let restarted = PostgresFactionCalendarStore::connect(&config)
        .map_err(|error| anyhow::anyhow!("reopening faction/calendar store: {error}"))?;
    ensure!(
        restarted
            .load_world_state()
            .map_err(|error| anyhow::anyhow!("loading after restart: {error}"))?
            == Some((resolved_calendar.clone(), resolved_factions.clone())),
        "a fresh store loads the same normalized pair"
    );

    // A lease binding with a stale token fences writes. The scratch DB is
    // migrated to head, so the fixture lease uses the existing lease table.
    let stale = PostgresFactionCalendarStore::connect(&config)
        .map_err(|error| anyhow::anyhow!("creating stale-fence store: {error}"))?;
    stale
        .bind_zone_lease(&zone_id, owner, token + 1)
        .map_err(|error| anyhow::anyhow!("binding stale lease token: {error}"))?;
    let stale_write = stale.replace_world_state(&calendar_snapshot, &faction_snapshot);
    ensure!(
        matches!(stale_write, Err(game_api::ApiError::Conflict(_))),
        "a stale lease token must fence normalized writes with a conflict"
    );
    drop(stale);
    restarted
        .bind_zone_lease(&zone_id, owner, token)
        .map_err(|error| anyhow::anyhow!("binding reset lease: {error}"))?;
    restarted
        .reset_world_state(
            &engine_core::world_event::CalendarSnapshot::default(),
            &engine_core::faction::FactionSnapshot::default(),
        )
        .map_err(|error| anyhow::anyhow!("fenced fresh reset: {error}"))?;
    ensure!(
        restarted
            .load_world_state()
            .map_err(|error| anyhow::anyhow!("loading after fresh reset: {error}"))?
            .is_none(),
        "the fenced fresh reset returns normalized state to its empty bootstrap"
    );
    Ok(())
}
