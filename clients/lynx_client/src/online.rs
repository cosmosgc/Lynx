//! Explicit, headless online orchestration. This is not a login flow.
//!
//! Construct and connect on a live Tokio runtime. The runtime must remain alive
//! while Bevy polls; no reconnect or movement retry is performed automatically.
//! Resume tokens are deliberately discarded, so end-to-end resume is not provided.
//! Reliable replication (including owner inventory) is not implemented: a
//! delivery containing it fails the connection rather than acknowledging it.

use bevy::prelude::{ResMut, Resource};
use engine_net::envelope::{Envelope, MessageId, MessageKind};
use lynx_client_net::{Client, Config, Failure, SendError, Status};
use serde_json::json;
use std::collections::{HashMap, HashSet};

use crate::{
    chat::{ChatFeed, ChatMember, ChatMessage},
    reconciliation::Position,
    world_state::{self, WorldState},
};
use lynx_client_net::{NakamaChatClient, NakamaChatFailure};

const POLL_BUDGET: usize = 64;

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
    Failed(OnlineError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnlineError {
    Transport(Failure),
    Closed,
    Protocol,
    State(world_state::Error),
    Send(SendError),
}

/// Insert this resource to enable polling in the shared Bevy schedule.
/// Neither this resource nor its debug output retains server bearer tokens.
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
        })
    }

    /// Replaces any prior connection. Must be called inside a Tokio runtime.
    /// The caller supplies credentials and the hello acknowledgement explicitly.
    pub fn connect(&mut self, config: Config) -> Result<(), world_state::Error> {
        self.disconnect();
        let generation = self.world.begin_connection()?;
        let chat_bearer = config.bearer.clone();
        let authenticated_character = config.character.is_some();
        self.generation = Some(generation);
        self.client = Some(Client::connect(config));
        // Static development tokens authenticate only to the local demo world;
        // they are not Nakama session tokens and must never be offered to the
        // Nakama chat socket. Fresh account admissions carry a verified
        // character ID and use their Nakama access token for chat.
        if authenticated_character && let Ok(nakama_url) = std::env::var("LYNX_NAKAMA_URL") {
            let room = std::env::var("LYNX_CHAT_ROOM").unwrap_or_else(|_| "lynx-zone".to_owned());
            self.nakama_chat = NakamaChatClient::connect(&nakama_url, &chat_bearer, &room).ok();
        }
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

    pub fn disconnect(&mut self) {
        if let Some(mut client) = self.client.take() {
            client.close();
        }
        self.nakama_chat = None;
        if let Some(generation) = self.generation.take() {
            let _ = self.world.disconnect(generation);
        }
        self.pending_ack = None;
        self.pending_chat.clear();
        self.pending_history.clear();
        self.chat_bootstrapped = false;
        self.status = OnlineStatus::Disconnected;
    }

    fn fail(&mut self, error: OnlineError) {
        self.disconnect();
        self.status = OnlineStatus::Failed(error);
    }

    /// Queue one action exactly once. A send failure invalidates the baseline;
    /// the caller must explicitly reconnect and wait for a new snapshot.
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
        Ok(())
    }

    /// Process at most a bounded number of messages and coalesce tick acks.
    #[allow(clippy::too_many_lines)] // Poll is the single bounded dispatcher for connection-scoped events.
    pub fn poll(&mut self) {
        let Some(client) = self.client.as_ref() else {
            return;
        };
        match client.status() {
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
                    self.fail(OnlineError::Protocol);
                    return;
                }
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
            if matches!(envelope.kind.as_str(), "world.snapshot" | "world.delta") {
                self.pending_ack = self.world.tick();
                self.status = OnlineStatus::Active;
            }
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
