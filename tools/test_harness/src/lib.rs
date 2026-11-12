//! Reusable, GPU-free multiplayer test support.
//!
//! The harness launches the real in-process world service and connects through
//! the production headless WebSocket client. It records typed snapshots/deltas,
//! rotates resume credentials, and can inject deterministic envelope delay or
//! loss at the test-client boundary. The fault injector is not a packet-level
//! network emulator.

use std::collections::VecDeque;
use std::fmt;
use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use engine_core::id::{IdKind, StableId};
use engine_core::math::Vec3;
use engine_core::time::Tick;
use engine_net::envelope::{Envelope, MessageId, MessageKind};
use engine_net::protocol::Handshake;
use engine_net::replication::ReplicationCategory;
use engine_sim::zone::Zone;
use lynx_client_net::{Client, Config, SendError, Status};
use lynx_world::auth::{Authenticator, Grant, digest};
use lynx_world::rate_limit::{
    AdmissionLimitConfig, RateLimitConfig, SessionLimitConfig, TokenBucketConfig,
};
use lynx_world::{demo, service::Service, transport};
use serde_json::{Value, json};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use world_gateway::{EntitySnapshot, GatewayConfig, SystemResumeTokens, WorldGateway};
use world_server::{WorldServer, WorldUpdate};

const MAX_RECORDED_UPDATES: usize = 4_096;
const MAX_RECORDED_MESSAGES: usize = 256;
const MAX_PENDING_SENDS: usize = 256;
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// An in-process authoritative world edge for integration tests.
pub struct TestServer {
    url: String,
    metrics: Arc<lynx_world::service::Metrics>,
    shutdown: Option<watch::Sender<bool>>,
    stop_transport: Option<oneshot::Sender<()>>,
    service: Option<JoinHandle<()>>,
    transport: Option<JoinHandle<std::io::Result<()>>>,
}

impl fmt::Debug for TestServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TestServer")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

impl TestServer {
    /// Start the demo world with one static bearer token per test client.
    pub async fn start(tokens: &[&str]) -> anyhow::Result<Self> {
        Self::start_with_zone(tokens, |_, _| {}).await
    }

    /// Start the demo world after allowing a test to customize its entities.
    ///
    /// The callback receives trusted demo grants and the mutable zone before the
    /// gateway starts. This supports deterministic placement and visibility
    /// fixtures without replacing the real gateway or transport.
    pub async fn start_with_zone(
        tokens: &[&str],
        configure: impl FnOnce(&[Grant], &mut Zone),
    ) -> anyhow::Result<Self> {
        if tokens.is_empty() {
            bail!("the test server requires at least one client token");
        }
        let hashes = tokens.iter().map(|token| digest(token)).collect::<Vec<_>>();
        let authenticator = Authenticator::new(&hashes).context("configure test credentials")?;
        let grants = authenticator.grants().cloned().collect::<Vec<_>>();
        let mut zone = demo::build_zone(&grants).context("build test world")?;
        configure(&grants, &mut zone);

        let gateway = WorldGateway::new(
            zone,
            demo::handshake().context("build test handshake")?,
            GatewayConfig::default(),
            SystemResumeTokens::new(),
        )
        .context("start test gateway")?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .context("bind test world listener")?;
        let url = format!("ws://{}/world", listener.local_addr()?);
        let (events, receiver) = transport::event_channel();
        let (shutdown, shutdown_rx) = watch::channel(false);
        let world_service = Service::new(WorldServer::new(gateway), Duration::from_millis(50));
        let metrics = world_service.metrics();
        let service = tokio::spawn(world_service.run(receiver, shutdown_rx));
        let (stop_transport, stop_transport_rx) = oneshot::channel();
        // A load run connects its clients in a short burst. Keep production
        // defaults for authenticated gameplay, while giving the local harness
        // enough bounded handshake tokens to establish its configured clients.
        let admission = AdmissionLimitConfig::new(
            TokenBucketConfig::new(64, 32).context("configure harness handshake bucket")?,
            TokenBucketConfig::new(5, 1).context("configure harness failed-auth bucket")?,
            TokenBucketConfig::new(64, 32).context("configure harness churn bucket")?,
            16_384,
            Duration::from_secs(300),
        )?;
        let rate_limits = RateLimitConfig::new(admission, SessionLimitConfig::default());
        let app_state = transport::AppState::new(events, authenticator)
            .with_metrics(Arc::clone(&metrics))
            .with_rate_limits(rate_limits);
        let transport = tokio::spawn(transport::serve_with_shutdown(
            listener,
            Arc::new(app_state),
            async move {
                let _ = stop_transport_rx.await;
            },
        ));

        Ok(Self {
            url,
            metrics,
            shutdown: Some(shutdown),
            stop_transport: Some(stop_transport),
            service: Some(service),
            transport: Some(transport),
        })
    }

    /// WebSocket URL for clients in this test.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Snapshot the in-process authority's Prometheus metrics.
    #[must_use]
    pub fn metrics(&self) -> String {
        self.metrics.render()
    }

    /// Gracefully stop both the authority loop and WebSocket listener.
    pub async fn shutdown(mut self) -> anyhow::Result<()> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(true);
        }
        if let Some(stop) = self.stop_transport.take() {
            let _ = stop.send(());
        }
        if let Some(service) = self.service.take() {
            tokio::time::timeout(Duration::from_secs(5), service)
                .await
                .context("test world service did not stop")??;
        }
        if let Some(transport) = self.transport.take() {
            tokio::time::timeout(Duration::from_secs(5), transport)
                .await
                .context("test world transport did not stop")???;
        }
        Ok(())
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(true);
        }
        if let Some(stop) = self.stop_transport.take() {
            let _ = stop.send(());
        }
        if let Some(service) = self.service.take() {
            service.abort();
        }
        if let Some(transport) = self.transport.take() {
            transport.abort();
        }
    }
}

/// Deterministic application-envelope delay and loss settings.
///
/// This delays or discards messages at the harness boundary, not at the TCP
/// packet layer. A `drop_every_nth_*` value of 1 discards every envelope in that
/// direction; `None` disables loss.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NetworkConditions {
    /// Delay before an outgoing envelope is submitted to the transport client.
    pub outbound_latency: Duration,
    /// Delay before an incoming envelope is exposed to the test.
    pub inbound_latency: Duration,
    /// Discard each Nth outgoing envelope, if configured.
    pub drop_every_nth_outbound: Option<NonZeroU64>,
    /// Discard each Nth incoming envelope, if configured.
    pub drop_every_nth_inbound: Option<NonZeroU64>,
}

/// One typed snapshot or replication delta observed by a client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateKind {
    /// Complete current interest view on connect or resume.
    Snapshot,
    /// Tick-based interest and replication delivery.
    Delta,
}

/// A typed authoritative update plus its wire message kind.
#[derive(Debug, Clone, PartialEq)]
pub struct ObservedUpdate {
    /// Whether this was the initial baseline or a later tick.
    pub kind: UpdateKind,
    /// Decoded authoritative world payload.
    pub update: WorldUpdate,
}

struct DelayedEnvelope {
    ready_at: Instant,
    envelope: Envelope,
}

struct PendingSend {
    send_at: Instant,
    envelope: Envelope,
    drop: bool,
}

/// One headless protocol client with bounded observation history.
pub struct HeadlessClient {
    client: Client,
    url: String,
    character: Option<String>,
    handshake: Handshake,
    bearer: String,
    actor: Option<StableId>,
    resume_token: Option<String>,
    last_ack_tick: Tick,
    next_message_id: u64,
    next_movement_sequence: u64,
    conditions: NetworkConditions,
    outbound_count: u64,
    inbound_count: u64,
    inbound_envelopes: u64,
    outbound_bytes: u64,
    inbound_bytes: u64,
    recorded_messages: u64,
    dropped_outbound: u64,
    dropped_inbound: u64,
    pending_sends: VecDeque<PendingSend>,
    delayed_incoming: VecDeque<DelayedEnvelope>,
    updates: VecDeque<ObservedUpdate>,
    messages: VecDeque<Envelope>,
    received_snapshot: bool,
}

impl fmt::Debug for HeadlessClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HeadlessClient")
            .field("status", &self.client.status())
            .field("actor", &self.actor)
            .field("last_ack_tick", &self.last_ack_tick)
            .field("recorded_updates", &self.updates.len())
            .field("recorded_messages", &self.messages.len())
            .finish_non_exhaustive()
    }
}

impl HeadlessClient {
    /// Start connecting with the production headless client and a static token.
    #[must_use]
    pub fn connect(
        url: impl Into<String>,
        bearer: impl Into<String>,
        character: Option<String>,
        handshake: Handshake,
    ) -> Self {
        let url = url.into();
        let bearer = bearer.into();
        let client = Client::connect(Config {
            url: url.clone(),
            bearer: bearer.clone(),
            character: character.clone(),
            handshake: handshake.clone(),
            last_ack_tick: Tick::ZERO,
        });
        Self {
            client,
            url,
            character,
            handshake,
            bearer,
            actor: None,
            resume_token: None,
            last_ack_tick: Tick::ZERO,
            next_message_id: 1,
            next_movement_sequence: 1,
            conditions: NetworkConditions::default(),
            outbound_count: 0,
            inbound_count: 0,
            inbound_envelopes: 0,
            outbound_bytes: 0,
            inbound_bytes: 0,
            recorded_messages: 0,
            dropped_outbound: 0,
            dropped_inbound: 0,
            pending_sends: VecDeque::new(),
            delayed_incoming: VecDeque::new(),
            updates: VecDeque::new(),
            messages: VecDeque::new(),
            received_snapshot: false,
        }
    }

    /// Current connection state from the underlying transport client.
    #[must_use]
    pub fn status(&self) -> Status {
        self.client.status()
    }

    /// Server-assigned actor identity after `world.ready` has been observed.
    #[must_use]
    pub const fn actor(&self) -> Option<StableId> {
        self.actor
    }

    /// Latest authoritative tick observed by this client.
    #[must_use]
    pub const fn last_ack_tick(&self) -> Tick {
        self.last_ack_tick
    }

    /// Recorded snapshots and deltas, oldest first.
    pub fn updates(&self) -> impl DoubleEndedIterator<Item = &ObservedUpdate> {
        self.updates.iter()
    }

    /// Recorded non-secret protocol messages, oldest first.
    ///
    /// `world.token` messages are deliberately excluded because their bodies
    /// contain bearer credentials.
    pub fn messages(&self) -> impl DoubleEndedIterator<Item = &Envelope> {
        self.messages.iter()
    }

    /// Cumulative application-envelope counts and bytes for this client.
    /// Bytes exclude WebSocket framing, TLS, and TCP/IP overhead.
    #[must_use]
    pub const fn io_totals(&self) -> (u64, u64, u64, u64) {
        (
            self.outbound_count,
            self.inbound_envelopes,
            self.outbound_bytes,
            self.inbound_bytes,
        )
    }

    /// Cumulative non-replication server messages recorded by this client.
    #[must_use]
    pub const fn recorded_message_count(&self) -> u64 {
        self.recorded_messages
    }

    /// Number of envelopes discarded by the active outbound fault schedule.
    #[must_use]
    pub const fn dropped_outbound(&self) -> u64 {
        self.dropped_outbound
    }

    /// Number of envelopes discarded by the active inbound fault schedule.
    #[must_use]
    pub const fn dropped_inbound(&self) -> u64 {
        self.dropped_inbound
    }

    /// Replace deterministic delay/loss settings and restart their Nth-message
    /// schedules. Existing queued messages retain their assigned send times.
    pub fn set_network_conditions(&mut self, conditions: NetworkConditions) {
        self.conditions = conditions;
        self.outbound_count = 0;
        self.inbound_count = 0;
        self.dropped_outbound = 0;
        self.dropped_inbound = 0;
    }

    /// Drain available transport messages, apply configured envelope faults,
    /// and submit outgoing messages whose delay has elapsed.
    ///
    /// Returns the number of newly recorded world snapshots or deltas.
    pub fn poll(&mut self) -> anyhow::Result<usize> {
        let now = Instant::now();
        while self
            .pending_sends
            .front()
            .is_some_and(|pending| pending.send_at <= now)
        {
            let Some(pending) = self.pending_sends.pop_front() else {
                break;
            };
            if pending.drop {
                self.dropped_outbound = self.dropped_outbound.saturating_add(1);
                continue;
            }
            match self.client.try_send(&pending.envelope) {
                Ok(()) => {}
                Err(SendError::Full) => {
                    self.pending_sends.push_front(pending);
                    break;
                }
                Err(error) => bail!("headless client could not send test envelope: {error:?}"),
            }
        }

        while self.delayed_incoming.len() < MAX_PENDING_SENDS {
            let Ok(envelope) = self.client.try_recv() else {
                break;
            };
            self.inbound_count = self.inbound_count.saturating_add(1);
            if Self::should_drop(self.inbound_count, self.conditions.drop_every_nth_inbound) {
                self.dropped_inbound = self.dropped_inbound.saturating_add(1);
                continue;
            }
            self.delayed_incoming.push_back(DelayedEnvelope {
                ready_at: now + self.conditions.inbound_latency,
                envelope,
            });
        }

        let mut recorded = 0;
        while self
            .delayed_incoming
            .front()
            .is_some_and(|pending| pending.ready_at <= now)
        {
            let Some(pending) = self.delayed_incoming.pop_front() else {
                break;
            };
            recorded += usize::from(self.record(pending.envelope)?);
        }
        Ok(recorded)
    }

    /// Wait until a baseline snapshot and initial rotated resume credential
    /// arrive, or return a bounded timeout/failure.
    pub async fn wait_ready(&mut self, timeout: Duration) -> anyhow::Result<()> {
        tokio::time::timeout(timeout, async {
            loop {
                self.poll()?;
                if self.received_snapshot && self.resume_token.is_some() {
                    return Ok(());
                }
                self.ensure_not_failed()?;
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        })
        .await
        .context("headless client did not receive its initial world state")?
    }

    /// Queue an authoritative action request.
    pub fn send_action(
        &mut self,
        target: Option<StableId>,
        verb: &str,
        params: &[(String, String)],
    ) -> anyhow::Result<()> {
        let params = params
            .iter()
            .cloned()
            .collect::<std::collections::BTreeMap<_, _>>();
        self.queue(
            "action.request",
            json!({
                "target": target,
                "verb": verb,
                "params": params,
                "client_tick": self.last_ack_tick,
            }),
        )
    }

    /// Queue a sequenced movement intent. Movement remains server-authoritative.
    pub fn move_to(&mut self, destination: Vec3) -> anyhow::Result<u64> {
        let sequence = self.next_movement_sequence;
        self.next_movement_sequence = sequence.saturating_add(1);
        self.queue(
            "movement.request",
            json!({ "sequence": sequence, "destination": destination }),
        )?;
        Ok(sequence)
    }

    /// Queue a message to the authoritative zone chat channel.
    pub fn send_chat(&mut self, content: &str) -> anyhow::Result<()> {
        self.queue(
            "chat.send",
            json!({ "channel_id": "zone", "content": content }),
        )
    }

    /// Resume with the latest single-use token, rotating it again on success.
    pub async fn reconnect(&mut self, timeout: Duration) -> anyhow::Result<()> {
        self.poll()?;
        if self.resume_token.is_none() {
            self.wait_for_resume_token(timeout).await?;
        }
        self.bearer = self
            .resume_token
            .take()
            .context("headless client has no resume credential")?;
        self.client.close();
        self.received_snapshot = false;
        self.delayed_incoming.clear();
        self.pending_sends.clear();
        self.client = Client::connect(Config {
            url: self.url.clone(),
            bearer: self.bearer.clone(),
            character: self.character.clone(),
            handshake: self.handshake.clone(),
            last_ack_tick: self.last_ack_tick,
        });
        self.wait_ready(timeout).await
    }

    /// Latest public entity state reconstructed from recorded state-snapshot
    /// updates. Returns `None` if the entity has never been visible to the client.
    #[must_use]
    pub fn entity_snapshot(&self, entity: StableId) -> Option<EntitySnapshot> {
        self.updates.iter().rev().find_map(|observed| {
            observed
                .update
                .batch
                .updates
                .iter()
                .rev()
                .find(|update| {
                    matches!(
                        update.category,
                        ReplicationCategory::InterestManaged | ReplicationCategory::StateSnapshot
                    ) && update.entity == Some(entity)
                })
                .and_then(|update| serde_json::from_value(update.payload.clone()).ok())
        })
    }

    /// Whether a session-private movement reconciliation with this sequence was
    /// observed. Other observers must not receive it.
    #[must_use]
    pub fn observed_movement_sequence(&self, sequence: u64) -> bool {
        self.updates.iter().any(|observed| {
            observed
                .update
                .movement
                .as_ref()
                .is_some_and(|movement| movement.last_consumed_sequence == Some(sequence))
        })
    }

    fn queue(&mut self, kind: &str, body: Value) -> anyhow::Result<()> {
        if self.pending_sends.len() >= MAX_PENDING_SENDS {
            bail!("headless client pending-send queue is full");
        }
        let id = format!("harness-{}", self.next_message_id);
        self.next_message_id = self.next_message_id.saturating_add(1);
        let envelope = Envelope::new(
            MessageId::parse(&id).expect("generated test message IDs are valid"),
            MessageKind::parse(kind)
                .with_context(|| format!("invalid test message kind: {kind}"))?,
            body,
        );
        let encoded_len = u64::try_from(envelope.encode()?.len()).unwrap_or(u64::MAX);
        self.outbound_bytes = self.outbound_bytes.saturating_add(encoded_len);
        self.outbound_count = self.outbound_count.saturating_add(1);
        let drop = Self::should_drop(self.outbound_count, self.conditions.drop_every_nth_outbound);
        self.pending_sends.push_back(PendingSend {
            send_at: Instant::now() + self.conditions.outbound_latency,
            envelope,
            drop,
        });
        Ok(())
    }

    fn record(&mut self, envelope: Envelope) -> anyhow::Result<bool> {
        let encoded_len = u64::try_from(envelope.encode()?.len()).unwrap_or(u64::MAX);
        self.inbound_bytes = self.inbound_bytes.saturating_add(encoded_len);
        self.inbound_envelopes = self.inbound_envelopes.saturating_add(1);
        match envelope.kind.as_str() {
            "world.ready" => {
                self.actor = envelope
                    .body
                    .get("actor")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()
                    .context("decode actor identity from world.ready")?;
                self.push_message(envelope);
                Ok(false)
            }
            "world.snapshot" | "world.delta" => {
                let update: WorldUpdate = serde_json::from_value(envelope.body.clone())
                    .context("decode authoritative world update")?;
                self.last_ack_tick = self.last_ack_tick.max(update.tick);
                let kind = if envelope.kind.as_str() == "world.snapshot" {
                    self.received_snapshot = true;
                    UpdateKind::Snapshot
                } else {
                    UpdateKind::Delta
                };
                self.push_update(ObservedUpdate { kind, update });
                self.push_message(envelope);
                Ok(true)
            }
            "world.token" => {
                self.resume_token = envelope
                    .body
                    .get("resume")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                // Never retain credential-bearing envelopes in observable history.
                Ok(false)
            }
            _ => {
                self.push_message(envelope);
                Ok(false)
            }
        }
    }

    fn push_update(&mut self, update: ObservedUpdate) {
        if self.updates.len() == MAX_RECORDED_UPDATES {
            self.updates.pop_front();
        }
        self.updates.push_back(update);
    }

    fn push_message(&mut self, envelope: Envelope) {
        self.recorded_messages = self.recorded_messages.saturating_add(1);
        if self.messages.len() == MAX_RECORDED_MESSAGES {
            self.messages.pop_front();
        }
        self.messages.push_back(envelope);
    }

    async fn wait_for_resume_token(&mut self, timeout: Duration) -> anyhow::Result<()> {
        tokio::time::timeout(timeout, async {
            while self.resume_token.is_none() {
                self.poll()?;
                self.ensure_not_failed()?;
                tokio::time::sleep(POLL_INTERVAL).await;
            }
            Ok(())
        })
        .await
        .context("headless client did not receive a resume credential")?
    }

    fn ensure_not_failed(&self) -> anyhow::Result<()> {
        match self.client.status() {
            Status::Failed(failure) => bail!("headless transport failed: {failure:?}"),
            Status::Closed => bail!("headless transport closed before world state arrived"),
            Status::Connecting | Status::AwaitingReady | Status::Ready => Ok(()),
        }
    }

    fn should_drop(sequence: u64, every: Option<NonZeroU64>) -> bool {
        every.is_some_and(|every| sequence.is_multiple_of(every.get()))
    }
}

/// Assert that all observers have the same latest public state for an entity.
///
/// The entity must be visible to every client. Missing observer state and
/// divergent snapshots return an actionable test error.
pub fn assert_shared_entity_consistency(
    clients: &[&HeadlessClient],
    entity: StableId,
) -> anyhow::Result<EntitySnapshot> {
    let mut snapshots = clients.iter().map(|client| {
        client
            .entity_snapshot(entity)
            .with_context(|| format!("entity {entity} is not visible to a test observer"))
    });
    let first = snapshots
        .next()
        .context("shared-state assertion requires at least one observer")??;
    for snapshot in snapshots {
        let snapshot = snapshot?;
        if snapshot != first {
            bail!("observers disagree on authoritative entity {entity}");
        }
    }
    Ok(first)
}

/// Return the stable actor identity generated for a demo grant.
#[must_use]
pub fn demo_actor_id(grant: &Grant) -> StableId {
    StableId::from_parts(IdKind::Entity, 1, grant.principal.random_bits())
}
