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
use engine_core::schema::SchemaVersion;
use engine_core::world_event::CalendarSnapshot;
use engine_sim::snapshot::ZoneSnapshot;

mod postgres;

pub use postgres::{PostgresSnapshotStore, PostgresZoneLeaseStore, postgres_config_from_env};

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
}
