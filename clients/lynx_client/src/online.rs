//! Explicit, headless online orchestration. This is not a login flow.
//!
//! Construct and connect on a live Tokio runtime. The runtime must remain alive
//! while Bevy polls. Transient transport failures reconnect with exponential
//! backoff and the latest rotated resume token; protocol and state violations
//! are terminal until the caller connects again. Reliable replication (including
//! owner inventory) is not implemented: a delivery containing it fails the
//! connection rather than acknowledging it.

use bevy::prelude::{ResMut, Resource};
use engine_core::time::Tick;
use engine_net::envelope::{Envelope, MessageId, MessageKind};
use engine_net::protocol::Handshake;
use lynx_client_net::{Client, Config, Failure, SendError, Status};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::time::{Duration, Instant};

use crate::{
    chat::{ChatFeed, ChatMember, ChatMessage},
    reconciliation::Position,
    world_state::{self, WorldState},
};
use lynx_client_net::{NakamaChatClient, NakamaChatFailure};

const POLL_BUDGET: usize = 64;
const MIN_RESUME_TOKEN_CHARS: usize = 16;
const MAX_RESUME_TOKEN_CHARS: usize = 96;
const RECONNECT_INITIAL_DELAY: Duration = Duration::from_millis(250);
const RECONNECT_MAX_DELAY: Duration = Duration::from_secs(8);
/// The default authority tick is 50 ms with a 2 m/s server budget. A request
/// below that budget is accepted; a faster server tick rejects it and the
/// client halves its step until the server accepts it again.
const INITIAL_MOVE_STEP: f32 = 0.09;
const MIN_MOVE_STEP: f32 = 0.01;
const MOVE_STEP_GROWTH: f32 = 1.25;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ChatHistoryResponse {
    messages: Vec<ChatMessage>,
    next_before: Option<u64>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ChatPresenceResponse {
    members: Vec<ChatMember>,
    truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnlineStatus {
    Disconnected,
    Connecting,
    AwaitingSnapshot,
    Active,
    /// Waiting to retry after a transient transport failure.
    Reconnecting {
        attempt: u32,
    },
    Failed(OnlineError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnlineError {
    Transport(Failure),
    Closed,
    Protocol,
    /// The edge rejected a resume token; the next attempt uses the original
    /// credential instead.
    SessionExpired,
    /// The edge still considers the previous session attached.
    Conflict,
    State(world_state::Error),
    Send(SendError),
}

impl OnlineError {
    /// Whether a failure may recover on a fresh connection attempt.
    fn retryable(self, used_resume: bool) -> bool {
        match self {
            Self::SessionExpired | Self::Transport(Failure::Authentication) => used_resume,
            Self::Closed
            | Self::Conflict
            | Self::Transport(
                Failure::Timeout
                | Failure::Transport
                | Failure::RateLimited
                | Failure::Unavailable
                | Failure::IncomingQueueFull,
            ) => true,
            Self::Transport(_) | Self::Protocol | Self::State(_) | Self::Send(_) => false,
        }
    }
}

/// Everything needed to reconnect. Secrets are never included in `Debug`.
struct ConnectionSettings {
    url: String,
    /// Original credential: a static demo token or a Nakama access token.
    bearer: String,
    /// Character selected for fresh Nakama admission; omitted on resume.
    character: Option<String>,
    handshake: Handshake,
    /// Latest single-use token rotated by the world edge.
    resume: Option<String>,
}

impl fmt::Debug for ConnectionSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectionSettings")
            .field("url", &"[redacted]")
            .field("bearer", &"[redacted]")
            .field("character", &self.character.as_ref().map(|_| "[redacted]"))
            .field("resume", &self.resume.as_ref().map(|_| "[redacted]"))
            .finish_non_exhaustive()
    }
}

/// Insert this resource to enable polling in the shared Bevy schedule.
/// Credentials remain in memory only and are redacted from debug output.
#[derive(Debug, Resource)]
pub struct OnlineClient {
    world: WorldState,
    client: Option<Client>,
    generation: Option<u64>,
    status: OnlineStatus,
    pending_ack: Option<u64>,
    chat: ChatFeed,
    pending_chat: HashSet<String>,
    pending_history: HashMap<String, bool>,
    chat_request_sequence: u64,
    chat_bootstrapped: bool,
    nakama_chat: Option<NakamaChatClient>,
    settings: Option<ConnectionSettings>,
    runtime: Option<tokio::runtime::Handle>,
    retry_at: Option<Instant>,
    reconnect_attempts: u32,
    consecutive_failures: u32,
    attempt_used_resume: bool,
    attempt_received_token: bool,
    last_ack_tick: u64,
    movement_sent_at: Option<Instant>,
    last_round_trip: Option<Duration>,
    movement_step: f32,
    last_movement_applied: Option<bool>,
}

impl OnlineClient {
    pub fn new(max_prediction_distance: f32) -> Result<Self, world_state::Error> {
        Ok(Self {
            world: WorldState::new(max_prediction_distance)?,
            client: None,
            generation: None,
            status: OnlineStatus::Disconnected,
            pending_ack: None,
            chat: ChatFeed::default(),
            pending_chat: HashSet::new(),
            pending_history: HashMap::new(),
            chat_request_sequence: 0,
            chat_bootstrapped: false,
            nakama_chat: None,
            settings: None,
            runtime: None,
            retry_at: None,
            reconnect_attempts: 0,
            consecutive_failures: 0,
            attempt_used_resume: false,
            attempt_received_token: false,
            last_ack_tick: 0,
            movement_sent_at: None,
            last_round_trip: None,
            movement_step: INITIAL_MOVE_STEP,
            last_movement_applied: None,
        })
    }

    /// Replaces any prior connection. Must be called inside a Tokio runtime.
    /// The caller supplies credentials and the hello acknowledgement explicitly.
    pub fn connect(&mut self, config: Config) -> Result<(), world_state::Error> {
        self.close_transport();
        self.runtime = Some(tokio::runtime::Handle::current());
        self.nakama_chat = None;
        self.settings = Some(ConnectionSettings {
            url: config.url,
            bearer: config.bearer,
            character: config.character,
            handshake: config.handshake,
            resume: None,
        });
        self.reconnect_attempts = 0;
        self.consecutive_failures = 0;
        self.last_ack_tick = config.last_ack_tick.0;
        self.movement_step = INITIAL_MOVE_STEP;
        self.start_connection(false).map_err(|error| match error {
            OnlineError::State(error) => error,
            _ => world_state::Error::UnexpectedMessage,
        })
    }

    /// Re-open the transport immediately. Uses the latest rotated resume token
    /// when one is available and falls back to the original credential on the
    /// next scheduled attempt if the edge rejects it.
    pub fn reconnect(&mut self) -> Result<(), OnlineError> {
        if self.settings.is_none() {
            return Err(OnlineError::Protocol);
        }
        self.close_transport();
        self.consecutive_failures = 0;
        self.start_connection(true)
    }

    fn start_connection(&mut self, prefer_resume: bool) -> Result<(), OnlineError> {
        let runtime = self.runtime.clone().ok_or(OnlineError::Protocol)?;
        let _guard = runtime.enter();
        let Some(settings) = self.settings.as_ref() else {
            return Err(OnlineError::Protocol);
        };
        // Snapshot everything before mutating self; the settings borrow ends here.
        let url = settings.url.clone();
        let handshake = settings.handshake.clone();
        let resume = prefer_resume.then(|| settings.resume.clone()).flatten();
        let use_resume = resume.is_some();
        let (bearer, character) = resume.map_or_else(
            || (settings.bearer.clone(), settings.character.clone()),
            |token| (token, None),
        );
        let authenticated_character = character.is_some();
        let chat_bearer = bearer.clone();

        if authenticated_character
            && self.nakama_chat.is_none()
            && let Ok(nakama_url) = std::env::var("LYNX_NAKAMA_URL")
        {
            let room = std::env::var("LYNX_CHAT_ROOM").unwrap_or_else(|_| "lynx-zone".to_owned());
            self.nakama_chat = NakamaChatClient::connect(&nakama_url, &chat_bearer, &room).ok();
        }

        let generation = self.world.begin_connection().map_err(OnlineError::State)?;
        self.generation = Some(generation);
        self.attempt_used_resume = use_resume;
        self.attempt_received_token = false;
        self.pending_ack = None;
        self.pending_chat.clear();
        self.pending_history.clear();
        self.movement_sent_at = None;
        self.retry_at = None;
        if !use_resume {
            self.chat_bootstrapped = false;
        }
        self.client = Some(Client::connect(Config {
            url,
            bearer,
            character,
            handshake,
            last_ack_tick: Tick(self.last_ack_tick),
        }));
        self.status = OnlineStatus::Connecting;
        Ok(())
    }

    pub fn status(&self) -> OnlineStatus {
        self.status
    }
    pub fn world(&self) -> &WorldState {
        &self.world
    }
    pub fn chat(&self) -> &ChatFeed {
        &self.chat
    }
    /// Last authoritative tick acknowledged to the server; sent on resume.
    pub fn last_ack_tick(&self) -> u64 {
        self.last_ack_tick
    }
    /// Wall-clock time from sending the most recent movement intent to its
    /// authoritative result. This is a movement round trip, not a network-only
    /// RTT, and therefore includes at least one authority tick.
    pub fn last_round_trip(&self) -> Option<Duration> {
        self.last_round_trip
    }

    pub fn queue_depths(&self) -> (usize, usize) {
        self.client.as_ref().map_or((0, 0), Client::queue_depths)
    }
    /// Automatic reconnect attempts since the last explicit connect.
    pub fn reconnect_attempts(&self) -> u32 {
        self.reconnect_attempts
    }
    /// Whether a rotated resume token is available for the next reconnect.
    pub fn resume_available(&self) -> bool {
        self.settings
            .as_ref()
            .is_some_and(|settings| settings.resume.is_some())
    }
    /// Whether the current control session resumed after a disconnect.
    pub fn resumed(&self) -> bool {
        self.world.resumed()
    }
    /// Current bounded movement step in metres.
    pub fn movement_step(&self) -> f32 {
        self.movement_step
    }
    pub fn pending_movement(&self) -> bool {
        self.world.has_pending_movement()
    }
    pub fn last_movement_applied(&self) -> Option<bool> {
        self.last_movement_applied
    }

    pub fn send_chat(&mut self, content: &str) -> Result<(), OnlineError> {
        let request = game_api::SendChatMessage {
            channel_id: "zone".into(),
            content: content.to_owned(),
        };
        request.validate().map_err(|_| OnlineError::Protocol)?;
        if let Some(chat) = &self.nakama_chat {
            chat.send(content.trim()).map_err(|error| {
                self.chat.show_send_error(Some(match error {
                    NakamaChatFailure::QueueFull => "rate_limited",
                    NakamaChatFailure::Closed | NakamaChatFailure::Transport => "unavailable",
                    NakamaChatFailure::InvalidConfiguration => "invalid_request",
                }));
                OnlineError::Protocol
            })
        } else {
            self.send_chat_request(
                "chat.send",
                json!({"channel_id": "zone", "content": content.trim()}),
                None,
            )
        }
    }

    pub fn request_chat_history(
        &mut self,
        limit: u16,
        before: Option<String>,
    ) -> Result<(), OnlineError> {
        if game_api::validate_page(limit).is_err() || limit > 100 {
            return Err(OnlineError::Protocol);
        }
        if let Some(chat) = &self.nakama_chat {
            chat.history(
                u8::try_from(limit).map_err(|_| OnlineError::Protocol)?,
                before,
            )
            .map_err(|_| OnlineError::Protocol)
        } else {
            let before = before.and_then(|cursor| cursor.parse::<u64>().ok());
            self.send_chat_request(
                "chat.history",
                json!({"channel_id": "zone", "limit": limit, "before": before}),
                Some(before.is_some()),
            )
        }
    }

    pub fn request_chat_presence(&mut self) -> Result<(), OnlineError> {
        if self.nakama_chat.is_some() {
            Ok(())
        } else {
            self.send_chat_request("chat.presence", json!({}), None)
        }
    }

    fn send_chat_request(
        &mut self,
        kind: &str,
        body: serde_json::Value,
        history_older: Option<bool>,
    ) -> Result<(), OnlineError> {
        if self.status != OnlineStatus::Active {
            return Err(OnlineError::Protocol);
        }
        self.chat_request_sequence = self
            .chat_request_sequence
            .checked_add(1)
            .ok_or(OnlineError::Protocol)?;
        let id = format!("chat-{}", self.chat_request_sequence);
        let request = Envelope::new(
            MessageId::parse(&id).map_err(|_| OnlineError::Protocol)?,
            MessageKind::parse(kind).map_err(|_| OnlineError::Protocol)?,
            body,
        );
        self.client
            .as_ref()
            .expect("active client")
            .try_send(&request)
            .map_err(OnlineError::Send)?;
        self.pending_chat.insert(id.clone());
        if let Some(older) = history_older {
            self.pending_history.insert(id, older);
        }
        Ok(())
    }

    /// Advance presentation-only correction smoothing. This never produces a
    /// request or mutates the authoritative world view.
    pub fn smooth_presentation(&mut self, fraction: f32) {
        self.world.reconciliation_mut().smooth(fraction);
    }

    /// Tear down the current transport but retain reconnect settings and the
    /// rotated resume token. The world view is invalidated until a new baseline.
    fn close_transport(&mut self) {
        if let Some(mut client) = self.client.take() {
            client.close();
        }
        if let Some(generation) = self.generation.take() {
            let _ = self.world.disconnect(generation);
        }
        self.pending_ack = None;
        self.pending_chat.clear();
        self.pending_history.clear();
        self.movement_sent_at = None;
        self.retry_at = None;
    }

    /// Deliberate disconnect: stops retries and discards the resume token.
    pub fn disconnect(&mut self) {
        self.close_transport();
        self.nakama_chat = None;
        self.settings = None;
        self.runtime = None;
        self.reconnect_attempts = 0;
        self.chat_bootstrapped = false;
        self.status = OnlineStatus::Disconnected;
    }

    fn fail(&mut self, error: OnlineError) {
        let used_resume = self.attempt_used_resume;
        let received_token = self.attempt_received_token;
        self.close_transport();
        // A consumed resume token is only invalidated by an explicit rejection;
        // a plain transport loss keeps it so the next attempt can still resume.
        if used_resume
            && !received_token
            && matches!(
                error,
                OnlineError::SessionExpired | OnlineError::Transport(Failure::Authentication)
            )
            && let Some(settings) = self.settings.as_mut()
        {
            settings.resume = None;
        }
        if !error.retryable(used_resume) || self.consecutive_failures >= 6 {
            self.status = OnlineStatus::Failed(error);
            return;
        }
        self.reconnect_attempts = self.reconnect_attempts.saturating_add(1);
        self.consecutive_failures += 1;
        let exponent = self.consecutive_failures.saturating_sub(1).min(5);
        let delay = RECONNECT_INITIAL_DELAY
            .saturating_mul(1 << exponent)
            .min(RECONNECT_MAX_DELAY);
        self.retry_at = Some(Instant::now() + delay);
        self.status = OnlineStatus::Reconnecting {
            attempt: self.reconnect_attempts,
        };
    }

    fn try_scheduled_reconnect(&mut self) {
        if !matches!(self.status, OnlineStatus::Reconnecting { .. }) {
            return;
        }
        if self.retry_at.is_none_or(|at| Instant::now() < at) {
            return;
        }
        if let Err(error) = self.start_connection(true) {
            self.fail(error);
        }
    }

    /// Queue one action exactly once. A send failure invalidates the baseline;
    /// the caller must wait for the automatic reconnect or call `reconnect`.
    pub fn move_to(&mut self, destination: Position) -> Result<(), OnlineError> {
        if self.status != OnlineStatus::Active {
            return Err(OnlineError::Protocol);
        }
        let request = self
            .world
            .movement_request(destination)
            .map_err(OnlineError::State)?;
        let result = self
            .client
            .as_ref()
            .expect("active client")
            .try_send(&request);
        if let Err(error) = result {
            let error = OnlineError::Send(error);
            self.fail(error);
            return Err(error);
        }
        self.movement_sent_at = Some(Instant::now());
        Ok(())
    }

    /// Process at most a bounded number of messages and coalesce tick acks.
    #[allow(clippy::too_many_lines)] // Poll is the single bounded dispatcher for connection-scoped events.
    pub fn poll(&mut self) {
        self.try_scheduled_reconnect();
        let Some(client) = self.client.as_ref() else {
            return;
        };
        // Drain already-arrived envelopes before reacting to transport closure:
        // a final state error must not be masked by the socket closing first.
        let transport = client.status();
        for _ in 0..POLL_BUDGET {
            let Ok(envelope) = self.client.as_mut().expect("connected").try_recv() else {
                break;
            };
            if envelope.kind.as_str() == "protocol.error" {
                if let Some(correlation) = envelope.correlation.as_ref()
                    && self.pending_chat.remove(correlation.as_str())
                {
                    self.chat.show_send_error(envelope.body["code"].as_str());
                    continue;
                }
                if !self.world.is_pending_reply(&envelope) {
                    self.fail(protocol_error(&envelope));
                    return;
                }
            }
            if envelope.kind.as_str() == "world.token" {
                let Some(token) = envelope.body.get("resume").and_then(|v| v.as_str()) else {
                    self.fail(OnlineError::Protocol);
                    return;
                };
                if !valid_resume_token(token) {
                    self.fail(OnlineError::Protocol);
                    return;
                }
                if let Some(settings) = self.settings.as_mut() {
                    settings.resume = Some(token.to_owned());
                }
                self.attempt_received_token = true;
                continue;
            }
            match envelope.kind.as_str() {
                "chat.message" => {
                    if let Ok(message) =
                        serde_json::from_value::<ChatMessage>(envelope.body.clone())
                    {
                        self.chat.receive_message(message);
                    } else {
                        self.fail(OnlineError::Protocol);
                        return;
                    }
                }
                "chat.history.result" => {
                    let older = envelope
                        .correlation
                        .as_ref()
                        .and_then(|correlation| {
                            self.pending_chat.remove(correlation.as_str());
                            self.pending_history.remove(correlation.as_str())
                        })
                        .unwrap_or(false);
                    let Ok(history) =
                        serde_json::from_value::<ChatHistoryResponse>(envelope.body.clone())
                    else {
                        self.fail(OnlineError::Protocol);
                        return;
                    };
                    self.chat.receive_history(
                        history.messages,
                        history.next_before.map(|cursor| cursor.to_string()),
                        older,
                    );
                }
                "chat.presence.result" => {
                    if let Some(correlation) = envelope.correlation.as_ref() {
                        self.pending_chat.remove(correlation.as_str());
                    }
                    let Ok(presence) =
                        serde_json::from_value::<ChatPresenceResponse>(envelope.body.clone())
                    else {
                        self.fail(OnlineError::Protocol);
                        return;
                    };
                    self.chat
                        .receive_presence(presence.members, presence.truncated);
                }
                "chat.send.result" => {
                    if let Some(correlation) = envelope.correlation.as_ref() {
                        self.pending_chat.remove(correlation.as_str());
                    }
                }
                _ => {}
            }
            let generation = self.generation.expect("connected generation");
            if let Err(error) = self.world.receive(generation, &envelope) {
                self.fail(OnlineError::State(error));
                return;
            }
            if envelope.kind.as_str() == "world.ready"
                && self.attempt_used_resume
                && let Some(settings) = self.settings.as_mut()
            {
                // Admission consumed the old token; only the replacement may
                // be used after this point.
                settings.resume = None;
            }
            if matches!(envelope.kind.as_str(), "world.snapshot" | "world.delta") {
                self.pending_ack = self.world.tick();
                if let Some(tick) = self.world.tick() {
                    self.last_ack_tick = tick;
                }
                self.status = OnlineStatus::Active;
                self.consecutive_failures = 0;
            }
            if let Some(applied) = self.world.take_movement_applied() {
                self.last_movement_applied = Some(applied);
                if let Some(sent) = self.movement_sent_at.take() {
                    self.last_round_trip = Some(sent.elapsed());
                }
                // Adapt to a faster authority tick than the default 50 ms:
                // rejected endpoints shrink the bounded step, accepted ones
                // recover it without ever exceeding one default server budget.
                self.movement_step = if applied {
                    (self.movement_step * MOVE_STEP_GROWTH).min(INITIAL_MOVE_STEP)
                } else {
                    (self.movement_step * 0.5).max(MIN_MOVE_STEP)
                };
            }
        }
        if self.client.is_none() {
            return;
        }
        match transport {
            Status::Failed(error) => {
                self.fail(OnlineError::Transport(error));
                return;
            }
            Status::Closed => {
                self.fail(OnlineError::Closed);
                return;
            }
            Status::AwaitingReady | Status::Ready if self.status == OnlineStatus::Connecting => {
                self.status = OnlineStatus::AwaitingSnapshot;
            }
            _ => {}
        }
        if let Some(tick) = self.pending_ack {
            let ack = Envelope::new(
                MessageId::parse(&format!(
                    "ack-{}-{tick}",
                    self.generation.expect("connected generation")
                ))
                .expect("bounded numeric id"),
                MessageKind::parse("session.ack").expect("literal"),
                json!({"tick": tick}),
            );
            match self.client.as_ref().expect("connected").try_send(&ack) {
                Ok(()) => self.pending_ack = None,
                Err(SendError::Full) => {} // Cumulative acks can safely be coalesced.
                Err(error) => self.fail(OnlineError::Send(error)),
            }
        }
        if self.status == OnlineStatus::Active && !self.chat_bootstrapped {
            let bootstrapped = if self.nakama_chat.is_some() {
                // The Nakama join response triggers history bootstrap below.
                false
            } else {
                self.request_chat_history(50, None).is_ok() && self.request_chat_presence().is_ok()
            };
            if bootstrapped {
                self.chat_bootstrapped = true;
            }
        }
        self.poll_nakama_chat();
    }

    fn poll_nakama_chat(&mut self) {
        let Some(chat) = self.nakama_chat.as_mut() else {
            return;
        };
        let mut joined = false;
        for _ in 0..POLL_BUDGET {
            let Ok(envelope) = chat.try_recv() else {
                break;
            };
            if let Some(message) = envelope.get("channel_message") {
                let Some(message) = nakama_message(message) else {
                    continue;
                };
                self.chat.receive_message(message);
            } else if let Some(error) = envelope.get("error") {
                self.chat
                    .show_send_error(error.get("code").and_then(serde_json::Value::as_str));
            } else if let Some(history) = envelope.get("channel_history") {
                let mut messages: Vec<_> = history
                    .get("messages")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(nakama_message)
                    .collect();
                // Nakama's `forward=false` page is newest-first; keep the
                // presentation feed oldest-first so new messages append.
                messages.reverse();
                let cursor = history
                    .get("next_cursor")
                    .and_then(serde_json::Value::as_str)
                    .filter(|cursor| !cursor.is_empty())
                    .map(str::to_owned);
                self.chat.receive_history(
                    messages,
                    cursor,
                    envelope
                        .get("older")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                );
            } else if let Some(channel) = envelope.get("channel") {
                let presences: Vec<_> = channel
                    .get("presences")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(nakama_presence)
                    .collect();
                self.chat.receive_presence(presences, false);
                joined = true;
            } else if let Some(event) = envelope.get("channel_presence") {
                let joins = event
                    .get("joins")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(nakama_presence)
                    .collect();
                let leaves: Vec<String> = event
                    .get("leaves")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|presence| {
                        presence
                            .get("user_id")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                    })
                    .collect();
                self.chat.update_presence(joins, &leaves);
            }
        }
        if joined && !self.chat_bootstrapped && self.request_chat_history(50, None).is_ok() {
            self.chat_bootstrapped = true;
        }
    }
}

fn protocol_error(envelope: &Envelope) -> OnlineError {
    match envelope
        .body
        .get("code")
        .and_then(serde_json::Value::as_str)
    {
        Some("session_expired" | "unauthorized") => OnlineError::SessionExpired,
        Some("conflict") => OnlineError::Conflict,
        Some("rate_limited") => OnlineError::Transport(Failure::RateLimited),
        Some("unavailable") => OnlineError::Transport(Failure::Unavailable),
        _ => OnlineError::Protocol,
    }
}

fn valid_resume_token(token: &str) -> bool {
    (MIN_RESUME_TOKEN_CHARS..=MAX_RESUME_TOKEN_CHARS).contains(&token.len())
        && token.chars().all(|character| character.is_ascii_graphic())
}

fn nakama_presence(value: &serde_json::Value) -> Option<ChatMember> {
    let account_id = value.get("user_id")?.as_str()?;
    if account_id.len() > 128 {
        return None;
    }
    Some(ChatMember {
        account_id: account_id.to_owned(),
        character_id: safe_chat_label(
            value
                .get("username")
                .and_then(serde_json::Value::as_str)
                .filter(|username| !username.is_empty())
                .unwrap_or_else(|| {
                    value
                        .get("user_id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                }),
            40,
        ),
    })
}

fn nakama_message(value: &serde_json::Value) -> Option<ChatMessage> {
    let id = value.get("message_id")?.as_str()?;
    let channel_id = value.get("channel_id")?.as_str()?;
    let sender_id = value.get("sender_id")?.as_str()?;
    if id.len() > 128 || channel_id.len() > 128 || sender_id.len() > 128 {
        return None;
    }
    let content = value.get("content")?;
    let content = if let Some(content) = content.as_str() {
        serde_json::from_str::<serde_json::Value>(content).ok()?
    } else {
        content.clone()
    };
    let content = content.get("content")?.as_str()?;
    if content.chars().count() > game_api::MAX_MESSAGE_CHARS
        || content
            .chars()
            .any(|character| character.is_control() && character != '\n')
    {
        return None;
    }
    Some(ChatMessage {
        id: id.to_owned(),
        channel_id: channel_id.to_owned(),
        sender_id: sender_id.to_owned(),
        sender_name: safe_chat_label(
            value
                .get("username")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?"),
            80,
        ),
        content: content
            .chars()
            .map(|character| if character == '\n' { ' ' } else { character })
            .collect(),
        created_at_ms: value
            .get("create_time")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or_default(),
    })
}

fn safe_chat_label(value: &str, limit: usize) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(limit)
        .collect()
}

pub(crate) fn poll_online(client: Option<ResMut<OnlineClient>>) {
    if let Some(mut client) = client {
        client.poll();
    }
}

/// Keep smoothing bounded and independent from transport polling. A fixed
/// fraction avoids a local frame clock becoming gameplay input.
pub(crate) fn smooth_online(client: Option<ResMut<OnlineClient>>) {
    if let Some(mut client) = client {
        client.smooth_presentation(0.25);
    }
}
