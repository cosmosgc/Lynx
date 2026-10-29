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
use lynx_world::auth::{self, Authenticator, Grant, NakamaIdentityBackend};
use lynx_world::backend;
use lynx_world::config::{AuthMode, ServeArgs};
use lynx_world::demo;
use lynx_world::persistence::{
    FileSnapshotStore, PostgresSnapshotStore, SnapshotStore, postgres_config_from_env,
};
use lynx_world::service::Service;
use lynx_world::startup;
use lynx_world::transport::{self, AppState, OriginPolicy};
use postgres::NoTls;
use tokio::sync::watch;
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
enum Command {
    /// Run the world service.
    Serve(ServeArgs),
    /// Print the SHA-256 hash of one token read from standard input.
    ///
    /// Use this to produce configuration values without writing plaintext
    /// tokens to a file: `lynx-world token-hash < token.txt`.
    TokenHash,
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
    let identity_backend = (args.auth_mode == AuthMode::Nakama)
        .then(production_identity_backend)
        .transpose()?;
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
    let store = snapshot_store(&args)?;
    if args.fresh
        && let Some(path) = &args.state_file
    {
        tracing::info!(
            path = %path.display(),
            "--fresh: the played world is ignored and a freshly authored world is saved over the snapshot at startup"
        );
    }

    let now_ms = demo::now_ms();
    let startup::StartupWorld {
        zone,
        calendar,
        factions,
    } = startup::load(
        store
            .as_ref()
            .map(|store| store.as_ref() as &dyn SnapshotStore),
        args.fresh,
        &grants,
        now_ms,
    )
    .context("failed to start the demo world")?;
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
    let state = match (authenticator, identity_backend) {
        (Some(authenticator), None) => AppState::new(events, authenticator),
        (None, Some(backend)) => AppState::with_identity_backend(events, backend),
        _ => unreachable!("exactly one configured authentication mode is constructed"),
    };
    let state = Arc::new(
        state
            .with_metrics(service.metrics())
            .with_origin_policy(origins)
            .with_rate_limits(rate_limits),
    );

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
    let shutdown_signal = async move {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("shutdown requested");
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

fn snapshot_store(args: &ServeArgs) -> Result<Option<Arc<dyn SnapshotStore + Send + Sync>>> {
    if args.state_postgres {
        Ok(Some(Arc::new(
            PostgresSnapshotStore::from_env(demo::DEMO_WORLD_ID)
                .context("failed to open the PostgreSQL snapshot store")?,
        )))
    } else {
        Ok(args
            .state_file
            .as_ref()
            .map(|path| Arc::new(FileSnapshotStore::new(path.clone())) as _))
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
}
