//! Authenticated world sessions, resumable tokens, and reconnect state.
//!
//! A session is created only after trusted code has authenticated the player.
//! The client never chooses its session identifier, its principal, its staff
//! flag, or its resumable token. A disconnect keeps the character in the zone
//! and retains the session for a short, server-configured window; a successful
//! resume rotates the token so a captured token cannot be replayed.

use std::collections::BTreeMap;
use std::fmt;

use engine_core::error::{EngineError, ErrorCode};
use engine_core::id::{DefinitionId, IdKind, StableId};
use engine_core::time::{DurationMs, Millis, Tick};
use engine_net::protocol::Negotiated;
use engine_net::replication::SessionId;
use game_api::AccountId;
use serde::{Deserialize, Serialize};

/// Shortest accepted resume token, in bytes.
pub const MIN_RESUME_TOKEN_LEN: usize = 16;

/// Longest accepted resume token, in bytes.
///
/// Tokens are opaque bearer credentials. Session identifiers are independent
/// routing labels, so token length is not tied to
/// [`engine_net::MAX_SESSION_ID_LEN`].
pub const MAX_RESUME_TOKEN_LEN: usize = 96;

/// An opaque bearer token that lets a disconnected session resume.
///
/// The secret is redacted from debug output and is serialized only when the
/// server hands it to its own transport. Never log or include it in errors.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ResumeToken(String);

impl ResumeToken {
    /// Validate and wrap a server-issued token.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] when the token is too short, too
    /// long, or contains whitespace or control characters.
    pub fn new(value: impl Into<String>) -> Result<Self, EngineError> {
        let value = value.into();
        if value.len() < MIN_RESUME_TOKEN_LEN
            || value.len() > MAX_RESUME_TOKEN_LEN
            || value
                .chars()
                .any(|ch| ch.is_control() || ch.is_whitespace())
        {
            return Err(
                EngineError::new(ErrorCode::InvalidRequest, "invalid resume token")
                    .with_detail("min_bytes", MIN_RESUME_TOKEN_LEN.to_string())
                    .with_detail("max_bytes", MAX_RESUME_TOKEN_LEN.to_string()),
            );
        }
        Ok(Self(value))
    }

    /// Borrow the token for the authenticated transport handshake.
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ResumeToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ResumeToken([REDACTED])")
    }
}

impl TryFrom<String> for ResumeToken {
    type Error = EngineError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ResumeToken> for String {
    fn from(value: ResumeToken) -> Self {
        value.0
    }
}

/// Trusted identity established before a world session is created.
///
/// Only server code that has already authenticated the player may build these
/// claims. A client cannot choose its account, principal, or staff role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionClaims {
    /// Backend account that owns the character.
    pub account: AccountId,
    /// Character principal the session may drive.
    pub principal: StableId,
    /// Whether the account carries a staff role.
    pub staff: bool,
}

impl SessionClaims {
    /// Create claims for an authenticated character.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] when the principal is not a
    /// character ID.
    pub fn new(account: AccountId, principal: StableId) -> Result<Self, EngineError> {
        principal.expect_kind(IdKind::Character)?;
        Ok(Self {
            account,
            principal,
            staff: false,
        })
    }

    /// Mark the claims as staff-authenticated.
    #[must_use]
    pub const fn with_staff(mut self, staff: bool) -> Self {
        self.staff = staff;
        self
    }

    /// Re-check claims that may have been assembled through public fields.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] when the principal is not a
    /// character ID.
    pub fn validate(&self) -> Result<(), EngineError> {
        self.principal.expect_kind(IdKind::Character)?;
        Ok(())
    }
}

/// A UI panel currently open for one session.
///
/// The target, schema, and title are resolved by the server from the entity
/// that produced the panel. A client never chooses the target. It may echo the
/// schema on an action as an optimistic concurrency check, but the stored value
/// is authoritative.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenPanel {
    /// Entity that provides the interface.
    pub target: StableId,
    /// Declarative panel schema the client should render.
    pub schema: DefinitionId,
    /// Server-provided panel title.
    pub title: String,
}

/// A read-only view of one world session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewaySession {
    /// Gateway-issued session identifier.
    pub id: SessionId,
    /// Authenticated account.
    pub account: AccountId,
    /// Character principal the session may drive.
    pub principal: StableId,
    /// Entity instance controlled by the principal.
    pub actor: StableId,
    /// Whether the session carries a staff role.
    pub staff: bool,
    /// Whether the transport is currently attached.
    pub connected: bool,
    /// Highest server tick the client has confirmed.
    pub last_ack_tick: Tick,
}

/// Issues unguessable resume tokens.
///
/// Implementations are injected so tests, replays, and tools can be
/// deterministic. Production uses [`SystemResumeTokens`].
pub trait ResumeTokenSource: fmt::Debug + Send {
    /// Issue one fresh token.
    ///
    /// # Errors
    ///
    /// Returns an error when the source cannot produce a token.
    fn issue(&mut self) -> Result<ResumeToken, EngineError>;
}

/// Cryptographically random resume tokens backed by the operating system.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemResumeTokens;

impl SystemResumeTokens {
    /// Create a token source.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl ResumeTokenSource for SystemResumeTokens {
    fn issue(&mut self) -> Result<ResumeToken, EngineError> {
        let mut bytes = [0_u8; 16];
        getrandom::fill(&mut bytes).map_err(|error| {
            EngineError::new(ErrorCode::Internal, "failed to generate a resume token")
                .with_detail("error", error.to_string())
        })?;
        ResumeToken::new(hex(&bytes))
    }
}

/// A deterministic token source for tests, fixtures, and replays.
#[derive(Debug, Clone)]
pub struct SequentialResumeTokens {
    seed: u64,
    next: u64,
}

impl SequentialResumeTokens {
    /// Start a sequence at `seed`.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { seed, next: 0 }
    }
}

impl ResumeTokenSource for SequentialResumeTokens {
    fn issue(&mut self) -> Result<ResumeToken, EngineError> {
        let value = format!("{:016x}{:016x}", self.seed, self.next);
        self.next = self.next.wrapping_add(1);
        ResumeToken::new(value)
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(out, "{byte:02x}").expect("writing to a string cannot fail");
    }
    out
}

#[derive(Clone)]
struct ResumeState {
    expires_at: Millis,
}

#[derive(Clone)]
struct SessionRecord {
    view: GatewaySession,
    negotiated: Negotiated,
    token: ResumeToken,
    resume: Option<ResumeState>,
    panel: Option<OpenPanel>,
}

/// Server-side session table.
///
/// Disconnected sessions survive for the configured resume lifetime. Connected
/// and retained sessions both count against the session cap so a reconnect
/// cannot bypass admission limits.
pub struct SessionRegistry {
    ttl: DurationMs,
    max_sessions: usize,
    sessions: BTreeMap<SessionId, SessionRecord>,
    tokens: BTreeMap<ResumeToken, SessionId>,
    source: Box<dyn ResumeTokenSource>,
    next_session: u64,
}

impl fmt::Debug for SessionRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionRegistry")
            .field("ttl", &self.ttl)
            .field("max_sessions", &self.max_sessions)
            .field("sessions", &self.sessions.len())
            .field("retained_tokens", &self.tokens.len())
            .finish_non_exhaustive()
    }
}

impl SessionRegistry {
    /// Create an empty registry.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] when the resume lifetime or
    /// session limit is zero.
    pub fn new<T: ResumeTokenSource + 'static>(
        ttl: DurationMs,
        max_sessions: usize,
        source: T,
    ) -> Result<Self, EngineError> {
        if ttl.0 == 0 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "session resume lifetime must be at least one millisecond",
            ));
        }
        if max_sessions == 0 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "session limit must be at least one",
            ));
        }
        Ok(Self {
            ttl,
            max_sessions,
            sessions: BTreeMap::new(),
            tokens: BTreeMap::new(),
            source: Box::new(source),
            next_session: 0,
        })
    }

    /// Number of retained sessions, connected or disconnected.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Whether no session is retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Number of currently connected sessions.
    #[must_use]
    pub fn connected_count(&self) -> usize {
        self.sessions
            .values()
            .filter(|record| record.view.connected)
            .count()
    }

    /// Look up one session, connected or retained.
    #[must_use]
    pub fn get(&self, id: &SessionId) -> Option<&GatewaySession> {
        self.sessions.get(id).map(|record| &record.view)
    }

    /// Iterate over connected sessions in stable ID order.
    pub fn connected(&self) -> impl Iterator<Item = &GatewaySession> {
        self.sessions
            .values()
            .filter(|record| record.view.connected)
            .map(|record| &record.view)
    }

    /// The negotiated handshake retained for a session's locale and features.
    #[must_use]
    pub fn negotiated(&self, id: &SessionId) -> Option<&Negotiated> {
        self.sessions.get(id).map(|record| &record.negotiated)
    }

    /// The panel currently open for a session, if any.
    #[must_use]
    pub fn panel(&self, id: &SessionId) -> Option<&OpenPanel> {
        self.sessions
            .get(id)
            .and_then(|record| record.panel.as_ref())
    }

    /// Replace the session's open panel.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::SessionExpired`] when the session is unknown or
    /// disconnected.
    pub fn set_panel(&mut self, id: &SessionId, panel: OpenPanel) -> Result<(), EngineError> {
        let record = self.sessions.get_mut(id).ok_or_else(session_expired)?;
        if !record.view.connected {
            return Err(session_expired());
        }
        record.panel = Some(panel);
        Ok(())
    }

    /// Clear the session's open panel and return the previous value.
    ///
    /// Clearing an already closed panel is not an error.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::SessionExpired`] when the session is unknown or
    /// disconnected.
    pub fn clear_panel(&mut self, id: &SessionId) -> Result<Option<OpenPanel>, EngineError> {
        let record = self.sessions.get_mut(id).ok_or_else(session_expired)?;
        if !record.view.connected {
            return Err(session_expired());
        }
        Ok(record.panel.take())
    }

    /// Resolve a connected session, rejecting retained but detached sessions.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::SessionExpired`] when the session is unknown or
    /// disconnected.
    pub fn active(&self, id: &SessionId) -> Result<&GatewaySession, EngineError> {
        let record = self.sessions.get(id).ok_or_else(session_expired)?;
        if !record.view.connected {
            return Err(session_expired());
        }
        Ok(&record.view)
    }

    /// Create a connected session for authenticated claims.
    ///
    /// A principal may have at most one connected session. Retained but
    /// disconnected sessions for the same principal are discarded, so a fresh
    /// login replaces an abandoned resume window.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::Conflict`] when the principal is already connected,
    /// [`ErrorCode::CapacityExceeded`] when the table is full, and
    /// [`ErrorCode::Internal`] when the token source repeats a token.
    pub fn connect(
        &mut self,
        claims: SessionClaims,
        actor: StableId,
        negotiated: Negotiated,
        now: Millis,
    ) -> Result<(GatewaySession, ResumeToken), EngineError> {
        claims.validate()?;
        let _ = self.sweep(now);

        if self
            .sessions
            .values()
            .any(|record| record.view.connected && record.view.principal == claims.principal)
        {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "the character already has an active session",
            )
            .with_detail("principal", claims.principal.to_string()));
        }

        let stale: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, record)| {
                !record.view.connected && record.view.principal == claims.principal
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            let _ = self.remove(&id);
        }

        if self.sessions.len() >= self.max_sessions {
            return Err(EngineError::new(
                ErrorCode::CapacityExceeded,
                "world session limit reached",
            )
            .with_detail("max_sessions", self.max_sessions.to_string()));
        }

        let token = self.source.issue()?;
        if self.tokens.contains_key(&token) {
            return Err(EngineError::new(
                ErrorCode::Internal,
                "resume token source produced a duplicate token",
            ));
        }
        let id = self.next_session_id()?;

        let view = GatewaySession {
            id: id.clone(),
            account: claims.account,
            principal: claims.principal,
            actor,
            staff: claims.staff,
            connected: true,
            last_ack_tick: Tick::ZERO,
        };
        self.sessions.insert(
            id.clone(),
            SessionRecord {
                view: view.clone(),
                negotiated,
                token: token.clone(),
                resume: None,
                panel: None,
            },
        );
        self.tokens.insert(token.clone(), id);
        Ok((view, token))
    }

    /// Allocate an independent, process-local session routing label.
    ///
    /// Session IDs are not bearer credentials; the resume token is. Keeping
    /// them independent means exposing a session ID never exposes a token.
    fn next_session_id(&mut self) -> Result<SessionId, EngineError> {
        loop {
            let candidate = format!("sess-{:016x}", self.next_session);
            self.next_session = self.next_session.wrapping_add(1);
            let id = SessionId::new(candidate)?;
            if !self.sessions.contains_key(&id) {
                return Ok(id);
            }
        }
    }

    /// Detach a transport and start the resume window.
    ///
    /// Disconnecting an already disconnected session only updates the
    /// acknowledgement cursor; it never extends the resume deadline.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::NotFound`] when the session is unknown.
    pub fn disconnect(
        &mut self,
        id: &SessionId,
        last_ack_tick: Tick,
        now: Millis,
    ) -> Result<GatewaySession, EngineError> {
        let record = self
            .sessions
            .get_mut(id)
            .ok_or_else(|| EngineError::not_found("session", id))?;
        record.view.last_ack_tick = last_ack_tick;
        if record.view.connected {
            record.view.connected = false;
            record.panel = None;
            record.resume = Some(ResumeState {
                expires_at: Millis::new(now.value().saturating_add(self.ttl.as_i64())),
            });
        }
        Ok(record.view.clone())
    }

    /// Resume a detached session and rotate its token.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::SessionExpired`] when the token is unknown or
    /// expired, and [`ErrorCode::Conflict`] when the session is still connected.
    pub fn resume(
        &mut self,
        token: &ResumeToken,
        last_ack_tick: Tick,
        now: Millis,
    ) -> Result<(GatewaySession, ResumeToken), EngineError> {
        let _ = self.sweep(now);
        let id = self
            .tokens
            .get(token)
            .cloned()
            .ok_or_else(session_expired)?;
        let record = self.sessions.get(&id).ok_or_else(session_expired)?;
        if record.view.connected {
            return Err(
                EngineError::new(ErrorCode::Conflict, "the session is still connected")
                    .with_detail("session", id.to_string()),
            );
        }
        let expires_at = record
            .resume
            .as_ref()
            .ok_or_else(session_expired)?
            .expires_at;
        if now.is_at_or_after(expires_at) {
            let _ = self.remove(&id);
            return Err(session_expired());
        }

        let next = self.source.issue()?;
        if self.tokens.contains_key(&next) {
            return Err(EngineError::new(
                ErrorCode::Internal,
                "resume token source produced a duplicate token",
            ));
        }
        let record = self.sessions.get_mut(&id).ok_or_else(session_expired)?;
        let _ = self.tokens.remove(token);
        self.tokens.insert(next.clone(), id.clone());
        record.token = next.clone();
        record.resume = None;
        record.panel = None;
        record.view.connected = true;
        record.view.last_ack_tick = last_ack_tick;
        Ok((record.view.clone(), next))
    }

    /// Replace the negotiated handshake after a reconnect.
    pub(crate) fn update_negotiated(
        &mut self,
        id: &SessionId,
        negotiated: Negotiated,
    ) -> Result<(), EngineError> {
        let record = self.sessions.get_mut(id).ok_or_else(session_expired)?;
        record.negotiated = negotiated;
        Ok(())
    }

    /// Record the highest server tick the client has acknowledged.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::SessionExpired`] when the session is not connected.
    pub fn acknowledge(&mut self, id: &SessionId, tick: Tick) -> Result<(), EngineError> {
        let record = self.sessions.get_mut(id).ok_or_else(session_expired)?;
        if !record.view.connected {
            return Err(session_expired());
        }
        record.view.last_ack_tick = tick;
        Ok(())
    }

    /// Drop every retained session whose resume window has elapsed.
    pub fn sweep(&mut self, now: Millis) -> Vec<SessionId> {
        let expired: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, record)| {
                !record.view.connected
                    && record
                        .resume
                        .as_ref()
                        .is_some_and(|state| now.is_at_or_after(state.expires_at))
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            let _ = self.remove(id);
        }
        expired
    }

    /// Remove a session and invalidate its token.
    pub fn remove(&mut self, id: &SessionId) -> Option<GatewaySession> {
        let record = self.sessions.remove(id)?;
        let _ = self.tokens.remove(&record.token);
        Some(record.view)
    }
}

fn session_expired() -> EngineError {
    EngineError::new(
        ErrorCode::SessionExpired,
        "the world session is unknown or has expired",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_net::protocol::{Handshake, negotiate};

    fn version() -> semver::Version {
        semver::Version::new(0, 1, 0)
    }

    fn negotiated() -> Negotiated {
        let handshake = Handshake::new(version(), version());
        negotiate(&handshake, &handshake).unwrap()
    }

    fn character(seed: u128) -> StableId {
        StableId::from_parts(IdKind::Character, 1, seed)
    }

    fn entity(seed: u128) -> StableId {
        StableId::from_parts(IdKind::Entity, 1, seed)
    }

    fn claims(seed: u128) -> SessionClaims {
        SessionClaims::new(
            AccountId::new(format!("account-{seed}")).unwrap(),
            character(seed),
        )
        .unwrap()
    }

    fn registry() -> SessionRegistry {
        SessionRegistry::new(DurationMs::new(1_000), 16, SequentialResumeTokens::new(7)).unwrap()
    }

    #[test]
    fn tokens_are_redacted_and_validated() {
        let token = ResumeToken::new("0123456789abcdef").unwrap();
        assert_eq!(format!("{token:?}"), "ResumeToken([REDACTED])");
        assert_eq!(token.expose_secret(), "0123456789abcdef");
        for invalid in [
            "",
            "short",
            "has space 0123456789",
            "line\n0123456789abcdef",
        ] {
            assert!(ResumeToken::new(invalid).is_err());
        }
        assert!(ResumeToken::new("x".repeat(MAX_RESUME_TOKEN_LEN + 1)).is_err());
    }

    #[test]
    fn one_connected_session_per_principal() {
        let mut registry = registry();
        let (first, _) = registry
            .connect(claims(1), entity(10), negotiated(), Millis::new(1))
            .unwrap();
        let error = registry
            .connect(claims(1), entity(11), negotiated(), Millis::new(2))
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::Conflict);

        let _ = registry
            .disconnect(&first.id, Tick(0), Millis::new(3))
            .unwrap();
        // A fresh login replaces the retained window.
        let (second, _) = registry
            .connect(claims(1), entity(10), negotiated(), Millis::new(4))
            .unwrap();
        assert_ne!(first.id, second.id);
    }

    #[test]
    fn resume_rotates_the_token_and_rejects_the_old_one() {
        let mut registry = registry();
        let (session, token) = registry
            .connect(claims(1), entity(10), negotiated(), Millis::new(1_000))
            .unwrap();
        let _ = registry
            .disconnect(&session.id, Tick(4), Millis::new(1_500))
            .unwrap();

        let (resumed, next) = registry
            .resume(&token, Tick(9), Millis::new(2_000))
            .unwrap();
        assert_eq!(resumed.id, session.id);
        assert!(resumed.connected);
        assert_eq!(resumed.last_ack_tick, Tick(9));
        assert_ne!(next, token);

        let error = registry
            .resume(&token, Tick(9), Millis::new(2_100))
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::SessionExpired);
        assert!(
            registry
                .resume(&next, Tick(9), Millis::new(2_100))
                .unwrap_err()
                .code()
                == ErrorCode::Conflict,
            "a connected session cannot be resumed concurrently"
        );
    }

    #[test]
    fn resume_windows_expire_and_sweep_the_session() {
        let mut registry = registry();
        let (session, token) = registry
            .connect(claims(2), entity(20), negotiated(), Millis::new(1_000))
            .unwrap();
        let _ = registry
            .disconnect(&session.id, Tick(0), Millis::new(1_100))
            .unwrap();

        let swept = registry.sweep(Millis::new(2_100));
        assert_eq!(swept, vec![session.id.clone()]);
        assert!(registry.get(&session.id).is_none());
        assert_eq!(
            registry
                .resume(&token, Tick(0), Millis::new(2_100))
                .unwrap_err()
                .code(),
            ErrorCode::SessionExpired
        );
    }

    #[test]
    fn acknowledgements_require_a_connected_session() {
        let mut registry = registry();
        let (session, _) = registry
            .connect(claims(3), entity(30), negotiated(), Millis::new(1_000))
            .unwrap();
        registry.acknowledge(&session.id, Tick(12)).unwrap();
        assert_eq!(registry.get(&session.id).unwrap().last_ack_tick, Tick(12));

        let _ = registry
            .disconnect(&session.id, Tick(12), Millis::new(1_100))
            .unwrap();
        assert_eq!(
            registry
                .acknowledge(&session.id, Tick(13))
                .unwrap_err()
                .code(),
            ErrorCode::SessionExpired
        );
    }

    #[test]
    fn session_ids_and_tokens_are_not_interchangeable() {
        let mut registry = registry();
        let (session, token) = registry
            .connect(claims(4), entity(40), negotiated(), Millis::new(1_000))
            .unwrap();
        assert!(session.id.as_str().starts_with("sess-"));
        assert!(
            !session.id.as_str().contains(token.expose_secret()),
            "a session ID must never embed the resume secret"
        );
        assert_ne!(session.id.as_str(), token.expose_secret());
        assert!(registry.get(&session.id).is_some());
    }
}
