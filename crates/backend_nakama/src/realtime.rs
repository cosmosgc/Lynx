//! Long-lived Nakama realtime socket sessions.
//!
//! REST operations live in [`crate::NakamaClient`]. This module owns the
//! WebSocket lifecycle required by chat channels and parties; callers keep a
//! [`NakamaRealtimeSession`] alive for as long as they participate in a party.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use game_api::{
    AccountId, ApiError, ChatMessage, GroupId, SessionToken, validate_channel_id, validate_page,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

const DEFAULT_SOCKET_TIMEOUT: Duration = Duration::from_secs(10);
static NEXT_SOCKET_ID: AtomicU64 = AtomicU64::new(1);

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Typed destination for a Nakama realtime chat channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RealtimeChannelTarget {
    /// A public or private room name.
    Room(String),
    /// A direct conversation with another account.
    Direct(AccountId),
    /// A persistent Nakama group.
    Group(GroupId),
}

/// Channel details returned by Nakama after joining.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinedChannel {
    /// Nakama-issued channel ID used for sends and history requests.
    pub channel_id: String,
    /// Channel type.
    pub kind: RealtimeChannelKind,
    /// Number of presences reported by Nakama.
    pub member_count: u32,
}

/// Supported realtime channel types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeChannelKind {
    /// Room channel.
    Room,
    /// One-to-one channel.
    Direct,
    /// Persistent group channel.
    Group,
}

/// A Nakama party created on the current realtime socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealtimeParty {
    /// Nakama party identifier.
    pub id: String,
    /// Whether join requests require leader approval.
    pub open: bool,
    /// Whether this party is hidden from party listings.
    pub hidden: bool,
    /// Maximum party size.
    pub max_size: u32,
    /// Optional party label.
    pub label: Option<String>,
}

/// A member presence used when moderating a party.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartyPresence {
    /// Nakama user ID.
    pub user_id: String,
    /// The member's active session ID on the realtime socket.
    pub session_id: String,
    /// Username returned by Nakama, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

impl PartyPresence {
    fn validate(&self) -> Result<(), ApiError> {
        validate_id(&self.user_id, "party user")?;
        validate_id(&self.session_id, "party session")
    }
}

/// A persistent connection to Nakama's JSON realtime WebSocket.
///
/// Calls on one session are serialized by `&mut self`. Unsolicited realtime
/// events received while waiting for a response are buffered for
/// [`Self::next_event`]. The socket must remain open for Nakama party membership
/// to remain active.
pub struct NakamaRealtimeSession {
    socket: Socket,
    next_cid: u64,
    pending_events: VecDeque<Value>,
    timeout: Duration,
}

impl std::fmt::Debug for NakamaRealtimeSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NakamaRealtimeSession")
            .field("pending_events", &self.pending_events.len())
            .finish_non_exhaustive()
    }
}

impl NakamaRealtimeSession {
    /// Connect using the server base URL and an authenticated Nakama token.
    pub(crate) async fn connect(
        base_url: &str,
        token: &SessionToken,
        timeout: Duration,
    ) -> Result<Self, ApiError> {
        let url = socket_url(base_url, token);
        let timeout = if timeout.is_zero() {
            DEFAULT_SOCKET_TIMEOUT
        } else {
            timeout
        };
        let (socket, _) = tokio::time::timeout(timeout, connect_async(url))
            .await
            .map_err(|_| ApiError::Unavailable("Nakama realtime connection timed out".into()))?
            .map_err(|_| ApiError::Unavailable("Nakama realtime connection failed".into()))?;
        Ok(Self {
            socket,
            next_cid: NEXT_SOCKET_ID.fetch_add(1, Ordering::Relaxed),
            pending_events: VecDeque::new(),
            timeout,
        })
    }

    /// Join a room, direct, or group channel on this socket.
    pub async fn join_channel(
        &mut self,
        target: &RealtimeChannelTarget,
        persistence: bool,
        hidden: bool,
    ) -> Result<JoinedChannel, ApiError> {
        let (target, channel_type) = match target {
            RealtimeChannelTarget::Room(name) if valid_target(name) => (name.clone(), 1),
            RealtimeChannelTarget::Room(_) => {
                return Err(ApiError::InvalidRequest("invalid room name".into()));
            }
            RealtimeChannelTarget::Direct(account) => (account.to_string(), 2),
            RealtimeChannelTarget::Group(group) => (group.to_string(), 3),
        };
        let response = self
            .request(
                "channel_join",
                json!({
                    "target": target,
                    "type": channel_type,
                    "persistence": persistence,
                    "hidden": hidden,
                }),
            )
            .await?;
        let channel = response
            .get("channel")
            .ok_or_else(|| invalid_socket_response("channel join"))?;
        let channel_id = string_field(channel, "channelId")?;
        let channel_type = match channel
            .get("roomName")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            Some(_) => RealtimeChannelKind::Room,
            None if channel
                .get("groupId")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty()) =>
            {
                RealtimeChannelKind::Group
            }
            None => RealtimeChannelKind::Direct,
        };
        let member_count = channel
            .get("presences")
            .and_then(Value::as_array)
            .map_or(0, |presences| {
                u32::try_from(presences.len()).unwrap_or(u32::MAX)
            });
        Ok(JoinedChannel {
            channel_id,
            kind: channel_type,
            member_count,
        })
    }

    /// Leave a channel previously joined on this connection.
    pub async fn leave_channel(&mut self, channel_id: &str) -> Result<(), ApiError> {
        validate_id(channel_id, "channel")?;
        self.request("channel_leave", json!({ "channelId": channel_id }))
            .await?;
        Ok(())
    }

    /// Send a message to a channel joined on this connection.
    pub async fn send_channel_message(
        &mut self,
        channel_id: &str,
        content: &str,
    ) -> Result<ChannelMessageAck, ApiError> {
        validate_id(channel_id, "channel")?;
        let content = content.trim();
        if content.is_empty() || content.chars().count() > game_api::MAX_MESSAGE_CHARS {
            return Err(ApiError::InvalidRequest("invalid chat message".into()));
        }
        let response = self
            .request(
                "channel_message_send",
                json!({ "channelId": channel_id, "content": content }),
            )
            .await?;
        let ack = response
            .get("channelMessageAck")
            .ok_or_else(|| invalid_socket_response("channel message"))?;
        Ok(ChannelMessageAck {
            channel_id: string_field(ack, "channelId")?,
            message_id: string_field(ack, "messageId")?,
            code: ack.get("code").and_then(Value::as_i64),
        })
    }

    /// Retrieve persisted channel history from Nakama, oldest first when
    /// `forward` is true. The cursor is Nakama-issued and should be passed back
    /// unchanged to continue the same page direction.
    pub async fn list_channel_messages(
        &mut self,
        channel_id: &str,
        limit: u16,
        forward: bool,
        cursor: Option<&str>,
    ) -> Result<ChannelMessagePage, ApiError> {
        validate_channel_id(channel_id)?;
        validate_page(limit)?;
        if cursor.is_some_and(|value| value.is_empty() || value.len() > 4096) {
            return Err(ApiError::InvalidRequest(
                "invalid chat history cursor".into(),
            ));
        }
        let response = self
            .request(
                "channel_message_list",
                json!({
                    "channelId": channel_id,
                    "limit": limit,
                    "forward": forward,
                    "cursor": cursor.unwrap_or_default(),
                }),
            )
            .await?;
        let list = response
            .get("channelMessageList")
            .ok_or_else(|| invalid_socket_response("channel message list"))?;
        let message_values = match list.get("messages") {
            None => &[][..],
            Some(Value::Array(messages)) => messages.as_slice(),
            Some(_) => return Err(invalid_socket_response("channel messages")),
        };
        let messages = message_values
            .iter()
            .map(|message| {
                Ok(ChatMessage {
                    id: string_field(message, "messageId")?,
                    channel_id: string_field(message, "channelId")?,
                    sender_id: AccountId::new(string_field(message, "senderId")?)
                        .map_err(|_| invalid_socket_response("message sender"))?,
                    sender_name: message
                        .get("username")
                        .and_then(Value::as_str)
                        .filter(|name| !name.is_empty())
                        .map(str::to_owned),
                    content: string_field(message, "content")?,
                    created_at: None,
                })
            })
            .collect::<Result<Vec<_>, ApiError>>()?;
        Ok(ChannelMessagePage {
            messages,
            next_cursor: list
                .get("nextCursor")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            previous_cursor: list
                .get("prevCursor")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
        })
    }

    /// Create a realtime party on this socket.
    pub async fn create_party(
        &mut self,
        open: bool,
        hidden: bool,
        max_size: u32,
        label: Option<&str>,
    ) -> Result<RealtimeParty, ApiError> {
        if !(2..=100).contains(&max_size)
            || label.is_some_and(|value| value.len() > 256 || value.chars().any(char::is_control))
        {
            return Err(ApiError::InvalidRequest(
                "invalid party configuration".into(),
            ));
        }
        let response = self
            .request(
                "party_create",
                json!({
                    "open": open,
                    "hidden": hidden,
                    "maxSize": max_size,
                    "label": label.unwrap_or_default(),
                }),
            )
            .await?;
        party_from_response(&response)
    }

    /// Join an existing party on this socket.
    pub async fn join_party(&mut self, party_id: &str) -> Result<(), ApiError> {
        validate_id(party_id, "party")?;
        self.request("party_join", json!({ "partyId": party_id }))
            .await?;
        Ok(())
    }

    /// Leave a party without closing the realtime socket.
    pub async fn leave_party(&mut self, party_id: &str) -> Result<(), ApiError> {
        validate_id(party_id, "party")?;
        self.request("party_leave", json!({ "partyId": party_id }))
            .await?;
        Ok(())
    }

    /// Close a party. Nakama enforces that the caller is its leader.
    pub async fn close_party(&mut self, party_id: &str) -> Result<(), ApiError> {
        validate_id(party_id, "party")?;
        self.request("party_close", json!({ "partyId": party_id }))
            .await?;
        Ok(())
    }

    /// Promote a current member to party leader.
    pub async fn promote_party_member(
        &mut self,
        party_id: &str,
        presence: &PartyPresence,
    ) -> Result<(), ApiError> {
        validate_id(party_id, "party")?;
        presence.validate()?;
        self.request(
            "party_promote",
            json!({ "partyId": party_id, "presence": presence }),
        )
        .await?;
        Ok(())
    }

    /// Approve a pending join request. Nakama enforces that the caller leads
    /// the party.
    pub async fn accept_party_join_request(
        &mut self,
        party_id: &str,
        presence: &PartyPresence,
    ) -> Result<(), ApiError> {
        validate_id(party_id, "party")?;
        presence.validate()?;
        self.request(
            "party_accept",
            json!({ "partyId": party_id, "presence": presence }),
        )
        .await?;
        Ok(())
    }

    /// Remove a party member or decline their pending join request.
    pub async fn remove_party_member(
        &mut self,
        party_id: &str,
        presence: &PartyPresence,
    ) -> Result<(), ApiError> {
        validate_id(party_id, "party")?;
        presence.validate()?;
        self.request(
            "party_remove",
            json!({ "partyId": party_id, "presence": presence }),
        )
        .await?;
        Ok(())
    }

    /// List pending join requests for a party led by this session.
    pub async fn party_join_requests(
        &mut self,
        party_id: &str,
    ) -> Result<Vec<PartyPresence>, ApiError> {
        validate_id(party_id, "party")?;
        let response = self
            .request("party_join_request_list", json!({ "partyId": party_id }))
            .await?;
        let request_list = response
            .get("partyJoinRequestList")
            .ok_or_else(|| invalid_socket_response("party join requests"))?;
        let requests = match request_list.get("presences") {
            None => &[][..],
            Some(Value::Array(presences)) => presences.as_slice(),
            Some(_) => return Err(invalid_socket_response("party join requests")),
        };
        requests
            .iter()
            .map(|presence| {
                serde_json::from_value(presence.clone())
                    .map_err(|_| invalid_socket_response("party presence"))
            })
            .collect()
    }

    /// Receive the next unsolicited Nakama realtime event.
    pub async fn next_event(&mut self) -> Result<Value, ApiError> {
        if let Some(event) = self.pending_events.pop_front() {
            return Ok(event);
        }
        self.receive_value().await
    }

    /// Close the WebSocket, which also ends party membership on Nakama.
    pub async fn close(mut self) -> Result<(), ApiError> {
        self.socket
            .close(None)
            .await
            .map_err(|_| ApiError::Transport("Nakama realtime close failed".into()))
    }

    async fn request(&mut self, kind: &str, body: Value) -> Result<Value, ApiError> {
        let cid = self.next_cid.to_string();
        self.next_cid = self
            .next_cid
            .checked_add(1)
            .ok_or_else(|| ApiError::Unavailable("Nakama realtime request IDs exhausted".into()))?;
        let mut envelope = json!({ "cid": cid });
        envelope[kind] = body;
        self.socket
            .send(Message::Text(envelope.to_string().into()))
            .await
            .map_err(|_| ApiError::Transport("Nakama realtime send failed".into()))?;
        loop {
            let value = self.receive_value().await?;
            if value.get("cid").and_then(Value::as_str) == Some(&cid) {
                if value.get("error").is_some() {
                    return Err(ApiError::Backend {
                        status: 400,
                        code: value
                            .get("error")
                            .and_then(|error| error.get("code"))
                            .and_then(Value::as_i64),
                        message: "Nakama realtime request was rejected".into(),
                    });
                }
                return Ok(value);
            }
            if self.pending_events.len() >= 256 {
                return Err(ApiError::Unavailable(
                    "Nakama realtime event buffer is full".into(),
                ));
            }
            self.pending_events.push_back(value);
        }
    }

    async fn receive_value(&mut self) -> Result<Value, ApiError> {
        loop {
            let message = tokio::time::timeout(self.timeout, self.socket.next())
                .await
                .map_err(|_| ApiError::Unavailable("Nakama realtime read timed out".into()))?
                .ok_or_else(|| ApiError::Unavailable("Nakama realtime socket closed".into()))?
                .map_err(|_| ApiError::Transport("Nakama realtime read failed".into()))?;
            match message {
                Message::Text(text) => {
                    return serde_json::from_str(text.as_str()).map_err(|_| {
                        ApiError::Transport("Nakama returned invalid realtime JSON".into())
                    });
                }
                Message::Ping(payload) => self
                    .socket
                    .send(Message::Pong(payload))
                    .await
                    .map_err(|_| ApiError::Transport("Nakama realtime pong failed".into()))?,
                Message::Pong(_) | Message::Frame(_) => {}
                Message::Close(_) => {
                    return Err(ApiError::Unavailable(
                        "Nakama realtime socket closed".into(),
                    ));
                }
                Message::Binary(_) => {
                    return Err(ApiError::Transport(
                        "Nakama returned an unexpected binary realtime frame".into(),
                    ));
                }
            }
        }
    }
}

/// Acknowledgement returned by Nakama after storing a channel message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelMessageAck {
    /// Channel ID.
    pub channel_id: String,
    /// Nakama message ID.
    pub message_id: String,
    /// Optional application message code.
    pub code: Option<i64>,
}

/// A Nakama page of persisted realtime-channel messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelMessagePage {
    /// Messages in the requested page direction.
    pub messages: Vec<ChatMessage>,
    /// Cursor for the next page in the same direction.
    pub next_cursor: Option<String>,
    /// Cursor for the previous page in the same direction.
    pub previous_cursor: Option<String>,
}

#[derive(Deserialize)]
struct PartyResponse {
    party: PartyBody,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PartyBody {
    party_id: String,
    open: bool,
    hidden: bool,
    max_size: i64,
    #[serde(default)]
    label: String,
}

fn party_from_response(response: &Value) -> Result<RealtimeParty, ApiError> {
    let response: PartyResponse =
        serde_json::from_value(response.clone()).map_err(|_| invalid_socket_response("party"))?;
    Ok(RealtimeParty {
        id: response.party.party_id,
        open: response.party.open,
        hidden: response.party.hidden,
        max_size: u32::try_from(response.party.max_size)
            .map_err(|_| invalid_socket_response("party capacity"))?,
        label: (!response.party.label.is_empty()).then_some(response.party.label),
    })
}

fn socket_url(base_url: &str, token: &SessionToken) -> String {
    let websocket_base = if let Some(rest) = base_url.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base_url.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base_url.to_owned()
    };
    format!(
        "{websocket_base}/ws?lang=en&status=true&format=json&token={}",
        percent_encode(token.expose_secret())
    )
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

fn valid_target(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

fn validate_id(value: &str, kind: &str) -> Result<(), ApiError> {
    if !valid_target(value) || value.chars().any(char::is_whitespace) {
        return Err(ApiError::InvalidRequest(format!("invalid {kind} ID")));
    }
    Ok(())
}

fn string_field(value: &Value, field: &str) -> Result<String, ApiError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| invalid_socket_response("required field"))
}

fn invalid_socket_response(context: &str) -> ApiError {
    ApiError::Transport(format!(
        "Nakama returned an invalid realtime {context} response"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};

    #[test]
    fn socket_urls_encode_tokens_and_use_secure_schemes() {
        let token = SessionToken::new("jwt&secret").unwrap();
        assert_eq!(
            socket_url("https://nakama.example/base", &token),
            "wss://nakama.example/base/ws?lang=en&status=true&format=json&token=jwt%26secret"
        );
        assert_eq!(
            socket_url("http://localhost:7350", &token),
            "ws://localhost:7350/ws?lang=en&status=true&format=json&token=jwt%26secret"
        );
    }

    #[test]
    fn party_response_maps_camel_case_and_rejects_invalid_capacity() {
        let party = party_from_response(&json!({
            "party": {
                "partyId": "party-1",
                "open": false,
                "hidden": true,
                "maxSize": 5,
                "label": "raid",
            }
        }))
        .unwrap();
        assert_eq!(party.id, "party-1");
        assert_eq!(party.max_size, 5);
        assert_eq!(party.label.as_deref(), Some("raid"));
        assert!(party_from_response(&json!({ "party": { "maxSize": -1 } })).is_err());
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // One end-to-end realtime session fixture.
    async fn realtime_session_routes_chat_and_party_requests_over_one_socket() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();

            let join = read_request(&mut socket).await;
            assert_eq!(join["channel_join"]["target"], "global");
            assert_eq!(join["channel_join"]["type"], 1);
            let cid = join["cid"].as_str().unwrap();
            socket
                .send(Message::Text(
                    json!({ "statusPresenceEvent": { "joins": [] } })
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            socket.send(Message::Ping(vec![7].into())).await.unwrap();
            assert!(
                matches!(socket.next().await.unwrap().unwrap(), Message::Pong(payload) if payload.as_ref() == [7])
            );
            send_response(
                &mut socket,
                cid,
                json!({
                    "channel": {
                        "channelId": "channel-1",
                        "roomName": "global",
                        "presences": [{ "userId": "user-1" }],
                    }
                }),
            )
            .await;

            let history = read_request(&mut socket).await;
            assert_eq!(history["channel_message_list"]["channelId"], "channel-1");
            assert_eq!(history["channel_message_list"]["limit"], 10);
            send_response(
                &mut socket,
                history["cid"].as_str().unwrap(),
                json!({
                    "channelMessageList": {
                        "messages": [{
                            "channelId": "channel-1",
                            "messageId": "message-1",
                            "senderId": "user-1",
                            "username": "alice",
                            "content": "hello",
                        }],
                        "nextCursor": "cursor-2",
                    }
                }),
            )
            .await;

            let send = read_request(&mut socket).await;
            assert_eq!(send["channel_message_send"]["content"], "hello world");
            send_response(
                &mut socket,
                send["cid"].as_str().unwrap(),
                json!({ "channelMessageAck": { "channelId": "channel-1", "messageId": "message-2" } }),
            )
            .await;

            let create = read_request(&mut socket).await;
            assert_eq!(create["party_create"]["maxSize"], 5);
            send_response(
                &mut socket,
                create["cid"].as_str().unwrap(),
                json!({ "party": { "partyId": "party-1", "open": true, "hidden": false, "maxSize": 5 } }),
            )
            .await;

            for (key, expected_id) in [
                ("party_join", "party-1"),
                ("party_leave", "party-1"),
                ("party_close", "party-1"),
            ] {
                let request = read_request(&mut socket).await;
                assert_eq!(request[key]["partyId"], expected_id);
                send_response(&mut socket, request["cid"].as_str().unwrap(), json!({})).await;
            }
            let requests = read_request(&mut socket).await;
            assert_eq!(requests["party_join_request_list"]["partyId"], "party-1");
            send_response(
                &mut socket,
                requests["cid"].as_str().unwrap(),
                json!({
                    "partyJoinRequestList": {
                        "partyId": "party-1",
                        "presences": [{
                            "userId": "user-2",
                            "sessionId": "session-2",
                            "username": "bob",
                        }],
                    }
                }),
            )
            .await;
            for key in ["party_accept", "party_remove", "party_promote"] {
                let request = read_request(&mut socket).await;
                assert_eq!(request[key]["partyId"], "party-1");
                assert_eq!(request[key]["presence"]["userId"], "user-2");
                send_response(&mut socket, request["cid"].as_str().unwrap(), json!({})).await;
            }
        });

        let token = SessionToken::new("session-token").unwrap();
        let mut session = NakamaRealtimeSession::connect(
            &format!("http://{address}"),
            &token,
            DEFAULT_SOCKET_TIMEOUT,
        )
        .await
        .unwrap();
        let joined = session
            .join_channel(&RealtimeChannelTarget::Room("global".into()), true, false)
            .await
            .unwrap();
        assert_eq!(joined.channel_id, "channel-1");
        assert_eq!(joined.member_count, 1);
        assert_eq!(
            session.next_event().await.unwrap()["statusPresenceEvent"]["joins"]
                .as_array()
                .unwrap()
                .len(),
            0
        );

        let history = session
            .list_channel_messages("channel-1", 10, true, None)
            .await
            .unwrap();
        assert_eq!(history.messages[0].content, "hello");
        assert_eq!(history.messages[0].sender_name.as_deref(), Some("alice"));
        assert_eq!(history.next_cursor.as_deref(), Some("cursor-2"));
        let ack = session
            .send_channel_message("channel-1", "  hello world  ")
            .await
            .unwrap();
        assert_eq!(ack.message_id, "message-2");
        let party = session.create_party(true, false, 5, None).await.unwrap();
        assert_eq!(party.id, "party-1");
        session.join_party("party-1").await.unwrap();
        session.leave_party("party-1").await.unwrap();
        session.close_party("party-1").await.unwrap();
        let presence = PartyPresence {
            user_id: "user-2".into(),
            session_id: "session-2".into(),
            username: Some("bob".into()),
        };
        assert_eq!(
            session.party_join_requests("party-1").await.unwrap()[0],
            presence
        );
        session
            .accept_party_join_request("party-1", &presence)
            .await
            .unwrap();
        session
            .remove_party_member("party-1", &presence)
            .await
            .unwrap();
        session
            .promote_party_member("party-1", &presence)
            .await
            .unwrap();
        session.close().await.unwrap();
        server.await.unwrap();
    }

    async fn read_request(socket: &mut WebSocketStream<TcpStream>) -> Value {
        loop {
            match socket.next().await.unwrap().unwrap() {
                Message::Text(text) => return serde_json::from_str(text.as_str()).unwrap(),
                Message::Ping(payload) => socket.send(Message::Pong(payload)).await.unwrap(),
                other => panic!("unexpected client frame: {other:?}"),
            }
        }
    }

    async fn send_response(socket: &mut WebSocketStream<TcpStream>, cid: &str, response: Value) {
        let mut response = response;
        response["cid"] = json!(cid);
        socket
            .send(Message::Text(response.to_string().into()))
            .await
            .unwrap();
    }
}
