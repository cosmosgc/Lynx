//! Axum/WebSocket transport for the world service.
//!
//! The HTTP layer owns admission, health endpoints, and moving length-prefixed
//! envelopes between WebSocket connections. It never interprets gameplay
//! intent.

use std::collections::BTreeMap;
use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::connect_info::Connected;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::serve::Listener;
use axum::{Router, serve};
use backend_nakama::BackendTelemetrySnapshot;
use engine_content::{ContentVersionSnapshot, ContentVersionState};
use engine_core::error::{EngineError, ErrorCode};
use engine_core::id::StableId;
use engine_net::envelope::Envelope;
use engine_net::framing::{FRAME_HEADER_BYTES, MAX_FRAME_BYTES, decode_frame, encode_frame};
use engine_net::protocol::ErrorPayload;
use game_api::{
    AccountId, ApiError, CharacterCreationKey, CharacterId, CreateCharacter, CreateGroup, GroupId,
    GroupSearch, SessionToken, validate_group_page,
};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use world_gateway::{ERROR_KIND, ResumeToken};

use crate::auth::{
    Authenticator, CharacterManagementBackend, ChatManagementBackend, GroupManagementBackend,
    IdentityBackend, IdentityError, SocialManagementBackend, authenticate_selection,
};
use crate::multi_zone::ZoneConnection;
use crate::rate_limit::{RateLimitConfig, RateLimits};
use crate::service::{Admitted, Credential, HELLO_KIND, Hello, Metrics, ServerEvent, ServerFrame};

/// How long a peer has to send `session.hello` after the upgrade.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// Maximum time for one raw HTTP request header block to arrive.
pub const HTTP_HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Default messages queued for one connection before it is considered too
/// slow. The running service uses the validated `--outbound-queue` value.
const OUTBOUND_QUEUE: usize = crate::config::DEFAULT_OUTBOUND_QUEUE;

/// Events queued from all transports before backpressure reaches sockets.
const INBOUND_QUEUE: usize = 256;
const MAX_CONNECTIONS: usize = 1024;
const MAX_CONNECTIONS_PER_IP: usize = 16;
/// Hard cap for accepted TCP/HTTP connections, including requests that never
/// reach `/world`. The operating-system backlog applies backpressure once this
/// cap is full, rather than letting a slow-header flood consume more descriptors.
const MAX_ACCEPTED_CONNECTIONS: usize = 1_024;
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const AUTHORITY_QUEUE_TIMEOUT: Duration = Duration::from_secs(5);
/// Required production selection header. It is a character identifier, not an
/// authority grant: ownership is resolved from the authenticated session.
pub const CHARACTER_HEADER: &str = "x-lynx-character";
/// Maximum aggregate HTTP request header size accepted by the world route.
pub const MAX_REQUEST_HEADER_BYTES: usize = 16 * 1024;

#[derive(Debug, Default)]
struct Capacity {
    draining: bool,
    total: usize,
    ips: BTreeMap<IpAddr, usize>,
    rejected_draining: u64,
    rejected_total: u64,
    rejected_ip: u64,
}

#[derive(Debug)]
struct Permit {
    capacity: Arc<Mutex<Capacity>>,
    ip: IpAddr,
}

/// A TCP listener which keeps an owned permit for the full HTTP connection.
/// Route admission alone is too late: a peer can otherwise hold a descriptor
/// while sending an incomplete HTTP request.
struct ConnectionLimitedListener {
    listener: tokio::net::TcpListener,
    permits: Arc<Semaphore>,
}

struct LimitedStream {
    stream: tokio::net::TcpStream,
    _permit: OwnedSemaphorePermit,
    header_read_timeout: Duration,
    header_timer: Pin<Box<tokio::time::Sleep>>,
    header_timeout_active: bool,
    status_probe: [u8; 128],
    status_probe_len: usize,
    status_probe_done: bool,
}

impl LimitedStream {
    fn new(
        stream: tokio::net::TcpStream,
        permit: OwnedSemaphorePermit,
        header_read_timeout: Duration,
    ) -> Self {
        let header_deadline = tokio::time::Instant::now() + header_read_timeout;
        Self {
            stream,
            _permit: permit,
            header_read_timeout,
            header_timer: Box::pin(tokio::time::sleep_until(header_deadline)),
            header_timeout_active: true,
            status_probe: [0; 128],
            status_probe_len: 0,
            status_probe_done: false,
        }
    }

    fn reset_header_deadline(&mut self) {
        self.header_timer
            .as_mut()
            .reset(tokio::time::Instant::now() + self.header_read_timeout);
    }

    fn inspect_status_line(&mut self, bytes: &[u8]) {
        if self.status_probe_done {
            return;
        }
        for byte in bytes {
            if self.status_probe_len == self.status_probe.len() {
                self.status_probe_done = true;
                return;
            }
            self.status_probe[self.status_probe_len] = *byte;
            self.status_probe_len += 1;
            if self.status_probe_len >= 2
                && self.status_probe[self.status_probe_len - 2..self.status_probe_len] == *b"\r\n"
            {
                self.status_probe_done = true;
                let status = &self.status_probe[..self.status_probe_len];
                if status.starts_with(b"HTTP/1.1 101 ") || status.starts_with(b"HTTP/1.0 101 ") {
                    // The same IO becomes the upgraded WebSocket stream. Its
                    // idle policy is enforced at the WebSocket layer.
                    self.header_timeout_active = false;
                }
                return;
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct PeerAddress(SocketAddr);

impl AsyncRead for LimitedStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.header_timeout_active && self.status_probe_done {
            // A keep-alive connection may carry another request after the
            // previous response. Inspect its response status independently so
            // a later WebSocket upgrade also disables this HTTP-only timer.
            self.status_probe_len = 0;
            self.status_probe_done = false;
        }
        if self.header_timeout_active && self.header_timer.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "HTTP request headers were not received before the deadline",
            )));
        }
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for LimitedStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let result = Pin::new(&mut self.stream).poll_write(cx, buf);
        if let Poll::Ready(Ok(written)) = &result {
            if *written > 0 {
                self.inspect_status_line(&buf[..*written]);
                if self.header_timeout_active {
                    self.reset_header_deadline();
                }
            }
        }
        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

impl Listener for ConnectionLimitedListener {
    type Io = LimitedStream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        let permit = Arc::clone(&self.permits)
            .acquire_owned()
            .await
            .expect("connection admission listener owns its semaphore");
        let (stream, address) = loop {
            match self.listener.accept().await {
                Ok(connection) => break connection,
                Err(error) => tracing::warn!(%error, "TCP connection accept failed"),
            }
        };
        (
            LimitedStream::new(stream, permit, HTTP_HEADER_READ_TIMEOUT),
            address,
        )
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}

impl<'a> Connected<axum::serve::IncomingStream<'a, ConnectionLimitedListener>> for PeerAddress {
    fn connect_info(stream: axum::serve::IncomingStream<'a, ConnectionLimitedListener>) -> Self {
        Self(*stream.remote_addr())
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut capacity = self.capacity.lock().expect("capacity lock poisoned");
        capacity.total -= 1;
        let count = capacity.ips.get_mut(&self.ip).expect("permit IP exists");
        *count -= 1;
        if *count == 0 {
            capacity.ips.remove(&self.ip);
        }
    }
}

fn acquire(capacity: &Arc<Mutex<Capacity>>, ip: IpAddr) -> Result<Permit, &'static str> {
    let mut state = capacity.lock().expect("capacity lock poisoned");
    let reason = if state.draining {
        state.rejected_draining = state.rejected_draining.saturating_add(1);
        Some("draining")
    } else if state.total >= MAX_CONNECTIONS {
        state.rejected_total = state.rejected_total.saturating_add(1);
        Some("total_capacity")
    } else if state.ips.get(&ip).copied().unwrap_or(0) >= MAX_CONNECTIONS_PER_IP {
        state.rejected_ip = state.rejected_ip.saturating_add(1);
        Some("ip_capacity")
    } else {
        None
    };
    if let Some(reason) = reason {
        return Err(reason);
    }
    state.total += 1;
    *state.ips.entry(ip).or_default() += 1;
    Ok(Permit {
        capacity: Arc::clone(capacity),
        ip,
    })
}

/// Create the channel connecting transports to the authority loop.
#[must_use]
pub fn event_channel() -> (mpsc::Sender<ServerEvent>, mpsc::Receiver<ServerEvent>) {
    mpsc::channel(INBOUND_QUEUE)
}

/// Shared state for every route.
pub struct AppState {
    events: mpsc::Sender<ServerEvent>,
    admission: AdmissionAuthenticator,
    characters: Option<Arc<dyn CharacterManagementBackend + Send + Sync>>,
    social: Option<Arc<dyn SocialManagementBackend + Send + Sync>>,
    groups: Option<Arc<dyn GroupManagementBackend + Send + Sync>>,
    chat: Option<Arc<dyn ChatManagementBackend + Send + Sync>>,
    ready: AtomicBool,
    origins: OriginPolicy,
    limits: RateLimits,
    outbound_queue: usize,
    hello_timeout: Duration,
    idle_timeout: Duration,
    metrics: Arc<Metrics>,
    content_version: ContentVersionSnapshot,
    backend_telemetry: Option<Arc<dyn Fn() -> BackendTelemetrySnapshot + Send + Sync>>,
    capacity: Arc<Mutex<Capacity>>,
}

enum AdmissionAuthenticator {
    Demo(Authenticator),
    Identity(Arc<dyn IdentityBackend + Send + Sync>),
}

impl std::fmt::Debug for AdmissionAuthenticator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Demo(authenticator) => f.debug_tuple("Demo").field(authenticator).finish(),
            Self::Identity(_) => f.write_str("Identity([REDACTED BACKEND])"),
        }
    }
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("admission", &self.admission)
            .field("character_management", &self.characters.is_some())
            .field("social_management", &self.social.is_some())
            .field("group_management", &self.groups.is_some())
            .field("chat_target_discovery", &self.chat.is_some())
            .field("ready", &self.ready.load(Ordering::Relaxed))
            .field("outbound_queue", &self.outbound_queue)
            .field("idle_timeout", &self.idle_timeout)
            .field("backend_telemetry", &self.backend_telemetry.is_some())
            .finish_non_exhaustive()
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
            admission: AdmissionAuthenticator::Demo(authenticator),
            characters: None,
            social: None,
            groups: None,
            chat: None,
            ready: AtomicBool::new(true),
            origins: OriginPolicy::default(),
            limits: RateLimits::new(RateLimitConfig::default()),
            outbound_queue: OUTBOUND_QUEUE,
            hello_timeout: HELLO_TIMEOUT,
            idle_timeout: IDLE_TIMEOUT,
            metrics: Arc::new(Metrics::default()),
            content_version: ContentVersionSnapshot::absent(),
            backend_telemetry: None,
            capacity: Arc::new(Mutex::new(Capacity::default())),
        }
    }

    /// Create production admission state backed by trusted session and
    /// ownership verification. The backend is never exposed to transport
    /// clients and credential text is never retained in this state.
    #[must_use]
    pub fn with_identity_backend(
        events: mpsc::Sender<ServerEvent>,
        backend: Arc<dyn IdentityBackend + Send + Sync>,
    ) -> Self {
        Self {
            events,
            admission: AdmissionAuthenticator::Identity(backend),
            characters: None,
            social: None,
            groups: None,
            chat: None,
            ready: AtomicBool::new(true),
            origins: OriginPolicy::default(),
            limits: RateLimits::new(RateLimitConfig::default()),
            outbound_queue: OUTBOUND_QUEUE,
            hello_timeout: HELLO_TIMEOUT,
            idle_timeout: IDLE_TIMEOUT,
            metrics: Arc::new(Metrics::default()),
            content_version: ContentVersionSnapshot::absent(),
            backend_telemetry: None,
            capacity: Arc::new(Mutex::new(Capacity::default())),
        }
    }

    /// Enable authenticated character management on the player HTTP listener.
    #[must_use]
    pub fn with_character_backend(
        mut self,
        backend: Arc<dyn CharacterManagementBackend + Send + Sync>,
    ) -> Self {
        self.characters = Some(backend);
        self
    }

    /// Enable authenticated social relationship routes on the player API.
    #[must_use]
    pub fn with_social_backend(
        mut self,
        backend: Arc<dyn SocialManagementBackend + Send + Sync>,
    ) -> Self {
        self.social = Some(backend);
        self
    }

    /// Enable authenticated Nakama group routes on the player API.
    #[must_use]
    pub fn with_group_backend(
        mut self,
        backend: Arc<dyn GroupManagementBackend + Send + Sync>,
    ) -> Self {
        self.groups = Some(backend);
        self
    }

    /// Enable authorized chat-target discovery on the player API.
    #[must_use]
    pub fn with_chat_backend(
        mut self,
        backend: Arc<dyn ChatManagementBackend + Send + Sync>,
    ) -> Self {
        self.chat = Some(backend);
        self
    }

    /// Attach a scrape-time snapshot provider for bounded production backend
    /// measurements. The callback must not include credentials or user labels.
    #[must_use]
    pub fn with_backend_telemetry(
        mut self,
        provider: impl Fn() -> BackendTelemetrySnapshot + Send + Sync + 'static,
    ) -> Self {
        self.backend_telemetry = Some(Arc::new(provider));
        self
    }

    fn configured_demo_characters(&self) -> usize {
        match &self.admission {
            AdmissionAuthenticator::Demo(authenticator) => authenticator.grants().count(),
            AdmissionAuthenticator::Identity(_) => 0,
        }
    }

    /// Attach the authority's shared metrics.
    #[must_use]
    pub fn with_metrics(mut self, metrics: Arc<Metrics>) -> Self {
        self.metrics = metrics;
        self
    }

    /// Report the content bundle that startup actually loaded and verified.
    #[must_use]
    pub fn with_content_version(mut self, version: ContentVersionSnapshot) -> Self {
        self.content_version = version;
        self
    }

    /// Set the browser Origin policy before sharing the state with the router.
    #[must_use]
    pub fn with_origin_policy(mut self, origins: OriginPolicy) -> Self {
        self.origins = origins;
        self
    }

    /// Set bounded per-IP admission and per-server-session rate limits.
    ///
    /// Limits are keyed after authentication by the server-issued session ID,
    /// not by any client-provided identity field.
    #[must_use]
    pub fn with_rate_limits(mut self, limits: RateLimitConfig) -> Self {
        self.limits = RateLimits::new(limits);
        self
    }

    /// Set the per-connection outbound frame capacity. Callers should pass the
    /// value validated by [`ServeArgs::outbound_queue`](crate::config::ServeArgs::outbound_queue);
    /// the server rejects out-of-range configuration before building state.
    #[must_use]
    pub fn with_outbound_queue(mut self, capacity: usize) -> Self {
        debug_assert!(
            capacity >= 1,
            "an outbound queue must hold at least one frame"
        );
        self.outbound_queue = capacity;
        self
    }

    /// Set the maximum time without client activity on an admitted connection.
    #[must_use]
    pub fn with_idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = timeout;
        self
    }

    /// Revoke a principal's sessions after a trusted authorization change.
    /// This is a server-side control method, not an HTTP route; callers must
    /// supply a principal obtained from the identity provider or operator
    /// control plane, never from an untrusted request body.
    pub async fn revoke_principal_sessions(
        &self,
        principal: StableId,
    ) -> Result<usize, EngineError> {
        let (reply, response) = tokio::sync::oneshot::channel();
        self.events
            .send(ServerEvent::RevokePrincipal { principal, reply })
            .await
            .map_err(|_| {
                EngineError::new(
                    ErrorCode::Internal,
                    "the authority loop is unavailable for session revocation",
                )
            })?;
        response.await.map_err(|_| {
            EngineError::new(
                ErrorCode::Internal,
                "the authority loop did not complete session revocation",
            )
        })?
    }

    /// Mark readiness for load balancers and operators.
    pub fn set_ready(&self, ready: bool) {
        let mut capacity = self.capacity.lock().expect("capacity lock poisoned");
        if !ready {
            capacity.draining = true;
        }
        self.ready
            .store(ready && !capacity.draining, Ordering::Relaxed);
    }
}

/// Build the HTTP router.
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
        .route("/characters", get(list_characters).post(create_character))
        .route("/characters/{character}/select", post(select_character))
        .route("/social/friends", get(list_friends))
        .route("/social/friends/request", post(request_friend))
        .route("/social/friends/accept", post(accept_friend_request))
        .route("/social/friends/dismiss", post(dismiss_friend_request))
        .route("/social/friends/remove", post(remove_friend))
        .route("/social/blocks/block", post(block_account))
        .route("/social/blocks/unblock", post(unblock_account))
        .route("/social/chat/channels", get(discover_chat_channels))
        .route("/social/groups", get(my_groups).post(create_group))
        .route("/social/groups/discover", get(discover_groups))
        .route("/social/groups/{group}/join", post(join_group))
        .route("/social/groups/{group}/leave", post(leave_group))
        .route("/social/groups/{group}/members", get(group_members))
        .route(
            "/social/groups/{group}/members/add",
            post(add_group_members),
        )
        .route(
            "/social/groups/{group}/members/kick",
            post(kick_group_members),
        )
        .route(
            "/social/groups/{group}/members/promote",
            post(promote_group_members),
        )
        .route(
            "/social/groups/{group}/members/demote",
            post(demote_group_members),
        )
        .route(
            "/characters/{character}/inventory",
            get(character_inventory),
        )
        .route("/world", get(world))
        .route("/world/{zone_id}", get(world_in_zone))
        .with_state(state)
}

/// HTTP routes suitable for a trusted admin listener, without gameplay access.
pub fn admin_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
        .route("/diagnostics/replication", get(replication_diagnostics))
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
    let shutdown_state = Arc::clone(&state);
    serve(
        ConnectionLimitedListener {
            listener,
            permits: Arc::new(Semaphore::new(MAX_ACCEPTED_CONNECTIONS)),
        },
        router(state).into_make_service_with_connect_info::<PeerAddress>(),
    )
    .with_graceful_shutdown(async move {
        shutdown.await;
        shutdown_state.set_ready(false);
    })
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
                "characters": state.configured_demo_characters(),
            })),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "draining" })),
        )
    }
}

async fn metrics(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mut body = state.metrics.render();
    body.push_str(&encode_content_version(&state.content_version));
    if let Some(telemetry) = &state.backend_telemetry {
        body.push_str(&encode_backend_metrics(&telemetry()));
    }
    let admission = {
        let capacity = state.capacity.lock().expect("capacity lock poisoned");
        [
            capacity.rejected_draining,
            capacity.rejected_total,
            capacity.rejected_ip,
        ]
    };
    body.push_str(&encode_transport_metrics(
        admission,
        &state.limits.metrics(),
    ));
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
}

async fn replication_diagnostics(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(state.metrics.replication_diagnostics())
}

fn character_backend(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<
    (
        Arc<dyn CharacterManagementBackend + Send + Sync>,
        SessionToken,
    ),
    Box<Response>,
> {
    let Some(backend) = &state.characters else {
        return Err(Box::new(
            (StatusCode::NOT_FOUND, Json(json!({"error": "not_found"}))).into_response(),
        ));
    };
    let Some(token) = bearer(headers) else {
        return Err(Box::new(
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "unauthorized"})),
            )
                .into_response(),
        ));
    };
    let token = SessionToken::new(token.to_owned()).map_err(|_| {
        Box::new(
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "unauthorized"})),
            )
                .into_response(),
        )
    })?;
    Ok((Arc::clone(backend), token))
}

async fn list_characters(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let (backend, token) = match character_backend(&state, &headers) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    match tokio::task::spawn_blocking(move || backend.characters(&token)).await {
        Ok(Ok(characters)) => {
            (StatusCode::OK, Json(json!({"characters": characters}))).into_response()
        }
        Ok(Err(error)) => character_api_error(&error),
        Err(_) => character_unavailable(),
    }
}

async fn create_character(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<CreateCharacter>,
) -> Response {
    if let Err(error) = request.validate() {
        return character_api_error(&error);
    }
    let Some(key) = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "idempotency_key_required"})),
        )
            .into_response();
    };
    let key = match CharacterCreationKey::new(key) {
        Ok(key) => key,
        Err(error) => return character_api_error(&error),
    };
    let (backend, token) = match character_backend(&state, &headers) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    match tokio::task::spawn_blocking(move || backend.create_character(&token, &key, &request))
        .await
    {
        Ok(Ok(receipt)) => (StatusCode::CREATED, Json(json!(receipt))).into_response(),
        Ok(Err(error)) => character_api_error(&error),
        Err(_) => character_unavailable(),
    }
}

async fn select_character(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(character): Path<String>,
) -> Response {
    let character = match CharacterId::parse(&character) {
        Ok(character) => character,
        Err(error) => return character_api_error(&error),
    };
    let (backend, token) = match character_backend(&state, &headers) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    match tokio::task::spawn_blocking(move || backend.select_character(&token, character)).await {
        Ok(Ok(character)) => (StatusCode::OK, Json(json!(character))).into_response(),
        Ok(Err(error)) => character_api_error(&error),
        Err(_) => character_unavailable(),
    }
}

async fn character_inventory(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(character): Path<String>,
) -> Response {
    let character = match CharacterId::parse(&character) {
        Ok(character) => character,
        Err(error) => return character_api_error(&error),
    };
    let (backend, token) = match character_backend(&state, &headers) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    match tokio::task::spawn_blocking(move || backend.inventory(&token, character)).await {
        Ok(Ok(inventory)) => {
            (StatusCode::OK, Json(json!({"inventory": inventory}))).into_response()
        }
        Ok(Err(error)) => character_api_error(&error),
        Err(_) => character_unavailable(),
    }
}

async fn list_friends(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let (backend, token) = match social_backend(&state, &headers) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    match tokio::task::spawn_blocking(move || backend.friends(&token)).await {
        Ok(Ok(friends)) => (StatusCode::OK, Json(json!({"friends": friends}))).into_response(),
        Ok(Err(error)) => social_api_error(&error),
        Err(_) => social_unavailable(),
    }
}

#[derive(Clone, Copy)]
enum SocialAction {
    Request,
    Accept,
    Dismiss,
    Remove,
    Block,
    Unblock,
}

async fn request_friend(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<SocialAccountRequest>,
) -> Response {
    social_operation(state, headers, request.account_id, SocialAction::Request).await
}

async fn accept_friend_request(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<SocialAccountRequest>,
) -> Response {
    social_operation(state, headers, request.account_id, SocialAction::Accept).await
}

async fn dismiss_friend_request(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<SocialAccountRequest>,
) -> Response {
    social_operation(state, headers, request.account_id, SocialAction::Dismiss).await
}

async fn remove_friend(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<SocialAccountRequest>,
) -> Response {
    social_operation(state, headers, request.account_id, SocialAction::Remove).await
}

async fn block_account(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<SocialAccountRequest>,
) -> Response {
    social_operation(state, headers, request.account_id, SocialAction::Block).await
}

async fn unblock_account(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<SocialAccountRequest>,
) -> Response {
    social_operation(state, headers, request.account_id, SocialAction::Unblock).await
}

#[derive(serde::Deserialize)]
struct SocialAccountRequest {
    account_id: String,
}

async fn social_operation(
    state: Arc<AppState>,
    headers: HeaderMap,
    account: String,
    action: SocialAction,
) -> Response {
    let account = match AccountId::new(account) {
        Ok(account) => account,
        Err(error) => return social_api_error(&error),
    };
    let (backend, token) = match social_backend(&state, &headers) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    match tokio::task::spawn_blocking(move || match action {
        SocialAction::Request => backend.request_friend(&token, &account),
        SocialAction::Accept => backend.accept_friend_request(&token, &account),
        SocialAction::Dismiss => backend.dismiss_friend_request(&token, &account),
        SocialAction::Remove => backend.remove_friend(&token, &account),
        SocialAction::Block => backend.block_account(&token, &account),
        SocialAction::Unblock => backend.unblock_account(&token, &account),
    })
    .await
    {
        Ok(Ok(())) => (StatusCode::OK, Json(json!({"ok": true}))).into_response(),
        Ok(Err(error)) => social_api_error(&error),
        Err(_) => social_unavailable(),
    }
}

fn social_backend(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(Arc<dyn SocialManagementBackend + Send + Sync>, SessionToken), Box<Response>> {
    let Some(backend) = &state.social else {
        return Err(Box::new(
            (StatusCode::NOT_FOUND, Json(json!({"error": "not_found"}))).into_response(),
        ));
    };
    let Some(token) = bearer(headers) else {
        return Err(Box::new(
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "unauthorized"})),
            )
                .into_response(),
        ));
    };
    let token = SessionToken::new(token.to_owned()).map_err(|_| {
        Box::new(
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "unauthorized"})),
            )
                .into_response(),
        )
    })?;
    Ok((Arc::clone(backend), token))
}

fn social_api_error(error: &ApiError) -> Response {
    let (status, code) = match error {
        ApiError::Unauthenticated | ApiError::SessionExpired => {
            (StatusCode::UNAUTHORIZED, "unauthorized")
        }
        ApiError::NotFound(_) | ApiError::PermissionDenied => {
            (StatusCode::NOT_FOUND, "social_target_not_found")
        }
        ApiError::InvalidRequest(_) => (StatusCode::BAD_REQUEST, "invalid_request"),
        ApiError::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
        ApiError::RateLimited => (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
        ApiError::Unavailable(_) | ApiError::Transport(_) | ApiError::Backend { .. } => (
            StatusCode::SERVICE_UNAVAILABLE,
            "social_service_unavailable",
        ),
    };
    (status, Json(json!({"error": code}))).into_response()
}

fn social_unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error": "social_service_unavailable"})),
    )
        .into_response()
}

#[derive(serde::Deserialize)]
struct GroupSearchQuery {
    name: Option<String>,
    open: Option<bool>,
    members: Option<u32>,
    limit: Option<u16>,
    cursor: Option<String>,
}

async fn my_groups(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<GroupSearchQuery>,
) -> Response {
    let limit = query.limit.unwrap_or(50);
    if let Err(error) = validate_group_page(limit, query.cursor.as_deref()) {
        return social_api_error(&error);
    }
    let (backend, token) = match group_backend(&state, &headers) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    match tokio::task::spawn_blocking(move || {
        backend.my_groups(&token, limit, query.cursor.as_deref())
    })
    .await
    {
        Ok(Ok(groups)) => (StatusCode::OK, Json(json!(groups))).into_response(),
        Ok(Err(error)) => social_api_error(&error),
        Err(_) => social_unavailable(),
    }
}

async fn discover_chat_channels(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<GroupSearchQuery>,
) -> Response {
    let limit = query.limit.unwrap_or(50);
    if let Err(error) = validate_group_page(limit, query.cursor.as_deref()) {
        return social_api_error(&error);
    }
    let Some(backend) = &state.chat else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "not_found"}))).into_response();
    };
    let Some(token) = bearer(&headers) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    };
    let token = match SessionToken::new(token.to_owned()) {
        Ok(token) => token,
        Err(_) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "unauthorized"})),
            )
                .into_response();
        }
    };
    let backend = Arc::clone(backend);
    match tokio::task::spawn_blocking(move || {
        backend.discover_channels(&token, limit, query.cursor.as_deref())
    })
    .await
    {
        Ok(Ok(targets)) => (StatusCode::OK, Json(json!(targets))).into_response(),
        Ok(Err(error)) => social_api_error(&error),
        Err(_) => social_unavailable(),
    }
}

async fn discover_groups(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<GroupSearchQuery>,
) -> Response {
    let limit = query.limit.unwrap_or(50);
    if let Err(error) = validate_group_page(limit, query.cursor.as_deref()) {
        return social_api_error(&error);
    }
    let search = GroupSearch {
        name: query.name,
        open: query.open,
        members: query.members,
    };
    if let Err(error) = search.validate() {
        return social_api_error(&error);
    }
    let (backend, token) = match group_backend(&state, &headers) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    match tokio::task::spawn_blocking(move || {
        backend.discover_groups(&token, &search, limit, query.cursor.as_deref())
    })
    .await
    {
        Ok(Ok(groups)) => (StatusCode::OK, Json(json!(groups))).into_response(),
        Ok(Err(error)) => social_api_error(&error),
        Err(_) => social_unavailable(),
    }
}

async fn create_group(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<CreateGroup>,
) -> Response {
    if let Err(error) = request.validate() {
        return social_api_error(&error);
    }
    let (backend, token) = match group_backend(&state, &headers) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    match tokio::task::spawn_blocking(move || backend.create_group(&token, &request)).await {
        Ok(Ok(group)) => (StatusCode::CREATED, Json(json!(group))).into_response(),
        Ok(Err(error)) => social_api_error(&error),
        Err(_) => social_unavailable(),
    }
}

async fn join_group(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(group): Path<String>,
) -> Response {
    group_operation(state, headers, group, true).await
}

async fn leave_group(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(group): Path<String>,
) -> Response {
    group_operation(state, headers, group, false).await
}

async fn group_operation(
    state: Arc<AppState>,
    headers: HeaderMap,
    group: String,
    join: bool,
) -> Response {
    let group = match GroupId::new(group) {
        Ok(group) => group,
        Err(error) => return social_api_error(&error),
    };
    let (backend, token) = match group_backend(&state, &headers) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    match tokio::task::spawn_blocking(move || {
        if join {
            backend.request_join(&token, &group)
        } else {
            backend.leave_group(&token, &group)
        }
    })
    .await
    {
        Ok(Ok(())) => (StatusCode::OK, Json(json!({"ok": true}))).into_response(),
        Ok(Err(error)) => social_api_error(&error),
        Err(_) => social_unavailable(),
    }
}

async fn group_members(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(group): Path<String>,
    Query(query): Query<GroupSearchQuery>,
) -> Response {
    let group = match GroupId::new(group) {
        Ok(group) => group,
        Err(error) => return social_api_error(&error),
    };
    let limit = query.limit.unwrap_or(50);
    if let Err(error) = validate_group_page(limit, query.cursor.as_deref()) {
        return social_api_error(&error);
    }
    let (backend, token) = match group_backend(&state, &headers) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    match tokio::task::spawn_blocking(move || {
        backend.group_members(&token, &group, limit, query.cursor.as_deref())
    })
    .await
    {
        Ok(Ok(members)) => (StatusCode::OK, Json(json!(members))).into_response(),
        Ok(Err(error)) => social_api_error(&error),
        Err(_) => social_unavailable(),
    }
}

#[derive(serde::Deserialize)]
struct GroupMembersRequest {
    account_ids: Vec<String>,
}

#[derive(Clone, Copy)]
enum GroupMemberAction {
    Add,
    Kick,
    Promote,
    Demote,
}

async fn add_group_members(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(group): Path<String>,
    Json(request): Json<GroupMembersRequest>,
) -> Response {
    group_member_operation(
        state,
        headers,
        group,
        request.account_ids,
        GroupMemberAction::Add,
    )
    .await
}

async fn kick_group_members(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(group): Path<String>,
    Json(request): Json<GroupMembersRequest>,
) -> Response {
    group_member_operation(
        state,
        headers,
        group,
        request.account_ids,
        GroupMemberAction::Kick,
    )
    .await
}

async fn promote_group_members(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(group): Path<String>,
    Json(request): Json<GroupMembersRequest>,
) -> Response {
    group_member_operation(
        state,
        headers,
        group,
        request.account_ids,
        GroupMemberAction::Promote,
    )
    .await
}

async fn demote_group_members(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(group): Path<String>,
    Json(request): Json<GroupMembersRequest>,
) -> Response {
    group_member_operation(
        state,
        headers,
        group,
        request.account_ids,
        GroupMemberAction::Demote,
    )
    .await
}

async fn group_member_operation(
    state: Arc<AppState>,
    headers: HeaderMap,
    group: String,
    account_ids: Vec<String>,
    action: GroupMemberAction,
) -> Response {
    if account_ids.is_empty() || account_ids.len() > 100 {
        return social_api_error(&ApiError::InvalidRequest(
            "group account list must contain between 1 and 100 accounts".into(),
        ));
    }
    let accounts = match account_ids
        .into_iter()
        .map(AccountId::new)
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(accounts) => accounts,
        Err(error) => return social_api_error(&error),
    };
    let unique = accounts.iter().collect::<std::collections::BTreeSet<_>>();
    if unique.len() != accounts.len() {
        return social_api_error(&ApiError::InvalidRequest(
            "group account list contains duplicates".into(),
        ));
    }
    let group = match GroupId::new(group) {
        Ok(group) => group,
        Err(error) => return social_api_error(&error),
    };
    let (backend, token) = match group_backend(&state, &headers) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    match tokio::task::spawn_blocking(move || match action {
        GroupMemberAction::Add => backend.add_members(&token, &group, &accounts),
        GroupMemberAction::Kick => backend.kick_members(&token, &group, &accounts),
        GroupMemberAction::Promote => backend.promote_members(&token, &group, &accounts),
        GroupMemberAction::Demote => backend.demote_members(&token, &group, &accounts),
    })
    .await
    {
        Ok(Ok(())) => (StatusCode::OK, Json(json!({"ok": true}))).into_response(),
        Ok(Err(error)) => social_api_error(&error),
        Err(_) => social_unavailable(),
    }
}

fn group_backend(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(Arc<dyn GroupManagementBackend + Send + Sync>, SessionToken), Box<Response>> {
    let Some(backend) = &state.groups else {
        return Err(Box::new(
            (StatusCode::NOT_FOUND, Json(json!({"error": "not_found"}))).into_response(),
        ));
    };
    let Some(token) = bearer(headers) else {
        return Err(Box::new(
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "unauthorized"})),
            )
                .into_response(),
        ));
    };
    let token = SessionToken::new(token.to_owned()).map_err(|_| {
        Box::new(
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "unauthorized"})),
            )
                .into_response(),
        )
    })?;
    Ok((Arc::clone(backend), token))
}

fn character_api_error(error: &ApiError) -> Response {
    let (status, code) = match error {
        ApiError::Unauthenticated | ApiError::SessionExpired => {
            (StatusCode::UNAUTHORIZED, "unauthorized")
        }
        // Missing, deleted, suspended, and foreign characters deliberately share
        // one response to avoid exposing another account's character state.
        ApiError::NotFound(_) | ApiError::PermissionDenied => {
            (StatusCode::NOT_FOUND, "character_not_found")
        }
        ApiError::InvalidRequest(_) => (StatusCode::BAD_REQUEST, "invalid_request"),
        ApiError::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
        ApiError::RateLimited => (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
        ApiError::Unavailable(_) | ApiError::Transport(_) | ApiError::Backend { .. } => (
            StatusCode::SERVICE_UNAVAILABLE,
            "character_service_unavailable",
        ),
    };
    (status, Json(json!({"error": code}))).into_response()
}

fn character_unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error": "character_service_unavailable"})),
    )
        .into_response()
}

fn encode_content_version(snapshot: &ContentVersionSnapshot) -> String {
    let state = match snapshot.state() {
        ContentVersionState::Absent => "absent",
        ContentVersionState::Configured => "configured",
        ContentVersionState::Loaded => "loaded",
    };
    let mut text = String::from(
        "# HELP lynx_world_content_version_info Content bundle integration state and current version when configured.\n# TYPE lynx_world_content_version_info gauge\n",
    );
    if let Some(version) = snapshot.version() {
        use std::fmt::Write as _;
        let _ = writeln!(
            text,
            "lynx_world_content_version_info{{state=\"{state}\",version=\"{version}\"}} 1"
        );
    } else {
        use std::fmt::Write as _;
        let _ = writeln!(
            text,
            "lynx_world_content_version_info{{state=\"{state}\"}} 1"
        );
    }
    text
}

fn encode_backend_metrics(snapshot: &BackendTelemetrySnapshot) -> String {
    let adapters = [
        ("nakama", &snapshot.nakama),
        ("identity_store", &snapshot.identity_store),
        ("character_store", &snapshot.character_store),
    ];
    let mut text = String::from(
        "# HELP lynx_backend_operations_total Backend adapter operations completed, including failures.\n# TYPE lynx_backend_operations_total counter\n# HELP lynx_backend_operation_errors_total Backend adapter operations returning errors, including validation and domain errors.\n# TYPE lynx_backend_operation_errors_total counter\n# HELP lynx_backend_operation_duration_seconds Backend adapter operation duration, including lock waits and failures.\n# TYPE lynx_backend_operation_duration_seconds histogram\n",
    );
    for (adapter, measurement) in adapters {
        use std::fmt::Write as _;
        let _ = writeln!(
            text,
            "lynx_backend_operations_total{{adapter=\"{adapter}\"}} {}\nlynx_backend_operation_errors_total{{adapter=\"{adapter}\"}} {}",
            measurement.count, measurement.failures
        );
        for (bound, count) in ["0.001", "0.01", "0.1", "1", "10", "+Inf"]
            .into_iter()
            .zip(measurement.duration_buckets)
        {
            let _ = writeln!(
                text,
                "lynx_backend_operation_duration_seconds_bucket{{adapter=\"{adapter}\",le=\"{bound}\"}} {count}"
            );
        }
        let _ = writeln!(
            text,
            "lynx_backend_operation_duration_seconds_sum{{adapter=\"{adapter}\"}} {:.9}\nlynx_backend_operation_duration_seconds_count{{adapter=\"{adapter}\"}} {}",
            measurement.duration_seconds, measurement.count
        );
    }
    text
}

// Encode a scrape snapshot so the existing bounded counters remain the source
// of truth. The client emits OpenMetrics counter metadata without `_total`;
// Prometheus 0.0.4 associates metadata with the full sample name instead.
fn encode_transport_metrics(
    admission: [u64; 3],
    rows: &[crate::rate_limit::RejectionMetric],
) -> String {
    use prometheus_client::encoding::EncodeLabelSet;
    use prometheus_client::metrics::{counter::Counter, family::Family};
    use prometheus_client::registry::Registry;

    #[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
    struct Reason {
        reason: &'static str,
    }
    #[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
    struct Rate {
        class: &'static str,
        code: &'static str,
    }

    let mut registry = Registry::default();
    let admissions = Family::<Reason, Counter>::default();
    for (reason, count) in ["draining", "total_capacity", "ip_capacity"]
        .into_iter()
        .zip(admission)
    {
        admissions.get_or_create(&Reason { reason }).inc_by(count);
    }
    registry.register(
        "lynx_world_admission_rejections",
        "World-upgrade admission rejections by bounded reason",
        admissions,
    );
    let rates = Family::<Rate, Counter>::default();
    for row in rows {
        rates
            .get_or_create(&Rate {
                class: row.kind,
                code: row.code,
            })
            .inc_by(row.count);
    }
    registry.register(
        "lynx_world_rate_limit_rejections",
        "Rate limit rejections by bounded class and error code",
        rates,
    );
    let mut text = String::new();
    prometheus_client::encoding::text::encode_registry(&mut text, &registry)
        .expect("writing to a String cannot fail");
    for name in [
        "lynx_world_admission_rejections",
        "lynx_world_rate_limit_rejections",
    ] {
        text = text.replace(&format!("# HELP {name} "), &format!("# HELP {name}_total "));
        text = text.replace(
            &format!("# TYPE {name} counter"),
            &format!("# TYPE {name}_total counter"),
        );
    }
    if rows.is_empty() {
        text.push_str("# HELP lynx_world_rate_limit_rejections_total Rate limit rejections by bounded class and error code.\n# TYPE lynx_world_rate_limit_rejections_total counter\n");
    }
    text
}

#[expect(
    clippy::too_many_lines,
    reason = "upgrade admission validates bearer, origin, identity, and rate limits before socket acceptance"
)]
async fn world(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<PeerAddress>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    world_admit(state, peer, headers, upgrade, None).await
}

async fn world_in_zone(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<PeerAddress>,
    Path(zone_id): Path<String>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    world_admit(state, peer, headers, upgrade, Some(zone_id)).await
}

#[expect(
    clippy::too_many_lines,
    reason = "upgrade admission validates bearer, origin, identity, and rate limits before socket acceptance"
)]
async fn world_admit(
    state: Arc<AppState>,
    peer: PeerAddress,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
    zone_id: Option<String>,
) -> Response {
    if request_header_bytes(&headers) > MAX_REQUEST_HEADER_BYTES {
        return (
            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
            Json(json!({ "error": "request_headers_too_large" })),
        )
            .into_response();
    }
    let permit = match acquire(&state.capacity, peer.0.ip()) {
        Ok(permit) => permit,
        Err(reason) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": reason})),
            )
                .into_response();
        }
    };
    if let Err(error) = state
        .limits
        .admission()
        .handshake(peer.0.ip(), Instant::now())
    {
        if error.code() == ErrorCode::RateLimited {
            audit_admission_rate_limit(peer.0.ip(), "handshake");
        }
        return admission_rejected(&error);
    }
    if !state.origins.permits(&headers) {
        tracing::info!(reason = "origin_forbidden", "websocket admission rejected");
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "origin_forbidden" })),
        )
            .into_response();
    }
    let Some(presented) = bearer(&headers) else {
        return failed_auth_with_reason(&state, peer.0.ip(), Instant::now(), "credential_missing");
    };
    let credential = match &state.admission {
        AdmissionAuthenticator::Demo(authenticator) => match authenticator.grant_for(presented) {
            Some(grant) => Credential::Static(grant.clone()),
            None => match ResumeToken::new(presented.to_owned()) {
                Ok(token) => Credential::Resume(token),
                Err(_) => return failed_auth(&state, peer.0.ip()),
            },
        },
        AdmissionAuthenticator::Identity(backend) => {
            // A resume token is self-authenticating through the durable token
            // digest, so never derive its identity from selection headers.
            let selected = selected_character(&headers);
            if let Ok(character) = selected {
                let credential = presented.to_owned();
                let backend = Arc::clone(backend);
                let verified = tokio::task::spawn_blocking(move || {
                    authenticate_selection(backend.as_ref(), &credential, character)
                })
                .await
                .unwrap_or(Err(IdentityError::Unavailable));
                match verified {
                    Ok(claims) => Credential::Identity(claims),
                    Err(IdentityError::InvalidSession) => match ResumeToken::new(presented) {
                        Ok(token) => Credential::Resume(token),
                        Err(_) => {
                            return identity_rejected(
                                &state,
                                peer.0.ip(),
                                IdentityError::InvalidSession,
                                Some(character),
                            );
                        }
                    },
                    Err(error) => {
                        return identity_rejected(&state, peer.0.ip(), error, Some(character));
                    }
                }
            } else {
                match ResumeToken::new(presented) {
                    Ok(token) => Credential::Resume(token),
                    Err(_) => {
                        return identity_rejected(
                            &state,
                            peer.0.ip(),
                            IdentityError::CharacterForbidden,
                            None,
                        );
                    }
                }
            }
        }
    };
    if matches!(&credential, Credential::Resume(_))
        && let Err(error) = state
            .limits
            .admission()
            .resume_attempt(peer.0.ip(), Instant::now())
    {
        if error.code() == ErrorCode::RateLimited {
            audit_admission_rate_limit(peer.0.ip(), "resume_attempt");
        }
        return admission_rejected(&error);
    }
    if let Err(error) = state
        .limits
        .admission()
        .connection_churn(peer.0.ip(), Instant::now())
    {
        if error.code() == ErrorCode::RateLimited {
            audit_admission_rate_limit(peer.0.ip(), "connection_churn");
        }
        return admission_rejected(&error);
    }

    upgrade
        .max_message_size(MAX_FRAME_BYTES + FRAME_HEADER_BYTES)
        .max_frame_size(MAX_FRAME_BYTES + FRAME_HEADER_BYTES)
        .on_upgrade(move |socket| async move {
            let _permit = permit;
            connection(socket, credential, state, peer.0.ip(), zone_id).await;
        })
}

fn request_header_bytes(headers: &HeaderMap) -> usize {
    headers.iter().fold(0usize, |total, (name, value)| {
        total
            .saturating_add(name.as_str().len())
            .saturating_add(value.as_bytes().len())
            .saturating_add(4) // colon and CRLF
    })
}

fn admission_rejected(error: &EngineError) -> Response {
    let status = match error.code() {
        ErrorCode::RateLimited => StatusCode::TOO_MANY_REQUESTS,
        ErrorCode::CapacityExceeded => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::BAD_REQUEST,
    };
    let mut response = (
        status,
        Json(json!({
            "error": error.code().as_str(),
            "retry_after_ms": error.details().get("retry_after_ms"),
        })),
    )
        .into_response();
    if let Some(retry) = error.details().get("retry_after_ms")
        && let Ok(milliseconds) = retry.parse::<u64>()
    {
        let seconds = milliseconds.saturating_add(999) / 1_000;
        if let Ok(value) = axum::http::HeaderValue::from_str(&seconds.max(1).to_string()) {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
    }
    response
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "unauthorized" })),
    )
        .into_response()
}

fn failed_auth(state: &AppState, ip: IpAddr) -> Response {
    failed_auth_at(state, ip, Instant::now())
}

fn failed_auth_at(state: &AppState, ip: IpAddr, now: Instant) -> Response {
    failed_auth_with_reason(state, ip, now, "credential_rejected")
}

fn failed_auth_with_reason(
    state: &AppState,
    ip: IpAddr,
    now: Instant,
    denial_reason: &'static str,
) -> Response {
    match state.limits.admission().failed_auth(ip, now) {
        Ok(()) => {
            tracing::warn!(
                event = "world_admission_denied",
                source_ip = %ip,
                reason = denial_reason,
                "world credential admission denied"
            );
            unauthorized()
        }
        Err(error) => {
            if error.code() == ErrorCode::RateLimited {
                audit_admission_rate_limit(ip, "failed_auth");
            }
            admission_rejected(&error)
        }
    }
}

fn audit_admission_rate_limit(ip: IpAddr, class: &'static str) {
    tracing::warn!(
        event = "world_rate_limit_rejected",
        source_ip = %ip,
        class,
        reason = "rate_limited",
        "world admission rate limit reached"
    );
}

fn identity_rejected(
    state: &AppState,
    ip: IpAddr,
    error: IdentityError,
    character: Option<StableId>,
) -> Response {
    if error == IdentityError::CharacterForbidden
        && let Some(character) = character
    {
        // This structured record is the admission audit event. It deliberately
        // excludes the bearer credential and the backend's failure details.
        tracing::warn!(
            event = "world_admission_denied",
            source_ip = %ip,
            reason = error.code(),
            character = %character,
            "world character selection denied"
        );
    }
    if error != IdentityError::Unavailable {
        let limited = failed_auth(state, ip);
        if limited.status() == StatusCode::TOO_MANY_REQUESTS {
            return limited;
        }
    }
    let status = match error {
        IdentityError::InvalidSession => StatusCode::UNAUTHORIZED,
        IdentityError::CharacterForbidden => StatusCode::FORBIDDEN,
        IdentityError::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
    };
    (status, Json(json!({ "error": error.code() }))).into_response()
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

fn selected_character(headers: &HeaderMap) -> Result<StableId, IdentityError> {
    let mut values = headers.get_all(CHARACTER_HEADER).iter();
    let Some(value) = values.next() else {
        return Err(IdentityError::CharacterForbidden);
    };
    if values.next().is_some() {
        return Err(IdentityError::CharacterForbidden);
    }
    let value = value
        .to_str()
        .map_err(|_| IdentityError::CharacterForbidden)?;
    let character = StableId::parse(value).map_err(|_| IdentityError::CharacterForbidden)?;
    (character.kind() == engine_core::id::IdKind::Character)
        .then_some(character)
        .ok_or(IdentityError::CharacterForbidden)
}

async fn connection(
    mut socket: WebSocket,
    credential: Credential,
    state: Arc<AppState>,
    source_ip: IpAddr,
    requested_zone: Option<String>,
) {
    let hello = match tokio::time::timeout(state.hello_timeout, read_hello(&mut socket)).await {
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

    let is_resume = matches!(&credential, Credential::Resume(_));
    let (outbound, mut out_rx) = mpsc::channel(state.outbound_queue);
    let admitted_result: Result<(Admitted, Option<ZoneConnection>), EngineError> =
        if let Some(zone_id) = requested_zone {
            let (reply, reply_rx) = oneshot::channel();
            if state
                .events
                .send(ServerEvent::AdmitZone {
                    zone_id,
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
            match reply_rx.await {
                Ok(Ok(admitted)) => Ok((
                    Admitted {
                        connection: admitted.route.connection,
                        session: admitted.session,
                        actor: admitted.actor,
                        resumed: admitted.resumed,
                    },
                    Some(admitted.route),
                )),
                Ok(Err(error)) => Err(error),
                Err(_) => return,
            }
        } else {
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
            match reply_rx.await {
                Ok(Ok(admitted)) => Ok((admitted, None)),
                Ok(Err(error)) => Err(error),
                Err(_) => return,
            }
        };
    let (admitted, route) = match admitted_result {
        Ok(admitted) => admitted,
        Err(error) => {
            if is_resume {
                audit_resume_admission_denied(source_ip, &error);
            } else {
                tracing::info!(code = ?error.code(), "websocket admission rejected");
            }
            reject(&mut socket, &error).await;
            return;
        }
    };

    if let Err(error) = state
        .limits
        .sessions()
        .register(&admitted.session, Instant::now())
    {
        tracing::warn!(code = ?error.code(), "websocket rate-limit registration rejected");
        reject(&mut socket, &error).await;
        send_leave(&state, admitted.connection, route).await;
        return;
    }

    let end = pump(
        &mut socket,
        &state,
        source_ip,
        admitted.connection,
        &admitted.session,
        admitted.actor,
        route.clone(),
        &mut out_rx,
    )
    .await;
    if let End::Protocol(error) = end {
        tracing::warn!(code = ?error.code(), "closing websocket after a protocol error");
        reject(&mut socket, &error).await;
    }

    send_leave(&state, admitted.connection, route).await;
}

async fn send_leave(
    state: &AppState,
    connection: world_server::ConnectionId,
    route: Option<ZoneConnection>,
) {
    let event = route.map_or(ServerEvent::Leave { connection }, |route| {
        ServerEvent::LeaveZone { route }
    });
    let _ = state.events.send(event).await;
}

fn audit_resume_admission_denied(source_ip: IpAddr, error: &EngineError) {
    tracing::warn!(
        event = "world_resume_admission_denied",
        source_ip = %source_ip,
        reason = error.code().as_str(),
        "world resume admission denied"
    );
}

enum End {
    Clean,
    Protocol(EngineError),
}

async fn pump(
    socket: &mut WebSocket,
    state: &Arc<AppState>,
    source_ip: IpAddr,
    connection: world_server::ConnectionId,
    session: &engine_net::replication::SessionId,
    actor: StableId,
    route: Option<ZoneConnection>,
    out_rx: &mut mpsc::Receiver<ServerFrame>,
) -> End {
    let idle = tokio::time::sleep(state.idle_timeout);
    tokio::pin!(idle);
    loop {
        tokio::select! {
            () = &mut idle => return End::Clean,
            incoming = socket.recv() => {
                idle.as_mut().reset(tokio::time::Instant::now() + state.idle_timeout);
                match incoming {
                    Some(Ok(Message::Binary(bytes))) => {
                        match decode_frame(&bytes) {
                            Ok(envelope) => {
                                if let Err(error) = state.limits.sessions().check(session, &envelope, Instant::now()) {
                                    return rate_limit_end(source_ip, session, "message", error);
                                }
                                let queued = tokio::time::timeout(
                                    AUTHORITY_QUEUE_TIMEOUT,
                                    state.events.send(frame_event(connection, route.clone(), envelope)),
                                ).await;
                                if !matches!(queued, Ok(Ok(()))) {
                                    return End::Clean;
                                }
                            }
                            Err(error) => {
                                if let Err(limited) = state.limits.sessions().check_unknown(session, Instant::now()) {
                                    return rate_limit_end(source_ip, session, "unknown_message", limited);
                                }
                                return End::Protocol(error);
                            }
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        match Envelope::decode(text.as_str()) {
                            Ok(envelope) => {
                                if let Err(error) = state.limits.sessions().check(session, &envelope, Instant::now()) {
                                    return rate_limit_end(source_ip, session, "message", error);
                                }
                                let queued = tokio::time::timeout(
                                    AUTHORITY_QUEUE_TIMEOUT,
                                    state.events.send(frame_event(connection, route.clone(), envelope)),
                                ).await;
                                if !matches!(queued, Ok(Ok(()))) {
                                    return End::Clean;
                                }
                            }
                            Err(error) => {
                                if let Err(limited) = state.limits.sessions().check_unknown(session, Instant::now()) {
                                    return rate_limit_end(source_ip, session, "unknown_message", limited);
                                }
                                return End::Protocol(error);
                            }
                        }
                    }
                    Some(Ok(Message::Close(_)) | Err(_)) | None => return End::Clean,
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                }
            }
            outgoing = out_rx.recv() => {
                match outgoing {
                    Some(ServerFrame::Envelope(envelope)) => {
                        audit_outbound_rejection(source_ip, session, actor, &envelope);
                        if send_envelope(socket, &envelope).await.is_err() {
                            return End::Clean;
                        }
                    }
                    Some(ServerFrame::EncodedReplication { bytes, kind }) => {
                        let byte_count = bytes.len();
                        let sent = send_encoded(socket, bytes).await.is_ok();
                        state.metrics.record_replication_socket_write_for_session(
                            session,
                            kind,
                            byte_count,
                            sent,
                        );
                        if !sent {
                            return End::Clean;
                        }
                    }
                    Some(ServerFrame::Close) | None => {
                        let _ = tokio::time::timeout(WRITE_TIMEOUT, socket.send(Message::Close(None))).await;
                        return End::Clean;
                    }
                }
            }
        }
    }
}

fn frame_event(
    connection: world_server::ConnectionId,
    route: Option<ZoneConnection>,
    envelope: Envelope,
) -> ServerEvent {
    route.map_or(
        ServerEvent::Frame {
            connection,
            envelope: envelope.clone(),
        },
        |route| ServerEvent::FrameZone { route, envelope },
    )
}

fn audit_outbound_rejection(
    source_ip: IpAddr,
    session: &engine_net::replication::SessionId,
    actor: StableId,
    envelope: &Envelope,
) {
    let code = match envelope.kind.as_str() {
        ERROR_KIND => envelope
            .body
            .get("code")
            .cloned()
            .and_then(|value| serde_json::from_value::<ErrorCode>(value).ok()),
        "action.result" => {
            serde_json::from_value::<engine_sim::action::ActionResult>(envelope.body.clone())
                .ok()
                .and_then(|result| result.error().map(EngineError::code))
        }
        _ => None,
    };
    let Some(code) = code else {
        return;
    };
    let correlation_id = envelope.correlation.as_ref().map(|id| id.as_str());
    tracing::warn!(
        event = "world_authority_rejected",
        source_ip = %source_ip,
        session = %session,
        actor = %actor,
        correlation_id,
        reason = code.as_str(),
        "world authority rejected client operation"
    );
}

fn rate_limit_end(
    source_ip: IpAddr,
    session: &engine_net::replication::SessionId,
    class: &'static str,
    error: EngineError,
) -> End {
    tracing::warn!(
        event = "world_rate_limit_rejected",
        source_ip = %source_ip,
        %session,
        class,
        reason = error.code().as_str(),
        "world session rate limit reached"
    );
    End::Protocol(error)
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
    let _ = tokio::time::timeout(WRITE_TIMEOUT, socket.send(Message::Close(None))).await;
}

async fn send_envelope(socket: &mut WebSocket, envelope: &Envelope) -> Result<(), ()> {
    let frame = encode_frame(envelope).map_err(|_| ())?;
    send_encoded(socket, frame).await
}

async fn send_encoded(socket: &mut WebSocket, frame: Vec<u8>) -> Result<(), ()> {
    write_with_timeout(socket.send(Message::Binary(frame.into())), WRITE_TIMEOUT).await
}

async fn write_with_timeout<T>(
    write: impl Future<Output = Result<T, axum::Error>>,
    timeout: Duration,
) -> Result<T, ()> {
    tokio::time::timeout(timeout, write)
        .await
        .map_err(|_| ())?
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

#[cfg_attr(coverage_nightly, coverage(off))]
#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::id::{IdKind, StableId};
    use game_api::{AccountId, Character, CharacterCreationReceipt, CharacterStatus};
    use std::sync::Mutex as StdMutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tracing::field::{Field, Visit};
    use tracing_subscriber::layer::Context as LayerContext;
    use tracing_subscriber::prelude::*;

    #[derive(Clone)]
    struct CaptureEvents(Arc<StdMutex<Vec<String>>>);

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CaptureEvents {
        fn on_event(&self, event: &tracing::Event<'_>, _context: LayerContext<'_, S>) {
            let mut fields = CapturedFields::default();
            event.record(&mut fields);
            self.0.lock().unwrap().push(fields.0);
        }
    }

    #[derive(Default)]
    struct CapturedFields(String);

    impl Visit for CapturedFields {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            use std::fmt::Write as _;
            let _ = write!(self.0, "{}={value:?} ", field.name());
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            use std::fmt::Write as _;
            let _ = write!(self.0, "{}={value} ", field.name());
        }
    }

    #[tokio::test]
    async fn incomplete_http_headers_report_a_timeout() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        let (server, _) = listener.accept().await.unwrap();
        let permit = Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap();
        let mut server = LimitedStream::new(server, permit, Duration::from_millis(40));
        client
            .write_all(b"GET /healthz HTTP/1.1\r\nHost: ")
            .await
            .unwrap();
        let mut byte = [0; 1];
        let error = loop {
            match server.read(&mut byte).await {
                Ok(0) => panic!("client closed before the HTTP header deadline"),
                Ok(_) => continue,
                Err(error) => break error,
            }
        };
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[tokio::test]
    async fn slow_websocket_write_is_bounded_by_the_write_deadline() {
        let result = write_with_timeout(
            std::future::pending::<Result<(), axum::Error>>(),
            Duration::from_millis(10),
        )
        .await;
        assert!(result.is_err(), "a stalled write must time out");
    }

    const CHARACTER_TOKEN: &str = "character-api-secret";

    #[test]
    fn resume_denial_audit_uses_only_ip_and_stable_reason() {
        let events = Arc::new(StdMutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(CaptureEvents(Arc::clone(&events)));
        let error = EngineError::new(ErrorCode::SessionExpired, "resume rejected")
            .with_detail("credential", "must-not-be-logged");
        tracing::subscriber::with_default(subscriber, || {
            audit_resume_admission_denied("192.0.2.4".parse().unwrap(), &error);
        });
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].contains("world_resume_admission_denied"));
        assert!(events[0].contains("192.0.2.4"));
        assert!(events[0].contains("session_expired"));
        assert!(!events[0].contains("must-not-be-logged"));
    }

    #[test]
    fn failed_credential_audit_is_bounded_and_rate_limit_is_logged() {
        let (server_events, _receiver) = event_channel();
        let state = AppState::new(
            server_events,
            Authenticator::new(&[crate::auth::digest("unused")]).unwrap(),
        );
        let events = Arc::new(StdMutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(CaptureEvents(Arc::clone(&events)));
        let now = Instant::now();
        tracing::subscriber::with_default(subscriber, || {
            for _ in 0..6 {
                let _ = failed_auth_at(&state, "192.0.2.9".parse().unwrap(), now);
            }
        });
        let events = events.lock().unwrap();
        let credential_denials = events
            .iter()
            .filter(|event| event.contains("world_admission_denied"))
            .count();
        let rate_limit_events = events
            .iter()
            .filter(|event| event.contains("world_rate_limit_rejected"))
            .count();
        assert_eq!(credential_denials, 5, "the bucket allows five attempts");
        assert_eq!(rate_limit_events, 1, "the sixth attempt is rate-limited");
        assert!(events[0].contains("world_admission_denied"));
        assert!(events[0].contains("credential_rejected"));
        assert!(events[0].contains("192.0.2.9"));
    }

    #[test]
    fn security_audit_paths_redact_credentials_and_backend_details() {
        const SECRET: &str = "audit-redaction-sentinel";

        let (server_events, _receiver) = event_channel();
        let state = AppState::new(
            server_events,
            Authenticator::new(&[crate::auth::digest("unused")]).unwrap(),
        );
        let events = Arc::new(StdMutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(CaptureEvents(Arc::clone(&events)));
        let ip = "192.0.2.23".parse().unwrap();
        let character = StableId::from_parts(IdKind::Character, 1, 23);
        let detail_error = EngineError::new(ErrorCode::SessionExpired, "private backend message")
            .with_detail("credential", SECRET)
            .with_detail("provider_response", SECRET);

        tracing::subscriber::with_default(subscriber, || {
            let _ = failed_auth_at(&state, ip, Instant::now());
            audit_resume_admission_denied(ip, &detail_error);
            identity_rejected(
                &state,
                ip,
                IdentityError::CharacterForbidden,
                Some(character),
            );
            let rate_limit_error = EngineError::new(ErrorCode::RateLimited, "private rate detail")
                .with_detail("credential", SECRET);
            let _ = rate_limit_end(
                ip,
                &engine_net::replication::SessionId::new("public-session").unwrap(),
                "message",
                rate_limit_error,
            );
        });

        let events = events.lock().unwrap();
        let rendered = events.join("\n");
        assert!(rendered.contains("world_admission_denied"));
        assert!(rendered.contains("world_resume_admission_denied"));
        assert!(rendered.contains("world_rate_limit_rejected"));
        assert!(rendered.contains("character_forbidden"));
        assert!(!rendered.contains(SECRET));
        assert!(!rendered.contains("private backend message"));
        assert!(!rendered.contains("private rate detail"));
    }

    #[test]
    fn authority_rejection_audit_is_correlated_and_redacted() {
        const SECRET: &str = "authority-rejection-private-detail";
        let actor = StableId::from_parts(IdKind::Character, 8, 41);
        let session = engine_net::replication::SessionId::new("audit-session-41").unwrap();
        let envelope = Envelope::new(
            engine_net::MessageId::parse("request-41").unwrap(),
            engine_net::MessageKind::parse(ERROR_KIND).unwrap(),
            serde_json::json!({
                "code": "permission_denied",
                "message": SECRET,
                "details": { "credential": SECRET },
            }),
        )
        .with_correlation(engine_net::MessageId::parse("request-41").unwrap());

        let events = Arc::new(StdMutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(CaptureEvents(Arc::clone(&events)));
        tracing::subscriber::with_default(subscriber, || {
            audit_outbound_rejection("192.0.2.41".parse().unwrap(), &session, actor, &envelope);
        });

        let rendered = events.lock().unwrap().join("\n");
        assert!(rendered.contains("world_authority_rejected"));
        assert!(rendered.contains("192.0.2.41"));
        assert!(rendered.contains("audit-session-41"));
        assert!(rendered.contains(&actor.to_string()));
        assert!(rendered.contains("request-41"));
        assert!(rendered.contains("permission_denied"));
        assert!(!rendered.contains(SECRET));
    }

    #[tokio::test]
    async fn slow_peer_without_session_hello_is_closed_before_admission() {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message as ClientMessage;
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::http::{HeaderValue, header};

        const TOKEN: &str = "slow-handshake-test-secret";

        let (events, mut receiver) = event_channel();
        let mut app_state = AppState::new(
            events,
            Authenticator::new(&[crate::auth::digest(TOKEN)]).unwrap(),
        );
        assert_eq!(app_state.hello_timeout, HELLO_TIMEOUT);
        app_state.hello_timeout = Duration::from_millis(25);
        let metrics = Arc::clone(&app_state.metrics);
        let state = Arc::new(app_state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown, shutdown_rx) = oneshot::channel();
        let server = tokio::spawn(serve_with_shutdown(listener, state, async move {
            let _ = shutdown_rx.await;
        }));

        let mut request = format!("ws://{address}/world")
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {TOKEN}")).unwrap(),
        );
        let (mut socket, _) = tokio::time::timeout(
            Duration::from_secs(2),
            tokio_tungstenite::connect_async(request),
        )
        .await
        .unwrap()
        .unwrap();

        let response_message = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let ClientMessage::Binary(frame) = response_message else {
            panic!("slow handshake should receive a protocol error before close");
        };
        let error = decode_frame(frame.as_ref()).unwrap();
        assert_eq!(error.kind.as_str(), ERROR_KIND);
        assert_eq!(error.body["code"], "invalid_request");
        assert!(
            !serde_json::to_string(&error.body).unwrap().contains(TOKEN),
            "slow-handshake errors must not disclose the bearer credential"
        );
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            ClientMessage::Close(_)
        ));
        assert!(receiver.try_recv().is_err());
        assert!(
            metrics
                .render()
                .contains("lynx_world_session_attaches_total 0\n")
        );

        let _ = shutdown.send(());
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[test]
    fn outbound_queue_capacity_defaults_and_follows_configuration() {
        let authenticator = || Authenticator::new(&[crate::auth::digest("unused")]).unwrap();
        let (events, _receiver) = event_channel();
        let default = AppState::new(events, authenticator());
        assert_eq!(
            default.outbound_queue,
            crate::config::DEFAULT_OUTBOUND_QUEUE
        );

        let (events, _receiver) = event_channel();
        let configured = AppState::new(events, authenticator())
            .with_outbound_queue(crate::config::MAX_OUTBOUND_QUEUE);
        assert_eq!(configured.outbound_queue, crate::config::MAX_OUTBOUND_QUEUE);
    }

    #[tokio::test]
    async fn trusted_revocation_control_uses_authority_event_channel() {
        let (events, mut receiver) = event_channel();
        let state = Arc::new(AppState::new(
            events,
            Authenticator::new(&[crate::auth::digest("unused")]).unwrap(),
        ));
        let principal = StableId::from_parts(IdKind::Character, 1, 99);
        let request_state = Arc::clone(&state);
        let request =
            tokio::spawn(async move { request_state.revoke_principal_sessions(principal).await });
        let Some(ServerEvent::RevokePrincipal {
            principal: event_principal,
            reply,
        }) = receiver.recv().await
        else {
            panic!("expected trusted principal revocation event");
        };
        assert_eq!(event_principal, principal);
        reply.send(Ok(1)).unwrap();
        assert_eq!(request.await.unwrap().unwrap(), 1);
    }

    struct FakeCharacters;

    impl CharacterManagementBackend for FakeCharacters {
        fn characters(&self, token: &SessionToken) -> Result<Vec<Character>, ApiError> {
            if token.expose_secret() != CHARACTER_TOKEN {
                return Err(ApiError::Unauthenticated);
            }
            Ok(vec![fake_character()])
        }

        fn create_character(
            &self,
            token: &SessionToken,
            key: &CharacterCreationKey,
            request: &CreateCharacter,
        ) -> Result<CharacterCreationReceipt, ApiError> {
            if token.expose_secret() != CHARACTER_TOKEN {
                return Err(ApiError::Unauthenticated);
            }
            assert_eq!(key.as_str(), "retry-key-1");
            assert_eq!(request.name, "Hero");
            Ok(CharacterCreationReceipt {
                character_id: fake_character().id,
            })
        }

        fn select_character(
            &self,
            token: &SessionToken,
            character: CharacterId,
        ) -> Result<Character, ApiError> {
            if token.expose_secret() != CHARACTER_TOKEN {
                return Err(ApiError::Unauthenticated);
            }
            if character != fake_character().id {
                return Err(ApiError::NotFound("not disclosed".into()));
            }
            Ok(fake_character())
        }

        fn inventory(
            &self,
            token: &SessionToken,
            character: CharacterId,
        ) -> Result<Vec<game_api::InventorySlot>, ApiError> {
            if token.expose_secret() != CHARACTER_TOKEN {
                return Err(ApiError::Unauthenticated);
            }
            if character != fake_character().id {
                return Err(ApiError::NotFound("not disclosed".into()));
            }
            Ok(Vec::new())
        }
    }

    fn fake_character() -> Character {
        Character {
            id: CharacterId::new(StableId::from_parts(IdKind::Character, 1, 42)).unwrap(),
            account_id: AccountId::new("account-1").unwrap(),
            name: "Hero".into(),
            zone_id: None,
            status: CharacterStatus::Active,
            created_at: None,
        }
    }

    fn character_state() -> Arc<AppState> {
        let (events, _) = event_channel();
        Arc::new(
            AppState::new(
                events,
                Authenticator::new(&[crate::auth::digest("unused")]).unwrap(),
            )
            .with_character_backend(Arc::new(FakeCharacters)),
        )
    }

    fn bearer_headers(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        headers
    }

    async fn response_json(response: Response) -> (StatusCode, Value) {
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 16_384)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn character_list_requires_a_session_and_returns_backend_records() {
        let state = character_state();
        let (status, body) =
            response_json(list_characters(State(Arc::clone(&state)), HeaderMap::new()).await).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"], "unauthorized");

        let (status, body) =
            response_json(list_characters(State(state), bearer_headers(CHARACTER_TOKEN)).await)
                .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["characters"][0]["name"], "Hero");
        assert_eq!(body["characters"][0]["account_id"], "account-1");
    }

    #[tokio::test]
    async fn creation_is_idempotent_and_selection_only_returns_a_record() {
        let state = character_state();
        let request = || {
            Json(CreateCharacter {
                name: "Hero".into(),
            })
        };
        let (status, body) = response_json(
            create_character(
                State(Arc::clone(&state)),
                bearer_headers(CHARACTER_TOKEN),
                request(),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "idempotency_key_required");

        let mut headers = bearer_headers(CHARACTER_TOKEN);
        headers.insert("idempotency-key", "retry-key-1".parse().unwrap());
        let (status, body) =
            response_json(create_character(State(Arc::clone(&state)), headers, request()).await)
                .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["character_id"], fake_character().id.to_string());

        let (status, body) = response_json(
            select_character(
                State(state),
                bearer_headers(CHARACTER_TOKEN),
                Path(fake_character().id.to_string()),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "active");
        assert!(body.get("spawn").is_none());
    }

    #[tokio::test]
    async fn inventory_requires_authentication_and_hides_unowned_characters() {
        let state = character_state();
        let path = || Path(fake_character().id.to_string());
        let (status, body) = response_json(
            character_inventory(State(Arc::clone(&state)), HeaderMap::new(), path()).await,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"], "unauthorized");

        let other = StableId::from_parts(IdKind::Character, 1, 43).to_string();
        let (status, body) = response_json(
            character_inventory(
                State(Arc::clone(&state)),
                bearer_headers(CHARACTER_TOKEN),
                Path(other),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "character_not_found");

        let (status, body) = response_json(
            character_inventory(State(state), bearer_headers(CHARACTER_TOKEN), path()).await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["inventory"], json!([]));
    }

    #[tokio::test]
    async fn foreign_selection_is_non_disclosing() {
        let other = StableId::from_parts(IdKind::Character, 1, 43).to_string();
        let (status, body) = response_json(
            select_character(
                State(character_state()),
                bearer_headers(CHARACTER_TOKEN),
                Path(other),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "character_not_found");
        assert!(!body.to_string().contains("not disclosed"));
        assert!(!body.to_string().contains(CHARACTER_TOKEN));
    }

    #[tokio::test]
    async fn character_api_error_mapping_matches_catalog_and_redacts_backend_text() {
        const PRIVATE: &str = "backend-private-error-sentinel";
        let cases = [
            (
                ApiError::Unauthenticated,
                StatusCode::UNAUTHORIZED,
                "unauthorized",
            ),
            (
                ApiError::SessionExpired,
                StatusCode::UNAUTHORIZED,
                "unauthorized",
            ),
            (
                ApiError::NotFound(PRIVATE.into()),
                StatusCode::NOT_FOUND,
                "character_not_found",
            ),
            (
                ApiError::PermissionDenied,
                StatusCode::NOT_FOUND,
                "character_not_found",
            ),
            (
                ApiError::InvalidRequest(PRIVATE.into()),
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
            (
                ApiError::Conflict(PRIVATE.into()),
                StatusCode::CONFLICT,
                "conflict",
            ),
            (
                ApiError::RateLimited,
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
            ),
            (
                ApiError::Unavailable(PRIVATE.into()),
                StatusCode::SERVICE_UNAVAILABLE,
                "character_service_unavailable",
            ),
            (
                ApiError::Transport(PRIVATE.into()),
                StatusCode::SERVICE_UNAVAILABLE,
                "character_service_unavailable",
            ),
            (
                ApiError::Backend {
                    status: 500,
                    code: Some(17),
                    message: PRIVATE.into(),
                },
                StatusCode::SERVICE_UNAVAILABLE,
                "character_service_unavailable",
            ),
        ];

        for (error, expected_status, expected_code) in cases {
            let (status, body) = response_json(character_api_error(&error)).await;
            assert_eq!(status, expected_status);
            assert_eq!(body, json!({"error": expected_code}));
            assert!(!body.to_string().contains(PRIVATE));
        }
    }

    #[test]
    fn capacity_is_bounded_released_and_draining_is_sticky() {
        let capacity = Arc::new(Mutex::new(Capacity::default()));
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        let mut permits = Vec::new();
        for _ in 0..MAX_CONNECTIONS_PER_IP {
            permits.push(acquire(&capacity, ip).unwrap());
        }
        assert_eq!(acquire(&capacity, ip).unwrap_err(), "ip_capacity");
        drop(permits.pop());
        permits.push(acquire(&capacity, ip).unwrap());
        drop(permits);
        assert_eq!(capacity.lock().unwrap().total, 0);
        assert!(capacity.lock().unwrap().ips.is_empty());
        let (events, _) = event_channel();
        let state = AppState::new(
            events,
            Authenticator::new(&[crate::auth::digest("test")]).unwrap(),
        );
        state.set_ready(false);
        state.set_ready(true);
        assert_eq!(acquire(&state.capacity, ip).unwrap_err(), "draining");
        assert!(!state.ready.load(Ordering::Relaxed));
    }

    #[test]
    fn total_capacity_is_independent_of_per_ip() {
        let capacity = Arc::new(Mutex::new(Capacity::default()));
        let permits: Vec<_> = (0..MAX_CONNECTIONS)
            .map(|n| {
                acquire(
                    &capacity,
                    IpAddr::from([
                        10,
                        0,
                        u8::try_from(n / 256).unwrap(),
                        u8::try_from(n % 256).unwrap(),
                    ]),
                )
                .unwrap()
            })
            .collect();
        assert_eq!(
            acquire(&capacity, "192.0.2.1".parse().unwrap()).unwrap_err(),
            "total_capacity"
        );
        drop(permits);
        assert_eq!(capacity.lock().unwrap().total, 0);
    }

    #[tokio::test]
    async fn admin_listener_scrapes_without_gameplay_route() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (events, _receiver) = event_channel();
        let state = Arc::new(AppState::new(
            events,
            Authenticator::new(&[crate::auth::digest("test-secret")]).unwrap(),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task =
            tokio::spawn(async move { axum::serve(listener, admin_router(state)).await.unwrap() });
        for (path, status) in [
            ("/metrics", "200 OK"),
            ("/readyz", "200 OK"),
            ("/world", "404 Not Found"),
        ] {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            stream
                .write_all(
                    format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                        .as_bytes(),
                )
                .await
                .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
            assert!(response.contains(status), "{response}");
            if path == "/metrics" {
                assert!(response.contains("# TYPE lynx_world_ticks_total counter"));
                assert!(response.contains("lynx_world_content_version_info{state=\"absent\"} 1"));
            }
        }
        task.abort();
    }

    #[test]
    fn transport_encoding_preserves_samples_and_unique_metadata() {
        use crate::rate_limit::RejectionMetric;
        let rows = [RejectionMetric {
            kind: "movement",
            code: "rate_limited",
            count: 7,
        }];
        let text = encode_transport_metrics([2, 3, 4], &rows);
        for name in [
            "lynx_world_admission_rejections_total",
            "lynx_world_rate_limit_rejections_total",
        ] {
            assert_eq!(text.matches(&format!("# HELP {name} ")).count(), 1);
            assert_eq!(text.matches(&format!("# TYPE {name} counter\n")).count(), 1);
        }
        assert!(text.contains("lynx_world_admission_rejections_total{reason=\"draining\"} 2\n"));
        assert!(
            text.contains("lynx_world_admission_rejections_total{reason=\"total_capacity\"} 3\n")
        );
        assert!(text.contains("lynx_world_admission_rejections_total{reason=\"ip_capacity\"} 4\n"));
        assert!(text.contains(
            "lynx_world_rate_limit_rejections_total{class=\"movement\",code=\"rate_limited\"} 7\n"
        ));
        assert!(!text.contains("# EOF"));
    }

    #[test]
    fn backend_metrics_use_only_fixed_labels_and_cumulative_buckets() {
        let snapshot = BackendTelemetrySnapshot {
            nakama: backend_nakama::TelemetrySnapshot {
                count: 2,
                failures: 1,
                duration_seconds: 0.011,
                duration_buckets: [1, 2, 2, 2, 2, 2],
            },
            ..BackendTelemetrySnapshot::default()
        };
        let text = encode_backend_metrics(&snapshot);
        assert!(text.contains("lynx_backend_operations_total{adapter=\"nakama\"} 2\n"));
        assert!(text.contains("lynx_backend_operation_errors_total{adapter=\"nakama\"} 1\n"));
        assert!(text.contains(
            "lynx_backend_operation_duration_seconds_bucket{adapter=\"nakama\",le=\"+Inf\"} 2\n"
        ));
        assert!(text.contains(
            "lynx_backend_operation_duration_seconds_sum{adapter=\"nakama\"} 0.011000000\n"
        ));
        assert!(text.contains("adapter=\"identity_store\""));
        assert!(text.contains("adapter=\"character_store\""));
        assert!(!text.contains("account"));
        assert!(!text.contains("token"));
    }

    #[test]
    fn content_metrics_distinguish_absent_from_an_actual_version() {
        let absent = encode_content_version(&ContentVersionSnapshot::absent());
        assert!(absent.contains("lynx_world_content_version_info{state=\"absent\"} 1"));

        let manifest = engine_content::ContentManifest::new(
            semver::Version::parse("1.2.3+build.4").unwrap(),
            semver::VersionReq::STAR,
        );
        let loaded = ContentVersionSnapshot::from_loaded_manifest(&manifest).unwrap();
        let text = encode_content_version(&loaded);
        assert!(text.contains(
            "lynx_world_content_version_info{state=\"loaded\",version=\"1.2.3+build.4\"} 1"
        ));
    }

    #[tokio::test]
    async fn gameplay_and_admin_metrics_match_over_http() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (events, _receiver) = event_channel();
        let state = Arc::new(AppState::new(
            events,
            Authenticator::new(&[crate::auth::digest("test-secret")]).unwrap(),
        ));
        let mut responses = Vec::new();
        for app in [router(Arc::clone(&state)), admin_router(Arc::clone(&state))] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            stream
                .write_all(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
            assert!(response.contains("200 OK"));
            assert!(
                response
                    .to_ascii_lowercase()
                    .contains("content-type: text/plain; version=0.0.4; charset=utf-8")
            );
            responses.push(response.split_once("\r\n\r\n").unwrap().1.to_owned());
            task.abort();
        }
        let mut left: Vec<_> = responses[0].lines().collect();
        let mut right: Vec<_> = responses[1].lines().collect();
        left.sort_unstable();
        right.sort_unstable();
        assert_eq!(left, right);
        assert!(responses[0].contains("# TYPE lynx_world_admission_rejections_total counter\n"));
    }

    #[tokio::test]
    async fn admin_metrics_are_prometheus_text_and_redacted() {
        let (events, _receiver) = event_channel();
        let state = Arc::new(AppState::new(
            events,
            Authenticator::new(&[crate::auth::digest("secret-token")]).unwrap(),
        ));
        let response = metrics(State(Arc::clone(&state))).await.into_response();
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/plain; version=0.0.4; charset=utf-8"
        );
        let body = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        assert!(text.ends_with('\n'));
        assert_eq!(
            text.matches("# TYPE lynx_world_connections gauge\n")
                .count(),
            1,
            "a metric family must have one TYPE declaration"
        );
        assert!(text.contains(
            "# HELP lynx_world_admission_rejections_total World-upgrade admission rejections by bounded reason.\n# TYPE lynx_world_admission_rejections_total counter\n"
        ));
        for line in text.lines().filter(|line| !line.starts_with('#')) {
            let (name, value) = line.split_once(' ').unwrap();
            assert!(name.starts_with("lynx_world_"));
            assert!(value.parse::<f64>().is_ok());
        }
        assert!(!text.contains("secret-token"));
        assert_eq!(readyz(State(state)).await.0, StatusCode::OK);
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
    fn request_headers_are_bounded_by_aggregate_wire_size() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-test",
            "a".repeat(MAX_REQUEST_HEADER_BYTES - 11).parse().unwrap(),
        );
        assert!(request_header_bytes(&headers) <= MAX_REQUEST_HEADER_BYTES);
        headers.insert(
            "x-test",
            "a".repeat(MAX_REQUEST_HEADER_BYTES).parse().unwrap(),
        );
        assert!(request_header_bytes(&headers) > MAX_REQUEST_HEADER_BYTES);
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
