//! Connection lifecycle and wire delivery for one authoritative zone.
//!
//! [`WorldServer`] sits directly on top of [`WorldGateway`] and removes the
//! remaining work every transport would otherwise repeat:
//!
//! - allocate server-side connection IDs and map them to world sessions;
//! - turn the gateway's typed [`SessionDelivery`] values into versioned
//!   envelopes (`world.ready`, `world.snapshot`, `world.delta`);
//! - return correlated action, UI control, and protocol-error replies to the
//!   right connection;
//! - keep disconnect, resume, and session-revocation bookkeeping consistent.
//!
//! It owns no socket, thread, async runtime, or clock. A transport authenticates
//! a client, supplies trusted [`SessionClaims`], then moves frames and
//! [`Outbound`] values. That keeps authentication and TLS policy outside the
//! gameplay loop while still giving every transport the same session rules.

use std::collections::BTreeMap;
use std::fmt;

use engine_core::error::{EngineError, ErrorCode};
use engine_core::id::StableId;
use engine_core::time::{Millis, Tick};
use engine_net::envelope::{Envelope, MessageId, MessageKind};
use engine_net::interest::InterestUpdate;
use engine_net::protocol::{Handshake, Negotiated};
use engine_net::replication::{ReplicationBatch, SessionId};
use serde::{Deserialize, Serialize};
use world_gateway::replication::MovementReconciliation;
use world_gateway::{Connection, ResumeToken, SessionClaims, SessionDelivery, WorldGateway};

/// Sent once per successful open or resume.
pub const READY_KIND: &str = "world.ready";

/// Sent after a successful open or resume with the full current view.
pub const SNAPSHOT_KIND: &str = "world.snapshot";

/// Sent after a tick with the session's interest delta and replication.
pub const DELTA_KIND: &str = "world.delta";

/// A transport-owned connection identifier.
///
/// Connection IDs are allocated by the server loop and are not session IDs.
/// One session may move to a new connection after a resume; the ID is only a
/// routing label for the active transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConnectionId(u64);

impl ConnectionId {
    /// The raw counter value, for logs and transport maps.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

impl fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "conn-{:016x}", self.0)
    }
}

/// The body of `world.snapshot` and `world.delta`.
///
/// It carries the authoritative tick, the observer's interest delta, and the
/// audience-filtered replication batch. The receiving session is implied by the
/// transport, so it is not repeated in the body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorldUpdate {
    /// Authoritative tick shared by the interest and replication data.
    pub tick: Tick,
    /// Entities that entered or left the observer's view.
    pub interest: InterestUpdate,
    /// Replication updates after audience filtering.
    pub batch: ReplicationBatch,
    /// Session-private movement baseline or processing result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub movement: Option<MovementReconciliation>,
}

impl WorldUpdate {
    /// Capture a gateway delivery as a wire body.
    #[must_use]
    pub fn from_delivery(delivery: &SessionDelivery) -> Self {
        Self {
            tick: delivery.tick,
            interest: delivery.interest.clone(),
            batch: delivery.batch.clone(),
            movement: delivery.movement.clone(),
        }
    }
}

/// The body of `world.ready`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ready {
    /// Gateway-issued session identifier.
    pub session: SessionId,
    /// Entity the session may drive.
    pub actor: StableId,
    /// Whether this connection resumed an existing session.
    pub resumed: bool,
    /// Negotiated protocol, content, locale, and features.
    pub negotiated: Negotiated,
}

/// A successful open or resume.
///
/// The resume token is deliberately not part of [`Self::messages`]: the
/// transport stores it as a bearer secret for reconnect, and it must never be
/// logged or delivered to the client in a replication body.
#[derive(Debug, Clone)]
pub struct Opened {
    /// Transport connection that now owns the session.
    pub connection: ConnectionId,
    /// Gateway-issued session identifier.
    pub session: SessionId,
    /// Entity the session may drive.
    pub actor: StableId,
    /// Negotiated protocol, content, locale, and features.
    pub negotiated: Negotiated,
    /// Single-use resume token for this session.
    pub token: ResumeToken,
    /// Whether an existing session was resumed.
    pub resumed: bool,
    /// `world.ready` followed by `world.snapshot`.
    pub messages: Vec<Envelope>,
}

/// Why the server loop closed a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// The transport disconnected or was detached after an unsendable update.
    ClientDisconnected,
    /// The session expired, disappeared, or lost its actor.
    SessionExpired,
}

/// Work the transport must deliver.
#[derive(Debug, Clone)]
pub enum Outbound {
    /// One envelope for one connection.
    Message {
        /// Destination connection.
        connection: ConnectionId,
        /// Envelope to frame and send.
        envelope: Envelope,
    },
    /// The connection no longer has a session and should be closed.
    Closed {
        /// Connection to close.
        connection: ConnectionId,
        /// Session that ended, for logging.
        session: SessionId,
        /// Why the session ended.
        reason: CloseReason,
    },
}

impl Outbound {
    /// The connection this output targets.
    #[must_use]
    pub const fn connection(&self) -> ConnectionId {
        match self {
            Self::Message { connection, .. } | Self::Closed { connection, .. } => *connection,
        }
    }
}

/// Connection ownership and wire delivery for one [`WorldGateway`].
pub struct WorldServer {
    gateway: WorldGateway,
    connections: BTreeMap<ConnectionId, SessionId>,
    next_connection: u64,
    next_message: u64,
}

impl fmt::Debug for WorldServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorldServer")
            .field("gateway", &self.gateway)
            .field("connections", &self.connections.len())
            .field("next_connection", &self.next_connection)
            .field("next_message", &self.next_message)
            .finish_non_exhaustive()
    }
}

impl WorldServer {
    /// Wrap an existing gateway.
    #[must_use]
    pub fn new(gateway: WorldGateway) -> Self {
        Self {
            gateway,
            connections: BTreeMap::new(),
            next_connection: 0,
            next_message: 0,
        }
    }

    /// The authoritative gateway. Server systems may inspect the zone.
    #[must_use]
    pub const fn gateway(&self) -> &WorldGateway {
        &self.gateway
    }

    /// Mutable gateway access for loaders, tests, and authoritative systems.
    /// Client code never receives this handle.
    pub const fn gateway_mut(&mut self) -> &mut WorldGateway {
        &mut self.gateway
    }

    /// Number of currently attached connections.
    #[must_use]
    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }

    /// Iterate over attached connections in stable ID order.
    pub fn connections(&self) -> impl Iterator<Item = (ConnectionId, &SessionId)> {
        self.connections
            .iter()
            .map(|(connection, session)| (*connection, session))
    }

    /// The active connection for a session, if any.
    #[must_use]
    pub fn connection_for(&self, session: &SessionId) -> Option<ConnectionId> {
        self.connections
            .iter()
            .find(|(_, active)| *active == session)
            .map(|(connection, _)| *connection)
    }

    /// Create a session for already-authenticated claims.
    ///
    /// # Errors
    ///
    /// Returns the same handshake, authority, capacity, and conflict errors as
    /// [`WorldGateway::connect`]. No connection or session is retained on
    /// failure. An initial snapshot that cannot be encoded within the wire
    /// limits also fails admission with no retained session.
    pub fn open(
        &mut self,
        claims: SessionClaims,
        client: &Handshake,
        now: Millis,
    ) -> Result<Opened, EngineError> {
        let established = self.gateway.connect(claims, client, now)?;
        let connection = self.allocate_connection();
        self.attach(connection, established)
    }

    /// Resume a detached session on a new transport connection.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`WorldGateway::resume`]: `SessionExpired`
    /// for an unknown, expired, or replayed token, `Conflict` while the session
    /// is still attached, and handshake errors for an incompatible client. If
    /// the replacement snapshot cannot be encoded within the wire limits, the
    /// resumed session is revoked; the rotated token cannot be delivered.
    pub fn resume(
        &mut self,
        token: &ResumeToken,
        client: &Handshake,
        last_ack_tick: Tick,
        now: Millis,
    ) -> Result<Opened, EngineError> {
        let established = self.gateway.resume(token, client, last_ack_tick, now)?;
        let connection = self.allocate_connection();
        self.connections
            .retain(|_, session| session != &established.session);
        self.attach(connection, established)
    }

    /// Route one validated envelope from a connection.
    ///
    /// Ordinary gameplay rejections and unknown message kinds arrive as
    /// [`Outbound::Message`] envelopes. A session that expired or disappeared
    /// produces [`Outbound::Closed`] with [`CloseReason::SessionExpired`].
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::NotFound`] for an unknown connection and the
    /// envelope validation errors from [`WorldGateway::handle`]. The transport
    /// should close a connection that produces an error.
    pub fn receive(
        &mut self,
        connection: ConnectionId,
        envelope: &Envelope,
        now: Millis,
    ) -> Result<Vec<Outbound>, EngineError> {
        let session = self
            .connections
            .get(&connection)
            .cloned()
            .ok_or_else(|| unknown_connection(connection))?;
        match self.gateway.handle(&session, envelope, now) {
            Ok(replies) => Ok(replies
                .into_iter()
                .map(|envelope| Outbound::Message {
                    connection,
                    envelope,
                })
                .collect()),
            Err(error) if error.code() == ErrorCode::SessionExpired => {
                let _ = self.connections.remove(&connection);
                Ok(vec![Outbound::Closed {
                    connection,
                    session,
                    reason: CloseReason::SessionExpired,
                }])
            }
            Err(error) => Err(error),
        }
    }

    /// Detach a transport without revoking the session.
    ///
    /// Unknown connections are ignored so repeated close notifications are
    /// safe. The character stays in the zone and the token remains resumable
    /// until the gateway's resume window elapses.
    ///
    /// # Errors
    ///
    /// Returns an error only for a gateway fault. A session that already
    /// expired is reported as [`CloseReason::SessionExpired`].
    pub fn disconnect(
        &mut self,
        connection: ConnectionId,
        last_ack_tick: Tick,
        now: Millis,
    ) -> Result<Vec<Outbound>, EngineError> {
        let Some(session) = self.connections.get(&connection).cloned() else {
            return Ok(Vec::new());
        };
        let reason = match self.gateway.disconnect(&session, last_ack_tick, now) {
            Ok(()) => CloseReason::ClientDisconnected,
            Err(error) if error.code() == ErrorCode::NotFound => CloseReason::SessionExpired,
            Err(error) => return Err(error),
        };
        let _ = self.connections.remove(&connection);
        Ok(vec![Outbound::Closed {
            connection,
            session,
            reason,
        }])
    }

    /// Advance the zone and produce per-connection deliveries.
    ///
    /// Only sessions with a live connection receive messages. A connection
    /// whose session disappeared during the tick (for example because its actor
    /// left the zone) is closed with [`CloseReason::SessionExpired`]. An
    /// unsendable delta detaches only its connection with
    /// [`CloseReason::ClientDisconnected`]; resume sends a fresh snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error for a detach or encoding fault. A gateway tick fault
    /// detaches all connections with [`CloseReason::ClientDisconnected`].
    pub fn tick(&mut self, now: Millis) -> Result<Vec<Outbound>, EngineError> {
        self.tick_with_interval(now, 50)
    }

    /// Process one tick with the server-owned fixed movement interval.
    ///
    /// # Errors
    /// Returns detach or encoding errors. Gateway tick faults detach all
    /// connections so that they can recover from fresh snapshots.
    pub fn tick_with_interval(
        &mut self,
        now: Millis,
        interval_ms: u32,
    ) -> Result<Vec<Outbound>, EngineError> {
        let deliveries = match self.gateway.tick_with_interval(now, interval_ms) {
            Ok(deliveries) => deliveries,
            Err(_) => {
                // A tick may already have consumed movement and advanced interest.
                // Detach every connection so recovery uses a fresh baseline.
                return self.detach_all(now);
            }
        };
        let mut outbound = Vec::with_capacity(deliveries.len());
        for delivery in deliveries {
            let Some(connection) = self.connection_for(&delivery.session) else {
                continue;
            };
            let envelope = match self.delivery_envelope(DELTA_KIND, &delivery) {
                Ok(envelope) => envelope,
                Err(_) => {
                    outbound.retain(|output| matches!(output, Outbound::Closed { .. }));
                    outbound.extend(self.detach_all(now)?);
                    return Ok(outbound);
                }
            };
            if envelope.encode().is_err() {
                // The gateway has advanced this observer's state, so silently
                // dropping the update would leave the client permanently stale.
                let last_ack = self
                    .gateway
                    .session(&delivery.session)
                    .map_or(Tick::ZERO, |session| session.last_ack_tick);
                outbound.extend(self.disconnect(connection, last_ack, now)?);
            } else {
                outbound.push(Outbound::Message {
                    connection,
                    envelope,
                });
            }
        }

        let gateway = &self.gateway;
        let stale: Vec<(ConnectionId, SessionId)> = self
            .connections
            .iter()
            .filter(|(_, session)| gateway.session(session).is_none())
            .map(|(connection, session)| (*connection, session.clone()))
            .collect();
        for (connection, session) in stale {
            let _ = self.connections.remove(&connection);
            outbound.push(Outbound::Closed {
                connection,
                session,
                reason: CloseReason::SessionExpired,
            });
        }
        Ok(outbound)
    }

    fn detach_all(&mut self, now: Millis) -> Result<Vec<Outbound>, EngineError> {
        let connections: Vec<_> = self.connections.keys().copied().collect();
        let mut closed = Vec::with_capacity(connections.len());
        for connection in connections {
            let session = &self.connections[&connection];
            let ack = self
                .gateway
                .session(session)
                .map_or(Tick::ZERO, |s| s.last_ack_tick);
            closed.extend(self.disconnect(connection, ack, now)?);
        }
        Ok(closed)
    }

    fn attach(
        &mut self,
        connection: ConnectionId,
        established: Connection,
    ) -> Result<Opened, EngineError> {
        let session = established.session.clone();
        self.connections.insert(connection, session.clone());
        match self.opened(connection, established) {
            Ok(opened) => Ok(opened),
            Err(error) => {
                let _ = self.connections.remove(&connection);
                let _ = self.gateway.close_session(&session);
                Err(error)
            }
        }
    }

    fn opened(
        &mut self,
        connection: ConnectionId,
        established: Connection,
    ) -> Result<Opened, EngineError> {
        let ready = Ready {
            session: established.session.clone(),
            actor: established.actor,
            resumed: established.resumed,
            negotiated: established.negotiated.clone(),
        };
        let ready = Envelope::new(
            self.next_message_id(),
            message_kind(READY_KIND),
            serde_json::to_value(ready).map_err(|error| internal_error(&error))?,
        );
        let snapshot = self.delivery_envelope(SNAPSHOT_KIND, &established.snapshot)?;
        // Check the complete wire envelope before reporting successful admission.
        // The transport must never receive a ready message without a sendable
        // snapshot, and attach() revokes the session if this check fails.
        snapshot.encode()?;
        Ok(Opened {
            connection,
            session: established.session,
            actor: established.actor,
            negotiated: established.negotiated,
            token: established.token,
            resumed: established.resumed,
            messages: vec![ready, snapshot],
        })
    }

    fn delivery_envelope(
        &mut self,
        kind: &str,
        delivery: &SessionDelivery,
    ) -> Result<Envelope, EngineError> {
        let update = WorldUpdate::from_delivery(delivery);
        let body = serde_json::to_value(update).map_err(|error| internal_error(&error))?;
        Ok(Envelope::new(
            self.next_message_id(),
            message_kind(kind),
            body,
        ))
    }

    fn allocate_connection(&mut self) -> ConnectionId {
        loop {
            let connection = ConnectionId(self.next_connection);
            self.next_connection = self.next_connection.wrapping_add(1);
            if !self.connections.contains_key(&connection) {
                return connection;
            }
        }
    }

    fn next_message_id(&mut self) -> MessageId {
        self.next_message = self.next_message.wrapping_add(1);
        MessageId::parse(&format!("srv-{:016x}", self.next_message))
            .expect("generated server message ids are valid")
    }
}

fn unknown_connection(connection: ConnectionId) -> EngineError {
    EngineError::new(ErrorCode::NotFound, "unknown connection")
        .with_detail("connection", connection.to_string())
}

fn message_kind(kind: &str) -> MessageKind {
    MessageKind::parse(kind).expect("literal message kinds are valid")
}

fn internal_error(error: &serde_json::Error) -> EngineError {
    EngineError::new(ErrorCode::Internal, "failed to encode server message")
        .with_detail("error", error.to_string())
}
