//! Blocking HTTP client for Nakama's client API.

use std::fmt;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use engine_core::Locale;
use game_api::{
    Account, AccountId, ApiError, AuthService, DeviceLogin, Friend, FriendState, HealthService,
    HealthStatus, Session, SessionToken, SocialService,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use ureq::http::{Response, StatusCode};

/// Nakama HTTP connection settings.
#[derive(Clone)]
pub struct NakamaConfig {
    /// Base URL, such as `http://127.0.0.1:7350`.
    pub base_url: String,
    /// Nakama's HTTP key, used as HTTP Basic authentication for public APIs.
    pub http_key: Option<String>,
    /// End-to-end timeout, including response-body reads.
    pub timeout: Duration,
}

impl NakamaConfig {
    /// Create settings for a Nakama server.
    #[must_use]
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            http_key: None,
            timeout: Duration::from_secs(10),
        }
    }
}

impl fmt::Debug for NakamaConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NakamaConfig")
            .field("base_url", &"[REDACTED]")
            .field("http_key", &self.http_key.as_ref().map(|_| "[REDACTED]"))
            .field("timeout", &self.timeout)
            .finish()
    }
}

/// Nakama adapter implementing the shared health and authentication contracts.
pub struct NakamaClient {
    telemetry: crate::Telemetry,
    agent: ureq::Agent,
    base_url: String,
    http_key: Option<String>,
}

impl fmt::Debug for NakamaClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NakamaClient")
            .field("base_url", &"[REDACTED]")
            .field("http_key", &self.http_key.as_ref().map(|_| "[REDACTED]"))
            .finish_non_exhaustive()
    }
}

impl NakamaClient {
    /// Build a client after validating its URL, key, and timeout.
    pub fn new(config: NakamaConfig) -> Result<Self, ApiError> {
        let base_url = config.base_url.trim_end_matches('/');
        let uri: ureq::http::Uri = base_url
            .parse()
            .map_err(|_| ApiError::InvalidRequest("invalid Nakama base URL".into()))?;
        let scheme = uri.scheme_str();
        let authority = uri.authority().map(ureq::http::uri::Authority::as_str);
        if !matches!(scheme, Some("http" | "https"))
            || authority.is_none_or(|value| value.is_empty() || value.contains('@'))
            || uri.query().is_some()
            || config.timeout.is_zero()
        {
            return Err(ApiError::InvalidRequest(
                "Nakama URL must use HTTP(S) without credentials or query parameters, and timeout must be positive"
                    .into(),
            ));
        }
        if config
            .http_key
            .as_ref()
            .is_some_and(|key| key.is_empty() || key.chars().any(char::is_control))
        {
            return Err(ApiError::InvalidRequest("invalid Nakama HTTP key".into()));
        }

        let agent_config = ureq::Agent::config_builder()
            .timeout_global(Some(config.timeout))
            .http_status_as_error(false)
            .max_redirects(0)
            .build();

        Ok(Self {
            telemetry: crate::Telemetry::default(),
            agent: ureq::Agent::new_with_config(agent_config),
            base_url: base_url.to_owned(),
            http_key: config.http_key,
        })
    }

    /// Return a shared, process-local telemetry handle for this adapter.
    #[must_use]
    pub fn telemetry(&self) -> crate::Telemetry {
        self.telemetry.clone()
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    fn basic_auth(&self) -> Option<String> {
        self.http_key
            .as_ref()
            .map(|key| format!("Basic {}", BASE64.encode(format!("{key}:"))))
    }

    fn checked_body(mut response: Response<ureq::Body>) -> Result<String, ApiError> {
        let status = response.status();
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|_| ApiError::Transport("failed to read Nakama response".into()))?;
        if !status.is_success() {
            return Err(status_error(status, &body));
        }
        Ok(body)
    }

    fn decode_json<T: DeserializeOwned>(response: Response<ureq::Body>) -> Result<T, ApiError> {
        let body = Self::checked_body(response)?;
        serde_json::from_str(&body)
            .map_err(|_| ApiError::Transport("Nakama returned an invalid JSON response".into()))
    }

    fn send_json(
        request: ureq::RequestBuilder<ureq::typestate::WithBody>,
        body: &Value,
    ) -> Result<Response<ureq::Body>, ApiError> {
        let serialized = serde_json::to_vec(body)
            .map_err(|_| ApiError::InvalidRequest("could not encode Nakama request".into()))?;
        request
            .header("Content-Type", "application/json")
            .send(serialized)
            .map_err(|_| ApiError::Transport("Nakama request failed".into()))
    }

    fn authorized_get(
        &self,
        path: &str,
        token: &SessionToken,
    ) -> Result<Response<ureq::Body>, ApiError> {
        self.agent
            .get(self.endpoint(path))
            .header("Authorization", format!("Bearer {}", token.expose_secret()))
            .call()
            .map_err(|_| ApiError::Transport("Nakama request failed".into()))
    }

    fn parse_session(&self, response: Response<ureq::Body>) -> Result<Session, ApiError> {
        let response: NakamaSession = Self::decode_json(response)?;
        let token = SessionToken::new(response.token)
            .map_err(|_| ApiError::Transport("Nakama returned an invalid session token".into()))?;
        let refresh_token = SessionToken::new(response.refresh_token)
            .map_err(|_| ApiError::Transport("Nakama returned an invalid refresh token".into()))?;
        let account_id = self.account(&token)?.id;
        Ok(Session {
            account_id,
            token,
            refresh_token,
            created: response.created,
            expires_at: None,
        })
    }
}

impl HealthService for NakamaClient {
    fn health(&self) -> Result<HealthStatus, ApiError> {
        self.telemetry.measure(|| {
            let response = self
                .agent
                .get(self.endpoint("/healthcheck"))
                .call()
                .map_err(|_| ApiError::Unavailable("Nakama health check failed".into()))?;
            Self::checked_body(response)?;
            Ok(HealthStatus {
                healthy: true,
                message: None,
            })
        })
    }
}

impl SocialService for NakamaClient {
    fn friends(&self, token: &SessionToken) -> Result<Vec<Friend>, ApiError> {
        self.telemetry.measure(|| {
            const PAGE_SIZE: &str = "1000";
            const PAGE_LIMIT: usize = 100;
            let mut cursor: Option<String> = None;
            let mut friends = std::collections::BTreeMap::new();

            for _ in 0..PAGE_LIMIT {
                let mut request = self
                    .agent
                    .get(self.endpoint("/v2/friend"))
                    .header("Authorization", format!("Bearer {}", token.expose_secret()))
                    .query("limit", PAGE_SIZE);
                if let Some(value) = cursor.as_deref() {
                    request = request.query("cursor", value);
                }
                let page: NakamaFriendList = Self::decode_json(
                    request
                        .call()
                        .map_err(|_| ApiError::Transport("Nakama request failed".into()))?,
                )?;
                for relation in page.friends {
                    let account_id = AccountId::new(relation.user.id).map_err(|_| {
                        ApiError::Transport("Nakama returned an invalid friend account ID".into())
                    })?;
                    let state = match relation.state {
                        0 => FriendState::Friend,
                        1 => FriendState::OutgoingRequest,
                        2 => FriendState::IncomingRequest,
                        3 => FriendState::Blocked,
                        _ => {
                            return Err(ApiError::Transport(
                                "Nakama returned an unknown friend state".into(),
                            ));
                        }
                    };
                    friends.insert(
                        account_id.clone(),
                        Friend {
                            account_id,
                            username: relation.user.username,
                            display_name: relation.user.display_name,
                            state,
                        },
                    );
                }
                cursor = page.cursor.filter(|value| !value.is_empty());
                if cursor.is_none() {
                    return Ok(friends.into_values().collect());
                }
                if cursor.as_ref().is_some_and(|value| value.len() > 4096) {
                    return Err(ApiError::Transport(
                        "Nakama returned an invalid friend cursor".into(),
                    ));
                }
            }

            Err(ApiError::Transport(
                "Nakama friend list exceeded the bounded page limit".into(),
            ))
        })
    }

    fn request_friend(&self, token: &SessionToken, friend: &AccountId) -> Result<(), ApiError> {
        self.change_friend(token, "/v2/friend", friend, true)
    }

    fn accept_friend_request(
        &self,
        token: &SessionToken,
        friend: &AccountId,
    ) -> Result<(), ApiError> {
        // Nakama accepts an incoming request when the recipient adds the sender.
        self.change_friend(token, "/v2/friend", friend, true)
    }

    fn dismiss_friend_request(
        &self,
        token: &SessionToken,
        friend: &AccountId,
    ) -> Result<(), ApiError> {
        self.change_friend(token, "/v2/friend", friend, false)
    }

    fn remove_friend(&self, token: &SessionToken, friend: &AccountId) -> Result<(), ApiError> {
        self.change_friend(token, "/v2/friend", friend, false)
    }

    fn block_account(&self, token: &SessionToken, account: &AccountId) -> Result<(), ApiError> {
        self.change_friend(token, "/v2/friend/block", account, true)
    }

    fn unblock_account(&self, token: &SessionToken, account: &AccountId) -> Result<(), ApiError> {
        self.change_friend(token, "/v2/friend", account, false)
    }
}

impl NakamaClient {
    fn change_friend(
        &self,
        token: &SessionToken,
        path: &str,
        account: &AccountId,
        add: bool,
    ) -> Result<(), ApiError> {
        self.telemetry.measure(|| {
            let url = self.endpoint(path);
            let response = if add {
                self.agent
                    .post(url)
                    .header("Authorization", format!("Bearer {}", token.expose_secret()))
                    .query("ids", account.as_str())
                    .send_empty()
            } else {
                self.agent
                    .delete(url)
                    .header("Authorization", format!("Bearer {}", token.expose_secret()))
                    .query("ids", account.as_str())
                    .call()
            }
            .map_err(|_| ApiError::Transport("Nakama request failed".into()))?;
            Self::checked_body(response)?;
            Ok(())
        })
    }
}

impl AuthService for NakamaClient {
    fn authenticate_device(&self, request: &DeviceLogin) -> Result<Session, ApiError> {
        self.telemetry.measure(|| {
            request.validate()?;
            let mut builder = self
                .agent
                .post(self.endpoint("/v2/account/authenticate/device"))
                .query("create", request.create.to_string());
            if let Some(username) = &request.username {
                builder = builder.query("username", username);
            }
            if let Some(header) = self.basic_auth() {
                builder = builder.header("Authorization", header);
            }
            let response = Self::send_json(
                builder,
                &json!({
                    "id": request.device_id,
                    "vars": { "locale": request.locale.as_str() },
                }),
            )?;
            self.parse_session(response)
        })
    }

    fn refresh_session(&self, refresh_token: &SessionToken) -> Result<Session, ApiError> {
        self.telemetry.measure(|| {
            let request = self
                .agent
                .post(self.endpoint("/v2/account/session/refresh"))
                .header(
                    "Authorization",
                    format!("Bearer {}", refresh_token.expose_secret()),
                );
            let response = Self::send_json(request, &json!({}))?;
            self.parse_session(response)
        })
    }

    fn account(&self, token: &SessionToken) -> Result<Account, ApiError> {
        self.telemetry.measure(|| {
            let response = self.authorized_get("/v2/account", token)?;
            let response: NakamaAccountResponse = Self::decode_json(response)?;
            let locale = response
                .user
                .lang_tag
                .as_deref()
                .filter(|tag| !tag.is_empty())
                .map(Locale::parse)
                .transpose()
                .map_err(|_| ApiError::Transport("Nakama returned an invalid locale".into()))?
                .unwrap_or_default();
            Ok(Account {
                id: AccountId::new(response.user.id).map_err(|_| {
                    ApiError::Transport("Nakama returned an invalid account ID".into())
                })?,
                username: response.user.username,
                display_name: response.user.display_name,
                locale,
                created_at: None,
                avatar_url: response.user.avatar_url,
            })
        })
    }

    fn logout(&self, token: &SessionToken) -> Result<(), ApiError> {
        self.telemetry.measure(|| {
            let request = self
                .agent
                .post(self.endpoint("/v2/account/session/logout"))
                .header("Authorization", format!("Bearer {}", token.expose_secret()));
            let response = Self::send_json(request, &json!({}))?;
            Self::checked_body(response)?;
            Ok(())
        })
    }
}

#[derive(Deserialize)]
struct NakamaSession {
    token: String,
    refresh_token: String,
    #[serde(default)]
    created: bool,
}

#[derive(Deserialize)]
struct NakamaAccountResponse {
    user: NakamaUser,
}

#[derive(Deserialize)]
struct NakamaUser {
    id: String,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    avatar_url: Option<String>,
    #[serde(default)]
    lang_tag: Option<String>,
}

#[derive(Deserialize)]
struct NakamaFriendList {
    #[serde(default)]
    friends: Vec<NakamaFriend>,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Deserialize)]
struct NakamaFriend {
    user: NakamaUser,
    state: i32,
}

#[derive(Deserialize)]
struct NakamaErrorResponse {
    #[serde(default)]
    code: Option<i64>,
}

fn status_error(status: StatusCode, body: &str) -> ApiError {
    let parsed: Option<NakamaErrorResponse> = serde_json::from_str(body).ok();
    let code = parsed.as_ref().and_then(|response| response.code);
    // Never propagate Nakama's response text. Provider errors can echo request
    // data, including bearer credentials; the HTTP status and numeric code are
    // sufficient for stable categorization and bounded diagnostics.
    let message = match status.as_u16() {
        400 | 422 => "Nakama rejected the request".to_owned(),
        404 => "Nakama resource not found".to_owned(),
        409 => "Nakama reported a conflict".to_owned(),
        _ => format!("Nakama returned HTTP {}", status.as_u16()),
    };
    match status.as_u16() {
        401 => ApiError::Unauthenticated,
        403 => ApiError::PermissionDenied,
        404 => ApiError::NotFound(message),
        400 | 422 => ApiError::InvalidRequest(message),
        409 => ApiError::Conflict(message),
        429 => ApiError::RateLimited,
        _ => ApiError::Backend {
            status: status.as_u16(),
            code,
            message,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_rejects_invalid_urls_and_redacts_keys() {
        assert!(NakamaClient::new(NakamaConfig::new("file:///tmp/nakama")).is_err());
        let mut config = NakamaConfig::new("http://127.0.0.1:7350/");
        config.http_key = Some("private-key".into());
        let debug = format!("{config:?}");
        assert!(!debug.contains("private-key"));
        assert!(debug.contains("[REDACTED]"));
        assert!(NakamaClient::new(config).is_ok());
        let config = NakamaConfig::new("http://user:secret@localhost/private-token?token=secret");
        assert!(!format!("{config:?}").contains("secret"));
    }

    #[test]
    fn health_records_success_and_http_failure() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            for status in ["200 OK", "503 Service Unavailable"] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 1024];
                assert!(stream.read(&mut request).unwrap() > 0);
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
            }
        });
        let client = NakamaClient::new(NakamaConfig::new(format!("http://{address}"))).unwrap();
        let handle = client.telemetry();
        assert!(client.health().is_ok());
        assert!(client.health().is_err());
        worker.join().unwrap();
        let snapshot = handle.snapshot();
        assert_eq!(snapshot.count, 2);
        assert_eq!(snapshot.failures, 1);
        assert_eq!(snapshot.duration_buckets[5], 2);
    }

    #[test]
    fn social_contract_preserves_nakama_states_and_uses_authenticated_routes() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let expected = [
                ("GET", "/v2/friend?limit=1000"),
                ("GET", "/v2/friend?limit=1000&cursor=next-page"),
                ("POST", "/v2/friend?ids=outgoing"),
                ("POST", "/v2/friend?ids=incoming"),
                ("DELETE", "/v2/friend?ids=incoming"),
                ("POST", "/v2/friend/block?ids=blocked"),
                ("DELETE", "/v2/friend?ids=blocked"),
            ];
            for (index, (method, path)) in expected.into_iter().enumerate() {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut request_line = String::new();
                reader.read_line(&mut request_line).unwrap();
                let mut authorization = String::new();
                let mut content_length = 0;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header == "\r\n" || header.is_empty() {
                        break;
                    }
                    let lower = header.to_ascii_lowercase();
                    if lower.starts_with("authorization:") {
                        authorization = header.trim().to_owned();
                    }
                    if let Some(value) = lower.strip_prefix("content-length:") {
                        content_length = value.trim().parse::<usize>().unwrap();
                    }
                }
                let mut body = vec![0; content_length];
                reader.read_exact(&mut body).unwrap();
                assert_eq!(request_line.trim(), format!("{method} {path} HTTP/1.1"));
                assert_eq!(authorization, "authorization: Bearer test-token");
                let response_body = match index {
                    0 => {
                        r#"{"friends":[{"user":{"id":"friend","username":"friend-name"},"state":0}],"cursor":"next-page"}"#
                    }
                    1 => {
                        r#"{"friends":[{"user":{"id":"outgoing"},"state":1},{"user":{"id":"incoming"},"state":2},{"user":{"id":"blocked"},"state":3}]}"#
                    }
                    _ => "{}",
                };
                let mut stream = reader.into_inner();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                )
                .unwrap();
                stream.flush().unwrap();
            }
        });

        let client = NakamaClient::new(NakamaConfig::new(format!("http://{address}"))).unwrap();
        let token = SessionToken::new("test-token").unwrap();
        let friends = client.friends(&token).unwrap();
        assert_eq!(friends.len(), 4);
        assert_eq!(friends[0].state, FriendState::Blocked);
        assert_eq!(friends[1].state, FriendState::Friend);
        assert_eq!(friends[2].state, FriendState::IncomingRequest);
        assert_eq!(friends[3].state, FriendState::OutgoingRequest);

        let account = |value| AccountId::new(value).unwrap();
        client.request_friend(&token, &account("outgoing")).unwrap();
        client
            .accept_friend_request(&token, &account("incoming"))
            .unwrap();
        client
            .dismiss_friend_request(&token, &account("incoming"))
            .unwrap();
        client.block_account(&token, &account("blocked")).unwrap();
        client.unblock_account(&token, &account("blocked")).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn status_codes_map_to_shared_api_errors() {
        assert!(matches!(
            status_error(StatusCode::TOO_MANY_REQUESTS, ""),
            ApiError::RateLimited
        ));
        assert!(matches!(
            status_error(StatusCode::UNAUTHORIZED, ""),
            ApiError::Unauthenticated
        ));
        let error = status_error(
            StatusCode::SERVICE_UNAVAILABLE,
            r#"{"code":9,"message":"busy secret-bearer-token"}"#,
        );
        assert!(matches!(
            &error,
            ApiError::Backend {
                status: 503,
                code: Some(9),
                ..
            }
        ));
        assert!(!error.to_string().contains("secret-bearer-token"));
        assert!(error.is_retryable());
    }
}
