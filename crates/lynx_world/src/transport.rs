//! Axum/WebSocket transport for the world service.
//!
//! The HTTP layer owns exactly three things: static hashed-token admission,
//! health endpoints, and moving length-prefixed envelopes between WebSocket
//! connections. It never interprets gameplay intent.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Router, serve};
use engine_core::error::{EngineError, ErrorCode};
use engine_net::envelope::Envelope;
use engine_net::framing::{FRAME_HEADER_BYTES, MAX_FRAME_BYTES, decode_frame, encode_frame};
use engine_net::protocol::ErrorPayload;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use world_gateway::{ERROR_KIND, ResumeToken};

use crate::auth::Authenticator;
use crate::service::{Admitted, Credential, HELLO_KIND, Hello, ServerEvent, ServerFrame};

/// How long a peer has to send `session.hello` after the upgrade.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

/// Messages queued for one connection before it is considered too slow.
const OUTBOUND_QUEUE: usize = 64;

/// Events queued from all transports before backpressure reaches sockets.
const INBOUND_QUEUE: usize = 256;

/// Create the channel connecting transports to the authority loop.
#[must_use]
pub fn event_channel() -> (mpsc::Sender<ServerEvent>, mpsc::Receiver<ServerEvent>) {
    mpsc::channel(INBOUND_QUEUE)
}

/// Shared state for every route.
#[derive(Debug)]
pub struct AppState {
    events: mpsc::Sender<ServerEvent>,
    authenticator: Authenticator,
    ready: AtomicBool,
    origins: OriginPolicy,
    message_budget: MessageBudgetConfig,
}

/// Fixed-window limit for binary and text messages on an admitted connection.
#[derive(Debug, Clone, Copy)]
pub struct MessageBudgetConfig {
    limit: u32,
    window: Duration,
}

impl MessageBudgetConfig {
    /// Construct a bounded budget (1–10,000 messages, 1 ms–60 seconds).
    ///
    /// # Errors
    /// Returns an error if either value is outside the supported range.
    pub fn new(limit: u32, window: Duration) -> anyhow::Result<Self> {
        anyhow::ensure!((1..=10_000).contains(&limit), "message limit out of range");
        anyhow::ensure!(
            (Duration::from_millis(1)..=Duration::from_secs(60)).contains(&window),
            "message window out of range"
        );
        Ok(Self { limit, window })
    }
}

impl Default for MessageBudgetConfig {
    fn default() -> Self {
        Self {
            limit: 100,
            window: Duration::from_secs(1),
        }
    }
}

#[derive(Debug)]
struct MessageBudget {
    config: MessageBudgetConfig,
    start: Instant,
    used: u32,
}

impl MessageBudget {
    fn new(config: MessageBudgetConfig, now: Instant) -> Self {
        Self {
            config,
            start: now,
            used: 0,
        }
    }

    fn check(&mut self, now: Instant) -> Result<(), Duration> {
        let elapsed = now.saturating_duration_since(self.start);
        if elapsed >= self.config.window {
            self.start = now;
            self.used = 0;
        }
        if self.used >= self.config.limit {
            return Err(self.config.window.saturating_sub(elapsed));
        }
        self.used += 1;
        Ok(())
    }
}

/// Optional exact-match browser Origin allowlist. Requests without Origin are
/// permitted for native clients; this policy is not authentication.
#[derive(Debug, Default)]
pub struct OriginPolicy {
    allowed: Vec<String>,
}

impl OriginPolicy {
    /// Validate serialized HTTP(S) origins. An empty list disables the policy.
    ///
    /// # Errors
    ///
    /// Returns an error for an origin with a path, credentials, or an invalid
    /// scheme or authority.
    pub fn new(allowed: Vec<String>) -> anyhow::Result<Self> {
        for origin in &allowed {
            let uri: axum::http::Uri = origin
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid origin: {origin}"))?;
            let valid = uri
                .scheme_str()
                .is_some_and(|scheme| scheme == "http" || scheme == "https")
                && uri.authority().is_some_and(|authority| {
                    !authority.host().is_empty()
                        && !authority.as_str().contains('@')
                        && !authority.as_str().contains('*')
                        && authority
                            .as_str()
                            .strip_prefix(authority.host())
                            .is_some_and(|suffix| {
                                suffix.is_empty()
                                    || (suffix.starts_with(':') && authority.port_u16().is_some())
                            })
                        && format!("{}://{}", uri.scheme_str().unwrap(), authority) == *origin
                });
            anyhow::ensure!(valid, "invalid origin: {origin}");
        }
        Ok(Self { allowed })
    }

    fn permits(&self, headers: &HeaderMap) -> bool {
        if self.allowed.is_empty() {
            return true;
        }
        let mut origins = headers.get_all(header::ORIGIN).iter();
        let Some(origin) = origins.next() else {
            return true;
        };
        origins.next().is_none()
            && origin
                .to_str()
                .is_ok_and(|value| self.allowed.iter().any(|allowed| allowed == value))
    }
}

impl AppState {
    /// Create the state for a running service.
    #[must_use]
    pub fn new(events: mpsc::Sender<ServerEvent>, authenticator: Authenticator) -> Self {
        Self {
            events,
            authenticator,
            ready: AtomicBool::new(true),
            origins: OriginPolicy::default(),
            message_budget: MessageBudgetConfig::default(),
        }
    }

    /// Set the browser Origin policy before sharing the state with the router.
    #[must_use]
    pub fn with_origin_policy(mut self, origins: OriginPolicy) -> Self {
        self.origins = origins;
        self
    }

    /// Set the post-admission per-connection message budget.
    #[must_use]
    pub fn with_message_budget(mut self, budget: MessageBudgetConfig) -> Self {
        self.message_budget = budget;
        self
    }

    /// Mark readiness for load balancers and operators.
    pub fn set_ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::Relaxed);
    }
}

/// Build the HTTP router.
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/world", get(world))
        .with_state(state)
}

/// Serve the router until `shutdown` resolves.
///
/// # Errors
///
/// Returns the underlying hyper error when the listener or an HTTP connection
/// fails.
pub async fn serve_with_shutdown(
    listener: tokio::net::TcpListener,
    state: Arc<AppState>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    serve(listener, router(state))
        .with_graceful_shutdown(shutdown)
        .await
}

async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

async fn readyz(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    if state.ready.load(Ordering::Relaxed) {
        (
            StatusCode::OK,
            Json(json!({
                "status": "ready",
                "characters": state.authenticator.grants().count(),
            })),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "draining" })),
        )
    }
}

async fn world(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !state.origins.permits(&headers) {
        tracing::info!(reason = "origin_forbidden", "websocket admission rejected");
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "origin_forbidden" })),
        )
            .into_response();
    }
    let Some(presented) = bearer(&headers) else {
        return unauthorized();
    };
    let credential = match state.authenticator.grant_for(presented) {
        Some(grant) => Credential::Static(grant.clone()),
        None => match ResumeToken::new(presented.to_owned()) {
            Ok(token) => Credential::Resume(token),
            Err(_) => return unauthorized(),
        },
    };

    upgrade
        .max_message_size(MAX_FRAME_BYTES + FRAME_HEADER_BYTES)
        .max_frame_size(MAX_FRAME_BYTES + FRAME_HEADER_BYTES)
        .on_upgrade(move |socket| connection(socket, credential, state))
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "unauthorized" })),
    )
        .into_response()
}

/// Extract a bearer token without accepting a malformed authorization header.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    if token.is_empty() || token.chars().any(char::is_whitespace) {
        return None;
    }
    Some(token)
}

async fn connection(mut socket: WebSocket, credential: Credential, state: Arc<AppState>) {
    let hello = match tokio::time::timeout(HELLO_TIMEOUT, read_hello(&mut socket)).await {
        Ok(Ok(hello)) => hello,
        Ok(Err(error)) => {
            reject(&mut socket, &error).await;
            return;
        }
        Err(_) => {
            let error = EngineError::new(
                ErrorCode::InvalidRequest,
                "the session.hello handshake was not received in time",
            );
            reject(&mut socket, &error).await;
            return;
        }
    };

    let (outbound, mut out_rx) = mpsc::channel(OUTBOUND_QUEUE);
    let (reply, reply_rx) = oneshot::channel();
    if state
        .events
        .send(ServerEvent::Admit {
            credential,
            hello,
            outbound,
            reply,
        })
        .await
        .is_err()
    {
        return;
    }

    let admitted: Admitted = match reply_rx.await {
        Ok(Ok(admitted)) => admitted,
        Ok(Err(error)) => {
            tracing::info!(code = ?error.code(), "websocket admission rejected");
            reject(&mut socket, &error).await;
            return;
        }
        Err(_) => return,
    };

    let end = pump(&mut socket, &state, admitted.connection, &mut out_rx).await;
    if let End::Protocol(error) = end {
        tracing::warn!(code = ?error.code(), "closing websocket after a protocol error");
        reject(&mut socket, &error).await;
    }

    let _ = state
        .events
        .send(ServerEvent::Leave {
            connection: admitted.connection,
        })
        .await;
}

enum End {
    Clean,
    Protocol(EngineError),
}

async fn pump(
    socket: &mut WebSocket,
    state: &Arc<AppState>,
    connection: world_server::ConnectionId,
    out_rx: &mut mpsc::Receiver<ServerFrame>,
) -> End {
    let mut budget = MessageBudget::new(state.message_budget, Instant::now());
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Binary(bytes))) => {
                        if let Err(retry) = budget.check(Instant::now()) {
                            return rate_limit_end(retry);
                        }
                        match decode_frame(&bytes) {
                            Ok(envelope) => {
                                if state
                                    .events
                                    .send(ServerEvent::Frame { connection, envelope })
                                    .await
                                    .is_err()
                                {
                                    return End::Clean;
                                }
                            }
                            Err(error) => return End::Protocol(error),
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        if let Err(retry) = budget.check(Instant::now()) {
                            return rate_limit_end(retry);
                        }
                        match Envelope::decode(text.as_str()) {
                            Ok(envelope) => {
                                if state
                                    .events
                                    .send(ServerEvent::Frame { connection, envelope })
                                    .await
                                    .is_err()
                                {
                                    return End::Clean;
                                }
                            }
                            Err(error) => return End::Protocol(error),
                        }
                    }
                    Some(Ok(Message::Close(_)) | Err(_)) | None => return End::Clean,
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                }
            }
            outgoing = out_rx.recv() => {
                match outgoing {
                    Some(ServerFrame::Envelope(envelope)) => {
                        if send_envelope(socket, &envelope).await.is_err() {
                            return End::Clean;
                        }
                    }
                    Some(ServerFrame::Close) | None => {
                        let _ = socket.send(Message::Close(None)).await;
                        return End::Clean;
                    }
                }
            }
        }
    }
}

fn rate_limit_end(retry: Duration) -> End {
    tracing::warn!(
        code = "budget_exhausted",
        "websocket message budget exceeded"
    );
    let retry_ms = retry.as_millis().saturating_add(1);
    End::Protocol(
        EngineError::new(
            ErrorCode::BudgetExhausted,
            "connection message budget exceeded",
        )
        .with_detail("retry_after_ms", retry_ms.to_string()),
    )
}

async fn read_hello(socket: &mut WebSocket) -> Result<Hello, EngineError> {
    loop {
        match socket.recv().await {
            Some(Ok(Message::Binary(bytes))) => {
                return hello_from(&decode_frame(&bytes)?);
            }
            Some(Ok(Message::Text(text))) => {
                return hello_from(&Envelope::decode(text.as_str())?);
            }
            Some(Ok(Message::Close(_))) => {
                return Err(closed_before_hello());
            }
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            Some(Err(error)) => {
                return Err(EngineError::new(
                    ErrorCode::Internal,
                    "websocket read failed during handshake",
                )
                .with_detail("error", error.to_string()));
            }
            None => return Err(closed_before_hello()),
        }
    }
}

fn hello_from(envelope: &Envelope) -> Result<Hello, EngineError> {
    if envelope.kind.as_str() != HELLO_KIND {
        return Err(EngineError::new(
            ErrorCode::InvalidRequest,
            "the first message must be a session.hello envelope",
        )
        .with_detail("kind", envelope.kind.as_str()));
    }
    serde_json::from_value(envelope.body.clone()).map_err(|error| {
        EngineError::new(ErrorCode::InvalidRequest, "invalid session.hello body")
            .with_detail("error", error.to_string())
    })
}

fn closed_before_hello() -> EngineError {
    EngineError::new(
        ErrorCode::InvalidRequest,
        "the connection closed before session.hello",
    )
}

async fn reject(socket: &mut WebSocket, error: &EngineError) {
    if let Ok(envelope) = error_envelope(error) {
        let _ = send_envelope(socket, &envelope).await;
    }
    let _ = socket.send(Message::Close(None)).await;
}

async fn send_envelope(socket: &mut WebSocket, envelope: &Envelope) -> Result<(), ()> {
    let frame = encode_frame(envelope).map_err(|_| ())?;
    socket
        .send(Message::Binary(frame.into()))
        .await
        .map_err(|_| ())
}

fn error_envelope(error: &EngineError) -> Result<Envelope, EngineError> {
    let body = serde_json::to_value(ErrorPayload::from(error)).map_err(|error| {
        EngineError::new(ErrorCode::Internal, "failed to encode a protocol error")
            .with_detail("error", error.to_string())
    })?;
    Ok(Envelope::new(
        engine_net::MessageId::parse("srv-error").expect("literal message ids are valid"),
        engine_net::MessageKind::parse(ERROR_KIND).expect("literal message kinds are valid"),
        body,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_budget_bounds_burst_and_refills_independently() {
        assert!(MessageBudgetConfig::new(0, Duration::from_secs(1)).is_err());
        assert!(MessageBudgetConfig::new(10_001, Duration::from_secs(1)).is_err());
        assert!(MessageBudgetConfig::new(1, Duration::ZERO).is_err());
        assert!(MessageBudgetConfig::new(1, Duration::from_secs(61)).is_err());
        let config = MessageBudgetConfig::new(2, Duration::from_secs(1)).unwrap();
        let now = Instant::now();
        let mut first = MessageBudget::new(config, now);
        let mut second = MessageBudget::new(config, now);
        assert!(first.check(now).is_ok());
        assert!(first.check(now).is_ok());
        assert_eq!(first.check(now), Err(Duration::from_secs(1)));
        assert!(second.check(now).is_ok());
        assert!(first.check(now + Duration::from_secs(1)).is_ok());
    }

    fn headers(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, value.parse().unwrap());
        headers
    }

    #[test]
    fn bearer_accepts_one_case_insensitive_scheme() {
        assert_eq!(bearer(&headers("Bearer abc")), Some("abc"));
        assert_eq!(bearer(&headers("bearer abc")), Some("abc"));
        assert_eq!(bearer(&headers("BEARER abc")), Some("abc"));
    }

    #[test]
    fn malformed_authorization_is_rejected() {
        assert!(bearer(&headers("Basic abc")).is_none());
        assert!(bearer(&headers("Bearer")).is_none());
        assert!(bearer(&headers("Bearer   ")).is_none());
        assert!(bearer(&headers("Bearer two words")).is_none());
        assert!(bearer(&HeaderMap::new()).is_none());
    }

    #[test]
    fn origin_policy_validates_configuration_and_headers() {
        for invalid in [
            "null",
            "*",
            "https://example.com/",
            "https://example.com/path",
            "https://user@example.com",
            "https://example.com:bad",
            "ftp://example.com",
            "https://example.com?x=1",
        ] {
            assert!(
                OriginPolicy::new(vec![invalid.into()]).is_err(),
                "{invalid}"
            );
        }
        let policy = OriginPolicy::new(vec![
            "https://example.com".into(),
            "http://[::1]:8080".into(),
        ])
        .unwrap();
        let mut headers = HeaderMap::new();
        assert!(policy.permits(&headers));
        headers.insert(header::ORIGIN, "https://example.com".parse().unwrap());
        assert!(policy.permits(&headers));
        headers.insert(header::ORIGIN, "https://example.com.evil".parse().unwrap());
        assert!(!policy.permits(&headers));
        headers.insert(header::ORIGIN, "https://example.com".parse().unwrap());
        headers.append(header::ORIGIN, "https://example.com".parse().unwrap());
        assert!(!policy.permits(&headers));
        assert!(OriginPolicy::default().permits(&headers));
    }
}
