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
//! Without it, sessions, characters, and world state are transient.

use std::io::Read;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use engine_core::faction::FactionWorld;
use engine_core::world_event::WorldCalendar;
use lynx_world::auth::{self, Authenticator, Grant};
use lynx_world::backend;
use lynx_world::config::ServeArgs;
use lynx_world::demo;
use lynx_world::persistence::{FileSnapshotStore, SnapshotStore, WorldSnapshot};
use lynx_world::service::Service;
use lynx_world::transport::{self, AppState};
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

async fn serve(args: ServeArgs) -> Result<()> {
    let gateway_config = args.gateway_config()?;
    let authenticator = Authenticator::new(&args.token_hashes()?)?.with_staff(args.staff);
    let grants: Vec<Grant> = authenticator.grants().cloned().collect();
    let snapshot_interval = args
        .snapshot_interval()
        .context("invalid snapshot interval")?;

    // Durable state: behind the optional snapshot file, the service resumes
    // the world it wrote last; without it, the demo world starts fresh.
    let store: Option<Arc<dyn SnapshotStore + Send + Sync>> = args
        .state_file
        .as_ref()
        .map(|path| Arc::new(FileSnapshotStore::new(path.clone())) as _);
    let saved: Option<WorldSnapshot> = match &store {
        Some(store) => store
            .load()
            .context("failed to read the world snapshot file")?,
        None => None,
    };

    let zone = match saved.as_ref() {
        Some(world) => {
            tracing::info!(
                saved_at_ms = world.saved_at_ms,
                entities = world.zone.entities.len(),
                "restoring the world from the snapshot file"
            );
            demo::restore_zone(&world.zone).context("failed to restore the demo zone")?
        }
        None => demo::build_zone(&grants).context("failed to build the demo zone")?,
    };

    let now_ms = demo::now_ms();
    let (calendar, factions) = if let Some(world) = saved.as_ref()
        && world.calendar.is_some()
    {
        // Calendar and faction state are restored together: a conflict
        // resolution mutates both aggregates.
        (
            WorldCalendar::from_snapshot(world.calendar.clone().expect("checked above"))
                .context("failed to restore the demo calendar")?,
            FactionWorld::from_snapshot(world.factions.clone().expect("paired with the calendar"))
                .context("failed to restore faction state")?,
        )
    } else {
        let mut factions =
            demo::build_faction_world(now_ms).context("failed to build the demo faction world")?;
        let calendar = demo::build_calendar(&mut factions, now_ms)
            .context("failed to build the demo calendar")?;
        (calendar, factions)
    };
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
    let service = match &store {
        Some(store) => Service::new(server, args.tick_interval())
            .with_persistence(Arc::clone(store), snapshot_interval),
        None => Service::new(server, args.tick_interval()),
    };

    let (events, receivers) = transport::event_channel();
    let (shutdown, shutdown_rx) = watch::channel(false);
    let state = Arc::new(AppState::new(events, authenticator));

    let listener = tokio::net::TcpListener::bind(args.bind)
        .await
        .with_context(|| format!("failed to bind {}", args.bind))?;
    let local = listener
        .local_addr()
        .context("failed to read the listener address")?;
    tracing::info!(
        address = %local,
        characters = grants.len(),
        tick_ms = args.tick_ms,
        snapshot_ms = args.snapshot_ms,
        state_file = ?args.state_file,
        "lynx-world listening"
    );

    let loop_task = tokio::spawn(service.run(receivers, shutdown_rx));

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
    match loop_task.await {
        Ok(()) => {}
        Err(error) => tracing::error!(%error, "authority loop panicked"),
    }
    result.context("the HTTP server stopped with an error")
}
