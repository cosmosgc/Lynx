//! Small bounded JSON realtime client for Nakama chat rooms.

use std::{collections::VecDeque, fmt};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

const QUEUE_SIZE: usize = 64;

enum Command {
    Send(String),
    History { limit: u8, cursor: Option<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NakamaChatFailure {
    InvalidConfiguration,
    Transport,
    Closed,
    QueueFull,
}

/// Connected Nakama realtime chat socket. Tokens are excluded from debug output.
pub struct NakamaChatClient {
    outgoing: mpsc::Sender<Command>,
    incoming: mpsc::Receiver<Value>,
    task: JoinHandle<()>,
}

impl fmt::Debug for NakamaChatClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NakamaChatClient")
            .field("connected_task", &!self.task.is_finished())
            .finish_non_exhaustive()
    }
}

impl NakamaChatClient {
    /// Connect and join a persistent public room. The caller must use a
    /// Nakama-issued access token; client-supplied sender identity is ignored.
    pub fn connect(base_url: &str, bearer: &str, room: &str) -> Result<Self, NakamaChatFailure> {
        if bearer.is_empty()
            || room.is_empty()
            || room.len() > 128
            || room.chars().any(char::is_control)
        {
            return Err(NakamaChatFailure::InvalidConfiguration);
        }
        let socket_url = socket_url(base_url, bearer)?;
        let (outgoing, out_rx) = mpsc::channel(QUEUE_SIZE);
        let (in_tx, incoming) = mpsc::channel(QUEUE_SIZE);
        let task = tokio::spawn(run(
            socket_url,
            base_url.trim_end_matches('/').to_owned(),
            bearer.to_owned(),
            room.to_owned(),
            out_rx,
            in_tx,
        ));
        Ok(Self {
            outgoing,
            incoming,
            task,
        })
    }

    pub fn send(&self, content: &str) -> Result<(), NakamaChatFailure> {
        let message = serde_json::to_string(&json!({"content": content}))
            .map_err(|_| NakamaChatFailure::InvalidConfiguration)?;
        self.outgoing
            .try_send(Command::Send(message))
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => NakamaChatFailure::QueueFull,
                mpsc::error::TrySendError::Closed(_) => NakamaChatFailure::Closed,
            })
    }

    pub fn history(&self, limit: u8, cursor: Option<String>) -> Result<(), NakamaChatFailure> {
        if limit == 0 || limit > 100 || cursor.as_ref().is_some_and(|cursor| cursor.len() > 1024) {
            return Err(NakamaChatFailure::InvalidConfiguration);
        }
        self.outgoing
            .try_send(Command::History { limit, cursor })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => NakamaChatFailure::QueueFull,
                mpsc::error::TrySendError::Closed(_) => NakamaChatFailure::Closed,
            })
    }

    pub fn try_recv(&mut self) -> Result<Value, mpsc::error::TryRecvError> {
        self.incoming.try_recv()
    }
}

impl Drop for NakamaChatClient {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn socket_url(base_url: &str, bearer: &str) -> Result<String, NakamaChatFailure> {
    let (scheme, authority) = if let Some(authority) = base_url.strip_prefix("https://") {
        ("wss", authority)
    } else if let Some(authority) = base_url.strip_prefix("http://") {
        ("ws", authority)
    } else {
        return Err(NakamaChatFailure::InvalidConfiguration);
    };
    let authority = authority.trim_end_matches('/');
    if authority.is_empty() || authority.contains(['/', '?', '#', '@', '\\']) {
        return Err(NakamaChatFailure::InvalidConfiguration);
    }
    let bearer: String = bearer
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect();
    Ok(format!(
        "{scheme}://{authority}/ws?lang=pt-BR&status=false&token={bearer}&format=json"
    ))
}

#[allow(clippy::too_many_lines)] // Socket lifetime, bounded queues, and protocol dispatch share one task.
async fn run(
    url: String,
    base_url: String,
    bearer: String,
    room: String,
    mut outgoing: mpsc::Receiver<Command>,
    incoming: mpsc::Sender<Value>,
) {
    let history_agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(5)))
            .max_redirects(0)
            .proxy(None)
            .build(),
    );
    let mut socket_config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default();
    socket_config.max_message_size = Some(64 * 1024);
    socket_config.max_frame_size = Some(64 * 1024);
    let Ok((socket, _)) =
        tokio_tungstenite::connect_async_with_config(&url, Some(socket_config), false).await
    else {
        let _ = incoming.try_send(json!({"error": {"code": "unavailable"}}));
        return;
    };
    let (mut sink, mut stream) = socket.split();
    if sink
        .send(Message::Text(
            json!({"cid":"lynx-chat-join", "channel_join": {
                "target": room, "type": 1, "persistence": true, "hidden": false
            }})
            .to_string()
            .into(),
        ))
        .await
        .is_err()
    {
        let _ = incoming.try_send(json!({"error": {"code": "unavailable"}}));
        return;
    }
    let mut channel_id: Option<String> = None;
    let mut pending_messages: VecDeque<String> = VecDeque::new();
    loop {
        tokio::select! {
            incoming_message = stream.next() => {
                match incoming_message {
                    Some(Ok(Message::Text(text))) => {
                        let Ok(envelope) = serde_json::from_str::<Value>(&text) else { continue; };
                        if let Some(id) = envelope.pointer("/channel/id").and_then(Value::as_str) {
                            channel_id = Some(id.to_owned());
                            while let Some(content) = pending_messages.pop_front() {
                                let message = serde_json::from_str::<Value>(&content).unwrap_or(Value::Null);
                                let outgoing = json!({"channel_message_send": {
                                    "channel_id": channel_id.as_deref().unwrap_or_default(),
                                    "content": message
                                }});
                                if sink.send(Message::Text(outgoing.to_string().into())).await.is_err() {
                                    return;
                                }
                            }
                        }
                        if incoming.try_send(envelope).is_err() { break; }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if sink.send(Message::Pong(payload)).await.is_err() { break; }
                    }
                    Some(Ok(Message::Pong(_) | Message::Frame(_) | Message::Binary(_))) => {}
                    Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                }
            }
            outgoing_message = outgoing.recv() => {
                let Some(command) = outgoing_message else { break; };
                match command {
                    Command::Send(content) => {
                        let Some(channel_id) = channel_id.as_deref() else {
                            if pending_messages.len() >= QUEUE_SIZE {
                                if incoming.try_send(json!({"error": {"code": "rate_limited"}})).is_err() {
                                    break;
                                }
                            } else {
                                pending_messages.push_back(content);
                            }
                            continue;
                        };
                        let envelope = json!({"channel_message_send": {
                            "channel_id": channel_id,
                            "content": serde_json::from_str::<Value>(&content).unwrap_or(Value::Null)
                        }});
                        if sink.send(Message::Text(envelope.to_string().into())).await.is_err() { break; }
                    }
                    Command::History { limit, cursor } => {
                        let older = cursor.is_some();
                        let Some(channel_id) = channel_id.clone() else { continue; };
                        let base_url = base_url.clone();
                        let bearer = bearer.clone();
                        let history_agent = history_agent.clone();
                        let result = tokio::task::spawn_blocking(move || {
                            let mut request = history_agent.get(format!("{base_url}/v2/channel/{channel_id}"))
                                .header("Authorization", &format!("Bearer {bearer}"))
                                .query("limit", limit.to_string())
                                .query("forward", "false");
                            if let Some(cursor) = cursor {
                                request = request.query("cursor", cursor);
                            }
                            let mut response = request.call().map_err(|_| ())?;
                            let body = response.body_mut().read_to_string().map_err(|_| ())?;
                            serde_json::from_str::<Value>(&body).map_err(|_| ())
                        }).await;
                        match result {
                            Ok(Ok(history)) => {
                                if incoming.try_send(json!({"channel_history": history, "older": older})).is_err() {
                                    break;
                                }
                            }
                            _ => {
                                if incoming.try_send(json!({"error": {"code": "unavailable"}})).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    let _ = incoming.try_send(json!({"error": {"code": "unavailable"}}));
}
