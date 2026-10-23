//! Structured, serializable errors shared by server, client, and tools.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Stable machine-readable error categories.
///
/// UI and tooling should branch on these codes, never on message text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The request was malformed or missing required fields.
    InvalidRequest,
    /// The referenced object does not exist or is no longer available.
    NotFound,
    /// The target is outside an allowed range or bounds.
    OutOfRange,
    /// The target is not visible to the actor.
    NotVisible,
    /// The actor lacks permission for the operation.
    PermissionDenied,
    /// The object exists but is not in a valid state for the operation.
    InvalidState,
    /// The action is on cooldown.
    Cooldown,
    /// A capacity (slots, population, budget) would be exceeded.
    CapacityExceeded,
    /// Protocol, schema, or content versions are incompatible.
    VersionMismatch,
    /// Authored content failed validation.
    ContentInvalid,
    /// The operation conflicts with a concurrent change.
    Conflict,
    /// A bounded work budget was exhausted; retry is allowed.
    BudgetExhausted,
    /// The feature or operation is not supported by this build.
    Unsupported,
    /// An invariant failed or an unexpected internal error occurred.
    Internal,
}

impl ErrorCode {
    /// The canonical wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::NotFound => "not_found",
            Self::OutOfRange => "out_of_range",
            Self::NotVisible => "not_visible",
            Self::PermissionDenied => "permission_denied",
            Self::InvalidState => "invalid_state",
            Self::Cooldown => "cooldown",
            Self::CapacityExceeded => "capacity_exceeded",
            Self::VersionMismatch => "version_mismatch",
            Self::ContentInvalid => "content_invalid",
            Self::Conflict => "conflict",
            Self::BudgetExhausted => "budget_exhausted",
            Self::Unsupported => "unsupported",
            Self::Internal => "internal",
        }
    }

    /// Whether the same request may succeed later without changes.
    #[must_use]
    pub const fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::Cooldown | Self::BudgetExhausted | Self::Conflict
        )
    }

    /// Whether the failure is caused by the caller rather than the server.
    #[must_use]
    pub const fn is_client_fault(self) -> bool {
        matches!(
            self,
            Self::InvalidRequest
                | Self::NotFound
                | Self::OutOfRange
                | Self::NotVisible
                | Self::PermissionDenied
                | Self::InvalidState
                | Self::Cooldown
                | Self::CapacityExceeded
                | Self::VersionMismatch
                | Self::ContentInvalid
                | Self::Conflict
                | Self::Unsupported
        )
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An error with a stable code, a human-readable message, and safe details.
///
/// Details are stringly key/value pairs intended for humans and tools. They
/// must never contain credentials, tokens, or private player data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineError {
    code: ErrorCode,
    message: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    details: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    retryable: bool,
}

impl EngineError {
    /// Create an error with a code and message.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: BTreeMap::new(),
            retryable: code.is_retryable(),
        }
    }

    /// Attach a safe key/value detail.
    #[must_use]
    pub fn with_detail(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.details.insert(key.into(), value.into());
        self
    }

    /// Override retry behavior. Use sparingly; the code's default is usually
    /// correct.
    #[must_use]
    pub fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    /// The stable error code.
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        self.code
    }

    /// The human-readable message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Safe structured details.
    #[must_use]
    pub fn details(&self) -> &BTreeMap<String, String> {
        &self.details
    }

    /// Whether a retry of the same request may succeed.
    #[must_use]
    pub fn retryable(&self) -> bool {
        self.retryable
    }

    /// Convenience constructor for a missing object.
    #[must_use]
    pub fn not_found(what: &str, id: impl fmt::Display) -> Self {
        Self::new(ErrorCode::NotFound, format!("{what} not found"))
            .with_detail("kind", what)
            .with_detail("id", id.to_string())
    }

    /// Convenience constructor for invalid input.
    #[must_use]
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidRequest, message)
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for EngineError {}

impl From<serde_json::Error> for EngineError {
    fn from(value: serde_json::Error) -> Self {
        Self::new(
            ErrorCode::InvalidRequest,
            format!("invalid JSON at line {}: {}", value.line(), value),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_round_trips_through_json() {
        for code in [
            ErrorCode::InvalidRequest,
            ErrorCode::BudgetExhausted,
            ErrorCode::VersionMismatch,
            ErrorCode::Internal,
        ] {
            let json = serde_json::to_string(&code).unwrap();
            let parsed: ErrorCode = serde_json::from_str(&json).unwrap();
            assert_eq!(code, parsed);
            assert_eq!(json, format!("\"{}\"", code.as_str()));
        }
    }

    #[test]
    fn retryable_follows_code_by_default() {
        assert!(EngineError::new(ErrorCode::Cooldown, "wait").retryable());
        assert!(EngineError::new(ErrorCode::BudgetExhausted, "busy").retryable());
        assert!(!EngineError::new(ErrorCode::PermissionDenied, "no").retryable());
        assert!(
            !EngineError::new(ErrorCode::Cooldown, "wait")
                .with_retryable(false)
                .retryable()
        );
    }

    #[test]
    fn details_survive_json_and_empty_details_are_omitted() {
        let error = EngineError::new(ErrorCode::OutOfRange, "too far")
            .with_detail("distance_m", "12.5")
            .with_detail("max_m", "2.5");
        let json = serde_json::to_string(&error).unwrap();
        assert!(json.contains("\"distance_m\":\"12.5\""));
        let parsed: EngineError = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, error);

        let bare = EngineError::new(ErrorCode::Internal, "boom");
        let json = serde_json::to_string(&bare).unwrap();
        assert!(!json.contains("details"));
    }
}
