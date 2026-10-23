//! Append-only audit events for privileged and high-impact operations.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{EngineError, ErrorCode};
use crate::id::StableId;
use crate::time::Millis;

/// A correlation ID that ties a request, its effects, and its audit records
/// together.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CorrelationId(String);

impl CorrelationId {
    /// Validate and create a correlation ID.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] when empty, longer than 128
    /// characters, or containing control characters.
    pub fn new(value: impl Into<String>) -> Result<Self, EngineError> {
        let value = value.into();
        if value.is_empty() || value.len() > 128 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "correlation id must be 1..=128 characters",
            ));
        }
        if value.chars().any(char::is_control) {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "correlation id must not contain control characters",
            ));
        }
        Ok(Self(value))
    }

    /// The raw identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CorrelationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Who performed an action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Actor {
    /// A player character.
    Character {
        /// Character entity ID.
        id: StableId,
    },
    /// An AI-controlled entity.
    Ai {
        /// Entity ID.
        id: StableId,
    },
    /// A server process or scheduled job.
    System,
    /// An authenticated staff member.
    Admin {
        /// Stable admin account ID.
        id: String,
        /// Role at the time of the action.
        role: String,
    },
}

impl Actor {
    /// Whether this actor is staff.
    #[must_use]
    pub fn is_privileged(&self) -> bool {
        matches!(self, Self::Admin { .. })
    }
}

/// The outcome recorded for an audited action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    /// The request was received but has not completed.
    Attempted,
    /// The action completed.
    Succeeded,
    /// Validation rejected the action.
    Rejected,
    /// The action failed after validation.
    Failed,
}

/// One append-only audit record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    /// When the event occurred.
    pub at: Millis,
    /// Who acted.
    pub actor: Actor,
    /// Stable action name, for example `admin.teleport_player`.
    pub action: String,
    /// Affected entity, when applicable.
    pub target: Option<StableId>,
    /// Outcome at the time of writing.
    pub outcome: AuditOutcome,
    /// Request correlation.
    pub correlation: CorrelationId,
    /// Why the action was taken; required for destructive actions by policy.
    pub reason: Option<String>,
    /// Safe, redacted details.
    pub details: BTreeMap<String, String>,
}

impl AuditEvent {
    /// Start an audit record with the minimum required fields.
    #[must_use]
    pub fn new(
        at: Millis,
        actor: Actor,
        action: impl Into<String>,
        correlation: CorrelationId,
    ) -> Self {
        Self {
            at,
            actor,
            action: action.into(),
            target: None,
            outcome: AuditOutcome::Attempted,
            correlation,
            reason: None,
            details: BTreeMap::new(),
        }
    }

    /// Set the affected entity.
    #[must_use]
    pub fn with_target(mut self, target: StableId) -> Self {
        self.target = Some(target);
        self
    }

    /// Set the outcome.
    #[must_use]
    pub fn with_outcome(mut self, outcome: AuditOutcome) -> Self {
        self.outcome = outcome;
        self
    }

    /// Set the reason.
    #[must_use]
    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    /// Add a detail, rejecting keys that commonly hold credentials.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] for sensitive keys or oversized
    /// values.
    pub fn try_with_detail(
        mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self, EngineError> {
        let key = key.into();
        let value = value.into();
        if is_sensitive_key(&key) {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "audit details must not contain credential-like keys",
            )
            .with_detail("key", &key));
        }
        if key.len() > 64 || value.len() > 512 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "audit detail key or value is too long",
            )
            .with_detail("key", &key));
        }
        self.details.insert(key, value);
        Ok(self)
    }

    /// Validate structural requirements before persisting.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] for an empty or oversized action,
    /// or a privileged action without a reason.
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.action.is_empty() || self.action.len() > 128 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "audit action must be 1..=128 characters",
            ));
        }
        if self.actor.is_privileged() && self.reason.as_ref().is_none_or(String::is_empty) {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "privileged audit events require a reason",
            )
            .with_detail("action", &self.action));
        }
        Ok(())
    }
}

/// A sink for append-only audit records.
pub trait AuditSink {
    /// Persist one event.
    ///
    /// # Errors
    ///
    /// Returns an error when the event cannot be durably recorded. Callers must
    /// treat this as a failed operation for privileged changes.
    fn record(&mut self, event: AuditEvent) -> Result<(), EngineError>;
}

/// An in-memory audit sink for tests and single-process tools.
#[derive(Debug, Clone, Default)]
pub struct MemoryAuditSink {
    events: Vec<AuditEvent>,
}

impl MemoryAuditSink {
    /// An empty sink.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Recorded events in insertion order.
    #[must_use]
    pub fn events(&self) -> &[AuditEvent] {
        &self.events
    }

    /// Remove and return all events.
    pub fn take(&mut self) -> Vec<AuditEvent> {
        std::mem::take(&mut self.events)
    }
}

impl AuditSink for MemoryAuditSink {
    fn record(&mut self, event: AuditEvent) -> Result<(), EngineError> {
        event.validate()?;
        self.events.push(event);
        Ok(())
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let lowered = key.to_ascii_lowercase();
    [
        "password",
        "passwd",
        "secret",
        "token",
        "authorization",
        "api_key",
        "apikey",
        "credential",
        "cookie",
        "session_key",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::assert_is_empty)]

    use super::*;

    fn correlation() -> CorrelationId {
        CorrelationId::new("corr-test-1").unwrap()
    }

    fn character() -> Actor {
        Actor::Character {
            id: StableId::from_parts(crate::id::IdKind::Character, 1, 1),
        }
    }

    #[test]
    fn correlation_ids_are_validated() {
        assert!(CorrelationId::new("abc").is_ok());
        assert!(CorrelationId::new("").is_err());
        assert!(CorrelationId::new("bad\nvalue").is_err());
        assert!(CorrelationId::new("x".repeat(129)).is_err());
    }

    #[test]
    fn event_builder_records_safe_details() {
        let target = StableId::from_parts(crate::id::IdKind::Entity, 2, 2);
        let event = AuditEvent::new(
            Millis::new(123),
            character(),
            "inventory.transfer",
            correlation(),
        )
        .with_target(target)
        .with_outcome(AuditOutcome::Succeeded)
        .with_reason("trade accepted")
        .try_with_detail("quantity", "3")
        .unwrap();

        assert_eq!(event.target, Some(target));
        assert_eq!(event.outcome, AuditOutcome::Succeeded);
        assert_eq!(event.details.get("quantity").map(String::as_str), Some("3"));
        assert!(event.validate().is_ok());
    }

    #[test]
    fn credential_like_details_are_rejected() {
        let event = AuditEvent::new(Millis::new(1), Actor::System, "auth.login", correlation());
        for key in ["password", "access_token", "Authorization", "api_key"] {
            let error = event.clone().try_with_detail(key, "hunter2").unwrap_err();
            assert_eq!(error.code(), ErrorCode::InvalidRequest, "{key}");
        }
    }

    #[test]
    fn privileged_events_require_a_reason() {
        let admin = Actor::Admin {
            id: "admin-1".into(),
            role: "moderator".into(),
        };
        let event = AuditEvent::new(Millis::new(1), admin, "player.kick", correlation());
        assert_eq!(
            event.validate().unwrap_err().code(),
            ErrorCode::InvalidRequest
        );
        assert!(event.with_reason("harassment").validate().is_ok());
    }

    #[test]
    fn memory_sink_preserves_order_and_validates() {
        let mut sink = MemoryAuditSink::new();
        for index in 0..3 {
            let event = AuditEvent::new(
                Millis::new(index),
                Actor::System,
                format!("test.action.{index}"),
                correlation(),
            )
            .with_outcome(AuditOutcome::Succeeded);
            sink.record(event).unwrap();
        }
        let actions: Vec<_> = sink
            .events()
            .iter()
            .map(|event| event.action.as_str())
            .collect();
        assert_eq!(actions, ["test.action.0", "test.action.1", "test.action.2"]);
        assert_eq!(sink.take().len(), 3);
        assert!(sink.events().is_empty());
    }

    #[test]
    fn invalid_events_are_not_stored() {
        let mut sink = MemoryAuditSink::new();
        let invalid = AuditEvent::new(Millis::new(1), Actor::System, "", correlation());
        assert!(sink.record(invalid).is_err());
        assert!(sink.events().is_empty());
    }
}
