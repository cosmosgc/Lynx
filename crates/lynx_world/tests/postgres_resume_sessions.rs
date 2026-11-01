//! PostgreSQL integration coverage for durable resume-session rotation.
//!
//! This test uses only `LYNX_PG_TEST_DATABASE`, following the isolated scratch
//! database convention used by the other PostgreSQL integration tests.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use lynx_world::persistence::PostgresResumeSessionStore;
use postgres::{Config, NoTls};
use world_gateway::{ResumeSessionRecord, ResumeSessionStore, TokenDigest};

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
        .connect_timeout(std::time::Duration::from_secs(5));
    Ok(Some(config))
}

fn unique_suffix() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .to_string()
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
    client.execute(
        "DELETE FROM lynx.zone_leases WHERE zone_id = $1 AND owner = $2",
        &[&zone_id, &owner],
    )?;
    Ok(())
}
