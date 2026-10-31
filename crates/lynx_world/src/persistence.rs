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

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use engine_core::error::{EngineError, ErrorCode};
use engine_core::faction::{FactionSnapshot, FactionWorld};
use engine_core::id::{IdKind, StableId};
use engine_core::inventory::TransferReceipt;
use engine_core::schema::{PersistencePolicy, SchemaVersion};
use engine_core::time::Tick;
use engine_core::world_event::{CalendarSnapshot, WorldCalendar};
use engine_sim::action::ActionKey;
use engine_sim::entity::EntityRecord;
use engine_sim::link::{EntityLink, LinkEvent};
use engine_sim::snapshot::{QuarantinedRecord, ZoneSnapshot};

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

/// Documented upper bound for restoring world state during startup.
pub const RECOVERY_TARGET: std::time::Duration = std::time::Duration::from_secs(60);

/// Event that replaces or inserts one persistent entity after a checkpoint.
pub const EVENT_ENTITY_UPSERT: &str = "zone.entity.upsert";
/// Event that removes one persistent entity after a checkpoint.
pub const EVENT_ENTITY_REMOVE: &str = "zone.entity.remove";
/// Event that replaces the zone's persisted link list.
pub const EVENT_LINKS_REPLACE: &str = "zone.links.replace";
/// Event that replaces the zone's active action cooldowns.
pub const EVENT_COOLDOWNS_REPLACE: &str = "zone.cooldowns.replace";
/// Event that replaces the zone's active link cooldowns.
pub const EVENT_LINK_COOLDOWNS_REPLACE: &str = "zone.link-cooldowns.replace";
/// Event that appends newly applied inventory-transfer receipts.
pub const EVENT_TRANSFERS_APPEND: &str = "zone.transfers.append";
/// Event that replaces the paired calendar and faction aggregates.
pub const EVENT_CALENDAR_FACTIONS_REPLACE: &str = "world.calendar-factions.replace";

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
/// one replaces the prior recovered state; supported deltas after it update
/// entities, links, cooldowns, transfer receipts, and the paired calendar and
/// faction aggregates in sequence order. Lifecycle events remain audit-only.
/// Unknown non-zone event kinds are ignored for compatibility.
///
/// A retained log may begin after sequence zero, but only when it begins with a
/// full world checkpoint; that checkpoint supplies the baseline the remaining
/// deltas build on. Gaps after the first event are always rejected.
///
/// # Errors
///
/// Returns an error when event sequences contain a gap, a snapshot event is
/// malformed, a delta is invalid, or a payload fails integrity verification.
pub fn replay_world_snapshot_events(
    events: &[ZoneEvent],
    world_id: &str,
) -> Result<Option<WorldSnapshot>, EngineError> {
    let mut expected_sequence = match events.first() {
        Some(first) if first.sequence != 0 && first.kind != "world.snapshot" => {
            return Err(
                replay_error("a retained event log must begin with a world checkpoint")
                    .with_detail("sequence", first.sequence.to_string())
                    .with_detail("kind", first.kind.clone()),
            );
        }
        Some(first) => first.sequence,
        None => 0,
    };
    let mut recovered: Option<WorldSnapshot> = None;
    // Entity deltas are applied through a keyed map so replay stays O(log n)
    // per event and always emits the canonical stable-ID order of a snapshot.
    let mut entities: BTreeMap<StableId, EntityRecord> = BTreeMap::new();
    let mut last_tick = 0_u64;
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
        apply_replayed_event(
            event,
            world_id,
            &mut recovered,
            &mut entities,
            &mut last_tick,
        )?;
    }
    if let Some(mut snapshot) = recovered {
        snapshot.zone.entities = entities.into_values().collect();
        snapshot.validate()?;
        Ok(Some(snapshot))
    } else {
        Ok(None)
    }
}

fn apply_replayed_event(
    event: &ZoneEvent,
    world_id: &str,
    recovered: &mut Option<WorldSnapshot>,
    entities: &mut BTreeMap<StableId, EntityRecord>,
    last_tick: &mut u64,
) -> Result<(), EngineError> {
    match event.kind.as_str() {
        "world.snapshot" => {
            if let Some(snapshot) = decode_snapshot_event(event, world_id)? {
                let tick = snapshot.zone.tick.0;
                if tick < *last_tick {
                    return Err(
                        replay_error("a world checkpoint moves the recovered tick backwards")
                            .with_detail("previous_tick", last_tick.to_string())
                            .with_detail("checkpoint_tick", tick.to_string()),
                    );
                }
                *last_tick = tick;
                *entities = snapshot
                    .zone
                    .entities
                    .iter()
                    .cloned()
                    .map(|entity| (entity.id, entity))
                    .collect();
                *recovered = Some(snapshot);
            }
            Ok(())
        }
        EVENT_ENTITY_UPSERT => {
            let snapshot = recovered_snapshot(recovered)?;
            let tick = delta_tick(event, last_tick)?;
            apply_entity_upsert(snapshot, entities, event, tick)
        }
        EVENT_ENTITY_REMOVE => {
            let snapshot = recovered_snapshot(recovered)?;
            let tick = delta_tick(event, last_tick)?;
            apply_entity_remove(snapshot, entities, event, tick)
        }
        EVENT_LINKS_REPLACE => {
            let snapshot = recovered_snapshot(recovered)?;
            let tick = delta_tick(event, last_tick)?;
            apply_links_replace(snapshot, entities, event, tick)
        }
        EVENT_COOLDOWNS_REPLACE => {
            let snapshot = recovered_snapshot(recovered)?;
            let tick = delta_tick(event, last_tick)?;
            apply_cooldowns_replace(snapshot, entities, event, tick)
        }
        EVENT_LINK_COOLDOWNS_REPLACE => {
            let snapshot = recovered_snapshot(recovered)?;
            let tick = delta_tick(event, last_tick)?;
            apply_link_cooldowns_replace(snapshot, entities, event, tick)
        }
        EVENT_TRANSFERS_APPEND => {
            let snapshot = recovered_snapshot(recovered)?;
            let tick = delta_tick(event, last_tick)?;
            apply_transfers_append(snapshot, event, tick)
        }
        EVENT_CALENDAR_FACTIONS_REPLACE => {
            let snapshot = recovered_snapshot(recovered)?;
            let tick = delta_tick(event, last_tick)?;
            apply_calendar_factions_replace(snapshot, event, tick)
        }
        kind if kind.starts_with("zone.") => Err(replay_error("the zone event kind is not replayable")
            .with_detail("kind", event.kind.clone())),
        _ => Ok(()),
    }
}

fn recovered_snapshot(
    recovered: &mut Option<WorldSnapshot>,
) -> Result<&mut WorldSnapshot, EngineError> {
    recovered.as_mut().ok_or_else(|| {
        replay_error("a durable delta appears before the first world checkpoint")
    })
}

fn delta_tick(event: &ZoneEvent, last_tick: &mut u64) -> Result<u64, EngineError> {
    if event.schema_version != 1 {
        return Err(
            replay_error("the durable delta has an unsupported schema version")
                .with_detail("schema_version", event.schema_version.to_string()),
        );
    }
    let tick = event_tick(event)?;
    if tick < *last_tick {
        return Err(
            replay_error("a durable delta moves the recovered tick backwards")
                .with_detail("previous_tick", last_tick.to_string())
                .with_detail("event_tick", tick.to_string()),
        );
    }
    *last_tick = tick;
    Ok(tick)
}

fn event_payload_field<T: serde::de::DeserializeOwned>(
    event: &ZoneEvent,
    field: &str,
    message: &'static str,
) -> Result<T, EngineError> {
    let value = event
        .payload
        .get(field)
        .cloned()
        .ok_or_else(|| replay_error(message).with_detail("field", field))?;
    serde_json::from_value(value).map_err(|_| replay_error(message).with_detail("field", field))
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

fn event_tick(event: &ZoneEvent) -> Result<u64, EngineError> {
    u64::try_from(event.tick).map_err(|_| replay_error("the entity event tick is negative"))
}

fn apply_entity_upsert(
    snapshot: &mut WorldSnapshot,
    entities: &mut BTreeMap<StableId, EntityRecord>,
    event: &ZoneEvent,
    tick: u64,
) -> Result<(), EngineError> {
    let entity_value = event
        .payload
        .get("entity")
        .cloned()
        .ok_or_else(|| replay_error("the entity-upsert event is missing its entity payload"))?;
    let entity: EntityRecord = serde_json::from_value(entity_value)
        .map_err(|_| replay_error("the entity-upsert event contains an invalid entity"))?;
    entity.id.expect_kind(IdKind::Entity)?;
    let entity_id = entity.id.to_string();
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
    entities.insert(entity.id, entity);
    snapshot.zone.tick.0 = tick;
    Ok(())
}

fn apply_entity_remove(
    snapshot: &mut WorldSnapshot,
    entities: &mut BTreeMap<StableId, EntityRecord>,
    event: &ZoneEvent,
    tick: u64,
) -> Result<(), EngineError> {
    let entity_id = event
        .payload
        .get("entity_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| replay_error("the entity-remove event is missing its entity ID"))?;
    let parsed = StableId::parse(entity_id)
        .map_err(|_| replay_error("the entity-remove event has an invalid entity ID"))?;
    parsed.expect_kind(IdKind::Entity)?;
    if event.actor_id.as_deref() != Some(entity_id) {
        return Err(replay_error(
            "the entity-remove actor does not match its entity",
        ));
    }
    if entities.remove(&parsed).is_none() {
        return Err(
            replay_error("the entity-remove event references an absent entity")
                .with_detail("entity_id", entity_id),
        );
    }
    // Mirror `Zone::despawn`: a removed entity cannot remain a link endpoint,
    // cooldown actor, cooldown target, or link-cooldown endpoint.
    snapshot
        .zone
        .links
        .retain(|link| link.source != parsed && link.target != parsed);
    snapshot
        .zone
        .cooldowns
        .retain(|(key, _)| key.0 != parsed && key.2 != Some(parsed));
    snapshot
        .zone
        .link_cooldowns
        .retain(|((source, target, _), _)| *source != parsed && *target != parsed);
    snapshot.zone.tick.0 = tick;
    Ok(())
}

fn apply_links_replace(
    snapshot: &mut WorldSnapshot,
    entities: &BTreeMap<StableId, EntityRecord>,
    event: &ZoneEvent,
    tick: u64,
) -> Result<(), EngineError> {
    let links: Vec<EntityLink> = event_payload_field(
        event,
        "links",
        "the links-replace event contains an invalid link list",
    )?;
    for link in &links {
        if !entities.contains_key(&link.source) || !entities.contains_key(&link.target) {
            return Err(
                replay_error("the links-replace event references an absent entity")
                    .with_detail("source", link.source.to_string())
                    .with_detail("target", link.target.to_string()),
            );
        }
    }
    snapshot.zone.links = links;
    snapshot.zone.tick.0 = tick;
    Ok(())
}

fn apply_cooldowns_replace(
    snapshot: &mut WorldSnapshot,
    entities: &BTreeMap<StableId, EntityRecord>,
    event: &ZoneEvent,
    tick: u64,
) -> Result<(), EngineError> {
    let cooldowns: Vec<(ActionKey, Tick)> = event_payload_field(
        event,
        "cooldowns",
        "the cooldowns-replace event contains an invalid cooldown list",
    )?;
    for (key, _) in &cooldowns {
        if !entities.contains_key(&key.0)
            || key.2.is_some_and(|target| !entities.contains_key(&target))
        {
            return Err(
                replay_error("the cooldowns-replace event references an absent entity")
                    .with_detail("actor", key.0.to_string())
                    .with_detail(
                        "target",
                        key.2.map_or_else(|| "none".to_owned(), |target| target.to_string()),
                    ),
            );
        }
    }
    snapshot.zone.cooldowns = cooldowns;
    snapshot.zone.tick.0 = tick;
    Ok(())
}

fn apply_link_cooldowns_replace(
    snapshot: &mut WorldSnapshot,
    entities: &BTreeMap<StableId, EntityRecord>,
    event: &ZoneEvent,
    tick: u64,
) -> Result<(), EngineError> {
    let link_cooldowns: Vec<((StableId, StableId, LinkEvent), Tick)> = event_payload_field(
        event,
        "link_cooldowns",
        "the link-cooldowns-replace event contains an invalid cooldown list",
    )?;
    for ((source, target, _), _) in &link_cooldowns {
        if !entities.contains_key(source) || !entities.contains_key(target) {
            return Err(
                replay_error("the link-cooldowns-replace event references an absent entity")
                    .with_detail("source", source.to_string())
                    .with_detail("target", target.to_string()),
            );
        }
    }
    snapshot.zone.link_cooldowns = link_cooldowns;
    snapshot.zone.tick.0 = tick;
    Ok(())
}

fn apply_transfers_append(
    snapshot: &mut WorldSnapshot,
    event: &ZoneEvent,
    tick: u64,
) -> Result<(), EngineError> {
    let appended: Vec<TransferReceipt> = event_payload_field(
        event,
        "transfers",
        "the transfers-append event contains an invalid receipt list",
    )?;
    let mut by_key: BTreeMap<String, TransferReceipt> = snapshot
        .zone
        .transfers
        .drain(..)
        .map(|receipt| (receipt.key.clone(), receipt))
        .collect();
    for receipt in appended {
        if receipt.key.is_empty() || receipt.quantity == 0 {
            return Err(replay_error(
                "the transfers-append event contains an invalid receipt",
            ));
        }
        receipt.source.expect_kind(IdKind::Entity)?;
        receipt.destination.expect_kind(IdKind::Entity)?;
        by_key.insert(receipt.key.clone(), receipt);
    }
    snapshot.zone.transfers = by_key.into_values().collect();
    snapshot.zone.tick.0 = tick;
    Ok(())
}

fn apply_calendar_factions_replace(
    snapshot: &mut WorldSnapshot,
    event: &ZoneEvent,
    tick: u64,
) -> Result<(), EngineError> {
    let calendar: Option<CalendarSnapshot> = event_payload_field(
        event,
        "calendar",
        "the calendar-factions-replace event contains an invalid calendar",
    )?;
    let factions: Option<FactionSnapshot> = event_payload_field(
        event,
        "factions",
        "the calendar-factions-replace event contains invalid faction state",
    )?;
    match (calendar, factions) {
        (Some(calendar), Some(factions)) => {
            calendar.validate()?;
            factions.validate()?;
            let faction_world = FactionWorld::from_snapshot(factions.clone())?;
            WorldCalendar::from_snapshot(calendar.clone())?
                .validate_against(&faction_world)?;
            snapshot.calendar = Some(calendar);
            snapshot.factions = Some(factions);
        }
        (None, None) => {
            snapshot.calendar = None;
            snapshot.factions = None;
        }
        _ => {
            return Err(replay_error(
                "the calendar-factions-replace event must pair both aggregates",
            ));
        }
    }
    snapshot.zone.tick.0 = tick;
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

    /// Persist entries isolated during recovery so they stay operator-visible.
    ///
    /// Stores without a durable quarantine location accept the report and rely
    /// on the caller's log. File and PostgreSQL stores keep it next to the
    /// snapshot so a later `verify-snapshot` can report it.
    fn record_quarantine(
        &self,
        _zone_id: &str,
        _records: &[QuarantinedRecord],
    ) -> Result<(), EngineError> {
        Ok(())
    }

    /// Read the durable quarantine report for this world and zone.
    fn quarantine_records(&self, _zone_id: &str) -> Result<Vec<QuarantinedRecord>, EngineError> {
        Ok(Vec::new())
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

    fn record_quarantine(
        &self,
        _zone_id: &str,
        records: &[QuarantinedRecord],
    ) -> Result<(), EngineError> {
        if records.is_empty() {
            return Ok(());
        }
        let path = self.quarantine_path();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|error| io_error(&error, &path))?;
        }
        let body = serde_json::to_vec_pretty(records).map_err(|error| {
            EngineError::new(
                ErrorCode::Internal,
                "the recovery quarantine report could not be encoded",
            )
            .with_detail("error", error.to_string())
        })?;
        let temp = path.with_extension("tmp");
        fs::write(&temp, body).map_err(|error| io_error(&error, &temp))?;
        fs::rename(&temp, &path).map_err(|error| io_error(&error, &path))?;
        Ok(())
    }

    fn quarantine_records(&self, _zone_id: &str) -> Result<Vec<QuarantinedRecord>, EngineError> {
        let bytes = match fs::read(self.quarantine_path()) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(EngineError::new(
                    ErrorCode::Internal,
                    "the recovery quarantine report could not be read",
                )
                .with_detail("path", self.quarantine_path().display().to_string())
                .with_detail("error", error.to_string()));
            }
        };
        serde_json::from_slice(&bytes).map_err(|error| {
            EngineError::new(
                ErrorCode::InvalidRequest,
                "the recovery quarantine report is not valid",
            )
            .with_detail("path", self.quarantine_path().display().to_string())
            .with_detail("error", error.to_string())
        })
    }
}

impl FileSnapshotStore {
    /// Sidecar path holding the last recovery quarantine report.
    #[must_use]
    pub fn quarantine_path(&self) -> PathBuf {
        self.path.with_extension("quarantine.json")
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
    fn replay_applies_non_entity_state_replacements() {
        use engine_core::inventory::TransferReceipt;
        use engine_sim::action::Verb;
        use engine_sim::link::EntityLink;

        let mut baseline = world_snapshot();
        baseline.zone.entities = [1_u128, 2]
            .into_iter()
            .map(|seed| {
                EntityRecord::new(
                    StableId::from_parts(IdKind::Entity, 100, seed),
                    DefinitionId::new("entity.replay-test").unwrap(),
                    Vec3::new(1.0, 0.0, 1.0),
                )
                .with_persistence(PersistencePolicy::PersistentEntity)
            })
            .collect();
        let source = baseline.zone.entities[0].id;
        let target = baseline.zone.entities[1].id;

        let links = vec![EntityLink::new(
            DefinitionId::new("link.replay_test").unwrap(),
            source,
            target,
            [LinkEvent::Activated],
        )];
        let cooldowns = vec![((source, Verb::Inspect, Some(target)), Tick(9))];
        let link_cooldowns = vec![((source, target, LinkEvent::Activated), Tick(10))];
        let transfers = vec![TransferReceipt {
            key: "replay-transfer".to_owned(),
            source,
            source_slot: 0,
            destination: target,
            definition: DefinitionId::new("item.coin").unwrap(),
            quantity: 1,
            destination_slots: vec![0],
        }];
        let mut factions = demo::build_faction_world(1).unwrap();
        let calendar = demo::build_calendar(&mut factions, 1).unwrap();
        let calendar = calendar.snapshot();
        let factions = factions.snapshot();

        let event = |sequence: i64, tick: i64, kind: &str, payload: serde_json::Value| ZoneEvent {
            sequence,
            tick,
            occurred_at: std::time::SystemTime::UNIX_EPOCH,
            kind: kind.to_owned(),
            actor_id: None,
            payload,
            schema_version: 1,
        };
        let events = vec![
            snapshot_event(0, &baseline),
            event(
                1,
                2,
                EVENT_LINKS_REPLACE,
                serde_json::json!({ "links": links }),
            ),
            event(
                2,
                3,
                EVENT_COOLDOWNS_REPLACE,
                serde_json::json!({ "cooldowns": cooldowns }),
            ),
            event(
                3,
                4,
                EVENT_LINK_COOLDOWNS_REPLACE,
                serde_json::json!({ "link_cooldowns": link_cooldowns }),
            ),
            event(
                4,
                5,
                EVENT_TRANSFERS_APPEND,
                serde_json::json!({ "transfers": transfers }),
            ),
            event(
                5,
                6,
                EVENT_CALENDAR_FACTIONS_REPLACE,
                serde_json::json!({
                    "calendar": calendar,
                    "factions": factions,
                }),
            ),
        ];

        let mut expected = baseline.clone();
        expected.zone.links = events[1].payload["links"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| serde_json::from_value(value.clone()).unwrap())
            .collect();
        expected.zone.cooldowns = events[2].payload["cooldowns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| serde_json::from_value(value.clone()).unwrap())
            .collect();
        expected.zone.link_cooldowns = events[3].payload["link_cooldowns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| serde_json::from_value(value.clone()).unwrap())
            .collect();
        expected.zone.transfers = events[4].payload["transfers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| serde_json::from_value(value.clone()).unwrap())
            .collect();
        expected.calendar =
            Some(serde_json::from_value(events[5].payload["calendar"].clone()).unwrap());
        expected.factions =
            Some(serde_json::from_value(events[5].payload["factions"].clone()).unwrap());
        expected.zone.tick.0 = 6;

        assert_eq!(
            replay_world_snapshot_events(&events, "demo-world").unwrap(),
            Some(expected)
        );
    }

    #[test]
    fn replay_accepts_a_retained_log_that_starts_at_a_checkpoint() {
        let mut baseline = world_snapshot();
        baseline.zone.tick.0 = 7;
        let checkpoint = snapshot_event(7, &baseline);
        assert_eq!(
            replay_world_snapshot_events(&[checkpoint], "demo-world").unwrap(),
            Some(baseline)
        );
    }

    #[test]
    fn replay_rejects_a_retained_log_that_starts_after_a_checkpoint() {
        let entity = EntityRecord::new(
            StableId::from_parts(IdKind::Entity, 101, 1),
            DefinitionId::new("entity.replay-test").unwrap(),
            Vec3::new(1.0, 0.0, 1.0),
        )
        .with_persistence(PersistencePolicy::PersistentEntity);
        let event = ZoneEvent {
            sequence: 3,
            tick: 3,
            occurred_at: std::time::SystemTime::UNIX_EPOCH,
            kind: EVENT_ENTITY_UPSERT.to_owned(),
            actor_id: Some(entity.id.to_string()),
            payload: serde_json::json!({ "entity": entity }),
            schema_version: 1,
        };
        let error = replay_world_snapshot_events(&[event], "demo-world").unwrap_err();
        assert!(error.to_string().contains("must begin with a world checkpoint"));
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
    fn replay_rejects_a_checkpoint_that_moves_the_tick_backwards() {
        let mut baseline = world_snapshot();
        baseline.zone.tick.0 = 5;
        let mut older = world_snapshot();
        older.zone.tick.0 = 3;

        let error = replay_world_snapshot_events(
            &[snapshot_event(0, &baseline), snapshot_event(1, &older)],
            "demo-world",
        )
        .unwrap_err();
        assert!(error.to_string().contains("backwards"));
        assert_eq!(error.code(), ErrorCode::InvalidRequest);
    }

    #[test]
    fn replay_rejects_entity_events_with_a_malformed_identity() {
        let baseline = world_snapshot();
        let event = ZoneEvent {
            sequence: 1,
            tick: 1,
            occurred_at: std::time::SystemTime::UNIX_EPOCH,
            kind: EVENT_ENTITY_REMOVE.to_owned(),
            actor_id: Some("not-a-stable-id".to_owned()),
            payload: serde_json::json!({ "entity_id": "not-a-stable-id" }),
            schema_version: 1,
        };
        let error =
            replay_world_snapshot_events(&[snapshot_event(0, &baseline), event], "demo-world")
                .unwrap_err();
        assert!(error.to_string().contains("invalid entity ID"));
    }

    fn recovery_dataset(
        entities: u128,
        upserts: u128,
        removals: u128,
    ) -> (WorldSnapshot, Vec<ZoneEvent>) {
        let mut baseline = world_snapshot();
        baseline.zone.entities = (0..entities)
            .map(|index| {
                EntityRecord::new(
                    StableId::from_parts(
                        IdKind::Entity,
                        1_000 + u64::try_from(index).unwrap(),
                        index,
                    ),
                    DefinitionId::new("entity.recovery-reference").unwrap(),
                    Vec3::new(0.0, 0.0, 0.0),
                )
                .with_persistence(PersistencePolicy::PersistentEntity)
            })
            .collect();
        baseline.zone.entities.sort_by_key(|entity| entity.id);

        let mut events = Vec::with_capacity(1 + (upserts + removals) as usize);
        events.push(snapshot_event(0, &baseline));
        let mut sequence = 1_i64;
        for index in 0..upserts {
            let source = &baseline.zone.entities[usize::try_from(index % entities).unwrap()];
            let mut entity = source.clone();
            entity.transform.position.x += f32::from(u8::try_from(index % 7).unwrap()) * 0.01;
            let actor_id = entity.id.to_string();
            events.push(ZoneEvent {
                sequence,
                tick: sequence,
                occurred_at: std::time::SystemTime::UNIX_EPOCH,
                kind: EVENT_ENTITY_UPSERT.to_owned(),
                actor_id: Some(actor_id),
                payload: serde_json::json!({ "entity": entity }),
                schema_version: 1,
            });
            sequence += 1;
        }
        for index in 0..removals {
            let entity_id = baseline.zone.entities[usize::try_from(index).unwrap()]
                .id
                .to_string();
            events.push(ZoneEvent {
                sequence,
                tick: sequence,
                occurred_at: std::time::SystemTime::UNIX_EPOCH,
                kind: EVENT_ENTITY_REMOVE.to_owned(),
                actor_id: Some(entity_id.clone()),
                payload: serde_json::json!({ "entity_id": entity_id }),
                schema_version: 1,
            });
            sequence += 1;
        }
        (baseline, events)
    }

    #[test]
    fn recovery_reference_dataset_completes_within_the_documented_target() {
        // Reference dataset: 1,000 persistent entities, 5,000 upserts, and 400
        // removals. This is the measured recovery workload for the 60-second
        // target; see docs/operations.md.
        const ENTITIES: u128 = 1_000;
        const UPSERTS: u128 = 5_000;
        const REMOVALS: u128 = 400;

        let (_, events) = recovery_dataset(ENTITIES, UPSERTS, REMOVALS);
        let started = std::time::Instant::now();
        let recovered = replay_world_snapshot_events(&events, "demo-world")
            .expect("the reference dataset replays");
        let elapsed = started.elapsed();
        let repeated = replay_world_snapshot_events(&events, "demo-world")
            .expect("the reference dataset replays again");
        assert_eq!(recovered, repeated, "repeated recovery is deterministic");
        assert_eq!(
            recovered.unwrap().zone.entities.len(),
            usize::try_from(ENTITIES - REMOVALS).unwrap()
        );
        assert!(
            elapsed < RECOVERY_TARGET,
            "recovery took {elapsed:?}, over the documented target"
        );
    }

    #[test]
    #[ignore = "production-scale timing check; run explicitly with --ignored --nocapture"]
    fn recovery_production_scale_meets_the_target() {
        // Ten times the checked reference workload. This is a manual timing
        // check for headroom beyond the default test, not a CI gate.
        const ENTITIES: u128 = 10_000;
        const UPSERTS: u128 = 50_000;
        const REMOVALS: u128 = 5_000;
        const RUNS: usize = 5;

        let (_, events) = recovery_dataset(ENTITIES, UPSERTS, REMOVALS);
        let mut timings = Vec::with_capacity(RUNS);
        for run in 0..RUNS {
            let started = std::time::Instant::now();
            let recovered = replay_world_snapshot_events(&events, "demo-world")
                .expect("the production-scale dataset replays");
            let elapsed = started.elapsed();
            assert_eq!(
                recovered.unwrap().zone.entities.len(),
                usize::try_from(ENTITIES - REMOVALS).unwrap()
            );
            println!("production-scale recovery run {run}: {elapsed:?}");
            timings.push(elapsed);
        }
        timings.sort_unstable();
        let p95_index = (timings.len() * 95).div_ceil(100).saturating_sub(1);
        let p95 = timings[p95_index.min(timings.len() - 1)];
        let max = timings.last().copied().unwrap_or_default();
        println!(
            "production-scale recovery: events={} p95={p95:?} max={max:?}",
            events.len()
        );
        assert!(
            max < RECOVERY_TARGET,
            "production-scale recovery took {max:?}, over the documented target"
        );
    }

    #[test]
    fn file_store_persists_a_recovery_quarantine_report() {
        let dir = std::env::temp_dir().join("lynx-world-snapshot-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("quarantine.json");
        let store = FileSnapshotStore::new(&path);
        let _ = std::fs::remove_file(store.quarantine_path());
        assert_eq!(store.quarantine_records("zone_demo").unwrap().len(), 0);

        let records = vec![QuarantinedRecord {
            section: "entity".to_owned(),
            entity: Some(StableId::from_parts(IdKind::Entity, 1, 2)),
            code: "out_of_range".to_owned(),
            message: "entity position is outside the zone bounds".to_owned(),
        }];
        store.record_quarantine("zone_demo", &records).unwrap();
        assert_eq!(
            store.quarantine_records("zone_demo").unwrap(),
            records,
            "the operator report survives a restart"
        );
    }

    #[test]
    fn replay_rejects_a_missing_or_corrupt_snapshot_event() {
        let snapshot = world_snapshot();
        let gap = replay_world_snapshot_events(
            &[snapshot_event(0, &snapshot), snapshot_event(2, &snapshot)],
            "demo-world",
        )
        .unwrap_err();
        assert_eq!(gap.code(), ErrorCode::InvalidRequest);

        let mut corrupt = snapshot_event(0, &snapshot);
        corrupt.payload["snapshot_hash"] = serde_json::json!("not-a-real-hash");
        let error = replay_world_snapshot_events(&[corrupt], "demo-world").unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidRequest);
    }
}
