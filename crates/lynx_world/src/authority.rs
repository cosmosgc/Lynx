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

use std::collections::BTreeMap;
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

    /// Atomically append multiple events under this authority's lease fence.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::Conflict`] when ownership was lost or superseded,
    /// and an engine error when the store cannot atomically append the batch.
    pub fn append_events(&self, events: &[ZoneEventInput<'_>]) -> Result<Vec<i64>, EngineError> {
        self.store.append_events(
            &self.lease.zone_id,
            &self.lease.owner,
            self.lease.fencing_token,
            events,
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

/// A set of per-zone leases held by one server process.
///
/// Acquisition is deterministic by zone ID. If any zone is unavailable or a
/// store operation fails, release is attempted for leases acquired earlier in
/// this attempt before returning. This is a convenience for servers that host
/// several zone instances; each zone still has an independent fencing token and
/// recovery stream.
#[derive(Debug, Clone)]
pub struct ZoneAuthoritySet {
    zones: BTreeMap<String, ZoneAuthority>,
}

impl ZoneAuthoritySet {
    /// Acquire all requested zone leases for one owner.
    ///
    /// Returns `Ok(None)` when any zone is already owned. Empty and duplicate
    /// zone IDs are rejected.
    pub fn acquire(
        store: &Arc<dyn ZoneLeaseStore>,
        zone_ids: impl IntoIterator<Item = impl AsRef<str>>,
        owner: &str,
        ttl: Duration,
    ) -> Result<Option<Self>, EngineError> {
        let mut requested = zone_ids
            .into_iter()
            .map(|zone_id| zone_id.as_ref().to_owned())
            .collect::<Vec<_>>();
        requested.sort();
        if requested.is_empty() || requested.iter().any(String::is_empty) {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "a non-empty set of zone IDs is required",
            ));
        }
        if requested.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "zone IDs in an authority set must be unique",
            ));
        }

        let mut zones = BTreeMap::new();
        for zone_id in requested {
            match ZoneAuthority::acquire(Arc::clone(store), &zone_id, owner, ttl) {
                Ok(Some(authority)) => {
                    zones.insert(zone_id, authority);
                }
                Ok(None) => {
                    release_best_effort(zones.into_values());
                    return Ok(None);
                }
                Err(error) => {
                    release_best_effort(zones.into_values());
                    return Err(error);
                }
            }
        }
        Ok(Some(Self { zones }))
    }

    /// The authority for a zone in this set, if held.
    #[must_use]
    pub fn get(&self, zone_id: &str) -> Option<&ZoneAuthority> {
        self.zones.get(zone_id)
    }

    /// The zone IDs held in stable order.
    pub fn zone_ids(&self) -> impl ExactSizeIterator<Item = &str> {
        self.zones.keys().map(String::as_str)
    }

    /// Renew every held lease. Returns `false` if any lease was lost.
    ///
    /// A caller must stop serving every zone in the set when this returns
    /// `false`; partial authority is not treated as a successful renewal.
    pub fn renew_all(&self) -> Result<bool, EngineError> {
        let mut held = true;
        let mut first_error = None;
        for authority in self.zones.values() {
            match authority.renew() {
                Ok(Some(_)) => {}
                Ok(None) => held = false,
                Err(error) => {
                    held = false;
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(held)
    }

    /// Release every held lease. Returns `false` if any lease was already lost.
    pub fn release_all(&self) -> Result<bool, EngineError> {
        let mut released = true;
        let mut first_error = None;
        for authority in self.zones.values() {
            match authority.release() {
                Ok(value) => released &= value,
                Err(error) if first_error.is_none() => first_error = Some(error),
                Err(_) => {}
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(released)
    }
}

fn release_best_effort(authorities: impl IntoIterator<Item = ZoneAuthority>) {
    for authority in authorities {
        let _ = authority.release();
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
        zones: BTreeMap<String, MemoryZoneState>,
    }

    #[derive(Debug, Default)]
    struct MemoryZoneState {
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
            let zone = state.zones.entry(zone_id.to_owned()).or_default();
            if zone.owner.is_some() {
                return Ok(None);
            }
            zone.token += 1;
            zone.owner = Some(owner.to_owned());
            Ok(Some(ZoneLease {
                zone_id: zone_id.to_owned(),
                owner: owner.to_owned(),
                fencing_token: zone.token,
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
            let Some(zone) = state.zones.get(zone_id) else {
                return Ok(None);
            };
            Ok(
                (zone.owner.as_deref() == Some(owner) && zone.token == token).then(|| ZoneLease {
                    zone_id: zone_id.to_owned(),
                    owner: owner.to_owned(),
                    fencing_token: token,
                    expires_at: SystemTime::now() + Duration::from_secs(30),
                }),
            )
        }

        fn release(&self, zone_id: &str, owner: &str, token: i64) -> Result<bool, EngineError> {
            let mut state = self.state.lock().unwrap();
            let Some(zone) = state.zones.get_mut(zone_id) else {
                return Ok(false);
            };
            let held = zone.owner.as_deref() == Some(owner) && zone.token == token;
            if held {
                zone.owner = None;
            }
            Ok(held)
        }

        fn append_event(
            &self,
            zone_id: &str,
            owner: &str,
            token: i64,
            event: ZoneEventInput<'_>,
        ) -> Result<i64, EngineError> {
            self.append_events(zone_id, owner, token, &[event])
                .map(|mut sequences| sequences.remove(0))
        }

        fn append_events(
            &self,
            zone_id: &str,
            owner: &str,
            token: i64,
            events: &[ZoneEventInput<'_>],
        ) -> Result<Vec<i64>, EngineError> {
            let mut state = self.state.lock().unwrap();
            let zone = state.zones.entry(zone_id.to_owned()).or_default();
            if zone.owner.as_deref() != Some(owner) || zone.token != token {
                return Err(EngineError::new(
                    ErrorCode::Conflict,
                    "the zone lease is no longer authoritative",
                ));
            }
            let first_sequence = i64::try_from(zone.events.len()).map_err(|_| {
                EngineError::new(ErrorCode::Internal, "the zone event sequence overflowed")
            })?;
            let event_count = i64::try_from(events.len()).map_err(|_| {
                EngineError::new(ErrorCode::Internal, "the zone event batch is too large")
            })?;
            first_sequence.checked_add(event_count).ok_or_else(|| {
                EngineError::new(ErrorCode::Internal, "the zone event sequence overflowed")
            })?;
            let mut sequences = Vec::with_capacity(events.len());
            for event in events {
                let ZoneEventInput {
                    tick,
                    kind,
                    actor_id,
                    payload,
                    schema_version,
                } = *event;
                let sequence = first_sequence + i64::try_from(sequences.len()).unwrap();
                sequences.push(sequence);
                zone.events.push(ZoneEvent {
                    sequence,
                    tick,
                    occurred_at: SystemTime::now(),
                    kind: kind.to_owned(),
                    actor_id: actor_id.map(str::to_owned),
                    payload: payload.clone(),
                    schema_version,
                });
            }
            Ok(sequences)
        }

        fn events_after(
            &self,
            zone_id: &str,
            sequence: i64,
        ) -> Result<Vec<ZoneEvent>, EngineError> {
            let state = self.state.lock().unwrap();
            Ok(state
                .zones
                .get(zone_id)
                .into_iter()
                .flat_map(|zone| &zone.events)
                .filter(|event| event.sequence > sequence)
                .cloned()
                .collect())
        }
    }

    const ZONE: &str = "zone_00000000000000000000000001";
    const ZONE_B: &str = "zone_00000000000000000000000002";

    fn authority(store: &Arc<MemoryLeases>, owner: &str) -> ZoneAuthority {
        let store: Arc<dyn ZoneLeaseStore> = store.clone();
        ZoneAuthority::acquire(store, ZONE, owner, Duration::from_secs(30))
            .unwrap()
            .unwrap()
    }

    #[test]
    fn one_server_can_hold_independent_authority_for_multiple_zones() {
        let store = Arc::new(MemoryLeases::default());
        let store_dyn: Arc<dyn ZoneLeaseStore> = store.clone();
        let authorities = ZoneAuthoritySet::acquire(
            &store_dyn,
            [ZONE_B, ZONE],
            "owner-a",
            Duration::from_secs(30),
        )
        .unwrap()
        .unwrap();

        assert_eq!(authorities.zone_ids().collect::<Vec<_>>(), [ZONE, ZONE_B]);
        assert!(authorities.get(ZONE).is_some());
        assert!(authorities.get(ZONE_B).is_some());
        assert!(authorities.renew_all().unwrap());

        let other: Arc<dyn ZoneLeaseStore> = store.clone();
        assert!(
            ZoneAuthority::acquire(other, ZONE, "owner-b", Duration::from_secs(30))
                .unwrap()
                .is_none()
        );
        assert!(authorities.release_all().unwrap());

        let other: Arc<dyn ZoneLeaseStore> = store;
        assert!(
            ZoneAuthority::acquire(other, ZONE, "owner-b", Duration::from_secs(30))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn recovery_of_one_zone_does_not_fence_another_zones_log() {
        let store = Arc::new(MemoryLeases::default());
        let store_dyn: Arc<dyn ZoneLeaseStore> = store.clone();
        let authorities = ZoneAuthoritySet::acquire(
            &store_dyn,
            [ZONE, ZONE_B],
            "owner-a",
            Duration::from_secs(30),
        )
        .unwrap()
        .unwrap();
        let source = authorities.get(ZONE).unwrap();
        let destination = authorities.get(ZONE_B).unwrap();
        let payload = serde_json::json!({"checkpoint": true});

        assert_eq!(
            source
                .append_event(Tick(1), EVENT_WORLD_SNAPSHOT, None, &payload)
                .unwrap(),
            0
        );
        assert_eq!(
            destination
                .append_event(Tick(1), EVENT_WORLD_SNAPSHOT, None, &payload)
                .unwrap(),
            0
        );
        assert!(source.release().unwrap());

        let store_dyn: Arc<dyn ZoneLeaseStore> = store.clone();
        let recovered = ZoneAuthority::acquire(store_dyn, ZONE, "owner-b", Duration::from_secs(30))
            .unwrap()
            .unwrap();
        assert!(
            source
                .append_event(Tick(2), EVENT_WORLD_SNAPSHOT, None, &payload)
                .is_err()
        );
        assert_eq!(
            recovered
                .append_event(Tick(2), EVENT_WORLD_SNAPSHOT, None, &payload)
                .unwrap(),
            1
        );
        assert_eq!(
            destination
                .append_event(Tick(2), EVENT_WORLD_SNAPSHOT, None, &payload)
                .unwrap(),
            1
        );
        assert_eq!(recovered.events_after(0).unwrap().len(), 1);
        assert_eq!(destination.events_after(0).unwrap().len(), 1);
    }

    #[test]
    fn a_partial_zone_set_acquisition_releases_every_lease_it_took() {
        let store = Arc::new(MemoryLeases::default());
        let _occupied = {
            let store: Arc<dyn ZoneLeaseStore> = store.clone();
            ZoneAuthority::acquire(store, ZONE_B, "owner-b", Duration::from_secs(30))
                .unwrap()
                .unwrap()
        };
        let store_dyn: Arc<dyn ZoneLeaseStore> = store.clone();
        let authorities = ZoneAuthoritySet::acquire(
            &store_dyn,
            [ZONE, ZONE_B],
            "owner-a",
            Duration::from_secs(30),
        )
        .unwrap();
        assert!(authorities.is_none());

        let store_dyn: Arc<dyn ZoneLeaseStore> = store;
        assert!(
            ZoneAuthority::acquire(store_dyn, ZONE, "owner-c", Duration::from_secs(30))
                .unwrap()
                .is_some()
        );
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
