//! Zone lease ownership for the runnable world service.
//!
//! One process may own a zone at a time. [`ZoneAuthority`] acquires the lease
//! before the service serves, renews it while the authority loop runs, appends
//! lifecycle events, and releases it at shutdown. The fencing token carried by
//! the lease is presented on every durable write, so a process that lost
//! ownership is rejected rather than allowed to overwrite newer state.
//!
//! The lease is only used when the world state is durable in PostgreSQL. A
//! file-backed or memory-only service has no shared database to fence against
//! and runs without one.

use std::sync::Arc;
use std::time::Duration;

use engine_core::error::{EngineError, ErrorCode};
use engine_core::time::Tick;
use serde_json::Value;

use crate::persistence::{ZoneEvent, ZoneEventInput, ZoneLease, ZoneLeaseStore};

/// Event kind written when this process takes authority.
pub const EVENT_WORLD_STARTED: &str = "world.started";
/// Event kind written after each successful durable snapshot.
pub const EVENT_WORLD_SNAPSHOT: &str = "world.snapshot";
/// Event kind written when this process gives authority up.
pub const EVENT_WORLD_STOPPED: &str = "world.stopped";

/// Schema version stamped on every event this build writes.
pub const EVENT_SCHEMA_VERSION: i32 = 1;

/// A held zone lease plus the store used to renew it and append events.
#[derive(Debug, Clone)]
pub struct ZoneAuthority {
    store: Arc<dyn ZoneLeaseStore>,
    lease: ZoneLease,
    ttl: Duration,
}

impl ZoneAuthority {
    /// Acquire the lease for `zone_id`.
    ///
    /// Returns `Ok(None)` when another owner holds an unexpired lease; the
    /// caller must not serve that zone.
    ///
    /// # Errors
    ///
    /// Returns an engine error when the store cannot be reached or the request
    /// is invalid.
    pub fn acquire(
        store: Arc<dyn ZoneLeaseStore>,
        zone_id: &str,
        owner: &str,
        ttl: Duration,
    ) -> Result<Option<Self>, EngineError> {
        if ttl.is_zero() {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "the zone lease TTL must be positive",
            ));
        }
        let Some(lease) = store.acquire(zone_id, owner, ttl)? else {
            return Ok(None);
        };
        Ok(Some(Self { store, lease, ttl }))
    }

    /// The lease this process currently holds.
    #[must_use]
    pub const fn lease(&self) -> &ZoneLease {
        &self.lease
    }

    /// The zone this authority covers.
    #[must_use]
    pub fn zone_id(&self) -> &str {
        &self.lease.zone_id
    }

    /// The owner name presented to the lease store.
    #[must_use]
    pub fn owner(&self) -> &str {
        &self.lease.owner
    }

    /// The current fencing token; every durable write must present it.
    #[must_use]
    pub const fn fencing_token(&self) -> i64 {
        self.lease.fencing_token
    }

    /// How often the holder should renew: one third of the lease lifetime.
    #[must_use]
    pub fn renewal_interval(&self) -> Duration {
        (self.ttl / 3).max(Duration::from_millis(1))
    }

    /// Extend the lease. `Ok(None)` means ownership was lost.
    ///
    /// # Errors
    ///
    /// Returns an engine error when the store cannot be reached.
    pub fn renew(&self) -> Result<Option<ZoneLease>, EngineError> {
        self.store.renew(
            &self.lease.zone_id,
            &self.lease.owner,
            self.lease.fencing_token,
            self.ttl,
        )
    }

    /// Append one event under this lease's fence.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::Conflict`] when ownership was lost or superseded,
    /// and an engine error when the store cannot be reached.
    pub fn append_event(
        &self,
        tick: Tick,
        kind: &str,
        actor_id: Option<&str>,
        payload: &Value,
    ) -> Result<i64, EngineError> {
        let tick = i64::try_from(tick.0).unwrap_or(i64::MAX);
        self.store.append_event(
            &self.lease.zone_id,
            &self.lease.owner,
            self.lease.fencing_token,
            ZoneEventInput {
                tick,
                kind,
                actor_id,
                payload,
                schema_version: EVENT_SCHEMA_VERSION,
            },
        )
    }

    /// Read the zone event log after `sequence`.
    ///
    /// # Errors
    ///
    /// Returns an engine error when the store cannot be reached.
    pub fn events_after(&self, sequence: i64) -> Result<Vec<ZoneEvent>, EngineError> {
        self.store.events_after(&self.lease.zone_id, sequence)
    }

    /// Expire the lease so a replacement may take over immediately.
    ///
    /// # Errors
    ///
    /// Returns an engine error when the store cannot be reached.
    pub fn release(&self) -> Result<bool, EngineError> {
        self.store.release(
            &self.lease.zone_id,
            &self.lease.owner,
            self.lease.fencing_token,
        )
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::SystemTime;

    #[derive(Debug, Default)]
    pub(crate) struct MemoryLeases {
        state: Mutex<MemoryState>,
    }

    #[derive(Debug, Default)]
    struct MemoryState {
        owner: Option<String>,
        token: i64,
        events: Vec<ZoneEvent>,
    }

    impl ZoneLeaseStore for MemoryLeases {
        fn acquire(
            &self,
            zone_id: &str,
            owner: &str,
            _ttl: Duration,
        ) -> Result<Option<ZoneLease>, EngineError> {
            let mut state = self.state.lock().unwrap();
            if state.owner.is_some() {
                return Ok(None);
            }
            state.token += 1;
            state.owner = Some(owner.to_owned());
            Ok(Some(ZoneLease {
                zone_id: zone_id.to_owned(),
                owner: owner.to_owned(),
                fencing_token: state.token,
                expires_at: SystemTime::now() + Duration::from_secs(30),
            }))
        }

        fn renew(
            &self,
            zone_id: &str,
            owner: &str,
            token: i64,
            _ttl: Duration,
        ) -> Result<Option<ZoneLease>, EngineError> {
            let state = self.state.lock().unwrap();
            Ok(
                (state.owner.as_deref() == Some(owner) && state.token == token).then(|| {
                    ZoneLease {
                        zone_id: zone_id.to_owned(),
                        owner: owner.to_owned(),
                        fencing_token: token,
                        expires_at: SystemTime::now() + Duration::from_secs(30),
                    }
                }),
            )
        }

        fn release(&self, _zone_id: &str, owner: &str, token: i64) -> Result<bool, EngineError> {
            let mut state = self.state.lock().unwrap();
            let held = state.owner.as_deref() == Some(owner) && state.token == token;
            if held {
                state.owner = None;
            }
            Ok(held)
        }

        fn append_event(
            &self,
            _zone_id: &str,
            owner: &str,
            token: i64,
            event: ZoneEventInput<'_>,
        ) -> Result<i64, EngineError> {
            let ZoneEventInput {
                tick,
                kind,
                actor_id,
                payload,
                schema_version,
            } = event;
            let mut state = self.state.lock().unwrap();
            if state.owner.as_deref() != Some(owner) || state.token != token {
                return Err(EngineError::new(
                    ErrorCode::Conflict,
                    "the zone lease is no longer authoritative",
                ));
            }
            let sequence = i64::try_from(state.events.len()).unwrap();
            state.events.push(ZoneEvent {
                sequence,
                tick,
                occurred_at: SystemTime::now(),
                kind: kind.to_owned(),
                actor_id: actor_id.map(str::to_owned),
                payload: payload.clone(),
                schema_version,
            });
            Ok(sequence)
        }

        fn events_after(
            &self,
            _zone_id: &str,
            sequence: i64,
        ) -> Result<Vec<ZoneEvent>, EngineError> {
            let state = self.state.lock().unwrap();
            Ok(state
                .events
                .iter()
                .filter(|event| event.sequence > sequence)
                .cloned()
                .collect())
        }
    }

    const ZONE: &str = "zone_00000000000000000000000001";

    fn authority(store: &Arc<MemoryLeases>, owner: &str) -> ZoneAuthority {
        let store: Arc<dyn ZoneLeaseStore> = store.clone();
        ZoneAuthority::acquire(store, ZONE, owner, Duration::from_secs(30))
            .unwrap()
            .unwrap()
    }

    #[test]
    fn a_live_lease_rejects_a_second_owner() {
        let store = Arc::new(MemoryLeases::default());
        let _first = authority(&store, "owner-a");

        let second_store: Arc<dyn ZoneLeaseStore> = store.clone();
        let second =
            ZoneAuthority::acquire(second_store, ZONE, "owner-b", Duration::from_secs(30)).unwrap();
        assert!(second.is_none(), "an unexpired lease has one owner");
    }

    #[test]
    fn renewal_reports_a_lease_that_was_given_up() {
        let store = Arc::new(MemoryLeases::default());
        let first = authority(&store, "owner-a");
        assert!(first.renew().unwrap().is_some());
        assert!(first.release().unwrap());

        assert!(
            first.renew().unwrap().is_none(),
            "a released lease is not extendable"
        );
    }

    #[test]
    fn events_are_sequenced_and_fenced_across_takeover() {
        let store = Arc::new(MemoryLeases::default());
        let first = authority(&store, "owner-a");
        let payload = serde_json::json!({"tick": 1});
        assert_eq!(
            first
                .append_event(Tick(1), EVENT_WORLD_SNAPSHOT, None, &payload)
                .unwrap(),
            0
        );
        assert_eq!(
            first
                .append_event(Tick(2), EVENT_WORLD_SNAPSHOT, None, &payload)
                .unwrap(),
            1
        );

        assert!(first.release().unwrap());
        let second = authority(&store, "owner-b");
        assert!(second.fencing_token() > first.fencing_token());

        let stale = first
            .append_event(Tick(3), EVENT_WORLD_SNAPSHOT, None, &payload)
            .unwrap_err();
        assert_eq!(stale.code(), ErrorCode::Conflict);

        assert_eq!(
            second
                .append_event(Tick(4), EVENT_WORLD_SNAPSHOT, None, &payload)
                .unwrap(),
            2
        );
        let events = second.events_after(1).unwrap();
        assert_eq!(
            events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![2]
        );
    }
}
