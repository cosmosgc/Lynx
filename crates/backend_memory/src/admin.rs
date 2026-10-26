//! Audit-log reads and appends for the in-memory adapter.
//!
//! The in-memory store keeps records in a `Vec` ordered by event ID. The
//! append method is inherent rather than part of [`AdminService`] because it is
//! a trusted, server-side write path; clients may only read.

use engine_core::audit::AuditEvent;
use game_api::{
    AdminClaims, AdminService, ApiError, AuditPage, AuditQuery, AuditRecord, MAX_AUDIT_PAGE,
};

use crate::{MemoryBackend, State};

impl MemoryBackend {
    /// Append an audit event and return its assigned event ID.
    ///
    /// Callers must treat a failure here as a failed privileged operation. The
    /// event is validated before it is stored.
    pub fn record_audit(&self, event: &AuditEvent) -> Result<u64, ApiError> {
        event
            .validate()
            .map_err(|error| ApiError::InvalidRequest(error.message().to_owned()))?;
        let mut state = self.lock_state()?;
        state.next_event_id = state
            .next_event_id
            .checked_add(1)
            .ok_or_else(|| ApiError::Unavailable("in-memory audit ID space exhausted".into()))?;
        let event_id = state.next_event_id;
        state.audit.push(AuditRecord {
            event_id,
            event: event.clone(),
        });
        Ok(event_id)
    }
}

fn page(state: &State, query: &AuditQuery) -> AuditPage {
    let after = query.after_event_id.unwrap_or(0);
    let limit = usize::from(query.limit);
    let mut records = Vec::new();
    let mut next_after = None;

    for record in &state.audit {
        if record.event_id <= after {
            continue;
        }
        if let Some(action) = &query.action
            && record.event.action != *action
        {
            continue;
        }
        if records.len() == limit {
            next_after = records.last().map(|record: &AuditRecord| record.event_id);
            break;
        }
        records.push(record.clone());
    }

    AuditPage {
        records,
        next_after,
    }
}

impl AdminService for MemoryBackend {
    fn audit_log(&self, claims: &AdminClaims, query: &AuditQuery) -> Result<AuditPage, ApiError> {
        query.validate()?;
        if !claims.role.can_read_audit() {
            return Err(ApiError::PermissionDenied);
        }
        debug_assert!(
            usize::from(query.limit) <= usize::from(MAX_AUDIT_PAGE),
            "validated audit limits never exceed the maximum page size"
        );
        let state = self.lock_state()?;
        Ok(page(&state, query))
    }
}

#[cfg(test)]
mod tests {
    use engine_core::audit::{Actor, AuditOutcome, CorrelationId};
    use engine_core::time::Millis;
    use game_api::AdminRole;

    use super::*;

    fn claims(role: AdminRole) -> AdminClaims {
        AdminClaims::new("admin-1", role).unwrap()
    }

    fn event(sequence: i64, action: &str) -> AuditEvent {
        AuditEvent::new(
            Millis::new(sequence),
            Actor::System,
            action,
            CorrelationId::new(format!("corr-{sequence}")).unwrap(),
        )
        .with_outcome(AuditOutcome::Succeeded)
    }

    #[test]
    fn records_are_append_only_and_paginated() {
        let backend = MemoryBackend::new();
        let admin = claims(AdminRole::Observer);

        let first = backend
            .record_audit(&event(1, "admin.teleport_player"))
            .unwrap();
        let second = backend
            .record_audit(&event(2, "admin.kick_player"))
            .unwrap();
        let third = backend
            .record_audit(&event(3, "admin.teleport_player"))
            .unwrap();
        assert_eq!((first, second, third), (1, 2, 3));

        let all = backend.audit_log(&admin, &AuditQuery::default()).unwrap();
        assert_eq!(all.records.len(), 3);
        assert!(all.next_after.is_none());

        let first_page = backend
            .audit_log(
                &admin,
                &AuditQuery {
                    limit: 2,
                    ..AuditQuery::default()
                },
            )
            .unwrap();
        assert_eq!(first_page.records.len(), 2);
        assert_eq!(first_page.records[0].event_id, first);
        assert_eq!(first_page.next_after, Some(second));

        let second_page = backend
            .audit_log(
                &admin,
                &AuditQuery {
                    limit: 2,
                    after_event_id: first_page.next_after,
                    ..AuditQuery::default()
                },
            )
            .unwrap();
        assert_eq!(second_page.records.len(), 1);
        assert_eq!(second_page.records[0].event_id, third);
        assert!(second_page.next_after.is_none());

        let filtered = backend
            .audit_log(
                &admin,
                &AuditQuery {
                    action: Some("admin.teleport_player".into()),
                    ..AuditQuery::default()
                },
            )
            .unwrap();
        assert_eq!(
            filtered
                .records
                .iter()
                .map(|record| record.event_id)
                .collect::<Vec<_>>(),
            vec![first, third]
        );
    }

    #[test]
    fn invalid_events_and_queries_are_rejected() {
        let backend = MemoryBackend::new();
        let admin = claims(AdminRole::Moderator);

        let privileged = AuditEvent::new(
            Millis::new(1),
            Actor::Admin {
                id: "admin-1".into(),
                role: "moderator".into(),
            },
            "player.kick",
            CorrelationId::new("corr-1").unwrap(),
        );
        assert!(matches!(
            backend.record_audit(&privileged),
            Err(ApiError::InvalidRequest(_))
        ));
        assert!(
            backend
                .record_audit(&privileged.clone().with_reason("harassment"))
                .is_ok()
        );

        assert!(matches!(
            backend.audit_log(
                &admin,
                &AuditQuery {
                    limit: 0,
                    ..AuditQuery::default()
                }
            ),
            Err(ApiError::InvalidRequest(_))
        ));
        assert!(matches!(
            backend.audit_log(
                &admin,
                &AuditQuery {
                    limit: MAX_AUDIT_PAGE + 1,
                    ..AuditQuery::default()
                }
            ),
            Err(ApiError::InvalidRequest(_))
        ));
    }

    #[test]
    fn every_role_can_read_the_audit_log() {
        let backend = MemoryBackend::new();
        backend.record_audit(&event(1, "auth.login")).unwrap();
        for role in [
            AdminRole::Observer,
            AdminRole::Moderator,
            AdminRole::Operator,
            AdminRole::Engineer,
            AdminRole::Owner,
        ] {
            assert_eq!(
                backend
                    .audit_log(&claims(role), &AuditQuery::default())
                    .unwrap()
                    .records
                    .len(),
                1
            );
        }
    }
}
