//! Headless WebSocket transport. No gameplay state or rendering is performed here.
//!
//! Call [`Client::connect`] inside a Tokio runtime, poll its queues from the
//! application, and keep the runtime alive for the lifetime of the client.
//! Received envelopes, particularly `world.token`, may contain bearer secrets.
//! Bearer authentication requires `wss://`, except for development connections
//! using `ws://` to a literal loopback IP address (for example, `127.0.0.1` or
//! `[::1]`). Hostnames, including `localhost`, are not accepted for plaintext:
//! DNS resolution must not be able to redirect a bearer token to a remote host.
//! Loopback plaintext still exposes credentials to local traffic observers.

use std::fmt;
use std::net::IpAddr;
use std::time::Duration;

use engine_core::time::Tick;
use engine_net::envelope::{Envelope, MessageId, MessageKind};
use engine_net::framing::{FRAME_HEADER_BYTES, MAX_FRAME_BYTES, decode_frame, encode_frame};
use engine_net::protocol::Handshake;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::{
    Message,
    client::IntoClientRequest,
    http::{HeaderValue, header},
    protocol::WebSocketConfig,
};

const QUEUE_SIZE: usize = 64;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Connection lifecycle; failure descriptions never include peer data or credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Connecting,
    /// Upgrade succeeded and hello was sent; admission has not yet been confirmed.
    AwaitingReady,
    /// `world.ready` arrived; the initial snapshot may still be pending.
    Ready,
    Closed,
    Failed(Failure),
}

/// Coarse failure category, deliberately excluding untrusted error strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    InvalidConfiguration,
    Timeout,
    Transport,
    Protocol,
    IncomingQueueFull,
}

/// Nonblocking outgoing queue errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    InvalidEnvelope,
    Full,
    Closed,
}

/// Connection settings. The bearer token is never included in Debug output.
pub struct Config {
    /// `wss://` URL, or `ws://` with a literal loopback IP for local development.
    pub url: String,
    /// Static demo token, or a previously received single-use resume token.
    pub bearer: String,
    /// Client protocol and content advertisement.
    pub handshake: Handshake,
    /// Last authoritative tick received, used when resuming.
    pub last_ack_tick: Tick,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // URLs may also contain secrets, so do not format them either.
        f.debug_struct("Config")
            .field("url", &"[redacted]")
            .field("bearer", &"[redacted]")
            .finish_non_exhaustive()
    }
}

/// Owns a background task and bounded queues. Dropping it aborts network I/O.
pub struct Client {
    outgoing: mpsc::Sender<Vec<u8>>,
    incoming: mpsc::Receiver<Envelope>,
    status: watch::Receiver<Status>,
    task: JoinHandle<()>,
    closed: bool,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Starts connecting in the background on the current Tokio runtime.
    /// After hello is sent, `world.ready` and a subsequent `world.snapshot` must
    /// both arrive within 10 seconds or the connection fails with a timeout.
    /// This checks envelope kinds only; applications must validate snapshot data.
    pub fn connect(config: Config) -> Self {
        let (outgoing, out_rx) = mpsc::channel(QUEUE_SIZE);
        let (in_tx, incoming) = mpsc::channel(QUEUE_SIZE);
        let (status_tx, status) = watch::channel(Status::Connecting);
        let task = tokio::spawn(run(config, out_rx, in_tx, status_tx));
        Self {
            outgoing,
            incoming,
            status,
            task,
            closed: false,
        }
    }

    /// Current state. Admission failures can also arrive as `protocol.error` envelopes.
    pub fn status(&self) -> Status {
        if self.closed {
            Status::Closed
        } else {
            *self.status.borrow()
        }
    }

    /// Queue an encoded envelope without blocking. Full queues do not drop messages silently.
    pub fn try_send(&self, envelope: &Envelope) -> Result<(), SendError> {
        let frame = encode_frame(envelope).map_err(|_| SendError::InvalidEnvelope)?;
        self.outgoing.try_send(frame).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => SendError::Full,
            mpsc::error::TrySendError::Closed(_) => SendError::Closed,
        })
    }

    /// Receive one envelope without blocking. Never log envelopes containing tokens.
    pub fn try_recv(&mut self) -> Result<Envelope, mpsc::error::TryRecvError> {
        self.incoming.try_recv()
    }

    /// Abort the task immediately; queued messages are not flushed.
    pub fn close(&mut self) {
        self.task.abort();
        self.closed = true;
        self.outgoing = closed_sender();
        self.incoming.close();
    }
}

fn closed_sender() -> mpsc::Sender<Vec<u8>> {
    let (sender, receiver) = mpsc::channel(1);
    drop(receiver);
    sender
}

impl Drop for Client {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn run(
    config: Config,
    mut outgoing: mpsc::Receiver<Vec<u8>>,
    incoming: mpsc::Sender<Envelope>,
    status: watch::Sender<Status>,
) {
    let result = pump(config, &mut outgoing, &incoming, &status).await;
    status.send_replace(match result {
        Ok(()) => Status::Closed,
        Err(failure) => Status::Failed(failure),
    });
}

async fn pump(
    config: Config,
    outgoing: &mut mpsc::Receiver<Vec<u8>>,
    incoming: &mpsc::Sender<Envelope>,
    status: &watch::Sender<Status>,
) -> Result<(), Failure> {
    // Reject embedded credentials and query strings: neither is needed by this protocol.
    let mut request = config
        .url
        .as_str()
        .into_client_request()
        .map_err(|_| Failure::InvalidConfiguration)?;
    let uri = request.uri();
    if !matches!(uri.scheme_str(), Some("ws" | "wss"))
        || uri.authority().is_none_or(|a| a.as_str().contains('@'))
        || uri.query().is_some()
        || (uri.scheme_str() == Some("ws")
            && !uri.host().is_some_and(|host| {
                host.trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse::<IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
            }))
        || config.bearer.is_empty()
        || config.bearer.chars().any(char::is_whitespace)
    {
        return Err(Failure::InvalidConfiguration);
    }
    let mut authorization = HeaderValue::from_str(&format!("Bearer {}", config.bearer))
        .map_err(|_| Failure::InvalidConfiguration)?;
    authorization.set_sensitive(true);
    request
        .headers_mut()
        .insert(header::AUTHORIZATION, authorization);
    let ws_config = WebSocketConfig::default()
        .max_message_size(Some(MAX_FRAME_BYTES + FRAME_HEADER_BYTES))
        .max_frame_size(Some(MAX_FRAME_BYTES + FRAME_HEADER_BYTES));
    let (mut socket, _) = tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio_tungstenite::connect_async_with_config(request, Some(ws_config), false),
    )
    .await
    .map_err(|_| Failure::Timeout)?
    .map_err(|_| Failure::Transport)?;
    let hello = Envelope::new(
        MessageId::parse("client-hello").expect("valid literal"),
        MessageKind::parse("session.hello").expect("valid literal"),
        serde_json::json!({"handshake": config.handshake, "last_ack_tick": config.last_ack_tick}),
    );
    let frame = encode_frame(&hello).map_err(|_| Failure::Protocol)?;
    tokio::time::timeout(CONNECT_TIMEOUT, socket.send(Message::Binary(frame.into())))
        .await
        .map_err(|_| Failure::Timeout)?
        .map_err(|_| Failure::Transport)?;
    status.send_replace(Status::AwaitingReady);
    let deadline = tokio::time::sleep(CONNECT_TIMEOUT);
    tokio::pin!(deadline);
    let mut ready = false;
    let mut baseline = false;
    loop {
        tokio::select! {
            biased;
            () = &mut deadline, if !baseline => return Err(Failure::Timeout),
            message = socket.next() => {
                let envelope = match message {
                    Some(Ok(Message::Binary(bytes))) => decode_frame(&bytes).map_err(|_| Failure::Protocol)?,
                    Some(Ok(Message::Text(text))) => Envelope::decode(text.as_str()).map_err(|_| Failure::Protocol)?,
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                    Some(Ok(Message::Close(_))) | None => return Ok(()),
                    Some(Err(_)) => return Err(Failure::Transport),
                    Some(Ok(_)) => return Err(Failure::Protocol),
                };
                let is_ready = envelope.kind.as_str() == "world.ready";
                let is_snapshot = envelope.kind.as_str() == "world.snapshot";
                incoming.try_send(envelope).map_err(|error| match error {
                    mpsc::error::TrySendError::Full(_) => Failure::IncomingQueueFull,
                    mpsc::error::TrySendError::Closed(_) => Failure::Transport,
                })?;
                if is_ready { ready = true; status.send_replace(Status::Ready); }
                if ready && is_snapshot { baseline = true; }
            }
            frame = outgoing.recv(), if ready => {
                let Some(frame) = frame else { return Ok(()); };
                if baseline {
                    tokio::time::timeout(CONNECT_TIMEOUT, socket.send(Message::Binary(frame.into()))).await.map_err(|_| Failure::Timeout)?.map_err(|_| Failure::Transport)?;
                } else {
                    tokio::select! {
                        () = &mut deadline => return Err(Failure::Timeout),
                        result = socket.send(Message::Binary(frame.into())) => result.map_err(|_| Failure::Transport)?,
                    }
                }
            }
        }
    }
}
