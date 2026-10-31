//! Durable, file-backed world snapshots for the demo service.
//!
//! The demo has no database: snapshots are whole-aggregate files written by the
//! authority loop and loaded at startup. A [`WorldSnapshot`] carries the zone
//! snapshot plus the calendar and faction snapshots as one cross-validated
//! set, mirroring the [`CalendarPersistence`] commit contract: calendar state
//! and faction state are always written together because a conflict resolution
//! mutates both.
//!
//! Snapshots live behind a trusted seam. Loading validates the schema version
//! and the calendar/faction pairing; the gateway cross-validates the loaded
//! calendar against the loaded faction world before any session is accepted.
//! A token hash is never part of a snapshot, and session state (resume tokens,
//! connection IDs) stays in memory as before.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use engine_core::error::{EngineError, ErrorCode};
use engine_core::faction::FactionSnapshot;
use engine_core::schema::{PersistencePolicy, SchemaVersion};
use engine_core::world_event::CalendarSnapshot;
use engine_sim::entity::EntityRecord;
use engine_sim::snapshot::ZoneSnapshot;

mod postgres;

pub use postgres::{PostgresSnapshotStore, PostgresZoneLeaseStore, postgres_config_from_env};

/// Point-in-time result of an operator's read-only checkpoint verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotVerification {
    /// No snapshot or checkpoint event exists for this world.
    Absent,
    /// A legacy snapshot exists but is not paired with a checkpoint event.
    LegacyUncheckpointed { saved_at_ms: i64, tick: u64 },
    /// The snapshot row matches its latest durable checkpoint event and replay.
    Verified {
        event_sequence: i64,
        event_count: usize,
        saved_at_ms: i64,
        tick: u64,
    },
}

/// A currently-held zone lease. The fencing token changes on every takeover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneLease {
    pub zone_id: String,
    pub owner: String,
    pub fencing_token: i64,
    pub expires_at: std::time::SystemTime,
}

/// An event written to the per-zone append-only log.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ZoneEvent {
    pub sequence: i64,
    pub tick: i64,
    pub occurred_at: std::time::SystemTime,
    pub kind: String,
    pub actor_id: Option<String>,
    pub payload: serde_json::Value,
    pub schema_version: i32,
}

/// The schema version written by this service build.
pub const WORLD_SNAPSHOT_VERSION: SchemaVersion = SchemaVersion::new(1, 0);

/// Event that replaces or inserts one persistent entity after a checkpoint.
pub const EVENT_ENTITY_UPSERT: &str = "zone.entity.upsert";
/// Event that removes one persistent entity after a checkpoint.
pub const EVENT_ENTITY_REMOVE: &str = "zone.entity.remove";

/// One event to append under a zone lease.
#[derive(Debug, Clone, Copy)]
pub struct ZoneEventInput<'a> {
    /// Authoritative tick the event describes.
    pub tick: i64,
    /// Stable event kind, at most 128 bytes.
    pub kind: &'a str,
    /// Actor the event concerns, when any.
    pub actor_id: Option<&'a str>,
    /// JSON object payload; never credentials or tokens.
    pub payload: &'a serde_json::Value,
    /// Event payload schema version.
    pub schema_version: i32,
}

/// Rebuild the latest durable world state described by ordered zone events.
///
/// Snapshot events deliberately carry a complete [`WorldSnapshot`]. Applying
/// one replaces the prior recovered state; supported entity deltas after it
/// update persistent entity records in sequence order. Lifecycle events remain
/// audit-only. Unknown event kinds are ignored for compatibility.
///
/// # Errors
///
/// Returns an error when event sequences contain a gap, a snapshot event is
/// malformed, a delta is invalid, or a payload fails integrity verification.
pub fn replay_world_snapshot_events(
    events: &[ZoneEvent],
    world_id: &str,
) -> Result<Option<WorldSnapshot>, EngineError> {
    let mut expected_sequence = 0_i64;
    let mut recovered = None;
    for event in events {
        if event.sequence != expected_sequence {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "the zone event log has a sequence gap",
            )
            .with_detail("expected", expected_sequence.to_string())
            .with_detail("actual", event.sequence.to_string()));
        }
        expected_sequence = event.sequence.checked_add(1).ok_or_else(|| {
            EngineError::new(
                ErrorCode::InvalidRequest,
                "the zone event sequence cannot be advanced",
            )
        })?;

        if event.kind == "world.snapshot" {
            if let Some(snapshot) = decode_snapshot_event(event, world_id)? {
                recovered = Some(snapshot);
            }
        } else {
            apply_entity_delta(&mut recovered, event)?;
        }
    }
    if let Some(snapshot) = &recovered {
        snapshot.validate()?;
    }
    Ok(recovered)
}

fn decode_snapshot_event(
    event: &ZoneEvent,
    world_id: &str,
) -> Result<Option<WorldSnapshot>, EngineError> {
    if event.schema_version != 1 {
        return Err(
            replay_error("the zone snapshot event has an unsupported schema version")
                .with_detail("schema_version", event.schema_version.to_string()),
        );
    }
    let event_world = event
        .payload
        .get("world_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| replay_error("the zone snapshot event is missing its world ID"))?;
    if event_world != world_id {
        return Ok(None);
    }
    let snapshot_value =
        event.payload.get("snapshot").cloned().ok_or_else(|| {
            replay_error("the zone snapshot event is missing its snapshot payload")
        })?;
    let stored_hash = event
        .payload
        .get("snapshot_hash")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| replay_error("the zone snapshot event is missing its integrity hash"))?;
    let canonical = serde_json::to_string(&snapshot_value)
        .map_err(|_| replay_error("the zone snapshot event cannot be decoded"))?;
    if crate::auth::hash_hex(&canonical) != stored_hash {
        return Err(replay_error(
            "the zone snapshot event failed its integrity check",
        ));
    }
    let snapshot: WorldSnapshot = serde_json::from_value(snapshot_value)
        .map_err(|_| replay_error("the zone snapshot event is not a valid world snapshot"))?;
    snapshot.validate()?;
    Ok(Some(snapshot))
}

fn apply_entity_delta(
    recovered: &mut Option<WorldSnapshot>,
    event: &ZoneEvent,
) -> Result<(), EngineError> {
    let delta = matches!(
        event.kind.as_str(),
        EVENT_ENTITY_UPSERT | EVENT_ENTITY_REMOVE
    );
    if delta && event.schema_version != 1 {
        return Err(
            replay_error("the entity event has an unsupported schema version")
                .with_detail("schema_version", event.schema_version.to_string()),
        );
    }
    if delta {
        let Some(snapshot) = recovered else {
            return Err(replay_error(
                "the entity event appears before the first world checkpoint",
            ));
        };
        match event.kind.as_str() {
            EVENT_ENTITY_UPSERT => apply_entity_upsert(snapshot, event),
            EVENT_ENTITY_REMOVE => apply_entity_remove(snapshot, event),
            _ => unreachable!("delta kind was matched above"),
        }
    } else if event.kind.starts_with("zone.") {
        Err(replay_error("the zone event kind is not replayable")
            .with_detail("kind", event.kind.clone()))
    } else {
        Ok(())
    }
}

fn apply_entity_upsert(snapshot: &mut WorldSnapshot, event: &ZoneEvent) -> Result<(), EngineError> {
    let entity_value = event
        .payload
        .get("entity")
        .cloned()
        .ok_or_else(|| replay_error("the entity-upsert event is missing its entity payload"))?;
    let entity: EntityRecord = serde_json::from_value(entity_value)
        .map_err(|_| replay_error("the entity-upsert event contains an invalid entity"))?;
    let entity_id = entity.id.to_string();
    validate_entity_delta_tick(snapshot, event, &entity_id)?;
    if event.actor_id.as_deref() != Some(entity_id.as_str()) {
        return Err(replay_error(
            "the entity-upsert actor does not match its entity",
        ));
    }
    if entity.persistence != PersistencePolicy::PersistentEntity {
        return Err(replay_error(
            "the entity-upsert event does not describe a persistent entity",
        ));
    }
    if let Some(existing) = snapshot
        .zone
        .entities
        .iter_mut()
        .find(|value| value.id == entity.id)
    {
        *existing = entity;
    } else {
        snapshot.zone.entities.push(entity);
        snapshot.zone.entities.sort_by_key(|value| value.id);
    }
    snapshot.zone.tick.0 =
        u64::try_from(event.tick).map_err(|_| replay_error("the entity event tick is negative"))?;
    Ok(())
}

fn apply_entity_remove(snapshot: &mut WorldSnapshot, event: &ZoneEvent) -> Result<(), EngineError> {
    let entity_id = event
        .payload
        .get("entity_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| replay_error("the entity-remove event is missing its entity ID"))?;
    validate_entity_delta_tick(snapshot, event, entity_id)?;
    if event.actor_id.as_deref() != Some(entity_id) {
        return Err(replay_error(
            "the entity-remove actor does not match its entity",
        ));
    }
    let Some(index) = snapshot
        .zone
        .entities
        .iter()
        .position(|value| value.id.to_string() == entity_id)
    else {
        return Err(replay_error(
            "the entity-remove event references an absent entity",
        ));
    };
    snapshot.zone.entities.remove(index);
    snapshot.zone.tick.0 =
        u64::try_from(event.tick).map_err(|_| replay_error("the entity event tick is negative"))?;
    Ok(())
}

fn validate_entity_delta_tick(
    snapshot: &WorldSnapshot,
    event: &ZoneEvent,
    entity_id: &str,
) -> Result<(), EngineError> {
    let tick =
        u64::try_from(event.tick).map_err(|_| replay_error("the entity event tick is negative"))?;
    if tick < snapshot.zone.tick.0 {
        return Err(
            replay_error("the entity event predates the recovered checkpoint")
                .with_detail("entity_id", entity_id)
                .with_detail("checkpoint_tick", snapshot.zone.tick.0.to_string())
                .with_detail("event_tick", tick.to_string()),
        );
    }
    Ok(())
}

fn replay_error(message: &'static str) -> EngineError {
    EngineError::new(ErrorCode::InvalidRequest, message)
}

/// Single-writer locking and the append-only event log for one zone.
///
/// A holder proves authority with the lease's fencing token; every write that
/// depends on ownership must present the current token. A takeover increments
/// the token, so a process that lost the lease is rejected instead of writing
/// stale state. Implementations are shared behind an `Arc` and must serialize
/// connection access internally.
pub trait ZoneLeaseStore: std::fmt::Debug + Send + Sync {
    /// Acquire an absent or expired lease, or `None` when another owner holds a
    /// live one.
    ///
    /// # Errors
    /// Returns an engine error when the store cannot be reached.
    fn acquire(
        &self,
        zone_id: &str,
        owner: &str,
        ttl: std::time::Duration,
    ) -> Result<Option<ZoneLease>, EngineError>;

    /// Extend a lease that is still owned by `owner` under `token`.
    ///
    /// # Errors
    /// Returns an engine error when the store cannot be reached.
    fn renew(
        &self,
        zone_id: &str,
        owner: &str,
        token: i64,
        ttl: std::time::Duration,
    ) -> Result<Option<ZoneLease>, EngineError>;

    /// Expire a lease so another owner may take it over immediately.
    ///
    /// # Errors
    /// Returns an engine error when the store cannot be reached.
    fn release(&self, zone_id: &str, owner: &str, token: i64) -> Result<bool, EngineError>;

    /// Append one event atomically with fence validation and gap-free
    /// sequencing.
    ///
    /// # Errors
    /// Returns [`ErrorCode::Conflict`] when the lease was lost or superseded,
    /// and an engine error when the store cannot be reached.
    fn append_event(
        &self,
        zone_id: &str,
        owner: &str,
        token: i64,
        event: ZoneEventInput<'_>,
    ) -> Result<i64, EngineError>;

    /// Append a group of events atomically under the same lease fence.
    ///
    /// Implementations must either persist every event in order or persist
    /// none of them. This is used when one simulation tick changes multiple
    /// persistent entity records.
    fn append_events(
        &self,
        zone_id: &str,
        owner: &str,
        token: i64,
        events: &[ZoneEventInput<'_>],
    ) -> Result<Vec<i64>, EngineError>;

    /// Read events strictly after `sequence`, ordered by sequence.
    ///
    /// # Errors
    /// Returns an engine error when the store cannot be reached.
    fn events_after(&self, zone_id: &str, sequence: i64) -> Result<Vec<ZoneEvent>, EngineError>;
}

/// Everything needed to resume the world after a restart.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WorldSnapshot {
    /// Version written when the snapshot was created.
    pub schema_version: SchemaVersion,
    /// Wall-clock time of the snapshot, in epoch milliseconds.
    pub saved_at_ms: i64,
    /// Authoritative zone state.
    pub zone: ZoneSnapshot,
    /// Calendar aggregate, present when this world runs a calendar.
    pub calendar: Option<CalendarSnapshot>,
    /// Faction aggregate, always paired with `calendar`.
    pub factions: Option<FactionSnapshot>,
}

impl WorldSnapshot {
    /// Capture a snapshot of the authoritative world, stamped at `now_ms`.
    ///
    /// The zone state comes from the gateway's zone; the calendar and faction
    /// snapshots are written together when the gateway owns a calendar stage.
    #[must_use]
    pub fn capture(inputs: &'_ WorldSnapshotInputs, now_ms: i64) -> Self {
        let (calendar, factions) = match &inputs.calendar_factions {
            Some((calendar, factions)) => (Some(calendar.clone()), Some(factions.clone())),
            None => (None, None),
        };
        Self {
            schema_version: WORLD_SNAPSHOT_VERSION,
            saved_at_ms: now_ms,
            zone: inputs.zone.clone(),
            calendar,
            factions,
        }
    }

    /// Validate structural invariants before the gateway sees the state.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] for an unsupported version or an
    /// unpaired calendar/faction aggregate.
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.schema_version != WORLD_SNAPSHOT_VERSION {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "unsupported world snapshot schema version",
            )
            .with_detail("snapshot", self.schema_version.to_string())
            .with_detail("expected", WORLD_SNAPSHOT_VERSION.to_string()));
        }
        if self.calendar.is_some() != self.factions.is_some() {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "calendar and faction snapshots must be written together",
            ));
        }
        Ok(())
    }
}

/// The parts of a running world a snapshot is built from.
#[derive(Debug, Clone)]
pub struct WorldSnapshotInputs {
    /// Authoritative zone state.
    pub zone: ZoneSnapshot,
    /// Calendar and faction snapshots, present when the world runs a calendar.
    pub calendar_factions: Option<(CalendarSnapshot, FactionSnapshot)>,
}

/// Read and write world snapshots. Implementations own the storage location.
///
/// `load` returns `Ok(None)` when no snapshot exists yet; every other failure
/// is an engine error. File-based implementations must write atomically enough
/// that a crashed process never leaves the previous good snapshot unreadable.
pub trait SnapshotStore: std::fmt::Debug {
    /// Load the latest snapshot, or `None` before the first save.
    ///
    /// # Errors
    ///
    /// Returns an engine error when the snapshot exists but is unreadable or
    /// structurally invalid.
    fn load(&self) -> Result<Option<WorldSnapshot>, EngineError>;

    /// Persist a snapshot, replacing any previous one.
    ///
    /// # Errors
    ///
    /// Returns an engine error when the snapshot could not be written. The
    /// in-memory world remains authoritative either way.
    fn save(&self, snapshot: &WorldSnapshot) -> Result<(), EngineError>;

    /// Persist a snapshot at `tick`, recording its recovery event when the
    /// backing store supports a transactional event log.
    ///
    /// File and memory stores have no shared event log, so their default is a
    /// normal snapshot write. PostgreSQL writes the snapshot and its replayable
    /// event in one transaction under the current zone lease.
    fn save_at_tick(&self, snapshot: &WorldSnapshot, _tick: i64) -> Result<(), EngineError> {
        self.save(snapshot)
    }

    /// Whether [`Self::save_at_tick`] atomically records a snapshot event.
    ///
    /// Service implementations use this to retain the non-transactional
    /// lifecycle marker for file and test stores.
    fn records_snapshot_events_transactionally(&self) -> bool {
        false
    }
}

/// A single-file snapshot store.
///
/// Writes go to a sibling temporary file first and are then renamed over the
/// target, so a write failure leaves the previous snapshot intact. `load`
/// reads the same file the previous process last wrote.
#[derive(Debug, Clone)]
pub struct FileSnapshotStore {
    path: PathBuf,
}

impl FileSnapshotStore {
    /// A store backed by the file at `path`.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The configured snapshot file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl SnapshotStore for FileSnapshotStore {
    fn load(&self) -> Result<Option<WorldSnapshot>, EngineError> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(EngineError::new(
                    ErrorCode::Internal,
                    "the world snapshot file could not be read",
                )
                .with_detail("path", self.path.display().to_string())
                .with_detail("error", error.to_string()));
            }
        };
        let snapshot: WorldSnapshot = serde_json::from_slice(&bytes).map_err(|error| {
            EngineError::new(
                ErrorCode::InvalidRequest,
                "the world snapshot file is not a valid world snapshot",
            )
            .with_detail("path", self.path.display().to_string())
            .with_detail("error", error.to_string())
        })?;
        snapshot.validate()?;
        Ok(Some(snapshot))
    }

    fn save(&self, snapshot: &WorldSnapshot) -> Result<(), EngineError> {
        snapshot.validate()?;
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|error| io_error(&error, &self.path))?;
        }
        let temp = self.path.with_extension("tmp");
        let body = serde_json::to_vec_pretty(snapshot).map_err(|error| {
            EngineError::new(
                ErrorCode::Internal,
                "the world snapshot could not be encoded",
            )
            .with_detail("error", error.to_string())
        })?;
        fs::write(&temp, body).map_err(|error| io_error(&error, &temp))?;
        // Rename is not atomic on every platform, but the previous snapshot
        // stays readable while the temporary file is being written, which is
        // what this seam promises.
        fs::rename(&temp, &self.path).map_err(|error| io_error(&error, &self.path))?;
        Ok(())
    }
}

fn io_error(error: &io::Error, path: &Path) -> EngineError {
    EngineError::new(
        ErrorCode::Internal,
        "the world snapshot file could not be written",
    )
    .with_detail("path", path.display().to_string())
    .with_detail("error", error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo;
    use engine_core::id::{DefinitionId, IdKind, StableId};
    use engine_core::inventory::UniformItemRules;
    use engine_core::math::Vec3;
    use engine_sim::entity::EntityRecord;
    use engine_sim::nav::NavGrid;
    use engine_sim::zone::{Zone, ZoneConfig};

    fn zone() -> Zone {
        let config = ZoneConfig::new(
            DefinitionId::new("zone.demo").unwrap(),
            StableId::from_parts(IdKind::Zone, 1, 1),
            Vec3::ZERO,
            Vec3::new(20.0, 4.0, 20.0),
        )
        .unwrap();
        let nav = NavGrid::new(10, 10, 2.0, Vec3::ZERO).unwrap();
        Zone::new(config, nav, std::sync::Arc::new(UniformItemRules::new(9))).unwrap()
    }

    fn world_snapshot() -> WorldSnapshot {
        let mut factions = demo::build_faction_world(0).unwrap();
        let calendar = demo::build_calendar(&mut factions, 0).unwrap();
        WorldSnapshot {
            schema_version: WORLD_SNAPSHOT_VERSION,
            saved_at_ms: 1_000,
            zone: zone().snapshot(),
            calendar: Some(calendar.snapshot()),
            factions: Some(factions.snapshot()),
        }
    }

    #[test]
    fn a_snapshot_round_trips_through_a_file() {
        let dir = std::env::temp_dir().join("lynx-world-snapshot-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("round-trip.json");
        let _ = std::fs::remove_file(&path);
        let store = FileSnapshotStore::new(&path);

        assert_eq!(store.load().unwrap(), None, "no snapshot exists yet");

        let snapshot = world_snapshot();
        store.save(&snapshot).unwrap();
        let reloaded = store.load().unwrap().unwrap();
        assert_eq!(reloaded, snapshot);
        assert!(
            !path.with_extension("tmp").exists(),
            "the temporary file is gone"
        );
    }

    #[test]
    fn a_broken_snapshot_file_is_rejected_instead_of_imported() {
        let dir = std::env::temp_dir().join("lynx-world-snapshot-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("broken.json");
        std::fs::write(&path, b"{ not json }").unwrap();
        let error = FileSnapshotStore::new(&path).load().unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidRequest);
    }

    #[test]
    fn an_unpaired_calendar_snapshot_is_rejected() {
        let dir = std::env::temp_dir().join("lynx-world-snapshot-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("unpaired.json");
        let store = FileSnapshotStore::new(&path);

        let mut snapshot = world_snapshot();
        snapshot.factions = None;
        let error = store.save(&snapshot).unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidRequest);
        assert_eq!(store.load().unwrap(), None, "nothing valid was written");

        let mut future = world_snapshot();
        future.schema_version = SchemaVersion::new(9, 9);
        let error = store.save(&future).unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidRequest);
    }

    #[test]
    fn a_storage_failure_is_reported_not_swallowed() {
        // Saving over a directory fails on every supported platform.
        let dir = std::env::temp_dir().join("lynx-world-snapshot-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("as-directory.json");
        std::fs::create_dir_all(&path).unwrap();
        let error = FileSnapshotStore::new(&path)
            .save(&world_snapshot())
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::Internal);
    }

    fn snapshot_event(sequence: i64, snapshot: &WorldSnapshot) -> ZoneEvent {
        let value = serde_json::to_value(snapshot).unwrap();
        let hash = crate::auth::hash_hex(&serde_json::to_string(&value).unwrap());
        ZoneEvent {
            sequence,
            tick: sequence,
            occurred_at: std::time::SystemTime::UNIX_EPOCH,
            kind: "world.snapshot".to_owned(),
            actor_id: None,
            payload: serde_json::json!({
                "world_id": "demo-world",
                "snapshot": value,
                "snapshot_hash": hash,
            }),
            schema_version: 1,
        }
    }

    #[test]
    fn replaying_snapshot_events_recovers_the_latest_live_state() {
        let first = world_snapshot();
        let mut live = first.clone();
        let mut advanced_zone = zone();
        for _ in 0..3 {
            advanced_zone.advance_tick();
        }
        live.zone = advanced_zone.snapshot();
        live.saved_at_ms = 2_000;
        let events = vec![snapshot_event(0, &first), snapshot_event(1, &live)];

        assert_eq!(
            replay_world_snapshot_events(&events, "demo-world").unwrap(),
            Some(live)
        );
        assert_eq!(
            replay_world_snapshot_events(&events, "demo-world").unwrap(),
            replay_world_snapshot_events(&events, "demo-world").unwrap(),
            "repeated recovery is deterministic"
        );
    }

    #[test]
    fn replay_applies_persistent_entity_upserts_and_removals_after_checkpoint() {
        let baseline = world_snapshot();
        let entity = EntityRecord::new(
            StableId::from_parts(IdKind::Entity, 12, 1),
            DefinitionId::new("entity.test").unwrap(),
            Vec3::new(2.0, 0.0, 3.0),
        )
        .with_persistence(PersistencePolicy::PersistentEntity);
        let entity_id = entity.id.to_string();
        let upsert = ZoneEvent {
            sequence: 1,
            tick: 2,
            occurred_at: std::time::SystemTime::UNIX_EPOCH,
            kind: EVENT_ENTITY_UPSERT.to_owned(),
            actor_id: Some(entity_id.clone()),
            payload: serde_json::json!({ "entity": entity }),
            schema_version: 1,
        };
        let mut recovered = baseline.clone();
        recovered
            .zone
            .entities
            .push(serde_json::from_value(upsert.payload["entity"].clone()).unwrap());
        recovered.zone.tick.0 = 2;
        let remove = ZoneEvent {
            sequence: 2,
            tick: 3,
            occurred_at: std::time::SystemTime::UNIX_EPOCH,
            kind: EVENT_ENTITY_REMOVE.to_owned(),
            actor_id: Some(entity_id.clone()),
            payload: serde_json::json!({ "entity_id": entity_id }),
            schema_version: 1,
        };
        recovered.zone.entities.clear();
        recovered.zone.tick.0 = 3;

        assert_eq!(
            replay_world_snapshot_events(
                &[snapshot_event(0, &baseline), upsert, remove],
                "demo-world"
            )
            .unwrap(),
            Some(recovered)
        );
    }

    #[test]
    fn replay_rejects_nonpersistent_or_misidentified_entity_deltas() {
        let baseline = world_snapshot();
        let entity = EntityRecord::new(
            StableId::from_parts(IdKind::Entity, 13, 1),
            DefinitionId::new("entity.test").unwrap(),
            Vec3::ZERO,
        );
        let event = ZoneEvent {
            sequence: 1,
            tick: 1,
            occurred_at: std::time::SystemTime::UNIX_EPOCH,
            kind: EVENT_ENTITY_UPSERT.to_owned(),
            actor_id: Some("wrong-actor".to_owned()),
            payload: serde_json::json!({ "entity": entity }),
            schema_version: 1,
        };
        let error =
            replay_world_snapshot_events(&[snapshot_event(0, &baseline), event], "demo-world")
                .unwrap_err();
        assert!(error.to_string().contains("does not match its entity"));
    }

    #[test]
    fn replay_rejects_a_missing_or_corrupt_snapshot_event() {
        let snapshot = world_snapshot();
        let mut missing = snapshot_event(1, &snapshot);
        let gap = replay_world_snapshot_events(&[missing.clone()], "demo-world").unwrap_err();
        assert_eq!(gap.code(), ErrorCode::InvalidRequest);

        missing.sequence = 0;
        missing.payload["snapshot_hash"] = serde_json::json!("not-a-real-hash");
        let corrupt = replay_world_snapshot_events(&[missing], "demo-world").unwrap_err();
        assert_eq!(corrupt.code(), ErrorCode::InvalidRequest);
    }
}
