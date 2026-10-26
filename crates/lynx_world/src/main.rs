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
//! There is no database in this increment: sessions, characters, and world
//! state live in memory and reset on restart. See `docs/operations.md`.

use std::io::Read;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use lynx_world::auth::{self, Authenticator, Grant};
use lynx_world::backend;
use lynx_world::config::ServeArgs;
use lynx_world::demo;
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
    let zone = demo::build_zone(&grants).context("failed to build the demo zone")?;

    // The demo publishes one bounded, open conflict over the demo zone at
    // startup. Lifecycle transitions are staff-gated commands; enrollment is
    // open to every demo character.
    let now_ms = demo::now_ms();
    let mut factions =
        demo::build_faction_world(now_ms).context("failed to build the demo faction world")?;
    let calendar =
        demo::build_calendar(&mut factions, now_ms).context("failed to build the demo calendar")?;
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
    let service = Service::new(server, args.tick_interval());

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
