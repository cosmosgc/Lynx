//! `lynx-world` — the runnable authoritative world service.
//!
//! ```text
//! Authorization: Bearer <static or resume token>
//! WebSocket /world  ⇄  length-prefixed envelopes  ⇄  Service  ⇄  WorldGateway
//! ```
//!
//! Run it with at least one token hash:
//!
//! ```text
//! lynx-world serve --bind 127.0.0.1:8090 --token-hash <sha256-hex>
//! ```
//!
//! Add `--state-file world.json` for durable state: the service loads that
//! snapshot at startup and rewrites it on a fixed interval and at shutdown.
//! Add `--fresh` with it to discard the played world and restore the authored
//! demo. Without `--state-file`, sessions, characters, and world state are
//! transient.

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use backend_nakama::{
    NakamaClient, NakamaConfig, NakamaPostgresBackend, PostgresCharacterStore,
    PostgresIdentityStore,
};
use clap::{Parser, Subcommand};
use engine_core::time::Tick;
use lynx_world::auth::{self, Authenticator, Grant, NakamaIdentityBackend};
use lynx_world::authority::{self, ZoneAuthority};
use lynx_world::backend;
use lynx_world::config::{AuthMode, ServeArgs};
use lynx_world::demo;
use lynx_world::persistence::{
    FileSnapshotStore, PostgresSnapshotStore, PostgresZoneLeaseStore, RECOVERY_TARGET,
    SnapshotStore, SnapshotVerification, ZoneLeaseStore, postgres_config_from_env,
};
use lynx_world::service::Service;
use lynx_world::startup;
use lynx_world::transport::{self, AppState, OriginPolicy};
use postgres::NoTls;
use serde_json::json;
use tokio::sync::watch;
use tokio::time::MissedTickBehavior;
use world_gateway::{SystemResumeTokens, WorldGateway};
use world_server::WorldServer;

#[derive(Parser)]
#[command(name = "lynx-world")]
#[command(version)]
#[command(about = "Authoritative Lynx world service")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
#[allow(
    clippy::large_enum_variant,
    reason = "the CLI is parsed once and the serve arguments stay inline for readability"
)]
enum Command {
    /// Run the world service.
    Serve(ServeArgs),
    /// Print the SHA-256 hash of one token read from standard input.
    ///
    /// Use this to produce configuration values without writing plaintext
    /// tokens to a file: `lynx-world token-hash < token.txt`.
    TokenHash,
    /// Read-only verification of one PostgreSQL snapshot/event checkpoint.
    VerifySnapshot {
        /// World ID used in `lynx.world_snapshots` and checkpoint payloads.
        #[arg(long)]
        world_id: String,
        /// Zone whose ordered event log contains the checkpoint history.
        #[arg(long)]
        zone_id: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Native development convenience: pick up `.env` from the repository
    // root. Variables already present in the process always win.
    dotenvy::dotenv().ok();
    init_tracing();
    match Cli::parse().command {
        Command::Serve(args) => serve(args).await,
        Command::TokenHash => token_hash(),
        Command::VerifySnapshot { world_id, zone_id } => {
            tokio::task::spawn_blocking(move || verify_snapshot(&world_id, &zone_id))
                .await
                .context("snapshot verifier task failed")?
        }
    }
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

fn token_hash() -> Result<()> {
    let mut input = Vec::new();
    std::io::stdin()
        .read_to_end(&mut input)
        .context("failed to read a token from standard input")?;
    let token = String::from_utf8(input).context("the token must be valid UTF-8")?;
    let token = token.trim_end_matches(['\r', '\n']);
    if token.is_empty() {
        bail!("standard input did not contain a token");
    }
    println!("{}", auth::hash_hex(token));
    Ok(())
}

fn verify_snapshot(world_id: &str, zone_id: &str) -> Result<()> {
    let store = PostgresSnapshotStore::from_env(world_id)
        .context("failed to open the PostgreSQL snapshot store")?;
    let (verification, recovered) = store
        .verify_recovery_state(zone_id)
        .context("snapshot recovery verification failed")?;
    match verification {
        SnapshotVerification::Absent => {
            println!("ABSENT world_id={world_id} zone_id={zone_id} status=no-snapshot");
        }
        SnapshotVerification::LegacyUncheckpointed { saved_at_ms, tick } => {
            println!(
                "UNVERIFIED world_id={world_id} zone_id={zone_id} status=legacy-uncheckpointed saved_at_ms={saved_at_ms} tick={tick}"
            );
        }
        SnapshotVerification::Verified {
            event_sequence,
            event_count,
            saved_at_ms,
            tick,
        } => {
            println!(
                "VERIFIED world_id={world_id} zone_id={zone_id} checkpoint_sequence={event_sequence} event_count={event_count} saved_at_ms={saved_at_ms} tick={tick}"
            );
        }
    }

    if let Some(world) = recovered {
        let report =
            demo::restore_zone_quarantining(&world.zone).context("dry-run zone restore failed")?;
        println!(
            "RECOVERY tick={} entities={} quarantined={}",
            report.zone.tick().0,
            report.zone.entity_count(),
            report.quarantined.len()
        );
        for record in &report.quarantined {
            println!(
                "QUARANTINE section={} entity={} code={} message={:?}",
                record.section,
                record
                    .entity
                    .map_or_else(|| "unknown".to_owned(), |entity| entity.to_string()),
                record.code,
                record.message
            );
        }
    }

    let stored = store
        .quarantine_records(zone_id)
        .context("failed to read the durable quarantine report")?;
    if !stored.is_empty() {
        println!("DURABLE_QUARANTINE count={}", stored.len());
        for record in &stored {
            println!(
                "STORED_QUARANTINE section={} entity={} code={} message={:?}",
                record.section,
                record
                    .entity
                    .map_or_else(|| "unknown".to_owned(), |entity| entity.to_string()),
                record.code,
                record.message
            );
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // Process startup intentionally assembles every trusted subsystem.
async fn serve(args: ServeArgs) -> Result<()> {
    args.validate_demo_auth_bind()?;
    if args.auth_mode == AuthMode::Demo {
        tracing::warn!(
            "static bearer token authentication is for development only, not production authentication"
        );
        if !args.bind.ip().is_loopback() {
            tracing::warn!(
                address = %args.bind,
                "dangerous development opt-in enabled: static demo authentication is exposed on a non-loopback listener; bearer tokens may be intercepted"
            );
        }
    }
    let origins = OriginPolicy::new(args.allowed_origins.clone())
        .context("invalid --allow-origin configuration")?;
    let gateway_config = args.gateway_config()?;
    let rate_limits = args.rate_limit_config()?;
    let authenticator = (args.auth_mode == AuthMode::Demo)
        .then(|| Authenticator::new(&args.token_hashes()?).map(|auth| auth.with_staff(args.staff)))
        .transpose()?;
    let identity_backend = if args.auth_mode == AuthMode::Nakama {
        Some(
            tokio::task::spawn_blocking(production_identity_backend)
                .await
                .context("Nakama identity backend initialization task failed")??,
        )
    } else {
        None
    };
    let grants: Vec<Grant> = authenticator
        .as_ref()
        .map_or_else(Vec::new, |authenticator| {
            authenticator.grants().cloned().collect()
        });
    let snapshot_interval = args
        .snapshot_interval()
        .context("invalid snapshot interval")?;

    args.validate_snapshot_args()
        .context("invalid snapshot configuration")?;
    let stores = durable_stores(&args)?;
    let store = stores.snapshot.clone();
    if args.auth_mode == AuthMode::Nakama && store.is_none() {
        bail!(
            "Nakama character admission requires --state-file or --state-postgres for durable actor provisioning"
        );
    }
    if args.fresh
        && let Some(path) = &args.state_file
    {
        tracing::info!(
            path = %path.display(),
            "--fresh: the played world is ignored and a freshly authored world is saved over the snapshot at startup"
        );
    }

    let now_ms = demo::now_ms();
    // Take the lease before reading the snapshot, so the state this process
    // restores cannot be written by a superseded owner while it starts. The
    // demo zone ID is fixed, so it is known without loading anything.
    let authority = match (&stores.postgres, &stores.leases) {
        (Some(postgres), Some(leases)) => Some(Arc::new(start_authority(
            postgres,
            leases,
            &demo::zone_instance().to_string(),
            args.lease_ttl()
                .context("invalid zone lease configuration")?,
        )?)),
        _ => None,
    };
    let recovery_started = std::time::Instant::now();
    let startup_world = startup::load(
        store
            .as_ref()
            .map(|store| store.as_ref() as &dyn SnapshotStore),
        args.fresh,
        &grants,
        now_ms,
    )
    .context("failed to start the demo world")?;
    let recovery_elapsed = recovery_started.elapsed();
    let startup::StartupWorld {
        zone,
        calendar,
        factions,
        quarantined,
    } = startup_world;
    let recovery_ms = u64::try_from(recovery_elapsed.as_millis()).unwrap_or(u64::MAX);
    if recovery_elapsed > RECOVERY_TARGET {
        tracing::error!(
            elapsed_ms = recovery_ms,
            target_ms = u64::try_from(RECOVERY_TARGET.as_millis()).unwrap_or(u64::MAX),
            "world recovery exceeded the documented target"
        );
    } else {
        tracing::info!(
            elapsed_ms = recovery_ms,
            quarantined = quarantined.len(),
            "world recovery completed"
        );
    }
    if !quarantined.is_empty() {
        let zone_id = demo::zone_instance().to_string();
        if let Some(store) = &store {
            store
                .record_quarantine(&zone_id, &quarantined)
                .context("failed to persist the recovery quarantine report")?;
        }
        tracing::warn!(
            count = quarantined.len(),
            zone = %zone_id,
            "recovery quarantined saved entries instead of adopting them"
        );
    }
    let mut gateway = WorldGateway::new(
        zone,
        demo::handshake().context("failed to build the server handshake")?,
        gateway_config,
        SystemResumeTokens::new(),
    )
    .context("failed to create the world gateway")?;
    gateway = gateway
        .with_calendar(calendar, factions)
        .context("failed to attach the demo calendar")?;
    gateway = gateway.with_calendar_store(Box::new(backend::DemoCalendarCommit::new(
        backend_memory::MemoryBackend::new(),
    )));
    let server = WorldServer::new(gateway);
    let mut service = match &store {
        Some(store) => Service::new(server, args.tick_interval())
            .with_persistence(Arc::clone(store), snapshot_interval),
        None => Service::new(server, args.tick_interval()),
    };
    if let Some(authority) = &authority {
        service = service.with_zone_authority(authority.as_ref().clone());
    }
    // Establish the baseline snapshot before the loop starts, so the authored
    // world (or the freshly reset one under --fresh) is durable from the very
    // first second instead of waiting for the first interval save. Under
    // --fresh a failed write aborts startup: serving would otherwise hide that
    // the played snapshot is still on disk.
    let baseline = service.try_save_snapshot();
    if args.fresh {
        baseline.context("--fresh could not overwrite the world snapshot")?;
    } else if let Err(error) = baseline {
        tracing::warn!(%error, "the startup world snapshot could not be written");
    }

    let (events, receivers) = transport::event_channel();
    let (shutdown, shutdown_rx) = watch::channel(false);
    let metrics = service.metrics();
    let state = match (authenticator, identity_backend) {
        (Some(authenticator), None) => AppState::new(events, authenticator),
        (None, Some(backend)) => {
            let telemetry = Arc::clone(&backend);
            AppState::with_identity_backend(events, backend.clone())
                .with_character_backend(backend)
                .with_backend_telemetry(move || telemetry.telemetry_snapshot())
        }
        _ => unreachable!("exactly one configured authentication mode is constructed"),
    };
    let state = Arc::new(
        state
            .with_metrics(Arc::clone(&metrics))
            .with_origin_policy(origins)
            .with_rate_limits(rate_limits),
    );

    // Renewal runs outside the authority loop so a slow database round trip
    // never delays a tick. Losing the lease is fatal: durable writes are
    // already fenced by the token, and continuing to serve would present stale
    // authority. Losing it also turns readiness false and stops the process.
    let renewal = authority.as_ref().map(|authority| {
        let authority = Arc::clone(authority);
        let metrics = Arc::clone(&metrics);
        let state = Arc::clone(&state);
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            let mut expires_at = authority.lease().expires_at;
            let mut ticker = tokio::time::interval(authority.renewal_interval());
            ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let attempt = Arc::clone(&authority);
                match tokio::task::spawn_blocking(move || attempt.renew()).await {
                    Ok(Ok(Some(lease))) => {
                        expires_at = lease.expires_at;
                        metrics.record_lease_renewal(true);
                        tracing::debug!(
                            zone = authority.zone_id(),
                            fencing_token = authority.fencing_token(),
                            "zone lease renewed"
                        );
                    }
                    Ok(Ok(None)) => {
                        metrics.record_lease_renewal(false);
                        tracing::error!(
                            zone = authority.zone_id(),
                            "the zone lease was taken over; stopping this process"
                        );
                        break;
                    }
                    Ok(Err(error)) => {
                        metrics.record_lease_renewal(false);
                        tracing::warn!(
                            zone = authority.zone_id(),
                            code = ?error.code(),
                            details = %error,
                            "the zone lease could not be renewed"
                        );
                        if std::time::SystemTime::now() >= expires_at {
                            tracing::error!(
                                zone = authority.zone_id(),
                                "the zone lease expired before it could be renewed; stopping this process"
                            );
                            break;
                        }
                    }
                    Err(error) => {
                        tracing::error!(
                            zone = authority.zone_id(),
                            %error,
                            "the zone lease renewal task failed; stopping this process"
                        );
                        break;
                    }
                }
            }
            metrics.set_lease_held(false);
            state.set_ready(false);
            let _ = shutdown.send(true);
        })
    });

    let listener = tokio::net::TcpListener::bind(args.bind)
        .await
        .with_context(|| format!("failed to bind {}", args.bind))?;
    let admin_listener = tokio::net::TcpListener::bind(args.admin_bind)
        .await
        .with_context(|| format!("failed to bind admin listener {}", args.admin_bind))?;
    tracing::info!(address = %admin_listener.local_addr()?, "admin listener started (unauthenticated)");
    let local = listener
        .local_addr()
        .context("failed to read the listener address")?;
    tracing::info!(
        address = %local,
        auth_mode = ?args.auth_mode,
        characters = grants.len(),
        tick_ms = args.tick_ms,
        message_burst = args.message_burst,
        message_rate = args.message_rate,
        snapshot_ms = args.snapshot_ms,
        state_file = ?args.state_file,
        "lynx-world listening"
    );

    let loop_task = tokio::spawn(service.run(receivers, shutdown_rx));

    let mut admin_shutdown = shutdown.subscribe();
    let admin_task = tokio::spawn(
        axum::serve(admin_listener, transport::admin_router(Arc::clone(&state)))
            .with_graceful_shutdown(async move {
                while !*admin_shutdown.borrow() {
                    if admin_shutdown.changed().await.is_err() {
                        break;
                    }
                }
            })
            .into_future(),
    );

    let signal_state = Arc::clone(&state);
    let signal_shutdown = shutdown.clone();
    let mut signal_watch = shutdown.subscribe();
    let shutdown_signal = async move {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => tracing::info!("shutdown requested"),
            // The renewal task also drives shutdown when authority is lost.
            () = async {
                while !*signal_watch.borrow() {
                    if signal_watch.changed().await.is_err() {
                        break;
                    }
                }
            } => tracing::warn!("shutdown requested by the authority"),
        }
        signal_state.set_ready(false);
        let _ = signal_shutdown.send(true);
    };

    let result = transport::serve_with_shutdown(listener, state, shutdown_signal).await;
    let _ = shutdown.send(true);
    admin_task
        .await
        .context("admin listener task failed")?
        .context("admin listener failed")?;
    match loop_task.await {
        Ok(()) => {}
        Err(error) => tracing::error!(%error, "authority loop panicked"),
    }
    if let Some(renewal) = renewal {
        // The loop already appended its stop marker; do not let a renewal race
        // the release below.
        renewal.abort();
    }
    if let Some(authority) = &authority {
        match authority.release() {
            Ok(true) => tracing::info!(zone = authority.zone_id(), "zone lease released"),
            Ok(false) => tracing::warn!(
                zone = authority.zone_id(),
                "zone lease was already expired or superseded"
            ),
            Err(error) => tracing::warn!(
                code = ?error.code(),
                details = %error,
                "zone lease could not be released"
            ),
        }
    }
    result.context("the HTTP server stopped with an error")
}

fn production_identity_backend() -> Result<Arc<NakamaIdentityBackend>> {
    let base_url = std::env::var("LYNX_NAKAMA_URL")
        .or_else(|_| std::env::var("NAKAMA_URL"))
        .or_else(|_| std::env::var("NAKAMA_BASE_URL"))
        .context("Nakama authentication requires LYNX_NAKAMA_URL")?;
    let mut nakama_config = NakamaConfig::new(base_url);
    nakama_config.http_key = std::env::var("LYNX_NAKAMA_HTTP_KEY").ok();
    nakama_config.timeout = Duration::from_secs(10);
    let nakama = NakamaClient::new(nakama_config)
        .map_err(|_| anyhow::anyhow!("invalid Nakama identity configuration"))?;
    let identities = PostgresIdentityStore::new(
        postgres_config_from_env()
            .context("invalid PostgreSQL identity configuration")?
            .connect(NoTls)
            .context("failed to connect the PostgreSQL identity store")?,
    );
    let characters = PostgresCharacterStore::new(
        postgres_config_from_env()
            .context("invalid PostgreSQL character configuration")?
            .connect(NoTls)
            .context("failed to connect the PostgreSQL character store")?,
    );
    Ok(Arc::new(NakamaIdentityBackend::new(
        NakamaPostgresBackend::new(nakama, identities, characters),
    )))
}

/// Durable stores selected by the command line.
struct DurableStores {
    /// Snapshot store handed to the service.
    snapshot: Option<Arc<dyn SnapshotStore + Send + Sync>>,
    /// Concrete PostgreSQL store, kept so the zone lease can fence it.
    postgres: Option<Arc<PostgresSnapshotStore>>,
    /// Zone lease and event-log operations; only PostgreSQL provides them.
    leases: Option<Arc<PostgresZoneLeaseStore>>,
}

fn durable_stores(args: &ServeArgs) -> Result<DurableStores> {
    if args.state_postgres {
        let postgres = Arc::new(
            PostgresSnapshotStore::from_env(demo::DEMO_WORLD_ID)
                .context("failed to open the PostgreSQL snapshot store")?,
        );
        let leases = Arc::new(
            PostgresZoneLeaseStore::from_env()
                .context("failed to open the PostgreSQL zone lease store")?,
        );
        let snapshot: Arc<dyn SnapshotStore + Send + Sync> = postgres.clone();
        Ok(DurableStores {
            snapshot: Some(snapshot),
            postgres: Some(postgres),
            leases: Some(leases),
        })
    } else {
        Ok(DurableStores {
            snapshot: args
                .state_file
                .as_ref()
                .map(|path| Arc::new(FileSnapshotStore::new(path.clone())) as _),
            postgres: None,
            leases: None,
        })
    }
}

/// Take the zone lease and bind it to the durable snapshot store.
///
/// A failure after acquisition releases the lease, so a process that cannot
/// finish starting does not block its replacement until the TTL expires.
fn start_authority(
    postgres: &PostgresSnapshotStore,
    leases: &Arc<PostgresZoneLeaseStore>,
    zone_id: &str,
    ttl: Duration,
) -> Result<ZoneAuthority> {
    let owner = format!("lynx-world/{}", std::process::id());
    let authority = ZoneAuthority::acquire(
        Arc::clone(leases) as Arc<dyn ZoneLeaseStore>,
        zone_id,
        &owner,
        ttl,
    )
    .context("failed to acquire the zone lease")?
    .with_context(|| format!("zone {zone_id} is already owned by another live process"))?;
    let started = (|| -> Result<()> {
        postgres
            .bind_zone_lease(zone_id, &owner, authority.fencing_token())
            .context("failed to fence snapshots on the zone lease")?;
        authority
            .append_event(
                Tick::ZERO,
                authority::EVENT_WORLD_STARTED,
                None,
                &json!({
                    "owner": owner,
                    "world_id": demo::DEMO_WORLD_ID,
                    "fencing_token": authority.fencing_token(),
                }),
            )
            .context("failed to record the world start event")?;
        Ok(())
    })();
    match started {
        Ok(()) => {
            tracing::info!(
                zone = zone_id,
                owner = %owner,
                fencing_token = authority.fencing_token(),
                ttl_ms = u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX),
                "zone lease acquired"
            );
            Ok(authority)
        }
        Err(error) => {
            let _ = authority.release();
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cli_rejects_exposed_demo_auth_before_other_startup_work() {
        let cli = Cli::try_parse_from(["lynx-world", "serve", "--bind", "0.0.0.0:8090"]).unwrap();
        let Command::Serve(args) = cli.command else {
            panic!("expected serve")
        };
        let error = serve(args).await.unwrap_err().to_string();
        assert!(error.contains("--dangerous-allow-non-loopback-demo-auth"));

        let cli = Cli::try_parse_from([
            "lynx-world",
            "serve",
            "--bind",
            "0.0.0.0:8090",
            "--dangerous-allow-non-loopback-demo-auth",
        ])
        .unwrap();
        let Command::Serve(args) = cli.command else {
            panic!("expected serve")
        };
        assert!(args.validate_demo_auth_bind().is_ok());
    }

    #[test]
    fn verify_snapshot_cli_requires_explicit_world_and_zone_ids() {
        let cli = Cli::try_parse_from([
            "lynx-world",
            "verify-snapshot",
            "--world-id",
            "demo",
            "--zone-id",
            "zone_00000000000000000000000000",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::VerifySnapshot { world_id, zone_id }
                if world_id == "demo" && zone_id == "zone_00000000000000000000000000"
        ));
        assert!(Cli::try_parse_from(["lynx-world", "verify-snapshot"]).is_err());
    }
}
