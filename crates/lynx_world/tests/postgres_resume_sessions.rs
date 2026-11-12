//! PostgreSQL integration coverage for durable resume-session rotation.
//!
//! This test uses only `LYNX_PG_TEST_DATABASE`, following the isolated scratch
//! database convention used by the other PostgreSQL integration tests.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use engine_core::time::{DurationMs, Millis, Tick};
use lynx_world::auth::{Authenticator, digest};
use lynx_world::authority::ZoneAuthority;
use lynx_world::demo;
use lynx_world::persistence::{PostgresResumeSessionStore, PostgresZoneLeaseStore};
use lynx_world::service::{Credential, Hello, ServerEvent, ServerFrame, Service};
use lynx_world::transport::{AppState, event_channel};
use postgres::{Config, NoTls};
use world_gateway::{
    GatewayConfig, ResumeSessionRecord, ResumeSessionStore, SystemResumeTokens, TokenDigest,
    WorldGateway,
};
use world_server::WorldServer;

static DATABASE_TEST_LOCK: Mutex<()> = Mutex::new(());

struct RowGuard {
    config: Config,
    session_id: String,
}

impl Drop for RowGuard {
    fn drop(&mut self) {
        if let Ok(mut client) = self.config.connect(NoTls) {
            let _ = client.execute(
                "DELETE FROM lynx.resume_sessions WHERE session_id = $1",
                &[&self.session_id],
            );
        }
    }
}

struct LeaseGuard {
    config: Config,
    zone_id: String,
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        if let Ok(mut client) = self.config.connect(NoTls) {
            let _ = client.execute(
                "DELETE FROM lynx.zone_leases WHERE zone_id = $1",
                &[&self.zone_id],
            );
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
            eprintln!(
                "skipping PostgreSQL resume-session test: LYNX_PG_TEST_DATABASE is not configured"
            );
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
        .application_name("lynx-resume-session-integration-test")
        .connect_timeout(Duration::from_secs(5))
        .options(
            "-c statement_timeout=5000 -c lock_timeout=2000 -c idle_in_transaction_session_timeout=5000",
        );
    Ok(Some(config))
}

fn unique_suffix() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .to_string()
}

/// Wait until a terminated PostgreSQL backend has actually left the server.
///
/// `pg_terminate_backend` returns before the target process exits, so a retry
/// that starts immediately can still block on the terminated backend's row
/// locks. Waiting removes that race from the rollback test.
fn wait_for_backend_exit(client: &mut postgres::Client, pid: i32) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let remaining: i64 = client
            .query_one(
                "SELECT count(*) FROM pg_stat_activity WHERE pid = $1",
                &[&pid],
            )?
            .get(0);
        if remaining == 0 {
            return Ok(());
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "a terminated PostgreSQL backend did not exit in time"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one isolated database scenario covers the complete token lifecycle"
)]
fn postgres_resume_token_rotation_is_single_use_and_generation_fenced() -> anyhow::Result<()> {
    let _lock = DATABASE_TEST_LOCK.lock().unwrap();
    let Some(config) = test_config()? else {
        return Ok(());
    };
    let suffix = unique_suffix();
    let session_id = format!("resume-test-{suffix}");
    let original = "a raw bearer token that must not be stored";
    let old_digest = TokenDigest::of(original);
    let record = ResumeSessionRecord {
        session_id: session_id.clone(),
        zone_id: "resume-test-zone".to_owned(),
        account_id: "account-test".to_owned(),
        principal_id: "character-test".to_owned(),
        actor_id: "actor-test".to_owned(),
        staff: true,
        token_digest: old_digest,
        expires_at_ms: None,
        connected: true,
        generation: 0,
        revoked: false,
    };
    let store = PostgresResumeSessionStore::connect(&config)
        .map_err(|error| anyhow::anyhow!("connecting resume-session store: {error}"))?;
    let mut cleanup = config.connect(NoTls)?;
    let _guard = RowGuard {
        config: config.clone(),
        session_id: session_id.clone(),
    };
    store
        .create(&record)
        .map_err(|error| anyhow::anyhow!("creating resume-session record: {error}"))?;

    anyhow::ensure!(
        store
            .consume_and_rotate(old_digest, TokenDigest::of("connected rotation"), 10_000)
            .unwrap_err()
            .code()
            == engine_core::error::ErrorCode::Conflict,
        "a token belonging to an already-connected session is rejected as a conflict"
    );

    // No raw token column/value is persisted: the row contains only its digest.
    let stored: Vec<u8> = cleanup
        .query_one(
            "SELECT token_digest FROM lynx.resume_sessions WHERE session_id = $1",
            &[&session_id],
        )?
        .get(0);
    anyhow::ensure!(
        stored.as_slice() == old_digest.as_bytes(),
        "database must contain the token digest"
    );
    let raw_token_column_exists: bool = cleanup.query_one(
        "SELECT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema = 'lynx' AND table_name = 'resume_sessions' AND column_name IN ('token', 'raw_token'))",
        &[],
    )?.get(0);
    anyhow::ensure!(
        !raw_token_column_exists,
        "the schema must not provide a raw-token column"
    );

    anyhow::ensure!(
        store.detach(&session_id, 0, 20_000)?,
        "current generation detaches"
    );
    anyhow::ensure!(
        !store.detach(&session_id, 0, 30_000)?,
        "stale generation cannot detach again"
    );

    // Two independent connections race the same detached token. The conditional
    // update is the claim: exactly one caller can rotate it.
    let config_a = config.clone();
    let config_b = config.clone();
    let digest_a = TokenDigest::of("rotated token A");
    let digest_b = TokenDigest::of("rotated token B");
    let first = std::thread::spawn(move || {
        let store = PostgresResumeSessionStore::connect(&config_a).unwrap();
        store
            .consume_and_rotate(old_digest, digest_a, 10_000)
            .unwrap()
            .is_some()
    });
    let second = std::thread::spawn(move || {
        let store = PostgresResumeSessionStore::connect(&config_b).unwrap();
        store
            .consume_and_rotate(old_digest, digest_b, 10_000)
            .unwrap()
            .is_some()
    });
    let first_won = first.join().unwrap();
    let second_won = second.join().unwrap();
    anyhow::ensure!(
        first_won ^ second_won,
        "concurrent reuse must have exactly one winner"
    );
    anyhow::ensure!(
        store
            .consume_and_rotate(old_digest, TokenDigest::of("third token"), 10_000)?
            .is_none(),
        "consumed digest is rejected"
    );

    let generation: i64 = cleanup
        .query_one(
            "SELECT generation FROM lynx.resume_sessions WHERE session_id = $1",
            &[&session_id],
        )?
        .get(0);
    anyhow::ensure!(
        generation == 2,
        "detach and successful rotation both advance the fence"
    );
    anyhow::ensure!(
        store.revoke(&session_id, generation)?,
        "matching generation revokes"
    );
    anyhow::ensure!(
        !store.revoke(&session_id, generation)?,
        "revoked session cannot be revoked with stale generation"
    );
    Ok(())
}

#[test]
fn postgres_principal_revocation_flows_through_app_state_and_service() -> anyhow::Result<()> {
    let _lock = DATABASE_TEST_LOCK.lock().unwrap();
    let Some(config) = test_config()? else {
        return Ok(());
    };
    let grant = Authenticator::new(&[digest("postgres-service-revocation-token")])?
        .grants()
        .next()
        .context("revocation test grant")?
        .clone();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let resume_store = Arc::new(
        runtime
            .block_on(async {
                tokio::task::block_in_place(|| PostgresResumeSessionStore::connect(&config))
            })
            .map_err(|error| anyhow::anyhow!("connecting resume-session store: {error}"))?,
    );
    let gateway = WorldGateway::new(
        demo::build_zone(std::slice::from_ref(&grant))?,
        demo::handshake()?,
        GatewayConfig::new(DurationMs::new(30_000)),
        SystemResumeTokens::new(),
    )?
    .with_resume_store(resume_store.clone());
    let service = Service::new(WorldServer::new(gateway), Duration::from_secs(60));
    let (events, receiver) = event_channel();
    let (shutdown, shutdown_receiver) = tokio::sync::watch::channel(false);
    let state = AppState::new(
        events.clone(),
        Authenticator::new(&[digest("unused-test-authenticator-token")])?,
    );
    let mut session_id = None;

    let result = runtime.block_on(async {
        let service_task = tokio::spawn(service.run(receiver, shutdown_receiver));

        let (outbound, mut outbound_receiver) = tokio::sync::mpsc::channel(16);
        let (reply, response) = tokio::sync::oneshot::channel();
        events
            .send(ServerEvent::Admit {
                credential: Credential::Static(grant.clone()),
                hello: Hello {
                    handshake: demo::handshake()?,
                    last_ack_tick: Tick::ZERO,
                },
                outbound,
                reply,
            })
            .await?;
        let admitted = tokio::time::timeout(Duration::from_secs(10), response)
            .await
            .context("timed out waiting for service admission")???;
        session_id = Some(admitted.session.to_string());
        while outbound_receiver.try_recv().is_ok() {}

        anyhow::ensure!(
            tokio::time::timeout(
                Duration::from_secs(10),
                state.revoke_principal_sessions(grant.principal),
            )
            .await
            .context("timed out waiting for service-side principal revocation")??
                == 1,
            "AppState control request revokes the principal through the service"
        );
        anyhow::ensure!(
            matches!(outbound_receiver.try_recv()?, ServerFrame::Close),
            "the service closes the attached transport"
        );
        shutdown.send(true)?;
        tokio::time::timeout(Duration::from_secs(10), service_task)
            .await
            .context("timed out waiting for service shutdown")??;
        Ok(())
    });
    let session_id = session_id.context("service admission returned no session ID")?;
    let _guard = RowGuard {
        config: config.clone(),
        session_id: session_id.clone(),
    };
    result?;

    let revoked: bool = config
        .connect(NoTls)?
        .query_one(
            "SELECT revoked FROM lynx.resume_sessions WHERE session_id = $1",
            &[&session_id],
        )?
        .get(0);
    anyhow::ensure!(revoked, "the service control path persists revocation");
    runtime.block_on(async {
        tokio::task::block_in_place(|| drop(resume_store));
    });
    drop(runtime);
    Ok(())
}

#[test]
fn postgres_resume_rotation_survives_backend_loss_before_commit() -> anyhow::Result<()> {
    let _lock = DATABASE_TEST_LOCK.lock().unwrap();
    let Some(config) = test_config()? else {
        return Ok(());
    };
    let suffix = unique_suffix();
    let session_id = format!("resume-crash-{suffix}");
    let initial_token = TokenDigest::of("pre-rotation crash token");
    let rotated_token = TokenDigest::of("post-rollback retry token");
    let store = PostgresResumeSessionStore::connect(&config)
        .map_err(|error| anyhow::anyhow!("connecting resume-session store: {error}"))?;
    let _guard = RowGuard {
        config: config.clone(),
        session_id: session_id.clone(),
    };
    store.create(&ResumeSessionRecord {
        session_id: session_id.clone(),
        zone_id: "resume-crash-zone".into(),
        account_id: "crash-test-account".into(),
        principal_id: "crash-test-principal".into(),
        actor_id: "crash-test-actor".into(),
        staff: false,
        token_digest: initial_token,
        expires_at_ms: None,
        connected: true,
        generation: 0,
        revoked: false,
    })?;
    anyhow::ensure!(store.detach(&session_id, 0, 30_000)?, "session detaches");

    // Match consume_and_rotate's validation lock, then terminate that database
    // backend before the token update/transaction commit. PostgreSQL must roll
    // the read transaction back and leave the original digest claimable.
    let mut interrupted = config.connect(NoTls)?;
    let mut transaction = interrupted.transaction()?;
    let _validated = transaction.query_opt(
        "SELECT session_id FROM lynx.resume_sessions WHERE token_digest = $1 AND NOT connected AND NOT revoked AND expires_at_ms > $2 FOR UPDATE",
        &[&&initial_token.as_bytes()[..], &20_000_i64],
    )?.context("pre-crash transaction validates the detached token")?;
    let backend_pid: i32 = transaction
        .query_one("SELECT pg_backend_pid()", &[])?
        .get(0);
    let mut terminator = config.connect(NoTls)?;
    let terminated: bool = terminator
        .query_one("SELECT pg_terminate_backend($1)", &[&backend_pid])?
        .get(0);
    anyhow::ensure!(
        terminated,
        "the validation transaction backend is terminated"
    );
    drop(transaction);
    drop(interrupted);
    wait_for_backend_exit(&mut terminator, backend_pid)?;

    let restored = store
        .consume_and_rotate(initial_token, rotated_token, 20_000)?
        .context("the original digest remains usable after transaction rollback")?;
    anyhow::ensure!(
        restored.session_id == session_id && restored.generation == 2,
        "retry consumes the same session at the next fence"
    );
    let persisted: Vec<u8> = terminator
        .query_one(
            "SELECT token_digest FROM lynx.resume_sessions WHERE session_id = $1",
            &[&session_id],
        )?
        .get(0);
    anyhow::ensure!(
        persisted.as_slice() == rotated_token.as_bytes(),
        "only the retried rotation is committed"
    );
    Ok(())
}

#[test]
fn postgres_resume_session_expiry_is_checked_and_swept() -> anyhow::Result<()> {
    let _lock = DATABASE_TEST_LOCK.lock().unwrap();
    let Some(config) = test_config()? else {
        return Ok(());
    };
    let session_id = format!("resume-expiry-{}", unique_suffix());
    let token = TokenDigest::of("expiry bearer token");
    let store = PostgresResumeSessionStore::connect(&config)
        .map_err(|error| anyhow::anyhow!("connecting resume-session store: {error}"))?;
    let _guard = RowGuard {
        config: config.clone(),
        session_id: session_id.clone(),
    };
    store
        .create(&ResumeSessionRecord {
            session_id: session_id.clone(),
            zone_id: "resume-expiry-zone".into(),
            account_id: "a".into(),
            principal_id: "p".into(),
            actor_id: "e".into(),
            staff: false,
            token_digest: token,
            expires_at_ms: None,
            connected: true,
            generation: 0,
            revoked: false,
        })
        .map_err(|error| anyhow::anyhow!("creating resume-session record: {error}"))?;
    store.detach(&session_id, 0, 100)?;
    anyhow::ensure!(
        store
            .consume_and_rotate(token, TokenDigest::of("late rotated token"), 100)?
            .is_none(),
        "expiry is exclusive"
    );
    anyhow::ensure!(
        store.sweep_expired(100)? == 1,
        "expired detached row is swept"
    );
    Ok(())
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one database-backed scenario covers admission, lease takeover, actor restore, and resume"
)]
fn postgres_gateway_resumes_session_after_process_restart_with_lease_fencing() -> anyhow::Result<()>
{
    let _lock = DATABASE_TEST_LOCK.lock().unwrap();
    let Some(config) = test_config()? else {
        return Ok(());
    };
    let suffix = unique_suffix();
    let owner_a = format!("gateway-resume-a-{suffix}");
    let owner_b = format!("gateway-resume-b-{suffix}");
    let resume_store = Arc::new(
        PostgresResumeSessionStore::connect(&config)
            .map_err(|error| anyhow::anyhow!("connecting resume-session store: {error}"))?,
    );
    let lease_store = Arc::new(PostgresZoneLeaseStore::connect(&config)?);
    let authenticator = Authenticator::new(&[digest("gateway-restart-test-token")])?;
    let grant = authenticator.grants().next().unwrap().clone();
    let demo_zone = demo::build_zone(std::slice::from_ref(&grant))?;
    let zone_id = demo_zone.config().instance.to_string();
    let lease_cleanup = LeaseGuard {
        config: config.clone(),
        zone_id: zone_id.clone(),
    };
    let ttl = Duration::from_secs(30);
    let first_authority = ZoneAuthority::acquire(lease_store.clone(), &zone_id, &owner_a, ttl)?
        .context("first process acquires zone lease")?;
    let first_fence = first_authority.fencing_token();

    let mut first_gateway = WorldGateway::new(
        demo_zone,
        demo::handshake()?,
        GatewayConfig::new(DurationMs::new(30_000)),
        SystemResumeTokens::new(),
    )?
    .with_resume_store(resume_store.clone());
    let opened =
        first_gateway.connect(grant.claims()?, &demo::handshake()?, Millis::new(10_000))?;
    let session_id = opened.session.to_string();
    let actor_id = opened.actor;
    let row_cleanup = RowGuard {
        config: config.clone(),
        session_id: session_id.clone(),
    };
    let durable_actor_world = first_gateway.zone().snapshot();

    // Simulate process loss: the lease expires/releases but the database still
    // says the session is connected. The successor must prove its fresh fence
    // before converting those stale connections to resume windows.
    anyhow::ensure!(first_authority.release()?, "first process releases lease");
    let second_authority = ZoneAuthority::acquire(lease_store, &zone_id, &owner_b, ttl)?
        .context("successor process acquires zone lease")?;
    anyhow::ensure!(
        second_authority.fencing_token() > first_fence,
        "lease takeover advances its fencing token"
    );
    anyhow::ensure!(
        resume_store
            .detach_stale_connected(&zone_id, &owner_a, first_fence, 40_000)
            .is_err(),
        "the stale process cannot detach sessions under its old fence"
    );
    anyhow::ensure!(
        resume_store.detach_stale_connected(
            &zone_id,
            &owner_b,
            second_authority.fencing_token(),
            40_000,
        )? == 1,
        "the current process detaches the persisted connected session"
    );

    // Restore the actor snapshot independently of the old gateway, as a new
    // process would, then resume through the same PostgreSQL-backed store.
    let restored_zone = demo::restore_zone(&durable_actor_world)?;
    let mut restarted_gateway = WorldGateway::new(
        restored_zone,
        demo::handshake()?,
        GatewayConfig::new(DurationMs::new(30_000)),
        SystemResumeTokens::new(),
    )?
    .with_resume_store(resume_store.clone());
    let resumed = restarted_gateway.resume(
        &opened.token,
        &demo::handshake()?,
        Tick::ZERO,
        Millis::new(20_000),
    )?;
    anyhow::ensure!(resumed.resumed, "restart admission is a resume");
    anyhow::ensure!(
        resumed.session.to_string() == session_id,
        "session identity survives restart"
    );
    anyhow::ensure!(
        resumed.actor == actor_id,
        "restored character actor is retained"
    );
    anyhow::ensure!(
        restarted_gateway.zone().entity(actor_id).is_some(),
        "the resumed actor is present in the restored zone"
    );
    anyhow::ensure!(
        restarted_gateway.retained_sessions() == 1,
        "the fresh process restores the retained session"
    );
    anyhow::ensure!(
        resumed.token != opened.token,
        "successful restart resume atomically rotates the token"
    );
    anyhow::ensure!(
        restarted_gateway
            .resume(
                &opened.token,
                &demo::handshake()?,
                Tick::ZERO,
                Millis::new(20_001)
            )
            .unwrap_err()
            .code()
            == engine_core::error::ErrorCode::SessionExpired,
        "the pre-restart token is single-use"
    );

    anyhow::ensure!(
        restarted_gateway.revoke_principal_sessions(grant.principal)? == 1,
        "trusted role-change revocation removes the resumed session"
    );
    let revoked: bool = config
        .connect(NoTls)?
        .query_one(
            "SELECT revoked FROM lynx.resume_sessions WHERE session_id = $1",
            &[&session_id],
        )?
        .get(0);
    anyhow::ensure!(revoked, "principal revocation is persisted");
    anyhow::ensure!(
        restarted_gateway
            .resume(
                &resumed.token,
                &demo::handshake()?,
                Tick::ZERO,
                Millis::new(20_002),
            )
            .unwrap_err()
            .code()
            == engine_core::error::ErrorCode::SessionExpired,
        "the rotated token is rejected after principal revocation"
    );

    anyhow::ensure!(second_authority.release()?, "successor releases lease");
    drop(row_cleanup);
    drop(lease_cleanup);
    Ok(())
}

#[test]
fn startup_detach_requires_the_current_zone_lease() -> anyhow::Result<()> {
    let _lock = DATABASE_TEST_LOCK.lock().unwrap();
    let Some(config) = test_config()? else {
        return Ok(());
    };
    let suffix = unique_suffix();
    let session_id = format!("resume-startup-{suffix}");
    let zone_id = format!("zone_{:026}", suffix.parse::<u128>()?);
    let owner = format!("resume-test-{suffix}");
    let mut client = config.connect(NoTls)?;
    let token = TokenDigest::of("startup detached token");
    let store = PostgresResumeSessionStore::connect(&config)
        .map_err(|error| anyhow::anyhow!("connecting resume-session store: {error}"))?;
    let _guard = RowGuard {
        config: config.clone(),
        session_id: session_id.clone(),
    };
    store
        .create(&ResumeSessionRecord {
            session_id: session_id.clone(),
            zone_id: zone_id.clone(),
            account_id: "account".into(),
            principal_id: "principal".into(),
            actor_id: "actor".into(),
            staff: true,
            token_digest: token,
            expires_at_ms: None,
            connected: true,
            generation: 0,
            revoked: false,
        })
        .map_err(|error| anyhow::anyhow!("creating resume-session record: {error}"))?;
    client.execute("INSERT INTO lynx.zone_leases (zone_id, owner, expires_at) VALUES ($1, $2, clock_timestamp() + interval '60 seconds')", &[&zone_id, &owner])?;
    let fence: i64 = client
        .query_one(
            "SELECT fencing_token FROM lynx.zone_leases WHERE zone_id = $1",
            &[&zone_id],
        )?
        .get(0);
    anyhow::ensure!(
        store
            .detach_stale_connected(&zone_id, &owner, fence + 1, 50_000)
            .is_err(),
        "stale lease must be rejected"
    );
    let connected: bool = client
        .query_one(
            "SELECT connected FROM lynx.resume_sessions WHERE session_id = $1",
            &[&session_id],
        )?
        .get(0);
    anyhow::ensure!(connected, "rejected transition leaves session unchanged");
    anyhow::ensure!(
        store.detach_stale_connected(&zone_id, &owner, fence, 50_000)? == 1,
        "the current lease detaches the stale connected session"
    );
    let rotated = TokenDigest::of("after-restart rotated token");
    let restored = store
        .consume_and_rotate(token, rotated, 10_000)?
        .context("a detached session can be consumed after restart")?;
    anyhow::ensure!(
        restored.session_id == session_id
            && restored.staff
            && restored.token_digest == rotated
            && restored.generation == 2,
        "restart consumption returns trusted fenced metadata and the rotated digest"
    );
    anyhow::ensure!(
        store
            .consume_and_rotate(token, TokenDigest::of("replayed token"), 10_000)?
            .is_none(),
        "the pre-restart token cannot be replayed"
    );
    anyhow::ensure!(
        store.revoke_principal("principal")? == 1,
        "trusted principal revocation invalidates all retained claims"
    );
    anyhow::ensure!(
        store
            .consume_and_rotate(rotated, TokenDigest::of("post-revocation token"), 10_000)?
            .is_none(),
        "a token is not resumable after principal revocation"
    );
    client.execute(
        "DELETE FROM lynx.zone_leases WHERE zone_id = $1 AND owner = $2",
        &[&zone_id, &owner],
    )?;
    Ok(())
}
