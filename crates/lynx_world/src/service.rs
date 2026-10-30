//! The single-threaded authority loop for the runnable world service.
//!
//! Sockets never touch [`WorldServer`](world_server::WorldServer) directly.
//! Each WebSocket task forwards admission requests, decoded envelopes, and
//! disconnects over one channel; this loop serializes them with the tick,
//! dispatches per-connection output, and closes connections whose sessions
//! ended. Keeping the simulation single-threaded preserves the gateway's
//! ordering guarantees and makes backpressure explicit.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use engine_core::error::{EngineError, ErrorCode};
use engine_core::id::StableId;
use engine_core::time::{Millis, Tick};
use engine_net::envelope::{Envelope, MessageId, MessageKind};
use engine_net::protocol::{ErrorPayload, Handshake};
use engine_net::replication::SessionId;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::MissedTickBehavior;
use world_gateway::{ERROR_KIND, ResumeToken, SessionClaims};
use world_server::{CloseReason, ConnectionId, Opened, Outbound, WorldServer};

use crate::auth::Grant;
use crate::authority::{EVENT_WORLD_SNAPSHOT, EVENT_WORLD_STOPPED, ZoneAuthority};
use crate::persistence::{SnapshotStore, WorldSnapshot, WorldSnapshotInputs};

/// Client handshake message kind. Its body is [`Hello`].
pub const HELLO_KIND: &str = "session.hello";

/// Control message carrying the resume token to its own connection.
///
/// The token is a bearer secret: transports must not log it, and it is never
/// included in a snapshot or replication body.
pub const TOKEN_KIND: &str = "world.token";

/// The first frame of a connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    /// Client protocol, content, engine, locale, and feature advertisement.
    pub handshake: Handshake,
    /// Highest authoritative tick the client received before reconnecting.
    #[serde(default)]
    pub last_ack_tick: Tick,
}

/// How a peer proved its identity before the upgrade.
#[derive(Debug, Clone)]
pub enum Credential {
    /// A static token bound to one demo character. Starts a new session.
    Static(Grant),
    /// A production identity backend verified this account and selected
    /// character before the WebSocket upgrade.
    Identity(SessionClaims),
    /// A gateway-issued, single-use resume token.
    Resume(ResumeToken),
}

/// Work sent from one transport task to the authority loop.
#[derive(Debug)]
pub enum ServerEvent {
    /// Finish authentication and create or resume a session.
    Admit {
        /// Verified credential.
        credential: Credential,
        /// Client handshake.
        hello: Hello,
        /// Queue for messages addressed to this connection.
        outbound: mpsc::Sender<ServerFrame>,
        /// Admission outcome, including the connection ID to use afterwards.
        reply: oneshot::Sender<Result<Admitted, EngineError>>,
    },
    /// One validated envelope from an admitted connection.
    Frame {
        /// Sending connection.
        connection: ConnectionId,
        /// Decoded envelope.
        envelope: Envelope,
    },
    /// The transport ended, cleanly or otherwise.
    Leave {
        /// Connection that ended.
        connection: ConnectionId,
    },
}

/// Output for one connection.
#[derive(Debug)]
pub enum ServerFrame {
    /// Frame and send this envelope.
    Envelope(Envelope),
    /// Already encoded replication frame, reused for byte accounting and sending.
    Encoded(Vec<u8>),
    /// Send a close frame and stop reading.
    Close,
}

/// A successful admission.
#[derive(Debug, Clone)]
pub struct Admitted {
    /// Connection that now owns the session.
    pub connection: ConnectionId,
    /// Gateway-issued session identifier.
    pub session: SessionId,
    /// Entity the session may drive.
    pub actor: StableId,
    /// Whether an existing session was resumed.
    pub resumed: bool,
}

/// Connection ownership for one authoritative zone.
pub struct Service {
    server: WorldServer,
    tick: Duration,
    writers: BTreeMap<ConnectionId, mpsc::Sender<ServerFrame>>,
    reply_sequence: u64,
    token_sequence: u64,
    persistence: Option<Persistence>,
    authority: Option<ZoneAuthority>,
    metrics: Arc<Metrics>,
}

/// Process-local authority measurements with bounded labels.
#[derive(Debug, Default)]
pub struct Metrics {
    ticks: AtomicU64,
    tick_failures: AtomicU64,
    tick_nanos: AtomicU64,
    tick_overruns: AtomicU64,
    tick_deadline_misses: AtomicU64,
    entities: AtomicU64,
    connections: AtomicU64,
    inputs: AtomicU64,
    retained_sessions: AtomicU64,
    actions: AtomicU64,
    action_rejections: Mutex<BTreeMap<&'static str, u64>>,
    action_applied: AtomicU64,
    action_duration: Mutex<SnapshotWriteDuration>,
    resume_success: AtomicU64,
    resume_failure: AtomicU64,
    resume_failure_codes: Mutex<BTreeMap<&'static str, u64>>,
    session_attaches: AtomicU64,
    session_detaches: AtomicU64,
    session_expired_closes: AtomicU64,
    queue_depth: AtomicU64,
    queue_capacity: AtomicU64,
    replication_enqueued_bytes: AtomicU64,
    replication_snapshots_enqueued: AtomicU64,
    replication_deltas_enqueued: AtomicU64,
    ai_ticks: AtomicU64,
    ai_decisions: AtomicU64,
    ai_controller_scan_limit: AtomicU64,
    ai_update_limit: AtomicU64,
    ai_perception_candidate_limit: AtomicU64,
    ai_perception_los_limit: AtomicU64,
    ai_action_budget_exhaustions: AtomicU64,
    outbound_full: AtomicU64,
    snapshot_success: AtomicU64,
    snapshot_failure: AtomicU64,
    snapshot_write_duration: Mutex<SnapshotWriteDuration>,
    lease_held: AtomicU64,
    lease_renew_success: AtomicU64,
    lease_renew_failure: AtomicU64,
}

// Cumulative histogram bounds in nanoseconds: 1 ms, 10 ms, 100 ms, 1 s, 10 s.
const SNAPSHOT_WRITE_BOUNDS: [u64; 5] = [
    1_000_000,
    10_000_000,
    100_000_000,
    1_000_000_000,
    10_000_000_000,
];

#[derive(Debug, Default)]
struct SnapshotWriteDuration {
    buckets: [u64; 5],
    count: u64,
    nanos: u64,
}

impl SnapshotWriteDuration {
    fn observe(&mut self, duration: Duration) {
        let nanos = u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX);
        for (bucket, bound) in self.buckets.iter_mut().zip(SNAPSHOT_WRITE_BOUNDS) {
            if nanos <= bound {
                *bucket += 1;
            }
        }
        self.count += 1;
        self.nanos = self.nanos.saturating_add(nanos);
    }
}

impl Metrics {
    /// Render the authority measurements in Prometheus text format.
    #[must_use]
    #[allow(
        clippy::too_many_lines,
        reason = "one scrape snapshot keeps related metric families together"
    )]
    #[allow(
        clippy::cast_precision_loss,
        reason = "Prometheus samples are floating point seconds"
    )]
    pub fn render(&self) -> String {
        let get = |value: &AtomicU64| value.load(Ordering::Relaxed);
        let mut text = format!(
            "# HELP lynx_world_ticks_total Authority ticks attempted.\n# TYPE lynx_world_ticks_total counter\nlynx_world_ticks_total {}\n# HELP lynx_world_tick_duration_seconds_total Time spent executing ticks in seconds.\n# TYPE lynx_world_tick_duration_seconds_total counter\nlynx_world_tick_duration_seconds_total {:.9}\n# HELP lynx_world_tick_overruns_total Ticks taking longer than the configured interval.\n# TYPE lynx_world_tick_overruns_total counter\nlynx_world_tick_overruns_total {}\n# HELP lynx_world_entities Current entities in the demo zone.\n# TYPE lynx_world_entities gauge\nlynx_world_entities {}\n# HELP lynx_world_connections Current attached connections.\n# TYPE lynx_world_connections gauge\nlynx_world_connections {}\n# HELP lynx_world_inputs_total Envelopes delivered to the authority.\n# TYPE lynx_world_inputs_total counter\nlynx_world_inputs_total {}\n",
            get(&self.ticks),
            get(&self.tick_nanos) as f64 / 1_000_000_000.0,
            get(&self.tick_overruns),
            get(&self.entities),
            get(&self.connections),
            get(&self.inputs)
        );
        let _ = write!(
            text,
            "# HELP lynx_world_tick_deadline_misses_total Scheduled ticks that began after their configured deadline.\n# TYPE lynx_world_tick_deadline_misses_total counter\nlynx_world_tick_deadline_misses_total {}\n",
            get(&self.tick_deadline_misses),
        );
        let _ = write!(
            text,
            "# HELP lynx_world_retained_sessions Current sessions including disconnected resumable sessions.\n# TYPE lynx_world_retained_sessions gauge\nlynx_world_retained_sessions {}\n# HELP lynx_world_action_requests_total Action requests delivered to the authority.\n# TYPE lynx_world_action_requests_total counter\nlynx_world_action_requests_total {}\n# HELP lynx_world_resume_attempts_total Resume admissions by outcome.\n# TYPE lynx_world_resume_attempts_total counter\nlynx_world_resume_attempts_total{{outcome=\"success\"}} {}\nlynx_world_resume_attempts_total{{outcome=\"failure\"}} {}\n# HELP lynx_world_outbound_queue_full_total Outbound queue full events.\n# TYPE lynx_world_outbound_queue_full_total counter\nlynx_world_outbound_queue_full_total {}\n# HELP lynx_world_action_rejections_total Authority action rejections by error code.\n# TYPE lynx_world_action_rejections_total counter\n",
            get(&self.retained_sessions),
            get(&self.actions),
            get(&self.resume_success),
            get(&self.resume_failure),
            get(&self.outbound_full)
        );
        for (code, count) in self.action_rejections.lock().unwrap().iter() {
            let _ = writeln!(
                text,
                "lynx_world_action_rejections_total{{code=\"{code}\"}} {count}"
            );
        }
        let _ = write!(
            text,
            "# HELP lynx_world_actions_applied_total Newly applied actions, excluding cached retries.\n# TYPE lynx_world_actions_applied_total counter\nlynx_world_actions_applied_total {}\n# HELP lynx_world_session_attaches_total Authority session attachments, including resumed sessions.\n# TYPE lynx_world_session_attaches_total counter\nlynx_world_session_attaches_total {}\n# HELP lynx_world_session_detaches_total Authority connection detach events.\n# TYPE lynx_world_session_detaches_total counter\nlynx_world_session_detaches_total {}\n# HELP lynx_world_session_expired_closes_total Connections closed because their session disappeared or expired.\n# TYPE lynx_world_session_expired_closes_total counter\nlynx_world_session_expired_closes_total {}\n# HELP lynx_world_outbound_queue_depth Sum of queued frames sampled during authority work.\n# TYPE lynx_world_outbound_queue_depth gauge\nlynx_world_outbound_queue_depth {}\n# HELP lynx_world_outbound_queue_capacity Sum of outbound queue capacities sampled during authority work.\n# TYPE lynx_world_outbound_queue_capacity gauge\nlynx_world_outbound_queue_capacity {}\n# HELP lynx_world_resume_failures_total Resume admission failures by error code.\n# TYPE lynx_world_resume_failures_total counter\n",
            get(&self.action_applied),
            get(&self.session_attaches),
            get(&self.session_detaches),
            get(&self.session_expired_closes),
            get(&self.queue_depth),
            get(&self.queue_capacity)
        );
        for (code, count) in self.resume_failure_codes.lock().unwrap().iter() {
            let _ = writeln!(
                text,
                "lynx_world_resume_failures_total{{code=\"{code}\"}} {count}"
            );
        }
        let _ = writeln!(
            text,
            "# HELP lynx_world_replication_enqueued_bytes_total Encoded snapshot and delta frame bytes successfully enqueued, not socket delivery bytes.\n# TYPE lynx_world_replication_enqueued_bytes_total counter\nlynx_world_replication_enqueued_bytes_total {}",
            get(&self.replication_enqueued_bytes)
        );
        let _ = write!(
            text,
            "# HELP lynx_world_replication_enqueued_frames_total Successfully enqueued replication frames by kind.\n# TYPE lynx_world_replication_enqueued_frames_total counter\nlynx_world_replication_enqueued_frames_total{{kind=\"snapshot\"}} {}\nlynx_world_replication_enqueued_frames_total{{kind=\"delta\"}} {}\n",
            get(&self.replication_snapshots_enqueued),
            get(&self.replication_deltas_enqueued)
        );
        let _ = writeln!(
            text,
            "# HELP lynx_world_ai_ticks_total Enabled AI ticks.\n# TYPE lynx_world_ai_ticks_total counter\nlynx_world_ai_ticks_total {}\n# HELP lynx_world_ai_decisions_total AI controller decisions evaluated.\n# TYPE lynx_world_ai_decisions_total counter\nlynx_world_ai_decisions_total {}\n# HELP lynx_world_ai_budget_exhaustions_total AI budget limits reached by bounded budget class.\n# TYPE lynx_world_ai_budget_exhaustions_total counter\nlynx_world_ai_budget_exhaustions_total{{budget=\"controller_scan\"}} {}\nlynx_world_ai_budget_exhaustions_total{{budget=\"update\"}} {}\nlynx_world_ai_budget_exhaustions_total{{budget=\"perception_candidates\"}} {}\nlynx_world_ai_budget_exhaustions_total{{budget=\"perception_line_of_sight\"}} {}\nlynx_world_ai_budget_exhaustions_total{{budget=\"action\"}} {}",
            get(&self.ai_ticks),
            get(&self.ai_decisions),
            get(&self.ai_controller_scan_limit),
            get(&self.ai_update_limit),
            get(&self.ai_perception_candidate_limit),
            get(&self.ai_perception_los_limit),
            get(&self.ai_action_budget_exhaustions),
        );
        let duration = self.action_duration.lock().unwrap();
        text.push_str("# HELP lynx_world_action_authority_duration_seconds Time in authority receive for action requests.\n# TYPE lynx_world_action_authority_duration_seconds histogram\n");
        for (bound, count) in ["0.001", "0.01", "0.1", "1", "10"]
            .iter()
            .zip(duration.buckets)
        {
            let _ = writeln!(
                text,
                "lynx_world_action_authority_duration_seconds_bucket{{le=\"{bound}\"}} {count}"
            );
        }
        let _ = write!(
            text,
            "lynx_world_action_authority_duration_seconds_bucket{{le=\"+Inf\"}} {}\nlynx_world_action_authority_duration_seconds_sum {:.9}\nlynx_world_action_authority_duration_seconds_count {}\n",
            duration.count,
            duration.nanos as f64 / 1_000_000_000.0,
            duration.count
        );
        let _ = write!(
            text,
            "# HELP lynx_world_tick_failures_total Authority ticks that returned an error.\n# TYPE lynx_world_tick_failures_total counter\nlynx_world_tick_failures_total {}\n# HELP lynx_world_snapshot_writes_total Snapshot store writes by outcome, including startup and shutdown.\n# TYPE lynx_world_snapshot_writes_total counter\nlynx_world_snapshot_writes_total{{outcome=\"success\"}} {}\nlynx_world_snapshot_writes_total{{outcome=\"failure\"}} {}\n",
            get(&self.tick_failures),
            get(&self.snapshot_success),
            get(&self.snapshot_failure)
        );
        let duration = self.snapshot_write_duration.lock().unwrap();
        text.push_str("# HELP lynx_world_snapshot_write_duration_seconds Time spent in snapshot store save calls, including failures.\n# TYPE lynx_world_snapshot_write_duration_seconds histogram\n");
        for (bound, count) in ["0.001", "0.01", "0.1", "1", "10"]
            .iter()
            .zip(duration.buckets)
        {
            let _ = writeln!(
                text,
                "lynx_world_snapshot_write_duration_seconds_bucket{{le=\"{bound}\"}} {count}"
            );
        }
        let _ = write!(
            text,
            "lynx_world_snapshot_write_duration_seconds_bucket{{le=\"+Inf\"}} {}\nlynx_world_snapshot_write_duration_seconds_sum {:.9}\nlynx_world_snapshot_write_duration_seconds_count {}\n",
            duration.count,
            duration.nanos as f64 / 1_000_000_000.0,
            duration.count
        );
        let _ = write!(
            text,
            "# HELP lynx_world_zone_lease_held Whether this process currently holds the zone lease (1) or not (0).\n# TYPE lynx_world_zone_lease_held gauge\nlynx_world_zone_lease_held {}\n# HELP lynx_world_zone_lease_renewals_total Zone lease renewal attempts by outcome.\n# TYPE lynx_world_zone_lease_renewals_total counter\nlynx_world_zone_lease_renewals_total{{outcome=\"success\"}} {}\nlynx_world_zone_lease_renewals_total{{outcome=\"failure\"}} {}\n",
            get(&self.lease_held),
            get(&self.lease_renew_success),
            get(&self.lease_renew_failure)
        );
        text
    }

    fn reject_action(&self, code: ErrorCode) {
        *self
            .action_rejections
            .lock()
            .unwrap()
            .entry(code.as_str())
            .or_default() += 1;
    }

    /// Record whether this process currently holds the zone lease.
    pub fn set_lease_held(&self, held: bool) {
        self.lease_held.store(u64::from(held), Ordering::Relaxed);
    }

    /// Count one lease renewal attempt by outcome.
    pub fn record_lease_renewal(&self, success: bool) {
        let counter = if success {
            &self.lease_renew_success
        } else {
            &self.lease_renew_failure
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }
}

/// How and when the service writes durable world snapshots.
#[derive(Debug, Clone)]
pub struct Persistence {
    /// The trusted snapshot store.
    store: Arc<dyn SnapshotStore + Send + Sync>,
    /// Ticks between automatic saves; one tick when the interval is shorter
    /// than the world tick.
    interval_ticks: u64,
}

impl std::fmt::Debug for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Service")
            .field("server", &self.server)
            .field("tick", &self.tick)
            .field("connections", &self.writers.len())
            .finish_non_exhaustive()
    }
}

impl Service {
    /// Wrap a world server and choose the tick interval.
    #[must_use]
    pub fn new(server: WorldServer, tick: Duration) -> Self {
        let service = Self {
            server,
            tick,
            writers: BTreeMap::new(),
            reply_sequence: 0,
            token_sequence: 0,
            persistence: None,
            authority: None,
            metrics: Arc::new(Metrics::default()),
        };
        service.update_gauges();
        service
    }

    /// Share measurements with the HTTP listener.
    #[must_use]
    pub fn metrics(&self) -> Arc<Metrics> {
        Arc::clone(&self.metrics)
    }

    fn update_gauges(&self) {
        let (depth, capacity) =
            self.writers
                .values()
                .fold((0u64, 0u64), |(depth, capacity), writer| {
                    (
                        depth.saturating_add((writer.max_capacity() - writer.capacity()) as u64),
                        capacity.saturating_add(writer.max_capacity() as u64),
                    )
                });
        self.metrics.queue_depth.store(depth, Ordering::Relaxed);
        self.metrics
            .queue_capacity
            .store(capacity, Ordering::Relaxed);
        self.metrics.entities.store(
            self.server.gateway().zone().entity_count() as u64,
            Ordering::Relaxed,
        );
        self.metrics
            .connections
            .store(self.server.connections().count() as u64, Ordering::Relaxed);
        self.metrics.retained_sessions.store(
            self.server.gateway().retained_sessions() as u64,
            Ordering::Relaxed,
        );
        let ai = self.server.gateway().zone().ai_diagnostics();
        self.metrics.ai_ticks.store(ai.ticks, Ordering::Relaxed);
        self.metrics
            .ai_decisions
            .store(ai.decisions, Ordering::Relaxed);
        self.metrics
            .ai_controller_scan_limit
            .store(ai.controller_scan_limit_reached, Ordering::Relaxed);
        self.metrics
            .ai_update_limit
            .store(ai.update_limit_reached, Ordering::Relaxed);
        self.metrics
            .ai_perception_candidate_limit
            .store(ai.perception_candidate_limit_reached, Ordering::Relaxed);
        self.metrics
            .ai_perception_los_limit
            .store(ai.perception_los_limit_reached, Ordering::Relaxed);
        self.metrics
            .ai_action_budget_exhaustions
            .store(ai.action_budget_exhaustions, Ordering::Relaxed);
    }

    /// Write world snapshots on a fixed interval and at shutdown.
    ///
    /// A save failure never stops the loop: the in-memory world stays
    /// authoritative and the error is reported through structured logging.
    #[must_use]
    pub fn with_persistence(
        mut self,
        store: Arc<dyn SnapshotStore + Send + Sync>,
        interval: Duration,
    ) -> Self {
        let tick_ms = u64::try_from(self.tick.as_millis()).unwrap_or(1).max(1);
        let interval_ticks = u64::try_from(interval.as_millis())
            .unwrap_or(1)
            .max(tick_ms)
            / tick_ms;
        self.persistence = Some(Persistence {
            store,
            interval_ticks: interval_ticks.max(1),
        });
        self
    }

    /// Take durable authority for the served zone.
    ///
    /// The caller must already hold the lease and must bind the same lease to
    /// the snapshot store; this method does not acquire or renew it. The
    /// service appends a `world.snapshot` event after each successful save and
    /// a `world.stopped` event when the loop ends. If another process takes the
    /// lease over, those writes fail with [`ErrorCode::Conflict`] instead of
    /// silently extending a log this process no longer owns.
    #[must_use]
    pub fn with_zone_authority(mut self, authority: ZoneAuthority) -> Self {
        self.metrics.set_lease_held(true);
        self.authority = Some(authority);
        self
    }

    /// Drive admission, input, replication, and shutdown until the channel
    /// closes or the shutdown flag flips.
    pub async fn run(
        mut self,
        mut events: mpsc::Receiver<ServerEvent>,
        mut shutdown: watch::Receiver<bool>,
    ) {
        self.update_gauges();
        let mut ticker = tokio::time::interval(self.tick);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        ticker.tick().await;
        let mut next_deadline = Instant::now() + self.tick;
        let mut ticks_since_save = 0u64;

        loop {
            tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                // A continuously ready input queue must not starve the world clock.
                // Delay prevents missed ticks from becoming an unbounded catch-up burst.
                _ = ticker.tick() => {
                    if Instant::now() > next_deadline {
                        self.metrics.tick_deadline_misses.fetch_add(1, Ordering::Relaxed);
                    }
                    next_deadline += self.tick;
                    self.advance();
                    ticks_since_save = ticks_since_save.wrapping_add(1);
                    if let Some(persistence) = &self.persistence
                        && ticks_since_save >= persistence.interval_ticks
                    {
                        ticks_since_save = 0;
                        self.save_snapshot();
                    }
                }
                event = events.recv() => {
                    let Some(event) = event else { break };
                    self.handle(event);
                }
            }
        }

        self.save_snapshot();
        self.close_all();
        self.stop_authority();
        tracing::debug!("world service loop stopped");
    }

    /// Capture and write one world snapshot now.
    ///
    /// Failures are recorded in the log and never stop the loop: the
    /// in-memory world stays authoritative. The authority loop calls this on
    /// its save cadence and at shutdown.
    pub fn save_snapshot(&mut self) {
        if let Err(error) = self.try_save_snapshot() {
            tracing::warn!(
                code = ?error.code(),
                details = %error,
                "the world snapshot could not be written"
            );
        }
    }

    /// Capture and write one world snapshot, returning write failures.
    ///
    /// Startup uses this to make the authored baseline — or a `--fresh` reset —
    /// durable before serving, where a failed write must be handled by the
    /// caller instead of only logged.
    ///
    /// # Errors
    ///
    /// Returns the store's error when the snapshot could not be written. A
    /// service without persistence succeeds without writing anything.
    pub fn try_save_snapshot(&mut self) -> Result<(), EngineError> {
        let Some(persistence) = &self.persistence else {
            return Ok(());
        };
        let store = Arc::clone(&persistence.store);
        let world = WorldSnapshot::capture(&self.snapshot_inputs(), clock().value());
        let started = Instant::now();
        let tick = self.server.gateway().zone().tick();
        let result = store.save_at_tick(&world, i64::try_from(tick.0).unwrap_or(i64::MAX));
        let elapsed = started.elapsed();
        self.metrics
            .snapshot_write_duration
            .lock()
            .unwrap()
            .observe(elapsed);
        let counter = if result.is_ok() {
            &self.metrics.snapshot_success
        } else {
            &self.metrics.snapshot_failure
        };
        counter.fetch_add(1, Ordering::Relaxed);
        if result.is_ok() && !store.records_snapshot_events_transactionally() {
            self.record_snapshot_event(&world);
        }
        result
    }

    /// Append a boundary marker for stores that do not write it transactionally.
    fn record_snapshot_event(&self, world: &WorldSnapshot) {
        let Some(authority) = &self.authority else {
            return;
        };
        let tick = self.server.gateway().zone().tick();
        let payload = serde_json::json!({
            "saved_at_ms": world.saved_at_ms,
            "tick": tick.0,
            "entities": u64::try_from(world.zone.entities.len()).unwrap_or(u64::MAX),
            "schema_version": world.schema_version.to_string(),
        });
        if let Err(error) = authority.append_event(tick, EVENT_WORLD_SNAPSHOT, None, &payload) {
            tracing::warn!(
                code = ?error.code(),
                details = %error,
                "the snapshot could not be recorded in the zone event log"
            );
        }
    }

    /// Record shutdown and give the zone lease back.
    fn stop_authority(&self) {
        let Some(authority) = &self.authority else {
            return;
        };
        let tick = self.server.gateway().zone().tick();
        let payload = serde_json::json!({"tick": tick.0});
        if let Err(error) = authority.append_event(tick, EVENT_WORLD_STOPPED, None, &payload) {
            tracing::warn!(
                code = ?error.code(),
                details = %error,
                "the world stop event could not be appended"
            );
        }
        match authority.release() {
            Ok(true) => tracing::info!(zone = authority.zone_id(), "zone lease released"),
            Ok(false) => tracing::warn!(
                zone = authority.zone_id(),
                "zone lease was already expired or superseded"
            ),
            Err(error) => tracing::warn!(
                code = ?error.code(),
                details = %error,
                "the zone lease could not be released"
            ),
        }
        self.metrics.set_lease_held(false);
    }

    /// The authoritative state a snapshot is built from.
    fn snapshot_inputs(&self) -> WorldSnapshotInputs {
        let gateway = self.server.gateway();
        let calendar_factions = gateway.calendar_stage().map(|stage| {
            let (calendar, factions) = stage.snapshots();
            (calendar, factions)
        });
        WorldSnapshotInputs {
            zone: gateway.zone().snapshot(),
            calendar_factions,
        }
    }

    fn handle(&mut self, event: ServerEvent) {
        match event {
            ServerEvent::Admit {
                credential,
                hello,
                outbound,
                reply,
            } => {
                let result = self.admit(&credential, &hello, &outbound);
                if matches!(credential, Credential::Resume(_)) {
                    let counter = if result.is_ok() {
                        &self.metrics.resume_success
                    } else {
                        &self.metrics.resume_failure
                    };
                    counter.fetch_add(1, Ordering::Relaxed);
                    if let Err(error) = &result {
                        *self
                            .metrics
                            .resume_failure_codes
                            .lock()
                            .unwrap()
                            .entry(error.code().as_str())
                            .or_default() += 1;
                    }
                }
                let _ = reply.send(result);
            }
            ServerEvent::Frame {
                connection,
                envelope,
            } => self.receive(connection, &envelope),
            ServerEvent::Leave { connection } => self.leave(connection),
        }
        self.update_gauges();
    }

    fn admit(
        &mut self,
        credential: &Credential,
        hello: &Hello,
        outbound: &mpsc::Sender<ServerFrame>,
    ) -> Result<Admitted, EngineError> {
        let now = clock();
        let opened = match credential {
            Credential::Static(grant) => {
                self.server.open(grant.claims()?, &hello.handshake, now)?
            }
            Credential::Identity(claims) => {
                // Character actors are deterministic and may be provisioned
                // on first admission, but admission is not acknowledged until
                // that actor is covered by the durable world snapshot. A
                // retry after a crash therefore either restores the same
                // actor or deterministically recreates it.
                let has_actor = match self.server.gateway().player_actor(claims.principal) {
                    Ok(_) => true,
                    Err(error) if error.code() == ErrorCode::NotFound => false,
                    Err(error) => return Err(error),
                };
                if !has_actor && self.persistence.is_none() {
                    return Err(EngineError::new(
                        ErrorCode::Internal,
                        "Nakama character provisioning requires a durable world snapshot",
                    ));
                }
                if self.persistence.is_some() {
                    self.server
                        .gateway_mut()
                        .ensure_player_actor(claims.principal)?;
                    self.try_save_snapshot()?;
                }
                self.server.open(claims.clone(), &hello.handshake, now)?
            }
            Credential::Resume(token) => {
                self.server
                    .resume(token, &hello.handshake, hello.last_ack_tick, now)?
            }
        };

        let connection = opened.connection;
        self.metrics
            .session_attaches
            .fetch_add(1, Ordering::Relaxed);
        self.writers.insert(connection, outbound.clone());
        self.push(&opened, connection);
        if !self.writers.contains_key(&connection) {
            return Err(EngineError::new(
                ErrorCode::CapacityExceeded,
                "the admission output could not be delivered",
            ));
        }
        let admitted = Admitted {
            connection,
            session: opened.session.clone(),
            actor: opened.actor,
            resumed: opened.resumed,
        };
        tracing::info!(
            connection = %connection,
            session = %admitted.session,
            actor = %admitted.actor,
            resumed = admitted.resumed,
            connections = self.writers.len(),
            "world session admitted"
        );
        Ok(admitted)
    }

    fn push(&mut self, opened: &Opened, connection: ConnectionId) {
        for envelope in &opened.messages {
            self.send(connection, ServerFrame::Envelope(envelope.clone()));
        }
        let token = self.token_envelope(opened);
        self.send(connection, ServerFrame::Envelope(token));
    }

    fn receive(&mut self, connection: ConnectionId, envelope: &Envelope) {
        // A transport can have already queued input when its writer fails.
        // Never let that input reach the authority after dropping the writer.
        if !self.writers.contains_key(&connection) {
            return;
        }
        self.metrics.inputs.fetch_add(1, Ordering::Relaxed);
        let action = envelope.kind.as_str() == "action.request";
        if action {
            self.metrics.actions.fetch_add(1, Ordering::Relaxed);
        }
        let before = self.server.gateway().zone().stats().applied_actions;
        let started = Instant::now();
        let result = self.server.receive(connection, envelope, clock());
        if action {
            self.metrics
                .action_duration
                .lock()
                .unwrap()
                .observe(started.elapsed());
            let after = self.server.gateway().zone().stats().applied_actions;
            self.metrics
                .action_applied
                .fetch_add(after.saturating_sub(before), Ordering::Relaxed);
        }
        match result {
            Ok(outbound) => {
                if action {
                    for output in &outbound {
                        if let Outbound::Message { envelope, .. } = output
                            && envelope.kind.as_str() == ERROR_KIND
                            && let Some(value) = envelope.body.get("code")
                            && let Ok(code) = serde_json::from_value::<ErrorCode>(value.clone())
                        {
                            self.metrics.reject_action(code);
                        }
                        if let Outbound::Message { envelope, .. } = output
                            && envelope.kind.as_str() == "action.result"
                            && let Ok(result) = serde_json::from_value::<
                                engine_sim::action::ActionResult,
                            >(envelope.body.clone())
                            && let Some(error) = result.error()
                        {
                            self.metrics.reject_action(error.code());
                        }
                        if matches!(
                            output,
                            Outbound::Closed {
                                reason: CloseReason::SessionExpired,
                                ..
                            }
                        ) {
                            self.metrics.reject_action(ErrorCode::SessionExpired);
                        }
                    }
                }
                self.dispatch(outbound);
            }
            Err(error) => {
                if action {
                    self.metrics.reject_action(error.code());
                }
                tracing::warn!(
                    connection = %connection,
                    code = ?error.code(),
                    "closing connection after input error"
                );
                let reply = self.error_reply(envelope, &error);
                self.send(connection, ServerFrame::Envelope(reply));
                self.close(connection);
            }
        }
    }

    fn leave(&mut self, connection: ConnectionId) {
        let _ = self.writers.remove(&connection);
        let last_ack = self.last_ack(connection);
        match self.server.disconnect(connection, last_ack, clock()) {
            Ok(outbound) => self.dispatch(outbound),
            Err(error) => {
                tracing::warn!(
                    connection = %connection,
                    code = ?error.code(),
                    "disconnect did not complete; revoking session"
                );
                self.revoke_after_disconnect_error(connection);
            }
        }
    }

    fn revoke_after_disconnect_error(&mut self, connection: ConnectionId) {
        let session = self
            .server
            .connections()
            .find_map(|(id, session)| (id == connection).then(|| session.clone()));
        if let Some(session) = session {
            // Revocation cancels queued movement and invalidates the token. A
            // second disconnect removes the server's now-stale connection.
            self.server.gateway_mut().close_session(&session);
            if let Err(error) = self.server.disconnect(connection, Tick::ZERO, clock()) {
                tracing::error!(connection = %connection, code = ?error.code(), "revoked connection could not be removed");
            }
        }
    }

    fn advance(&mut self) {
        let started = Instant::now();
        // This is the configured interval, not elapsed wall time: late ticks
        // do not grant extra movement or bank unused time.
        let interval_ms = u32::try_from(self.tick.as_millis()).unwrap_or(u32::MAX);
        match self.server.tick_with_interval(clock(), interval_ms) {
            Ok(outbound) => self.dispatch(outbound),
            Err(error) => {
                self.metrics.tick_failures.fetch_add(1, Ordering::Relaxed);
                tracing::error!(
                    code = ?error.code(),
                    "authoritative tick failed"
                );
            }
        }
        let elapsed = started.elapsed();
        self.metrics.ticks.fetch_add(1, Ordering::Relaxed);
        self.metrics.tick_nanos.fetch_add(
            u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        if elapsed > self.tick {
            self.metrics.tick_overruns.fetch_add(1, Ordering::Relaxed);
        }
        self.update_gauges();
    }

    fn dispatch(&mut self, outbound: Vec<Outbound>) {
        for output in outbound {
            match output {
                Outbound::Message {
                    connection,
                    envelope,
                } => self.send(connection, ServerFrame::Envelope(envelope)),
                Outbound::Closed {
                    connection,
                    session,
                    reason,
                } => {
                    match reason {
                        CloseReason::ClientDisconnected => &self.metrics.session_detaches,
                        CloseReason::SessionExpired => &self.metrics.session_expired_closes,
                    }
                    .fetch_add(1, Ordering::Relaxed);
                    Self::log_close(connection, &session, reason);
                    self.close(connection);
                }
            }
        }
    }

    fn send(&mut self, connection: ConnectionId, frame: ServerFrame) {
        let Some(writer) = self.writers.get_mut(&connection) else {
            return;
        };
        let replication_kind = match &frame {
            ServerFrame::Envelope(envelope)
                if envelope.kind.as_str() == world_server::SNAPSHOT_KIND =>
            {
                Some("snapshot")
            }
            ServerFrame::Envelope(envelope)
                if envelope.kind.as_str() == world_server::DELTA_KIND =>
            {
                Some("delta")
            }
            _ => None,
        };
        let frame = match frame {
            ServerFrame::Envelope(envelope)
                if matches!(
                    envelope.kind.as_str(),
                    world_server::SNAPSHOT_KIND | world_server::DELTA_KIND
                ) =>
            {
                match engine_net::framing::encode_frame(&envelope) {
                    Ok(bytes) => ServerFrame::Encoded(bytes),
                    // Preserve the transport's existing encode-failure handling.
                    Err(_) => ServerFrame::Envelope(envelope),
                }
            }
            frame => frame,
        };
        let replication_bytes = match (&frame, replication_kind) {
            (ServerFrame::Encoded(bytes), Some(kind)) => Some((bytes.len() as u64, kind)),
            _ => None,
        };
        match writer.try_send(frame) {
            Ok(()) => {
                if let Some((bytes, kind)) = replication_bytes {
                    self.metrics
                        .replication_enqueued_bytes
                        .fetch_add(bytes, Ordering::Relaxed);
                    let counter = if kind == "snapshot" {
                        &self.metrics.replication_snapshots_enqueued
                    } else {
                        &self.metrics.replication_deltas_enqueued
                    };
                    counter.fetch_add(1, Ordering::Relaxed);
                }
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.metrics.outbound_full.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    connection = %connection,
                    reason = "slow_client_outbound_queue_full",
                    "outbound queue is full; dropping the connection"
                );
                self.writers.remove(&connection);
                self.leave(connection);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.writers.remove(&connection);
                self.leave(connection);
            }
        }
    }

    fn close(&mut self, connection: ConnectionId) {
        if let Some(writer) = self.writers.remove(&connection) {
            let _ = writer.try_send(ServerFrame::Close);
        }
    }

    fn close_all(&mut self) {
        for (connection, writer) in std::mem::take(&mut self.writers) {
            let _ = writer.try_send(ServerFrame::Close);
            tracing::debug!(connection = %connection, "closing connection at shutdown");
        }
    }

    fn last_ack(&self, connection: ConnectionId) -> Tick {
        self.server
            .connections()
            .find(|(candidate, _)| *candidate == connection)
            .and_then(|(_, session)| self.server.gateway().session(session))
            .map_or(Tick::ZERO, |session| session.last_ack_tick)
    }

    fn token_envelope(&mut self, opened: &Opened) -> Envelope {
        self.token_sequence = self.token_sequence.wrapping_add(1);
        let id = MessageId::parse(&format!("tok-{:016x}", self.token_sequence))
            .expect("generated token message ids are valid");
        Envelope::new(
            id,
            message_kind(TOKEN_KIND),
            serde_json::json!({ "resume": opened.token.expose_secret() }),
        )
    }

    fn error_reply(&mut self, request: &Envelope, error: &EngineError) -> Envelope {
        self.reply_sequence = self.reply_sequence.wrapping_add(1);
        let id = MessageId::parse(&format!("err-{:016x}", self.reply_sequence))
            .expect("generated error message ids are valid");
        let body = serde_json::to_value(ErrorPayload::from(error))
            .unwrap_or_else(|_| serde_json::json!({ "code": "internal" }));
        request.reply(id, message_kind(ERROR_KIND), body)
    }

    fn log_close(connection: ConnectionId, session: &SessionId, reason: CloseReason) {
        match reason {
            CloseReason::ClientDisconnected => {
                tracing::info!(%connection, %session, "world session detached");
            }
            CloseReason::SessionExpired => {
                tracing::info!(%connection, %session, "world session expired");
            }
        }
    }
}

fn message_kind(value: &str) -> MessageKind {
    MessageKind::parse(value).expect("literal message kinds are valid")
}

fn clock() -> Millis {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    Millis::new(i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    use engine_core::id::IdKind;
    use engine_core::time::DurationMs;
    use engine_sim::zone::Zone;
    use tokio::sync::watch;

    use crate::auth::{Authenticator, digest};
    use crate::demo;
    use world_gateway::{GatewayConfig, SystemResumeTokens, WorldGateway};
    use world_server::{READY_KIND, SNAPSHOT_KIND};

    const TOKEN: &str = "service-test-token";

    #[test]
    fn metrics_follow_real_session_input_and_tick() {
        let (mut service, grant, _) = service();
        let metrics = service.metrics();
        assert!(metrics.render().contains("lynx_world_entities 5\n"));
        let (outbound, mut receiver) = mpsc::channel(16);
        let (reply, mut result) = oneshot::channel();
        service.handle(ServerEvent::Admit {
            credential: Credential::Static(grant),
            hello: Hello {
                handshake: demo::handshake().unwrap(),
                last_ack_tick: Tick::ZERO,
            },
            outbound,
            reply,
        });
        let admitted = result.try_recv().unwrap().unwrap();
        let initial = metrics.render();
        assert!(initial.contains("lynx_world_session_attaches_total 1\n"));
        assert!(initial.contains("lynx_world_outbound_queue_depth 3\n"));
        assert!(initial.contains("lynx_world_outbound_queue_capacity 16\n"));
        assert!(
            initial.contains("lynx_world_replication_enqueued_frames_total{kind=\"snapshot\"} 1\n")
        );
        assert!(
            initial.contains("lynx_world_replication_enqueued_frames_total{kind=\"delta\"} 0\n")
        );
        assert!(metrics.replication_enqueued_bytes.load(Ordering::Relaxed) > 0);
        let mut queued_bytes = 0;
        while let Ok(frame) = receiver.try_recv() {
            if let ServerFrame::Encoded(bytes) = frame {
                let envelope = engine_net::framing::decode_frame(&bytes).unwrap();
                assert_eq!(envelope.kind.as_str(), world_server::SNAPSHOT_KIND);
                queued_bytes += bytes.len() as u64;
            }
        }
        assert_eq!(
            metrics.replication_enqueued_bytes.load(Ordering::Relaxed),
            queued_bytes
        );
        service.handle(ServerEvent::Frame {
            connection: admitted.connection,
            envelope: Envelope::new(
                MessageId::parse("private-message").unwrap(),
                message_kind("bogus.kind"),
                serde_json::json!({"secret": TOKEN}),
            ),
        });
        service.advance();
        let text = metrics.render();
        assert!(text.contains("lynx_world_ticks_total 1\n"));
        assert!(text.contains("lynx_world_tick_failures_total 0\n"));
        assert!(text.contains("lynx_world_entities 5\n"));
        assert!(text.contains("lynx_world_connections 1\n"));
        assert!(text.contains("lynx_world_inputs_total 1\n"));
        assert!(text.contains("lynx_world_replication_enqueued_frames_total{kind=\"delta\"} 0\n"));
        assert!(!text.contains(TOKEN));
        assert!(!text.contains("private-message"));
        service.handle(ServerEvent::Leave {
            connection: admitted.connection,
        });
        assert!(metrics.render().contains("lynx_world_connections 0\n"));
        assert!(
            metrics
                .render()
                .contains("lynx_world_session_detaches_total 1\n")
        );
        assert!(
            metrics
                .render()
                .contains("lynx_world_outbound_queue_capacity 0\n")
        );
        assert!(
            metrics
                .render()
                .contains("lynx_world_retained_sessions 1\n")
        );
    }

    #[test]
    fn metrics_count_action_errors_and_resume_outcomes() {
        let (mut service, grant, _) = service();
        let metrics = service.metrics();
        let hello = Hello {
            handshake: demo::handshake().unwrap(),
            last_ack_tick: Tick::ZERO,
        };
        let (outbound, mut receiver) = mpsc::channel(16);
        let (reply, mut result) = oneshot::channel();
        service.handle(ServerEvent::Admit {
            credential: Credential::Static(grant),
            hello: hello.clone(),
            outbound,
            reply,
        });
        let admitted = result.try_recv().unwrap().unwrap();
        receiver.try_recv().unwrap();
        receiver.try_recv().unwrap();
        let ServerFrame::Envelope(token) = receiver.try_recv().unwrap() else {
            panic!("expected token")
        };
        let token = ResumeToken::new(token.body["resume"].as_str().unwrap()).unwrap();
        for (id, body) in [
            ("malformed", serde_json::json!({})),
            ("rejected", serde_json::json!({"verb": "open"})),
        ] {
            service.handle(ServerEvent::Frame {
                connection: admitted.connection,
                envelope: Envelope::new(
                    MessageId::parse(id).unwrap(),
                    message_kind("action.request"),
                    body,
                ),
            });
        }
        let ServerFrame::Envelope(malformed) = receiver.try_recv().unwrap() else {
            panic!("expected error reply")
        };
        assert_eq!(malformed.kind.as_str(), ERROR_KIND);
        let ServerFrame::Envelope(rejected) = receiver.try_recv().unwrap() else {
            panic!("expected action result")
        };
        assert_eq!(rejected.kind.as_str(), "action.result");
        let result: engine_sim::action::ActionResult =
            serde_json::from_value(rejected.body).unwrap();
        let code = result.error().unwrap().code();
        service.handle(ServerEvent::Leave {
            connection: admitted.connection,
        });
        for credential in [
            Credential::Resume(token),
            Credential::Resume(ResumeToken::new("unknown-secret-token").unwrap()),
        ] {
            let (outbound, _receiver) = mpsc::channel(16);
            let (reply, mut result) = oneshot::channel();
            service.handle(ServerEvent::Admit {
                credential,
                hello: hello.clone(),
                outbound,
                reply,
            });
            let _ = result.try_recv().unwrap();
        }
        let text = metrics.render();
        assert!(text.contains("lynx_world_action_requests_total 2\n"));
        assert!(text.contains("lynx_world_action_rejections_total{code=\"invalid_request\"} "));
        assert!(text.contains(&format!(
            "lynx_world_action_rejections_total{{code=\"{}\"}} ",
            code.as_str()
        )));
        assert!(text.contains("lynx_world_resume_attempts_total{outcome=\"success\"} 1\n"));
        assert!(text.contains("lynx_world_resume_attempts_total{outcome=\"failure\"} 1\n"));
        assert!(text.contains("lynx_world_resume_failures_total{code=\"session_expired\"} 1\n"));
        assert!(text.contains("lynx_world_action_authority_duration_seconds_count 2\n"));
        assert!(
            text.contains("lynx_world_action_authority_duration_seconds_bucket{le=\"+Inf\"} 2\n")
        );
        assert!(!text.contains("unknown-secret-token"));
    }

    fn service() -> (Service, Grant, Vec<Grant>) {
        let authenticator = Authenticator::new(&[digest(TOKEN)]).unwrap();
        let grants: Vec<Grant> = authenticator.grants().cloned().collect();
        let zone = demo::build_zone(&grants).unwrap();
        let gateway = WorldGateway::new(
            zone,
            demo::handshake().unwrap(),
            GatewayConfig::new(DurationMs::new(30_000)),
            SystemResumeTokens::new(),
        )
        .unwrap();
        let service = Service::new(WorldServer::new(gateway), Duration::from_millis(20));
        (service, grants[0].clone(), grants)
    }

    #[test]
    fn service_uses_configured_interval_for_each_movement_tick() {
        let (mut service, grant, _) = service();
        let (outbound, _receiver) = mpsc::channel(16);
        let admitted = service
            .admit(
                &Credential::Static(grant),
                &Hello {
                    handshake: demo::handshake().unwrap(),
                    last_ack_tick: Tick::ZERO,
                },
                &outbound,
            )
            .unwrap();
        let start = service
            .server
            .gateway()
            .zone()
            .entity(admitted.actor)
            .unwrap()
            .position();
        let request = |sequence, dx| {
            Envelope::new(
                MessageId::parse("movement-test").unwrap(),
                message_kind("movement.request"),
                serde_json::json!({"sequence": sequence, "destination": {"x": start.x + dx, "y": start.y, "z": start.z}}),
            )
        };
        service.receive(admitted.connection, &request(1, 0.05));
        service.advance();
        assert_eq!(
            service
                .server
                .gateway()
                .zone()
                .entity(admitted.actor)
                .unwrap()
                .position(),
            start
        );
        service.receive(admitted.connection, &request(2, 0.02));
        service.advance();
        assert_eq!(
            service
                .server
                .gateway()
                .zone()
                .entity(admitted.actor)
                .unwrap()
                .position()
                .x,
            start.x + 0.02
        );
        assert_eq!(service.server.gateway().zone().tick(), Tick(2));
    }

    #[test]
    fn disconnect_error_recovery_revokes_session_and_cancels_movement() {
        let (mut service, grant, _) = service();
        let hello = Hello {
            handshake: demo::handshake().unwrap(),
            last_ack_tick: Tick::ZERO,
        };
        let (outbound, mut receiver) = mpsc::channel(16);
        let admitted = service
            .admit(&Credential::Static(grant), &hello, &outbound)
            .unwrap();
        receiver.try_recv().unwrap();
        receiver.try_recv().unwrap();
        let ServerFrame::Envelope(token) = receiver.try_recv().unwrap() else {
            panic!("expected token");
        };
        let token = ResumeToken::new(token.body["resume"].as_str().unwrap()).unwrap();
        let start = service
            .server
            .gateway()
            .zone()
            .entity(admitted.actor)
            .unwrap()
            .position();
        service.receive(
            admitted.connection,
            &Envelope::new(
                MessageId::parse("movement-test").unwrap(),
                message_kind("movement.request"),
                serde_json::json!({"sequence": 1, "destination": {"x": start.x + 0.02, "y": start.y, "z": start.z}}),
            ),
        );

        // The current gateway cannot produce a non-NotFound disconnect error;
        // exercise the recovery directly as if one occurred after writer removal.
        service.writers.remove(&admitted.connection);
        service.revoke_after_disconnect_error(admitted.connection);
        assert_eq!(service.server.connections().count(), 0);
        assert!(
            service
                .server
                .gateway()
                .session(&admitted.session)
                .is_none()
        );
        service.advance();
        assert_eq!(
            service
                .server
                .gateway()
                .zone()
                .entity(admitted.actor)
                .unwrap()
                .position(),
            start
        );
        assert!(
            service
                .admit(&Credential::Resume(token), &hello, &outbound)
                .is_err()
        );
        service.revoke_after_disconnect_error(admitted.connection);
    }

    #[test]
    fn full_outbound_detaches_and_cancels_movement_before_resume() {
        let (mut service, grant, _) = service();
        let hello = Hello {
            handshake: demo::handshake().unwrap(),
            last_ack_tick: Tick::ZERO,
        };
        let (outbound, mut receiver) = mpsc::channel(3);
        let admitted = service
            .admit(&Credential::Static(grant), &hello, &outbound)
            .unwrap();
        receiver.try_recv().unwrap();
        receiver.try_recv().unwrap();
        let ServerFrame::Envelope(token) = receiver.try_recv().unwrap() else {
            panic!("expected token");
        };
        let token = ResumeToken::new(token.body["resume"].as_str().unwrap()).unwrap();
        let start = service
            .server
            .gateway()
            .zone()
            .entity(admitted.actor)
            .unwrap()
            .position();
        let movement = |sequence| {
            Envelope::new(
                MessageId::parse("movement-test").unwrap(),
                message_kind("movement.request"),
                serde_json::json!({"sequence": sequence, "destination": {"x": start.x + 0.02, "y": start.y, "z": start.z}}),
            )
        };
        for _ in 0..3 {
            outbound.try_send(ServerFrame::Close).unwrap();
        }
        // The movement is queued before its acknowledgement encounters backpressure.
        service.receive(admitted.connection, &movement(1));
        assert!(
            service
                .metrics
                .render()
                .contains("lynx_world_outbound_queue_full_total 1\n")
        );
        assert!(!service.writers.contains_key(&admitted.connection));
        assert!(
            service
                .server
                .connections()
                .all(|(id, _)| id != admitted.connection)
        );
        // Stale transport input must not revive work while the queue drains.
        service.receive(admitted.connection, &movement(2));
        service.advance();
        assert_eq!(
            service
                .server
                .gateway()
                .zone()
                .entity(admitted.actor)
                .unwrap()
                .position(),
            start
        );

        let (resume_tx, mut resume_rx) = mpsc::channel(16);
        let resumed = service
            .admit(&Credential::Resume(token), &hello, &resume_tx)
            .unwrap();
        assert!(resumed.resumed);
        assert_ne!(resumed.connection, admitted.connection);
        // A delayed Leave from the old pump cannot detach the resumed connection.
        service.leave(admitted.connection);
        for _ in 0..3 {
            resume_rx.try_recv().unwrap();
        }
        service.receive(resumed.connection, &movement(2));
        service.advance();
        assert_eq!(
            service
                .server
                .gateway()
                .zone()
                .entity(admitted.actor)
                .unwrap()
                .position()
                .x,
            start.x + 0.02
        );
    }

    #[test]
    fn failed_admission_delivery_does_not_leave_an_attached_connection() {
        let (mut service, grant, _) = service();
        let (outbound, _receiver) = mpsc::channel(1);
        let error = service
            .admit(
                &Credential::Static(grant),
                &Hello {
                    handshake: demo::handshake().unwrap(),
                    last_ack_tick: Tick::ZERO,
                },
                &outbound,
            )
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::CapacityExceeded);
        assert!(service.writers.is_empty());
        assert_eq!(service.server.connections().count(), 0);
    }

    #[test]
    fn closed_outbound_detaches_immediately() {
        let (mut service, grant, _) = service();
        let (outbound, receiver) = mpsc::channel(16);
        let admitted = service
            .admit(
                &Credential::Static(grant),
                &Hello {
                    handshake: demo::handshake().unwrap(),
                    last_ack_tick: Tick::ZERO,
                },
                &outbound,
            )
            .unwrap();
        drop(receiver);
        service.receive(
            admitted.connection,
            &Envelope::new(
                MessageId::parse("bad-kind").unwrap(),
                message_kind("bogus.kind"),
                serde_json::json!({}),
            ),
        );
        assert!(service.writers.is_empty());
        assert_eq!(service.server.connections().count(), 0);
    }

    async fn next(outbound: &mut mpsc::Receiver<ServerFrame>) -> Envelope {
        let frame = tokio::time::timeout(Duration::from_secs(2), outbound.recv())
            .await
            .expect("the service should answer")
            .expect("the queue should stay open");
        match frame {
            ServerFrame::Envelope(envelope) => envelope,
            ServerFrame::Encoded(bytes) => engine_net::framing::decode_frame(&bytes).unwrap(),
            ServerFrame::Close => panic!("unexpected close frame"),
        }
    }

    async fn admit(
        events: &mpsc::Sender<ServerEvent>,
        credential: Credential,
        outbound: mpsc::Sender<ServerFrame>,
    ) -> Result<Admitted, EngineError> {
        let (reply, result) = oneshot::channel();
        events
            .send(ServerEvent::Admit {
                credential,
                hello: Hello {
                    handshake: demo::handshake().unwrap(),
                    last_ack_tick: Tick::ZERO,
                },
                outbound,
                reply,
            })
            .await
            .unwrap();
        result.await.unwrap()
    }

    #[tokio::test]
    async fn a_static_token_admits_one_character_and_returns_a_resume_secret() {
        let (service, grant, _) = service();
        let (events, receiver) = mpsc::channel(8);
        let (shutdown, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(service.run(receiver, shutdown_rx));

        let (out_tx, mut out_rx) = mpsc::channel(16);
        let admitted = admit(&events, Credential::Static(grant.clone()), out_tx)
            .await
            .unwrap();
        assert!(!admitted.resumed);
        assert_eq!(admitted.actor.kind(), IdKind::Entity);

        assert_eq!(next(&mut out_rx).await.kind.as_str(), READY_KIND);
        assert_eq!(next(&mut out_rx).await.kind.as_str(), SNAPSHOT_KIND);
        let token = next(&mut out_rx).await;
        assert_eq!(token.kind.as_str(), TOKEN_KIND);
        let resume = ResumeToken::new(
            token.body["resume"]
                .as_str()
                .expect("the token body carries a string"),
        )
        .unwrap();

        // A second connection for the same character is rejected while the
        // first is attached.
        let (second_tx, _second_rx) = mpsc::channel(16);
        let duplicate = admit(&events, Credential::Static(grant), second_tx)
            .await
            .unwrap_err();
        assert_eq!(duplicate.code(), ErrorCode::Conflict);

        // Leaving keeps the session resumable with the rotated secret.
        events
            .send(ServerEvent::Leave {
                connection: admitted.connection,
            })
            .await
            .unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(2), out_rx.recv()).await;

        let (resume_tx, mut resume_rx) = mpsc::channel(16);
        let resumed = admit(&events, Credential::Resume(resume), resume_tx)
            .await
            .unwrap();
        assert!(resumed.resumed);
        assert_ne!(resumed.connection, admitted.connection);
        assert_eq!(next(&mut resume_rx).await.kind.as_str(), READY_KIND);
        let ready = next(&mut resume_rx).await;
        assert_eq!(ready.kind.as_str(), SNAPSHOT_KIND);
        assert_eq!(next(&mut resume_rx).await.kind.as_str(), TOKEN_KIND);

        shutdown.send(true).unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn an_unknown_resume_token_is_rejected_without_a_connection() {
        let (service, _, _) = service();
        let (events, receiver) = mpsc::channel(8);
        let (shutdown, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(service.run(receiver, shutdown_rx));

        let (out_tx, _out_rx) = mpsc::channel(16);
        let error = admit(
            &events,
            Credential::Resume(ResumeToken::new("unknown-resume-token").unwrap()),
            out_tx,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), ErrorCode::SessionExpired);

        shutdown.send(true).unwrap();
        task.await.unwrap();
    }

    #[test]
    fn zone_builds_for_multiple_grants() {
        let authenticator =
            Authenticator::new(&[digest("one"), digest("two"), digest("three")]).unwrap();
        let grants: Vec<Grant> = authenticator.grants().cloned().collect();
        let zone: Zone = demo::build_zone(&grants).unwrap();
        assert_eq!(zone.entity_count(), grants.len() * 5);
        assert_eq!(zone.tick(), Tick::ZERO);
    }

    #[tokio::test]
    #[allow(
        clippy::items_after_statements,
        clippy::too_many_lines,
        reason = "The end-to-end oversized-delivery scenario keeps helpers and assertions together."
    )]
    async fn oversized_delta_closes_websocket_and_resume_depends_on_snapshot_size() {
        use engine_core::id::DefinitionId;
        use engine_core::math::Vec3;
        use engine_net::framing::{decode_frame, encode_frame};
        use engine_sim::entity::EntityRecord;
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::connect_async;
        use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest, http::header};

        let authenticator = Authenticator::new(&[digest(TOKEN), digest("other-token")]).unwrap();
        let grants: Vec<_> = authenticator.grants().cloned().collect();
        let zone = demo::build_zone(&grants).unwrap();
        let gateway = WorldGateway::new(
            zone,
            demo::handshake().unwrap(),
            GatewayConfig::new(DurationMs::new(30_000)),
            SystemResumeTokens::new(),
        )
        .unwrap();
        let mut service = Service::new(WorldServer::new(gateway), Duration::from_secs(60));
        let (events, mut receiver) = crate::transport::event_channel();
        let state = Arc::new(crate::transport::AppState::new(events, authenticator));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (server_shutdown, server_shutdown_rx) = oneshot::channel();
        let server_task = tokio::spawn(crate::transport::serve_with_shutdown(
            listener,
            state,
            async {
                let _ = server_shutdown_rx.await;
            },
        ));

        // Commands are serialized with transport events just like normal service work.
        let (commands, mut command_rx) = mpsc::channel::<(bool, oneshot::Sender<()>)>(4);
        let authority = tokio::spawn(async move {
            loop {
                tokio::select! {
                    Some(event) = receiver.recv() => service.handle(event),
                    Some((add, done)) = command_rx.recv() => {
                        for seed in 100..500 {
                            let id = StableId::from_parts(IdKind::Entity, 9, seed);
                            if add {
                                service.server.gateway_mut().zone_mut().spawn(EntityRecord::new(
                                    id,
                                    DefinitionId::new("prop.snapshot_padding").unwrap(),
                                    Vec3::new(60.0, 0.0, 60.0),
                                )).unwrap();
                            } else {
                                service.server.gateway_mut().zone_mut().despawn(id).unwrap();
                            }
                        }
                        if add { service.advance(); }
                        let _ = done.send(());
                    }
                    else => break,
                }
            }
        });

        async fn connect(
            address: std::net::SocketAddr,
            token: &str,
        ) -> tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        > {
            let mut request = format!("ws://{address}/world")
                .into_client_request()
                .unwrap();
            request.headers_mut().insert(
                header::AUTHORIZATION,
                format!("Bearer {token}").parse().unwrap(),
            );
            let (mut socket, _) = connect_async(request).await.unwrap();
            let hello = Envelope::new(
                MessageId::parse("hello").unwrap(),
                message_kind(HELLO_KIND),
                serde_json::to_value(Hello {
                    handshake: demo::handshake().unwrap(),
                    last_ack_tick: Tick::ZERO,
                })
                .unwrap(),
            );
            socket
                .send(Message::Binary(encode_frame(&hello).unwrap().into()))
                .await
                .unwrap();
            socket
        }
        async fn frame(
            socket: &mut tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
        ) -> Message {
            tokio::time::timeout(Duration::from_secs(5), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
        }
        async fn envelope(
            socket: &mut tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
        ) -> Envelope {
            match frame(socket).await {
                Message::Binary(bytes) => decode_frame(&bytes).unwrap(),
                other => panic!("expected envelope, got {other:?}"),
            }
        }
        async fn admitted(
            socket: &mut tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            resumed: bool,
        ) -> String {
            let ready = envelope(socket).await;
            assert_eq!(ready.kind.as_str(), READY_KIND);
            assert_eq!(ready.body["resumed"], resumed);
            assert_eq!(envelope(socket).await.kind.as_str(), SNAPSHOT_KIND);
            let token = envelope(socket).await;
            assert_eq!(token.kind.as_str(), TOKEN_KIND);
            token.body["resume"].as_str().unwrap().to_owned()
        }
        async fn change(commands: &mpsc::Sender<(bool, oneshot::Sender<()>)>, add: bool) {
            let (done, wait) = oneshot::channel();
            commands.send((add, done)).await.unwrap();
            wait.await.unwrap();
        }

        // Grants are ordered by configured token, matching the demo's positions.
        let mut affected = connect(address, TOKEN).await;
        let token = admitted(&mut affected, false).await;
        let mut other = connect(address, "other-token").await;
        let _other_token = admitted(&mut other, false).await;
        change(&commands, true).await;
        assert!(matches!(frame(&mut affected).await, Message::Close(_)));
        // The other observer remains connected and can still submit requests.
        let request = Envelope::new(
            MessageId::parse("other-request").unwrap(),
            message_kind("bogus.kind"),
            serde_json::json!({}),
        );
        other
            .send(Message::Binary(encode_frame(&request).unwrap().into()))
            .await
            .unwrap();
        assert_eq!(envelope(&mut other).await.kind.as_str(), ERROR_KIND);

        change(&commands, false).await;
        let mut resumed = connect(address, &token).await;
        let rotated = admitted(&mut resumed, true).await;
        assert_ne!(token, rotated);

        change(&commands, true).await;
        assert!(matches!(frame(&mut resumed).await, Message::Close(_)));
        // The retained view is still too large: resume rejects and revokes the
        // rotated session rather than delivering a usable snapshot or token.
        let mut failed = connect(address, &rotated).await;
        assert_eq!(
            envelope(&mut failed).await.body["code"],
            "capacity_exceeded"
        );
        assert!(matches!(frame(&mut failed).await, Message::Close(_)));
        change(&commands, false).await;
        let mut replay = connect(address, &rotated).await;
        assert_eq!(envelope(&mut replay).await.body["code"], "session_expired");
        assert!(matches!(frame(&mut replay).await, Message::Close(_)));

        drop((affected, other, resumed, failed, replay, commands));
        authority.abort();
        let _ = server_shutdown.send(());
        tokio::time::timeout(Duration::from_secs(5), server_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    /// Counts saves and keeps the latest world snapshot.
    #[derive(Debug, Default)]
    struct MemoryStore {
        state: std::sync::Mutex<MemoryState>,
    }

    #[derive(Debug, Default)]
    struct MemoryState {
        saves: u64,
        last: Option<crate::persistence::WorldSnapshot>,
    }

    impl crate::persistence::SnapshotStore for MemoryStore {
        fn load(&self) -> Result<Option<crate::persistence::WorldSnapshot>, EngineError> {
            Ok(self.state.lock().unwrap().last.clone())
        }

        fn save(&self, snapshot: &crate::persistence::WorldSnapshot) -> Result<(), EngineError> {
            let mut state = self.state.lock().unwrap();
            state.saves += 1;
            state.last = Some(snapshot.clone());
            Ok(())
        }
    }

    #[test]
    fn durable_authority_records_snapshot_events_and_stops_on_takeover() {
        use crate::authority::tests::MemoryLeases;
        use crate::persistence::ZoneLeaseStore;

        let (service, _, _) = service();
        let zone = service
            .server
            .gateway()
            .zone()
            .config()
            .instance
            .to_string();
        let leases = Arc::new(MemoryLeases::default());
        let store: Arc<dyn ZoneLeaseStore> = leases.clone();
        let authority = ZoneAuthority::acquire(store, &zone, "owner-a", Duration::from_secs(30))
            .unwrap()
            .unwrap();
        let mut service = service
            .with_persistence(Arc::new(MemoryStore::default()), Duration::from_secs(1))
            .with_zone_authority(authority.clone());

        service.try_save_snapshot().unwrap();
        let events = authority.events_after(-1).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EVENT_WORLD_SNAPSHOT);
        assert_eq!(events[0].sequence, 0);
        assert_eq!(events[0].tick, 0);

        // A takeover increments the token; the old owner must not extend the log.
        assert!(authority.release().unwrap());
        let store: Arc<dyn ZoneLeaseStore> = leases.clone();
        let replacement = ZoneAuthority::acquire(store, &zone, "owner-b", Duration::from_secs(30))
            .unwrap()
            .unwrap();
        service.try_save_snapshot().unwrap();
        assert_eq!(
            authority.events_after(-1).unwrap().len(),
            1,
            "a superseded owner appends nothing"
        );
        service.stop_authority();
        assert_eq!(
            replacement.events_after(-1).unwrap().len(),
            1,
            "the stale owner's stop marker is fenced too"
        );
        assert_eq!(replacement.fencing_token(), authority.fencing_token() + 1);
    }

    #[derive(Debug, Default)]
    struct FailOnceStore {
        failed: std::sync::atomic::AtomicBool,
        last: std::sync::Mutex<Option<WorldSnapshot>>,
    }

    impl crate::persistence::SnapshotStore for FailOnceStore {
        fn load(&self) -> Result<Option<WorldSnapshot>, EngineError> {
            Ok(self.last.lock().unwrap().clone())
        }

        fn save(&self, snapshot: &WorldSnapshot) -> Result<(), EngineError> {
            if !self.failed.swap(true, Ordering::SeqCst) {
                return Err(EngineError::new(
                    ErrorCode::Internal,
                    "injected snapshot write failure",
                ));
            }
            *self.last.lock().unwrap() = Some(snapshot.clone());
            Ok(())
        }
    }

    #[test]
    fn failed_provision_snapshot_denies_admission_and_retry_reuses_actor() {
        use game_api::AccountId;
        use world_gateway::SessionClaims;

        let principal = StableId::from_parts(IdKind::Character, 1_700_000_000_002, 987_656);
        let claims =
            SessionClaims::new(AccountId::new("nakama-test-account").unwrap(), principal).unwrap();
        let (service, _, _) = service();
        let store = Arc::new(FailOnceStore::default());
        let mut service = service.with_persistence(store.clone(), Duration::from_secs(60));
        let (outbound, _receiver) = mpsc::channel(16);
        let hello = Hello {
            handshake: demo::handshake().unwrap(),
            last_ack_tick: Tick::ZERO,
        };

        assert!(
            service
                .admit(&Credential::Identity(claims.clone()), &hello, &outbound)
                .is_err()
        );
        assert_eq!(service.server.gateway().connected_sessions(), 0);
        let actor_count = service.server.gateway().zone().entity_count();
        let expected_actor = service
            .server
            .gateway()
            .player_actor(principal)
            .expect("retryable provision remains in memory");

        let admitted = service
            .admit(&Credential::Identity(claims), &hello, &outbound)
            .unwrap();
        assert_eq!(admitted.actor, expected_actor);
        assert_eq!(service.server.gateway().zone().entity_count(), actor_count);
        let saved = store.last.lock().unwrap().clone().unwrap();
        assert_eq!(
            saved
                .zone
                .entities
                .iter()
                .filter(|entity| entity.id == expected_actor)
                .count(),
            1
        );
    }

    #[test]
    fn identity_admission_persists_a_deterministic_player_actor_before_connecting() {
        use game_api::AccountId;
        use world_gateway::SessionClaims;

        let principal = StableId::from_parts(IdKind::Character, 1_700_000_000_000, 987_654);
        let claims =
            SessionClaims::new(AccountId::new("nakama-test-account").unwrap(), principal).unwrap();
        let (service, _, _) = service();
        let store = Arc::new(MemoryStore::default());
        let mut service = service.with_persistence(store.clone(), Duration::from_secs(60));
        let (outbound, _receiver) = mpsc::channel(16);

        let first = service
            .admit(
                &Credential::Identity(claims.clone()),
                &Hello {
                    handshake: demo::handshake().unwrap(),
                    last_ack_tick: Tick::ZERO,
                },
                &outbound,
            )
            .unwrap();
        assert_eq!(
            first.actor,
            StableId::from_parts(
                IdKind::Entity,
                principal.timestamp_ms(),
                principal.random_bits()
            )
        );
        let saved = store.state.lock().unwrap().last.clone().unwrap();
        assert!(
            saved
                .zone
                .entities
                .iter()
                .any(|entity| entity.id == first.actor)
        );
        assert_eq!(store.state.lock().unwrap().saves, 1);

        let restored_zone = demo::restore_zone(&saved.zone).unwrap();
        let restored_count = restored_zone.entity_count();
        assert!(restored_zone.entity(first.actor).is_some());
        let restored_gateway = WorldGateway::new(
            restored_zone,
            demo::handshake().unwrap(),
            GatewayConfig::new(DurationMs::new(30_000)),
            SystemResumeTokens::new(),
        )
        .unwrap();
        let mut restarted = Service::new(
            WorldServer::new(restored_gateway),
            Duration::from_millis(20),
        )
        .with_persistence(store, Duration::from_secs(60));
        let (restarted_outbound, _restarted_receiver) = mpsc::channel(16);
        let after_restart = restarted
            .admit(
                &Credential::Identity(claims),
                &Hello {
                    handshake: demo::handshake().unwrap(),
                    last_ack_tick: Tick::ZERO,
                },
                &restarted_outbound,
            )
            .unwrap();
        assert_eq!(after_restart.actor, first.actor);
        assert_eq!(
            restarted.server.gateway().zone().entity_count(),
            restored_count
        );
    }

    #[test]
    fn nakama_actor_provisioning_fails_closed_without_snapshot_storage() {
        use game_api::AccountId;
        use world_gateway::SessionClaims;

        let principal = StableId::from_parts(IdKind::Character, 1_700_000_000_001, 987_655);
        let claims =
            SessionClaims::new(AccountId::new("nakama-test-account").unwrap(), principal).unwrap();
        let (mut service, _, _) = service();
        let (outbound, _receiver) = mpsc::channel(16);

        let error = service
            .admit(
                &Credential::Identity(claims),
                &Hello {
                    handshake: demo::handshake().unwrap(),
                    last_ack_tick: Tick::ZERO,
                },
                &outbound,
            )
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::Internal);
        assert!(service.server.gateway().zone().entities().all(|entity| {
            entity
                .mind
                .as_ref()
                .and_then(|mind| mind.controller.player_character())
                != Some(principal)
        }));
    }

    /// Observes whether the last queued admission has completed at each save.
    #[derive(Debug, Default)]
    struct BacklogStore {
        last_reply: std::sync::Mutex<Option<oneshot::Receiver<Result<Admitted, EngineError>>>>,
        observations: std::sync::Mutex<Vec<(Tick, bool)>>,
    }

    impl crate::persistence::SnapshotStore for BacklogStore {
        fn load(&self) -> Result<Option<WorldSnapshot>, EngineError> {
            Ok(None)
        }

        fn save(&self, snapshot: &WorldSnapshot) -> Result<(), EngineError> {
            let reply = self.last_reply.lock().unwrap();
            let pending = reply.as_ref().unwrap().is_empty();
            self.observations
                .lock()
                .unwrap()
                .push((snapshot.zone.tick, pending));
            Ok(())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn due_tick_and_save_run_before_a_ready_admission_backlog() {
        let (service, grant, _) = service();
        let store = Arc::new(BacklogStore::default());
        let service = service.with_persistence(store.clone(), Duration::from_millis(20));
        let (events, receiver) = mpsc::channel(32);
        let (shutdown, shutdown_rx) = watch::channel(false);
        let task = service.run(receiver, shutdown_rx);
        tokio::pin!(task);
        assert!(futures_util::poll!(&mut task).is_pending());
        let (out_tx, mut out_rx) = mpsc::channel(32);
        let (reply, admitted) = oneshot::channel();
        events
            .try_send(ServerEvent::Admit {
                credential: Credential::Static(grant.clone()),
                hello: Hello {
                    handshake: demo::handshake().unwrap(),
                    last_ack_tick: Tick::ZERO,
                },
                outbound: out_tx.clone(),
                reply,
            })
            .unwrap();
        assert!(futures_util::poll!(&mut task).is_pending());
        admitted.await.unwrap().unwrap();
        for _ in 0..3 {
            next(&mut out_rx).await;
        }

        // Keep the service unpolled until the clock and queue are both ready.
        tokio::time::advance(Duration::from_millis(21)).await;
        // Queue real duplicate admissions without yielding to the service.
        let mut replies = Vec::new();
        for _ in 0..16 {
            let (reply, result) = oneshot::channel();
            events
                .try_send(ServerEvent::Admit {
                    credential: Credential::Static(grant.clone()),
                    hello: Hello {
                        handshake: demo::handshake().unwrap(),
                        last_ack_tick: Tick::ZERO,
                    },
                    outbound: out_tx.clone(),
                    reply,
                })
                .unwrap();
            replies.push(result);
        }
        *store.last_reply.lock().unwrap() = replies.pop();

        assert!(futures_util::poll!(&mut task).is_pending());
        for reply in replies {
            assert_eq!(
                reply.await.unwrap().unwrap_err().code(),
                ErrorCode::Conflict
            );
        }
        let last = store.last_reply.lock().unwrap().take().unwrap();
        assert_eq!(last.await.unwrap().unwrap_err().code(), ErrorCode::Conflict);
        let observations = store.observations.lock().unwrap().clone();
        assert_eq!(observations, vec![(Tick(1), true)]);

        // Restore a pending receiver for the final shutdown save.
        let (_reply, pending) = oneshot::channel();
        *store.last_reply.lock().unwrap() = Some(pending);
        shutdown.send(true).unwrap();
        task.await;
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_preempts_a_due_tick_and_queued_admission() {
        let (service, grant, _) = service();
        let store = Arc::new(MemoryStore::default());
        let service = service.with_persistence(store.clone(), Duration::from_millis(20));
        let (events, receiver) = mpsc::channel(8);
        let (shutdown, shutdown_rx) = watch::channel(false);
        let task = service.run(receiver, shutdown_rx);
        tokio::pin!(task);
        assert!(futures_util::poll!(&mut task).is_pending());
        tokio::time::advance(Duration::from_millis(21)).await;

        let (outbound, _out_rx) = mpsc::channel(16);
        let (reply, result) = oneshot::channel();
        events
            .try_send(ServerEvent::Admit {
                credential: Credential::Static(grant),
                hello: Hello {
                    handshake: demo::handshake().unwrap(),
                    last_ack_tick: Tick::ZERO,
                },
                outbound,
                reply,
            })
            .unwrap();
        shutdown.send(true).unwrap();
        task.await;
        assert!(result.await.is_err());
        let state = store.state.lock().unwrap();
        assert_eq!(state.saves, 1);
        assert_eq!(state.last.as_ref().unwrap().zone.tick, Tick::ZERO);
    }

    /// Rejects every write so startup can prove it handles the failure.
    #[derive(Debug)]
    struct FailingStore;

    impl crate::persistence::SnapshotStore for FailingStore {
        fn load(&self) -> Result<Option<crate::persistence::WorldSnapshot>, EngineError> {
            Ok(None)
        }

        fn save(&self, _snapshot: &crate::persistence::WorldSnapshot) -> Result<(), EngineError> {
            Err(EngineError::new(
                ErrorCode::Internal,
                "the test store rejects every write",
            ))
        }
    }

    #[test]
    fn a_rejected_snapshot_write_surfaces_to_the_caller() {
        let (mut service, _, _) = service();
        service.try_save_snapshot().unwrap();
        assert!(
            service
                .metrics()
                .render()
                .contains("lynx_world_snapshot_writes_total{outcome=\"success\"} 0\n")
        );
        assert!(
            service
                .metrics()
                .render()
                .contains("lynx_world_snapshot_write_duration_seconds_count 0\n")
        );
        let store: Arc<dyn crate::persistence::SnapshotStore + Send + Sync> =
            Arc::new(FailingStore);
        let mut service = service.with_persistence(store, Duration::from_millis(1));

        let error = service
            .try_save_snapshot()
            .expect_err("a failed write must reach startup");
        assert_eq!(error.code(), ErrorCode::Internal);

        // The cadence path stays best-effort and never panics.
        service.save_snapshot();
        assert!(
            service
                .metrics()
                .render()
                .contains("lynx_world_snapshot_writes_total{outcome=\"failure\"} 2\n")
        );
        assert!(
            service
                .metrics()
                .render()
                .contains("lynx_world_snapshot_write_duration_seconds_count 2\n")
        );
    }

    #[test]
    fn successful_snapshot_write_is_counted() {
        let (service, _, _) = service();
        let mut service =
            service.with_persistence(Arc::new(MemoryStore::default()), Duration::from_secs(1));
        service.try_save_snapshot().unwrap();
        let text = service.metrics().render();
        assert!(text.contains("lynx_world_snapshot_writes_total{outcome=\"success\"} 1\n"));
        assert!(text.contains("lynx_world_snapshot_writes_total{outcome=\"failure\"} 0\n"));
        assert!(text.contains("lynx_world_snapshot_write_duration_seconds_count 1\n"));
        assert!(
            text.contains("lynx_world_snapshot_write_duration_seconds_bucket{le=\"+Inf\"} 1\n")
        );
    }

    #[test]
    fn snapshot_write_histogram_has_cumulative_inclusive_buckets() {
        let metrics = Metrics::default();
        {
            let mut duration = metrics.snapshot_write_duration.lock().unwrap();
            duration.observe(Duration::from_millis(1));
            duration.observe(Duration::from_millis(11));
            duration.observe(Duration::from_secs(11));
        }
        let text = metrics.render();
        assert!(text.contains("# TYPE lynx_world_snapshot_write_duration_seconds histogram\n"));
        for (bound, count) in [
            ("0.001", 1),
            ("0.01", 1),
            ("0.1", 2),
            ("1", 2),
            ("10", 2),
            ("+Inf", 3),
        ] {
            assert!(text.contains(&format!(
                "lynx_world_snapshot_write_duration_seconds_bucket{{le=\"{bound}\"}} {count}\n"
            )));
        }
        assert!(text.contains("lynx_world_snapshot_write_duration_seconds_sum 11.012000000\n"));
        assert!(text.contains("lynx_world_snapshot_write_duration_seconds_count 3\n"));
    }

    #[test]
    fn scheduler_deadline_misses_are_prometheus_counters() {
        let metrics = Metrics::default();
        metrics.tick_deadline_misses.store(2, Ordering::Relaxed);
        let text = metrics.render();
        assert!(text.contains("# TYPE lynx_world_tick_deadline_misses_total counter\n"));
        assert!(text.contains("lynx_world_tick_deadline_misses_total 2\n"));
    }

    #[test]
    fn snapshot_write_histogram_times_the_store_call() {
        #[derive(Debug)]
        struct SlowStore;
        impl SnapshotStore for SlowStore {
            fn load(&self) -> Result<Option<WorldSnapshot>, EngineError> {
                Ok(None)
            }

            fn save(&self, _: &WorldSnapshot) -> Result<(), EngineError> {
                std::thread::sleep(Duration::from_millis(5));
                Ok(())
            }
        }

        let (service, _, _) = service();
        let mut service = service.with_persistence(Arc::new(SlowStore), Duration::from_secs(1));
        service.try_save_snapshot().unwrap();
        let metrics = service.metrics();
        let duration = metrics.snapshot_write_duration.lock().unwrap();
        assert_eq!(duration.count, 1);
        assert_eq!(duration.buckets[0], 0);
        assert!(duration.nanos >= 5_000_000);
    }

    #[tokio::test]
    async fn the_service_snapshots_the_world_and_saves_once_more_at_shutdown() {
        let (service, grant, _) = service();
        let counts = Arc::new(MemoryStore::default());
        let store: Arc<dyn crate::persistence::SnapshotStore + Send + Sync> =
            Arc::clone(&counts) as _;
        let service = service.with_persistence(store, Duration::from_millis(1));
        let metrics = service.metrics();

        let (events, receiver) = mpsc::channel(8);
        let (shutdown, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(service.run(receiver, shutdown_rx));

        let (out_tx, mut out_rx) = mpsc::channel(16);
        let _admitted = admit(&events, Credential::Static(grant), out_tx)
            .await
            .unwrap();
        // Drain the admission messages so the writer queue cannot back up.
        while tokio::time::timeout(Duration::from_millis(50), out_rx.recv())
            .await
            .is_ok()
        {}

        tokio::time::sleep(Duration::from_millis(120)).await;
        let saves_while_up = counts.state.lock().unwrap().saves;
        assert!(saves_while_up >= 1, "the interval cadence saves the world");

        shutdown.send(true).unwrap();
        task.await.unwrap();
        let state = counts.state.lock().unwrap();
        assert!(
            state.saves > saves_while_up,
            "shutdown performs a final save"
        );
        assert_eq!(
            metrics.snapshot_write_duration.lock().unwrap().count,
            state.saves
        );
        let last = state.last.as_ref().unwrap();
        assert_eq!(last.zone.config.definition.as_str(), "zone.demo");
        assert!(
            last.calendar.is_none() && last.factions.is_none(),
            "this test gateway runs no calendar, so no calendar snapshot is written"
        );
    }
}
