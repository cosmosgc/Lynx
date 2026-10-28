//! The single-threaded authority loop for the runnable world service.
//!
//! Sockets never touch [`WorldServer`](world_server::WorldServer) directly.
//! Each WebSocket task forwards admission requests, decoded envelopes, and
//! disconnects over one channel; this loop serializes them with the tick,
//! dispatches per-connection output, and closes connections whose sessions
//! ended. Keeping the simulation single-threaded preserves the gateway's
//! ordering guarantees and makes backpressure explicit.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
        Self {
            server,
            tick,
            writers: BTreeMap::new(),
            reply_sequence: 0,
            token_sequence: 0,
            persistence: None,
        }
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

    /// Drive admission, input, replication, and shutdown until the channel
    /// closes or the shutdown flag flips.
    pub async fn run(
        mut self,
        mut events: mpsc::Receiver<ServerEvent>,
        mut shutdown: watch::Receiver<bool>,
    ) {
        let mut ticker = tokio::time::interval(self.tick);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        ticker.tick().await;
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
        store.save(&world)
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
                let _ = reply.send(result);
            }
            ServerEvent::Frame {
                connection,
                envelope,
            } => self.receive(connection, &envelope),
            ServerEvent::Leave { connection } => self.leave(connection),
        }
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
                if let Some(existing) = self.connected_principal(grant.principal) {
                    return Err(EngineError::new(
                        ErrorCode::Conflict,
                        "the character already has a connected session",
                    )
                    .with_detail("connection", existing.to_string()));
                }
                let claims: SessionClaims = grant.claims()?;
                self.server.open(claims, &hello.handshake, now)?
            }
            Credential::Resume(token) => {
                self.server
                    .resume(token, &hello.handshake, hello.last_ack_tick, now)?
            }
        };

        let connection = opened.connection;
        self.writers.insert(connection, outbound.clone());
        self.push(&opened, connection);
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
        match self.server.receive(connection, envelope, clock()) {
            Ok(outbound) => self.dispatch(outbound),
            Err(error) => {
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
        let last_ack = self.last_ack(connection);
        match self.server.disconnect(connection, last_ack, clock()) {
            Ok(outbound) => self.dispatch(outbound),
            Err(error) => {
                tracing::warn!(
                    connection = %connection,
                    code = ?error.code(),
                    "disconnect did not complete"
                );
            }
        }
        let _ = self.writers.remove(&connection);
    }

    fn advance(&mut self) {
        match self.server.tick(clock()) {
            Ok(outbound) => self.dispatch(outbound),
            Err(error) => {
                tracing::error!(
                    code = ?error.code(),
                    "authoritative tick failed"
                );
            }
        }
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
        match writer.try_send(frame) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!(
                    connection = %connection,
                    "outbound queue is full; dropping the connection"
                );
                self.writers.remove(&connection);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.writers.remove(&connection);
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

    fn connected_principal(&self, principal: StableId) -> Option<ConnectionId> {
        self.server.connections().find_map(|(connection, session)| {
            self.server
                .gateway()
                .session(session)
                .filter(|active| active.principal == principal)
                .map(|_| connection)
        })
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

    async fn next(outbound: &mut mpsc::Receiver<ServerFrame>) -> Envelope {
        let frame = tokio::time::timeout(Duration::from_secs(2), outbound.recv())
            .await
            .expect("the service should answer")
            .expect("the queue should stay open");
        match frame {
            ServerFrame::Envelope(envelope) => envelope,
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
        let (service, _, _) = service();
        let store: Arc<dyn crate::persistence::SnapshotStore + Send + Sync> =
            Arc::new(FailingStore);
        let mut service = service.with_persistence(store, Duration::from_millis(1));

        let error = service
            .try_save_snapshot()
            .expect_err("a failed write must reach startup");
        assert_eq!(error.code(), ErrorCode::Internal);

        // The cadence path stays best-effort and never panics.
        service.save_snapshot();
    }

    #[tokio::test]
    async fn the_service_snapshots_the_world_and_saves_once_more_at_shutdown() {
        let (service, grant, _) = service();
        let counts = Arc::new(MemoryStore::default());
        let store: Arc<dyn crate::persistence::SnapshotStore + Send + Sync> =
            Arc::clone(&counts) as _;
        let service = service.with_persistence(store, Duration::from_millis(1));

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
        let last = state.last.as_ref().unwrap();
        assert_eq!(last.zone.config.definition.as_str(), "zone.demo");
        assert!(
            last.calendar.is_none() && last.factions.is_none(),
            "this test gateway runs no calendar, so no calendar snapshot is written"
        );
    }
}
