//! Shared backend contracts and authentication/world DTOs.

use std::fmt;

use engine_core::time::Millis;
use engine_core::{EngineError, ErrorCode, Locale};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors returned by backend adapters.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ApiError {
    #[error("backend unavailable: {0}")]
    Unavailable(String),
    #[error("transport failure: {0}")]
    Transport(String),
    #[error("authentication failed")]
    Unauthenticated,
    #[error("session expired")]
    SessionExpired,
    #[error("permission denied")]
    PermissionDenied,
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("rate limited")]
    RateLimited,
    #[error("backend rejected the request with status {status}: {message}")]
    Backend {
        status: u16,
        code: Option<i64>,
        message: String,
    },
}

impl ApiError {
    /// Map adapter failures to the engine's stable error categories.
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        match self {
            Self::Unavailable(_) | Self::Transport(_) => ErrorCode::Internal,
            Self::Unauthenticated | Self::PermissionDenied => ErrorCode::PermissionDenied,
            Self::SessionExpired => ErrorCode::SessionExpired,
            Self::NotFound(_) => ErrorCode::NotFound,
            Self::InvalidRequest(_) => ErrorCode::InvalidRequest,
            Self::Conflict(_) => ErrorCode::Conflict,
            Self::RateLimited => ErrorCode::CapacityExceeded,
            Self::Backend { status, .. } => match status {
                400 | 422 => ErrorCode::InvalidRequest,
                401 | 403 => ErrorCode::PermissionDenied,
                404 => ErrorCode::NotFound,
                409 => ErrorCode::Conflict,
                429 => ErrorCode::CapacityExceeded,
                _ => ErrorCode::Internal,
            },
        }
    }

    /// Whether retrying the same operation later may succeed.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        match self {
            Self::Unavailable(_) | Self::Transport(_) | Self::RateLimited => true,
            Self::Backend { status, .. } => *status == 429 || *status >= 500,
            _ => false,
        }
    }

    /// Convert this adapter error to an engine-facing error.
    #[must_use]
    pub fn to_engine_error(&self) -> EngineError {
        let message = match self {
            Self::Unavailable(message)
            | Self::Transport(message)
            | Self::NotFound(message)
            | Self::InvalidRequest(message)
            | Self::Conflict(message)
            | Self::Backend { message, .. } => message.as_str(),
            Self::Unauthenticated => "authentication failed",
            Self::SessionExpired => "session expired",
            Self::PermissionDenied => "permission denied",
            Self::RateLimited => "backend rate limit exceeded",
        };
        EngineError::new(self.code(), message).with_retryable(self.is_retryable())
    }
}

/// Health result returned by a backend health check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthStatus {
    /// Whether the backend is accepting requests.
    pub healthy: bool,
    /// Optional non-sensitive status description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// A backend session token. Its secret is omitted from debug output.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct SessionToken(String);

impl SessionToken {
    /// Validate and wrap a backend-issued token.
    pub fn new(value: impl Into<String>) -> Result<Self, ApiError> {
        let value = value.into();
        if value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
            return Err(ApiError::InvalidRequest("invalid session token".into()));
        }
        Ok(Self(value))
    }

    /// Borrow the token for an authenticated backend request.
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionToken([REDACTED])")
    }
}

impl TryFrom<String> for SessionToken {
    type Error = ApiError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// Stable backend account identifier.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct AccountId(String);

impl AccountId {
    /// Validate and wrap an account identifier.
    pub fn new(value: impl Into<String>) -> Result<Self, ApiError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 128
            || value
                .chars()
                .any(|ch| ch.is_control() || ch.is_whitespace())
        {
            return Err(ApiError::InvalidRequest(
                "invalid account identifier".into(),
            ));
        }
        Ok(Self(value))
    }

    /// The identifier as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for AccountId {
    type Error = ApiError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// Credentials and options for device-based account authentication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceLogin {
    pub device_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default = "default_true")]
    pub create: bool,
    #[serde(default)]
    pub locale: Locale,
}

impl DeviceLogin {
    /// Validate fields before sending them to a backend.
    pub fn validate(&self) -> Result<(), ApiError> {
        if self.device_id.is_empty()
            || self.device_id.len() > 128
            || self.device_id.chars().any(char::is_control)
        {
            return Err(ApiError::InvalidRequest("invalid device identifier".into()));
        }
        if let Some(username) = &self.username
            && (username.len() < 3
                || username.len() > 32
                || !username
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
        {
            return Err(ApiError::InvalidRequest(
                "username must be 3-32 ASCII letters, digits, or underscores".into(),
            ));
        }
        Ok(())
    }
}

const fn default_true() -> bool {
    true
}

/// Result of a successful authentication or refresh.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub account_id: AccountId,
    pub token: SessionToken,
    pub refresh_token: SessionToken,
    #[serde(default)]
    pub created: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Millis>,
}

/// Account information safe to expose to game clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub id: AccountId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default)]
    pub locale: Locale,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
}

/// Request to create a character.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateCharacter {
    pub name: String,
}

impl CreateCharacter {
    /// Validate the player-facing character name.
    pub fn validate(&self) -> Result<(), ApiError> {
        let character_count = self.name.chars().count();
        if !(1..=32).contains(&character_count)
            || self.name.trim() != self.name
            || self.name.chars().any(char::is_control)
        {
            return Err(ApiError::InvalidRequest(
                "character name must contain 1-32 non-control characters without surrounding whitespace"
                    .into(),
            ));
        }
        Ok(())
    }
}

/// A world/shard available to the player.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Zone {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub player_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<u32>,
}

/// A successful world entry, including the server-provided connection details.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldSession {
    pub zone_id: String,
    pub session_id: String,
    pub endpoint: String,
}

/// Backend health checks.
pub trait HealthService {
    fn health(&self) -> Result<HealthStatus, ApiError>;
}

/// Account and session operations.
pub trait AuthService {
    fn authenticate_device(&self, request: &DeviceLogin) -> Result<Session, ApiError>;
    fn refresh_session(&self, refresh_token: &SessionToken) -> Result<Session, ApiError>;
    fn account(&self, token: &SessionToken) -> Result<Account, ApiError>;
    fn logout(&self, token: &SessionToken) -> Result<(), ApiError>;
}

/// World discovery and entry operations.
pub trait WorldService {
    fn zones(&self, token: &SessionToken) -> Result<Vec<Zone>, ApiError>;
    fn enter_world(&self, token: &SessionToken, zone_id: &str) -> Result<WorldSession, ApiError>;
    fn leave_world(&self, token: &SessionToken, session: &WorldSession) -> Result<(), ApiError>;
}

/// Aggregate backend contract for implementations that provide every service.
pub trait Backend: HealthService + AuthService + WorldService {}

impl<T: HealthService + AuthService + WorldService> Backend for T {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_redacted_and_checked_on_deserialization() {
        let token = SessionToken::new("secret").unwrap();
        assert_eq!(format!("{token:?}"), "SessionToken([REDACTED])");
        assert!(SessionToken::new("").is_err());
        assert!(serde_json::from_str::<SessionToken>("\"\"").is_err());
        assert_eq!(serde_json::to_string(&token).unwrap(), "\"secret\"");
    }

    #[test]
    fn login_and_character_input_is_validated() {
        let login = DeviceLogin {
            device_id: "device-1".into(),
            username: Some("player_1".into()),
            create: true,
            locale: Locale::default(),
        };
        assert!(login.validate().is_ok());
        assert!(
            DeviceLogin {
                username: Some("bad name".into()),
                ..login.clone()
            }
            .validate()
            .is_err()
        );
        assert!(
            CreateCharacter {
                name: "  name".into()
            }
            .validate()
            .is_err()
        );
        assert!(
            CreateCharacter {
                name: "name".into()
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn retryability_and_status_mapping_are_stable() {
        let error = ApiError::Backend {
            status: 503,
            code: None,
            message: "busy".into(),
        };
        assert_eq!(error.code(), ErrorCode::Internal);
        assert!(error.is_retryable());
        let engine_error = error.to_engine_error();
        assert!(engine_error.retryable());
        assert_eq!(engine_error.message(), "busy");

        let error = ApiError::Backend {
            status: 404,
            code: None,
            message: "missing".into(),
        };
        assert_eq!(error.code(), ErrorCode::NotFound);
        assert!(!error.is_retryable());

        let expired = ApiError::SessionExpired;
        assert_eq!(expired.code(), ErrorCode::SessionExpired);
        assert!(!expired.is_retryable());
        assert_eq!(expired.to_engine_error().code(), ErrorCode::SessionExpired);
    }
}
