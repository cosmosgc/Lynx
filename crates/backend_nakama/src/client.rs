//! Blocking HTTP client for Nakama's client API.

use std::fmt;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use engine_core::Locale;
use game_api::{
    Account, AccountId, ApiError, AuthService, DeviceLogin, HealthService, HealthStatus, Session,
    SessionToken,
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
            .field("base_url", &self.base_url)
            .field("http_key", &self.http_key.as_ref().map(|_| "[REDACTED]"))
            .field("timeout", &self.timeout)
            .finish()
    }
}

/// Nakama adapter implementing the shared health and authentication contracts.
pub struct NakamaClient {
    agent: ureq::Agent,
    base_url: String,
    http_key: Option<String>,
}

impl fmt::Debug for NakamaClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NakamaClient")
            .field("base_url", &self.base_url)
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
            agent: ureq::Agent::new_with_config(agent_config),
            base_url: base_url.to_owned(),
            http_key: config.http_key,
        })
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
    }
}

impl AuthService for NakamaClient {
    fn authenticate_device(&self, request: &DeviceLogin) -> Result<Session, ApiError> {
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
    }

    fn refresh_session(&self, refresh_token: &SessionToken) -> Result<Session, ApiError> {
        let request = self
            .agent
            .post(self.endpoint("/v2/account/session/refresh"))
            .header(
                "Authorization",
                format!("Bearer {}", refresh_token.expose_secret()),
            );
        let response = Self::send_json(request, &json!({}))?;
        self.parse_session(response)
    }

    fn account(&self, token: &SessionToken) -> Result<Account, ApiError> {
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
            id: AccountId::new(response.user.id)
                .map_err(|_| ApiError::Transport("Nakama returned an invalid account ID".into()))?,
            username: response.user.username,
            display_name: response.user.display_name,
            locale,
            created_at: None,
            avatar_url: response.user.avatar_url,
        })
    }

    fn logout(&self, token: &SessionToken) -> Result<(), ApiError> {
        let request = self
            .agent
            .post(self.endpoint("/v2/account/session/logout"))
            .header("Authorization", format!("Bearer {}", token.expose_secret()));
        let response = Self::send_json(request, &json!({}))?;
        Self::checked_body(response)?;
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct NakamaSession {
    token: String,
    refresh_token: String,
    #[serde(default)]
    created: bool,
}

#[derive(Debug, Deserialize)]
struct NakamaAccountResponse {
    user: NakamaUser,
}

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Deserialize)]
struct NakamaErrorResponse {
    #[serde(default)]
    code: Option<i64>,
    #[serde(default)]
    message: Option<String>,
}

fn status_error(status: StatusCode, body: &str) -> ApiError {
    let parsed: Option<NakamaErrorResponse> = serde_json::from_str(body).ok();
    let code = parsed.as_ref().and_then(|response| response.code);
    let message = parsed
        .and_then(|response| response.message)
        .map(|message| {
            message
                .chars()
                .filter(|character| !character.is_control())
                .take(512)
                .collect::<String>()
        })
        .filter(|message| !message.is_empty())
        .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
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
            r#"{"code":9,"message":"busy"}"#,
        );
        assert!(matches!(
            error,
            ApiError::Backend {
                status: 503,
                code: Some(9),
                ..
            }
        ));
        assert!(error.is_retryable());
    }
}
