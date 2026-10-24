//! The transport-agnostic authoritative world gateway.
//!
//! [`WorldGateway`] is the only component that turns authenticated client
//! intent into simulation calls and session-specific replication. It owns no
//! socket, runtime, database handle, or clock: callers supply a [`Zone`], the
//! server's handshake, an injected [`ResumeTokenSource`], and an explicit
//! [`Millis`] timestamp.
//!
//! Authority rules enforced here:
//!
//! - The action body cannot name an actor or principal. Those come from the
//!   authenticated session. Unknown body fields are rejected, not ignored.
//! - Request IDs are scoped per session before reaching [`Zone`], so one client
//!   cannot replay another client's cached result.
//! - UI panels are session state. `ui.action` and `ui.close` operate on the
//!   panel a validated action opened; a client cannot choose its target, and
//!   the schema it echoes is only an optimistic concurrency check. Every
//!   action is re-validated by the simulation.
//! - Every snapshot is filtered through [`engine_net::interest`] with the
//!   session's server-chosen view radius, principal, and staff flag.
//! - Whole-zone broadcasts do not exist. A session receives one delivery per
//!   tick containing only its interest delta and audience-filtered updates.

use std::collections::BTreeMap;
use std::fmt;

use engine_core::error::{EngineError, ErrorCode};
use engine_core::id::{DefinitionId, StableId};
use engine_core::time::{DurationMs, Millis, Tick};
use engine_net::envelope::{Envelope, MessageId, MessageKind};
use engine_net::interest::{
    DEFAULT_VIEW_RADIUS_M, InterestConfig, InterestManager, InterestObserver, InterestWorld,
    MAX_VIEW_RADIUS_M, ReplicationAudience, filter_batch,
};
use engine_net::protocol::{ErrorPayload, Handshake, Negotiated, negotiate};
use engine_net::replication::{
    ReplicationBatch, ReplicationCategory, ReplicationUpdate, SessionId,
};
use engine_sim::action::{ActionEffect, ActionRequest, ActionResult, Verb};
use engine_sim::mind::Controller;
use engine_sim::zone::Zone;
use serde::Deserialize;
use serde_json::json;

use crate::replication::{EntitySnapshot, OwnerState, SessionDelivery, interest_entity};
use crate::session::{
    GatewaySession, OpenPanel, ResumeToken, ResumeTokenSource, SessionClaims, SessionRegistry,
};

/// The message kind used for protocol-level failures.
pub const ERROR_KIND: &str = "protocol.error";

/// A client request to close the session's open panel.
pub const UI_CLOSE_KIND: &str = "ui.close";

/// A client request to run one action from the session's open panel.
pub const UI_ACTION_KIND: &str = "ui.action";

/// The authoritative panel state sent to a client.
pub const UI_STATE_KIND: &str = "ui.state";

/// Gateway tuning. All values are chosen by the server, never by a client.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GatewayConfig {
    /// How long a disconnected session may be resumed.
    pub resume_ttl: DurationMs,
    /// View distance for ordinary sessions, in meters.
    pub view_radius_m: f32,
    /// View distance for staff sessions, in meters.
    pub staff_view_radius_m: f32,
    /// Interest tuning, including the exit hysteresis margin.
    pub interest: InterestConfig,
    /// Maximum retained sessions, connected or disconnected.
    pub max_sessions: usize,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            resume_ttl: DurationMs::new(30_000),
            view_radius_m: DEFAULT_VIEW_RADIUS_M,
            staff_view_radius_m: DEFAULT_VIEW_RADIUS_M,
            interest: InterestConfig::default(),
            max_sessions: 10_000,
        }
    }
}

impl GatewayConfig {
    /// Start from the default tuning with a custom resume lifetime.
    #[must_use]
    pub fn new(resume_ttl: DurationMs) -> Self {
        Self {
            resume_ttl,
            ..Self::default()
        }
    }

    /// Override the ordinary view distance.
    #[must_use]
    pub const fn with_view_radius(mut self, radius_m: f32) -> Self {
        self.view_radius_m = radius_m;
        self
    }

    /// Override the staff view distance.
    #[must_use]
    pub const fn with_staff_view_radius(mut self, radius_m: f32) -> Self {
        self.staff_view_radius_m = radius_m;
        self
    }

    /// Override the session limit.
    #[must_use]
    pub const fn with_max_sessions(mut self, max_sessions: usize) -> Self {
        self.max_sessions = max_sessions;
        self
    }

    /// Override interest tuning.
    #[must_use]
    pub const fn with_interest(mut self, interest: InterestConfig) -> Self {
        self.interest = interest;
        self
    }

    /// Validate every tunable.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] for a zero lifetime or session
    /// limit, or an out-of-range view distance.
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.resume_ttl.0 == 0 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "session resume lifetime must be at least one millisecond",
            ));
        }
        if self.max_sessions == 0 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "session limit must be at least one",
            ));
        }
        let _ = InterestConfig::new(self.interest.exit_margin, self.interest.max_entities)?;
        for (name, radius) in [
            ("view_radius_m", self.view_radius_m),
            ("staff_view_radius_m", self.staff_view_radius_m),
        ] {
            if !radius.is_finite() || radius <= 0.0 || radius > MAX_VIEW_RADIUS_M {
                return Err(EngineError::new(
                    ErrorCode::InvalidRequest,
                    "gateway view radius is out of range",
                )
                .with_detail("name", name)
                .with_detail("max_radius_m", MAX_VIEW_RADIUS_M.to_string()));
            }
        }
        Ok(())
    }
}

/// A successful connect or resume.
#[derive(Debug, Clone)]
pub struct Connection {
    /// Gateway-issued session identifier.
    pub session: SessionId,
    /// Entity the session may drive.
    pub actor: StableId,
    /// Resume token for this connection.
    pub token: ResumeToken,
    /// Negotiated protocol, content, locale, and features.
    pub negotiated: Negotiated,
    /// Whether this connection resumed an existing session.
    pub resumed: bool,
    /// Initial owner snapshot plus the session's interest-managed view.
    pub snapshot: SessionDelivery,
}

/// The body of `ui.state`.
///
/// The panel target, schema, and title are always server-resolved. An absent
/// panel means the client should close whatever panel it is showing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UiState {
    /// Authoritative tick that produced this state.
    pub tick: Tick,
    /// Open panel, or `None` when the client should show no panel.
    pub panel: Option<OpenPanel>,
}

#[derive(Debug, Clone)]
struct TrackedEntity {
    snapshot: EntitySnapshot,
    inventory: Option<engine_core::inventory::Inventory>,
    sequence: u64,
}

#[derive(Debug, Clone, Copy)]
enum Change {
    Appeared(StableId),
    Changed(StableId),
    InventoryChanged(StableId),
}

/// The authoritative gateway for one zone.
pub struct WorldGateway {
    zone: Zone,
    server: Handshake,
    config: GatewayConfig,
    sessions: SessionRegistry,
    interest: InterestManager,
    world: InterestWorld,
    tracked: BTreeMap<StableId, TrackedEntity>,
    reply_sequence: u64,
}

impl fmt::Debug for WorldGateway {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorldGateway")
            .field("zone", &self.zone)
            .field("config", &self.config)
            .field("sessions", &self.sessions)
            .field("tracked_entities", &self.tracked.len())
            .finish_non_exhaustive()
    }
}

impl WorldGateway {
    /// Create a gateway for one zone.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] for invalid tuning and
    /// [`EngineError`] from the session registry.
    pub fn new<T: ResumeTokenSource + 'static>(
        zone: Zone,
        server: Handshake,
        config: GatewayConfig,
        tokens: T,
    ) -> Result<Self, EngineError> {
        config.validate()?;
        let sessions = SessionRegistry::new(config.resume_ttl, config.max_sessions, tokens)?;
        let interest = InterestManager::new(config.interest);
        let mut gateway = Self {
            zone,
            server,
            config,
            sessions,
            interest,
            world: InterestWorld::new(),
            tracked: BTreeMap::new(),
            reply_sequence: 0,
        };
        let _ = gateway.sync_world();
        Ok(gateway)
    }

    /// The authoritative zone. Server systems may read it directly.
    #[must_use]
    pub const fn zone(&self) -> &Zone {
        &self.zone
    }

    /// Mutable zone access for authoritative server systems, loaders, and
    /// tests. Client code never receives this handle.
    pub const fn zone_mut(&mut self) -> &mut Zone {
        &mut self.zone
    }

    /// Static gateway tuning.
    #[must_use]
    pub const fn config(&self) -> &GatewayConfig {
        &self.config
    }

    /// The server handshake used for negotiation.
    #[must_use]
    pub const fn server_handshake(&self) -> &Handshake {
        &self.server
    }

    /// A retained session, connected or disconnected.
    #[must_use]
    pub fn session(&self, id: &SessionId) -> Option<GatewaySession> {
        self.sessions.get(id).cloned()
    }

    /// The panel currently open for a session, if any.
    ///
    /// This is server state derived from a validated `UiOpened` effect. It is
    /// exposed for authoritative systems and tests, never accepted from a
    /// client.
    #[must_use]
    pub fn panel(&self, session: &SessionId) -> Option<OpenPanel> {
        self.sessions.panel(session).cloned()
    }

    /// Number of currently connected sessions.
    #[must_use]
    pub fn connected_sessions(&self) -> usize {
        self.sessions.connected_count()
    }

    /// Number of retained sessions, including reconnect windows.
    #[must_use]
    pub fn retained_sessions(&self) -> usize {
        self.sessions.len()
    }

    /// Revoke a session immediately, invalidating its token and dropping its
    /// interest set.
    ///
    /// Use this for administrative kicks, aborted handshakes, and any state
    /// that must not be resumable. An ordinary transport drop should call
    /// [`WorldGateway::disconnect`] instead so the player can reconnect.
    pub fn close_session(&mut self, session: &SessionId) -> Option<GatewaySession> {
        let removed = self.sessions.remove(session);
        let _ = self.interest.remove_session(session);
        removed
    }

    /// Create a session for authenticated claims and bind its character.
    ///
    /// # Errors
    ///
    /// Returns handshake, authority, capacity, and conflict errors. No session
    /// is retained when binding or snapshot construction fails.
    pub fn connect(
        &mut self,
        claims: SessionClaims,
        client: &Handshake,
        now: Millis,
    ) -> Result<Connection, EngineError> {
        claims.validate()?;
        let negotiated = negotiate(client, &self.server)?;
        let actor = self.resolve_actor(claims.principal)?;
        let (session, token) = self
            .sessions
            .connect(claims, actor, negotiated.clone(), now)?;
        self.bind_actor(&session);
        let _ = self.sync_world();
        let snapshot = match self.initial_delivery(&session) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let _ = self.sessions.remove(&session.id);
                return Err(error);
            }
        };
        Ok(Connection {
            session: session.id,
            actor,
            token,
            negotiated,
            resumed: false,
            snapshot,
        })
    }

    /// Detach a transport and start the short resume window.
    ///
    /// The character stays in the zone and keeps its control binding so the
    /// player can continue after reconnecting.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::NotFound`] when the session is unknown.
    pub fn disconnect(
        &mut self,
        session: &SessionId,
        last_ack_tick: Tick,
        now: Millis,
    ) -> Result<(), EngineError> {
        let last_ack = clamp_tick(last_ack_tick, self.zone.tick());
        let _ = self.sessions.disconnect(session, last_ack, now)?;
        let _ = self.interest.remove_session(session);
        Ok(())
    }

    /// Resume a detached session, rotate its token, and rebuild its view.
    ///
    /// The client's last acknowledged tick is clamped to the authoritative
    /// tick. The reply is always a full owner snapshot plus a fresh interest
    /// snapshot; deltas after that are produced by [`WorldGateway::tick`].
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::SessionExpired`] for an unknown or expired token,
    /// [`ErrorCode::Conflict`] when the session is still connected, and
    /// handshake errors for an incompatible client.
    pub fn resume(
        &mut self,
        token: &ResumeToken,
        client: &Handshake,
        last_ack_tick: Tick,
        now: Millis,
    ) -> Result<Connection, EngineError> {
        let negotiated = negotiate(client, &self.server)?;
        let last_ack = clamp_tick(last_ack_tick, self.zone.tick());
        let (session, next_token) = self.sessions.resume(token, last_ack, now)?;
        self.sessions
            .update_negotiated(&session.id, negotiated.clone())?;
        if self.zone.entity(session.actor).is_none() {
            let _ = self.sessions.remove(&session.id);
            return Err(EngineError::new(
                ErrorCode::SessionExpired,
                "the session's character is no longer in the zone",
            ));
        }
        self.bind_actor(&session);
        let _ = self.sync_world();
        let snapshot = match self.initial_delivery(&session) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let _ = self.sessions.remove(&session.id);
                return Err(error);
            }
        };
        Ok(Connection {
            session: session.id,
            actor: session.actor,
            token: next_token,
            negotiated,
            resumed: true,
            snapshot,
        })
    }

    /// Route one validated protocol envelope from an authenticated session.
    ///
    /// Bad action bodies and unknown kinds produce a correlated
    /// [`ERROR_KIND`] reply instead of closing the session. Action outcomes,
    /// including gameplay rejections, arrive as `action.result`.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::SessionExpired`] when the transport references a
    /// session that is unknown or disconnected, and envelope validation errors.
    pub fn handle(
        &mut self,
        session: &SessionId,
        envelope: &Envelope,
        now: Millis,
    ) -> Result<Vec<Envelope>, EngineError> {
        envelope.validate()?;
        let (principal, actor) = {
            let active = self.sessions.active(session)?;
            (active.principal, active.actor)
        };
        match envelope.kind.as_str() {
            "action.request" => self.handle_action(session, principal, actor, envelope, now),
            "session.ack" => self.handle_ack(session, envelope),
            UI_CLOSE_KIND => self.handle_ui_close(session, envelope),
            UI_ACTION_KIND => self.handle_ui_action(session, principal, actor, envelope, now),
            _ => {
                let error = EngineError::new(ErrorCode::Unsupported, "unsupported message kind")
                    .with_detail("kind", envelope.kind.as_str());
                Ok(vec![self.error_reply(envelope, &error)])
            }
        }
    }

    /// Advance the zone one tick and produce one delivery per session.
    ///
    /// Dirty entities are found by comparing authoritative records with the
    /// previous tick. Each session gets only its own interest delta and the
    /// updates its principal and staff flag admit. Empty deliveries are
    /// omitted.
    ///
    /// # Errors
    ///
    /// Returns an error only when the configured interest or batch limits are
    /// violated; those indicate a server configuration fault.
    pub fn tick(&mut self, now: Millis) -> Result<Vec<SessionDelivery>, EngineError> {
        for id in self.sessions.sweep(now) {
            let _ = self.interest.remove_session(&id);
        }

        self.zone.advance_tick();
        let changes = self.sync_world();

        let sessions: Vec<GatewaySession> = self.sessions.connected().cloned().collect();
        let mut refreshed = Vec::with_capacity(sessions.len());
        for session in sessions {
            let Some(observer) = self.observer_for(&session) else {
                let _ = self.sessions.remove(&session.id);
                let _ = self.interest.remove_session(&session.id);
                continue;
            };
            let update = self.interest.refresh(&observer, &self.world)?;
            refreshed.push((session, update));
        }

        let tick = self.zone.tick();
        let mut base = ReplicationBatch::new(tick);
        for change in changes {
            match change {
                Change::Appeared(entity) => {
                    self.push_snapshot(&mut base, entity, ReplicationCategory::InterestManaged)?;
                }
                Change::Changed(entity) => {
                    self.push_snapshot(&mut base, entity, ReplicationCategory::StateSnapshot)?;
                }
                Change::InventoryChanged(entity) => {
                    self.push_owner_state(&mut base, entity)?;
                }
            }
        }

        let mut deliveries = Vec::with_capacity(refreshed.len());
        for (session, update) in refreshed {
            let mut batch = base.clone();
            for entity in &update.added {
                if !has_state_update(&batch, *entity) {
                    self.push_snapshot(&mut batch, *entity, ReplicationCategory::InterestManaged)?;
                }
            }
            let filtered = {
                let audience = ReplicationAudience::new(&session.id, &update.set, &self.world)
                    .with_principal(session.principal)
                    .with_staff(session.staff);
                filter_batch(&batch, &audience)
            };
            if filtered.is_empty() && update.is_empty() {
                continue;
            }
            deliveries.push(SessionDelivery {
                session: session.id,
                tick,
                interest: update,
                batch: filtered,
            });
        }
        Ok(deliveries)
    }

    fn handle_action(
        &mut self,
        session: &SessionId,
        principal: StableId,
        actor: StableId,
        envelope: &Envelope,
        now: Millis,
    ) -> Result<Vec<Envelope>, EngineError> {
        let intent = match serde_json::from_value::<ActionIntent>(envelope.body.clone()) {
            Ok(intent) => intent,
            Err(error) => {
                let error =
                    EngineError::new(ErrorCode::InvalidRequest, "invalid action request body")
                        .with_detail("error", error.to_string());
                return Ok(vec![self.error_reply(envelope, &error)]);
            }
        };

        let target = intent.target;
        if intent.verb == Verb::Play {
            let error = EngineError::new(
                ErrorCode::Unsupported,
                "minigame actions must use the ui.action channel",
            );
            return Ok(vec![self.error_reply(envelope, &error)]);
        }
        let mut request =
            ActionRequest::new(scoped_request_id(session, &envelope.id), actor, intent.verb);
        request.target = target;
        request.params = intent.params;
        request.client_tick = intent.client_tick;
        request.principal = Some(principal);
        let mut result = self.zone.apply_action(&request, now);
        result.request_id = envelope.id.as_str().to_string();

        let panel_changed = self.sync_panel(session, &result, target)?;

        let body = serde_json::to_value(&result).map_err(|error| internal_error(&error))?;
        let mut replies =
            vec![envelope.reply(self.reply_id(), message_kind("action.result"), body)];
        if panel_changed {
            replies.push(self.ui_state_reply(session, envelope)?);
        }
        Ok(replies)
    }

    fn handle_ui_close(
        &mut self,
        session: &SessionId,
        envelope: &Envelope,
    ) -> Result<Vec<Envelope>, EngineError> {
        if let Err(error) = serde_json::from_value::<EmptyBody>(envelope.body.clone()) {
            let error = EngineError::new(ErrorCode::InvalidRequest, "invalid UI close body")
                .with_detail("error", error.to_string());
            return Ok(vec![self.error_reply(envelope, &error)]);
        }
        let _ = self.sessions.clear_panel(session)?;
        Ok(vec![self.ui_state_reply(session, envelope)?])
    }

    fn handle_ui_action(
        &mut self,
        session: &SessionId,
        principal: StableId,
        actor: StableId,
        envelope: &Envelope,
        now: Millis,
    ) -> Result<Vec<Envelope>, EngineError> {
        let intent = match serde_json::from_value::<UiActionIntent>(envelope.body.clone()) {
            Ok(intent) => intent,
            Err(error) => {
                let error = EngineError::new(ErrorCode::InvalidRequest, "invalid UI action body")
                    .with_detail("error", error.to_string());
                return Ok(vec![self.error_reply(envelope, &error)]);
            }
        };

        let Some(panel) = self.sessions.panel(session).cloned() else {
            let error = EngineError::new(
                ErrorCode::InvalidState,
                "no UI panel is open for this session",
            );
            return Ok(vec![self.error_reply(envelope, &error)]);
        };
        if panel.schema != intent.schema {
            let error = EngineError::new(
                ErrorCode::Conflict,
                "the UI panel schema does not match the open panel",
            )
            .with_detail("expected", panel.schema.to_string())
            .with_detail("received", intent.schema.to_string());
            return Ok(vec![self.error_reply(envelope, &error)]);
        }
        let Some(verb) = Verb::parse(&intent.action) else {
            let error = EngineError::new(ErrorCode::InvalidRequest, "invalid panel action name")
                .with_detail("action", intent.action);
            return Ok(vec![self.error_reply(envelope, &error)]);
        };

        let mut request = ActionRequest::new(scoped_request_id(session, &envelope.id), actor, verb);
        request.target = Some(panel.target);
        request.params = intent.params;
        request.client_tick = intent.client_tick;
        request.principal = Some(principal);
        let mut result = self.zone.apply_action(&request, now);
        result.request_id = envelope.id.as_str().to_string();

        self.sync_panel(session, &result, Some(panel.target))?;

        let body = serde_json::to_value(&result).map_err(|error| internal_error(&error))?;
        let result_reply = envelope.reply(self.reply_id(), message_kind("action.result"), body);
        let state_reply = self.ui_state_reply(session, envelope)?;
        Ok(vec![result_reply, state_reply])
    }

    /// Reconcile the session's open panel with a validated action result.
    ///
    /// Returns whether the panel changed, so the caller can send `ui.state`.
    fn sync_panel(
        &mut self,
        session: &SessionId,
        result: &ActionResult,
        request_target: Option<StableId>,
    ) -> Result<bool, EngineError> {
        if let Some(panel) = opened_panel(result.effects()) {
            self.sessions.set_panel(session, panel)?;
            return Ok(true);
        }

        let current = self.sessions.panel(session).cloned();
        let closed = result.effects().iter().any(|effect| match effect {
            ActionEffect::UiClosed { target } => current
                .as_ref()
                .is_some_and(|panel| panel.target == *target),
            _ => false,
        });
        let provider_missing = result
            .error()
            .is_some_and(|error| error.code() == ErrorCode::NotFound)
            && current
                .as_ref()
                .is_some_and(|panel| Some(panel.target) == request_target);
        if closed || provider_missing {
            let _ = self.sessions.clear_panel(session);
            return Ok(true);
        }
        Ok(false)
    }

    fn ui_state_reply(
        &mut self,
        session: &SessionId,
        request: &Envelope,
    ) -> Result<Envelope, EngineError> {
        let state = UiState {
            tick: self.zone.tick(),
            panel: self.sessions.panel(session).cloned(),
        };
        let body = serde_json::to_value(state).map_err(|error| internal_error(&error))?;
        Ok(request.reply(self.reply_id(), message_kind(UI_STATE_KIND), body))
    }

    fn handle_ack(
        &mut self,
        session: &SessionId,
        envelope: &Envelope,
    ) -> Result<Vec<Envelope>, EngineError> {
        let ack = match serde_json::from_value::<AckBody>(envelope.body.clone()) {
            Ok(ack) => ack,
            Err(error) => {
                let error =
                    EngineError::new(ErrorCode::InvalidRequest, "invalid acknowledgement body")
                        .with_detail("error", error.to_string());
                return Ok(vec![self.error_reply(envelope, &error)]);
            }
        };
        self.sessions
            .acknowledge(session, clamp_tick(ack.tick, self.zone.tick()))?;
        Ok(Vec::new())
    }

    fn error_reply(&mut self, request: &Envelope, error: &EngineError) -> Envelope {
        let body = serde_json::to_value(ErrorPayload::from(error))
            .unwrap_or_else(|_| json!({ "code": "internal" }));
        request.reply(self.reply_id(), message_kind(ERROR_KIND), body)
    }

    fn reply_id(&mut self) -> MessageId {
        self.reply_sequence = self.reply_sequence.wrapping_add(1);
        MessageId::parse(&format!("res-{:016x}", self.reply_sequence))
            .expect("generated reply ids are valid")
    }

    fn resolve_actor(&self, principal: StableId) -> Result<StableId, EngineError> {
        let candidates: Vec<StableId> = self
            .zone
            .entities()
            .filter(|entity| {
                entity.mind.as_ref().is_some_and(|mind| {
                    mind.controller.player_character() == Some(principal)
                        || (mind.controller == Controller::None && entity.owner == Some(principal))
                })
            })
            .map(|entity| entity.id)
            .collect();
        match candidates.as_slice() {
            [] => Err(EngineError::new(
                ErrorCode::NotFound,
                "the character is not present in the zone",
            )
            .with_detail("principal", principal.to_string())),
            [actor] => Ok(*actor),
            _ => Err(EngineError::new(
                ErrorCode::Conflict,
                "the character is bound to more than one entity",
            )
            .with_detail("principal", principal.to_string())
            .with_detail("matches", candidates.len().to_string())),
        }
    }

    fn bind_actor(&mut self, session: &GatewaySession) {
        if let Some(entity) = self.zone.entity_mut(session.actor)
            && let Some(mind) = entity.mind.as_mut()
        {
            mind.controller = Controller::Player {
                character: session.principal,
                session: session.id.to_string(),
            };
        }
    }

    fn observer_for(&self, session: &GatewaySession) -> Option<InterestObserver> {
        let actor = self.zone.entity(session.actor)?;
        let radius = if session.staff {
            self.config.staff_view_radius_m
        } else {
            self.config.view_radius_m
        };
        Some(
            InterestObserver::new(session.id.clone(), actor.position())
                .with_principal(session.principal)
                .with_view_radius(radius)
                .with_staff(session.staff),
        )
    }

    fn initial_delivery(
        &mut self,
        session: &GatewaySession,
    ) -> Result<SessionDelivery, EngineError> {
        let Some(observer) = self.observer_for(session) else {
            return Err(EngineError::new(
                ErrorCode::SessionExpired,
                "the session's character is no longer in the zone",
            ));
        };
        let update = self.interest.refresh(&observer, &self.world)?;
        let tick = self.zone.tick();
        let mut batch = ReplicationBatch::new(tick);
        for entity in &update.added {
            self.push_snapshot(&mut batch, *entity, ReplicationCategory::InterestManaged)?;
        }
        self.push_owner_snapshot(&mut batch, session.principal)?;
        let filtered = {
            let audience = ReplicationAudience::new(&session.id, &update.set, &self.world)
                .with_principal(session.principal)
                .with_staff(session.staff);
            filter_batch(&batch, &audience)
        };
        Ok(SessionDelivery {
            session: session.id.clone(),
            tick,
            interest: update,
            batch: filtered,
        })
    }

    fn sync_world(&mut self) -> Vec<Change> {
        let mut changes = Vec::new();
        let mut world = InterestWorld::new();
        let mut tracked = BTreeMap::new();
        for entity in self.zone.entities() {
            let snapshot = EntitySnapshot::capture(entity);
            let inventory = entity.inventory.clone();
            world.upsert(interest_entity(entity));
            let sequence = if let Some(previous) = self.tracked.get(&entity.id) {
                let snapshot_changed = previous.snapshot != snapshot;
                let inventory_changed = previous.inventory != inventory;
                if snapshot_changed {
                    changes.push(Change::Changed(entity.id));
                }
                if inventory_changed {
                    changes.push(Change::InventoryChanged(entity.id));
                }
                if snapshot_changed || inventory_changed {
                    previous.sequence.saturating_add(1)
                } else {
                    previous.sequence
                }
            } else {
                changes.push(Change::Appeared(entity.id));
                if inventory.is_some() {
                    changes.push(Change::InventoryChanged(entity.id));
                }
                1
            };
            tracked.insert(
                entity.id,
                TrackedEntity {
                    snapshot,
                    inventory,
                    sequence,
                },
            );
        }
        self.world = world;
        self.tracked = tracked;
        changes
    }

    fn push_snapshot(
        &self,
        batch: &mut ReplicationBatch,
        entity: StableId,
        category: ReplicationCategory,
    ) -> Result<(), EngineError> {
        let Some(tracked) = self.tracked.get(&entity) else {
            return Ok(());
        };
        let payload =
            serde_json::to_value(&tracked.snapshot).map_err(|error| internal_error(&error))?;
        let tick = batch.tick;
        batch.push(
            ReplicationUpdate::new(category, tick)
                .with_entity(entity)
                .with_sequence(tracked.sequence)
                .with_payload(payload),
        )
    }

    fn push_owner_state(
        &self,
        batch: &mut ReplicationBatch,
        entity: StableId,
    ) -> Result<(), EngineError> {
        let Some(tracked) = self.tracked.get(&entity) else {
            return Ok(());
        };
        let Some(inventory) = tracked.inventory.clone() else {
            return Ok(());
        };
        if self
            .world
            .get(entity)
            .and_then(|entry| entry.owner)
            .is_none()
        {
            return Ok(());
        }
        let payload = serde_json::to_value(OwnerState { inventory })
            .map_err(|error| internal_error(&error))?;
        let tick = batch.tick;
        batch.push(
            ReplicationUpdate::new(ReplicationCategory::OwnerOnly, tick)
                .with_entity(entity)
                .with_sequence(tracked.sequence)
                .with_payload(payload),
        )
    }

    fn push_owner_snapshot(
        &self,
        batch: &mut ReplicationBatch,
        principal: StableId,
    ) -> Result<(), EngineError> {
        let owned: Vec<StableId> = self
            .world
            .iter()
            .filter(|entry| entry.owner == Some(principal))
            .map(|entry| entry.entity)
            .collect();
        for entity in owned {
            self.push_owner_state(batch, entity)?;
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionIntent {
    #[serde(default)]
    target: Option<StableId>,
    verb: Verb,
    #[serde(default)]
    params: BTreeMap<String, String>,
    #[serde(default)]
    client_tick: Tick,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AckBody {
    tick: Tick,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyBody {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UiActionIntent {
    schema: DefinitionId,
    action: String,
    #[serde(default)]
    params: BTreeMap<String, String>,
    #[serde(default)]
    client_tick: Tick,
}

fn opened_panel(effects: &[ActionEffect]) -> Option<OpenPanel> {
    effects.iter().find_map(|effect| match effect {
        ActionEffect::UiOpened {
            target,
            schema,
            title,
        } => Some(OpenPanel {
            target: *target,
            schema: schema.clone(),
            title: title.clone(),
        }),
        _ => None,
    })
}

fn scoped_request_id(session: &SessionId, id: &MessageId) -> String {
    format!(
        "{}:{}:{}",
        session.as_str().len(),
        session.as_str(),
        id.as_str()
    )
}

fn clamp_tick(tick: Tick, maximum: Tick) -> Tick {
    Tick(tick.0.min(maximum.0))
}

fn has_state_update(batch: &ReplicationBatch, entity: StableId) -> bool {
    batch.updates.iter().any(|update| {
        update.entity == Some(entity)
            && matches!(
                update.category,
                ReplicationCategory::InterestManaged | ReplicationCategory::StateSnapshot
            )
    })
}

fn message_kind(kind: &str) -> MessageKind {
    MessageKind::parse(kind).expect("literal message kinds are valid")
}

fn internal_error(error: &serde_json::Error) -> EngineError {
    EngineError::new(ErrorCode::Internal, "failed to encode gateway message")
        .with_detail("error", error.to_string())
}
