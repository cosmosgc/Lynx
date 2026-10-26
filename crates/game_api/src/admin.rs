//! Administration contracts and the read-only audit log.
//!
//! Admin authentication is separate from player authentication. Adapters and
//! transports must construct [`AdminClaims`] only after verifying a short-lived
//! admin session; they must never accept claims from a client message.

use std::fmt;

use engine_core::audit::{Actor, AuditEvent};
use serde::{Deserialize, Serialize};

use crate::service::ApiError;

/// Largest page returned by [`AdminService::audit_log`].
pub const MAX_AUDIT_PAGE: u16 = 200;

/// Page size used when a request omits one.
pub const DEFAULT_AUDIT_PAGE: u16 = 50;

/// Staff role from the initial role model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdminRole {
    /// Read-only inspection, live maps, and metrics.
    Observer,
    /// Mute, kick, quarantine, and report resolution.
    Moderator,
    /// Zone restart, safe content publish, and rollback.
    Operator,
    /// Diagnostics, feature flags, and configuration.
    Engineer,
    /// Role management, break-glass, and destructive actions.
    Owner,
}

impl AdminRole {
    /// Stable wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Observer => "observer",
            Self::Moderator => "moderator",
            Self::Operator => "operator",
            Self::Engineer => "engineer",
            Self::Owner => "owner",
        }
    }

    /// Every role may inspect the audit log.
    #[must_use]
    pub const fn can_read_audit(self) -> bool {
        true
    }

    /// Whether the role may apply moderation sanctions.
    #[must_use]
    pub const fn can_moderate(self) -> bool {
        !matches!(self, Self::Observer)
    }

    /// Whether the role may restart zones or publish and roll back content.
    #[must_use]
    pub const fn can_operate(self) -> bool {
        matches!(self, Self::Operator | Self::Engineer | Self::Owner)
    }

    /// Whether the role may change diagnostic and feature configuration.
    #[must_use]
    pub const fn can_configure(self) -> bool {
        matches!(self, Self::Engineer | Self::Owner)
    }

    /// Whether the role may grant other roles or perform break-glass actions.
    #[must_use]
    pub const fn can_manage_roles(self) -> bool {
        matches!(self, Self::Owner)
    }
}

impl fmt::Display for AdminRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Trusted claims for one authenticated admin session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminClaims {
    /// Stable admin account identifier.
    pub admin_id: String,
    /// Role at the time of the request.
    pub role: AdminRole,
}

impl AdminClaims {
    /// Validate and create admin claims.
    pub fn new(admin_id: impl Into<String>, role: AdminRole) -> Result<Self, ApiError> {
        let admin_id = admin_id.into();
        if admin_id.is_empty()
            || admin_id.len() > 128
            || admin_id
                .chars()
                .any(|ch| ch.is_control() || ch.is_whitespace())
        {
            return Err(ApiError::InvalidRequest("invalid admin identifier".into()));
        }
        Ok(Self { admin_id, role })
    }

    /// The audit actor representing these claims.
    #[must_use]
    pub fn actor(&self) -> Actor {
        Actor::Admin {
            id: self.admin_id.clone(),
            role: self.role.as_str().to_owned(),
        }
    }
}

/// Filters for audit-log reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditQuery {
    /// Maximum records to return, between 1 and [`MAX_AUDIT_PAGE`].
    #[serde(default = "default_audit_page")]
    pub limit: u16,
    /// Return records with a greater event ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_event_id: Option<u64>,
    /// Exact action name, for example `admin.teleport_player`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

const fn default_audit_page() -> u16 {
    DEFAULT_AUDIT_PAGE
}

impl Default for AuditQuery {
    fn default() -> Self {
        Self {
            limit: DEFAULT_AUDIT_PAGE,
            after_event_id: None,
            action: None,
        }
    }
}

impl AuditQuery {
    /// Validate filters before querying an adapter.
    pub fn validate(&self) -> Result<(), ApiError> {
        if self.limit == 0 || self.limit > MAX_AUDIT_PAGE {
            return Err(ApiError::InvalidRequest(format!(
                "audit limit must be between 1 and {MAX_AUDIT_PAGE}"
            )));
        }
        if let Some(action) = &self.action
            && (action.is_empty() || action.len() > 128 || action.chars().any(char::is_control))
        {
            return Err(ApiError::InvalidRequest(
                "invalid audit action filter".into(),
            ));
        }
        Ok(())
    }
}

/// One append-only audit record with its adapter-assigned ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecord {
    /// Monotonic record identifier, unique within the audit store.
    pub event_id: u64,
    /// The recorded event.
    pub event: AuditEvent,
}

/// A page of audit records, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditPage {
    /// Records matching the query.
    pub records: Vec<AuditRecord>,
    /// Pass this as `after_event_id` to fetch the next page, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_after: Option<u64>,
}

/// Read-only administration operations.
pub trait AdminService {
    /// Read the append-only audit log.
    ///
    /// Adapters append records in the same transaction as the state change
    /// they describe; this contract only exposes reads.
    fn audit_log(&self, claims: &AdminClaims, query: &AuditQuery) -> Result<AuditPage, ApiError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_expose_the_documented_capabilities() {
        assert!(AdminRole::Observer.can_read_audit());
        assert!(!AdminRole::Observer.can_moderate());
        assert!(AdminRole::Moderator.can_moderate());
        assert!(!AdminRole::Moderator.can_operate());
        assert!(AdminRole::Operator.can_operate());
        assert!(!AdminRole::Operator.can_configure());
        assert!(AdminRole::Engineer.can_configure());
        assert!(AdminRole::Owner.can_manage_roles());
        assert_eq!(AdminRole::Owner.as_str(), "owner");
    }

    #[test]
    fn claims_and_queries_are_validated() {
        assert!(AdminClaims::new("admin-1", AdminRole::Observer).is_ok());
        assert!(AdminClaims::new("", AdminRole::Observer).is_err());
        assert!(AdminClaims::new("bad admin", AdminRole::Observer).is_err());

        assert!(AuditQuery::default().validate().is_ok());
        assert!(
            AuditQuery {
                limit: 0,
                ..AuditQuery::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            AuditQuery {
                limit: MAX_AUDIT_PAGE + 1,
                ..AuditQuery::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            AuditQuery {
                action: Some("bad\naction".into()),
                ..AuditQuery::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn admin_claims_map_to_audit_actors() {
        let claims = AdminClaims::new("admin-1", AdminRole::Moderator).unwrap();
        assert_eq!(
            claims.actor(),
            Actor::Admin {
                id: "admin-1".into(),
                role: "moderator".into(),
            }
        );
    }
}
