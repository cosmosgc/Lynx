//! Independent recovery checkpoints for each authoritative zone.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use backend_nakama::postgres_pool::{PooledConnection, PostgresPool};
use engine_core::error::{EngineError, ErrorCode};
use engine_core::id::IdKind;
use engine_sim::snapshot::ZoneSnapshot;
use postgres::Client;
use serde_json::Value;

use super::ZoneSnapshotStore;
use crate::travel::{ZoneHandoffReceipt, ZoneHandoffStore};

/// One file per zone, replaced atomically at each checkpoint.
#[derive(Debug, Clone)]
pub struct FileZoneSnapshotStore {
    directory: PathBuf,
}

impl FileZoneSnapshotStore {
    /// Store zone checkpoints under `directory`.
    #[must_use]
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    /// Directory containing one checkpoint file per zone instance.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    fn path_for(&self, zone_id: &str) -> Result<PathBuf, EngineError> {
        validate_zone_id(zone_id)?;
        Ok(self.directory.join(format!("{zone_id}.json")))
    }
}

impl ZoneSnapshotStore for FileZoneSnapshotStore {
    fn load_zone(&self, zone_id: &str) -> Result<Option<ZoneSnapshot>, EngineError> {
        let path = self.path_for(zone_id)?;
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(file_error(
                    "zone checkpoint could not be read",
                    &path,
                    &error,
                ));
            }
        };
        let snapshot: ZoneSnapshot = serde_json::from_slice(&bytes).map_err(|error| {
            EngineError::new(
                ErrorCode::InvalidRequest,
                "the zone checkpoint file is not a valid snapshot",
            )
            .with_detail("path", path.display().to_string())
            .with_detail("error", error.to_string())
        })?;
        validate_snapshot(&snapshot)?;
        ensure_zone_match(zone_id, &snapshot)?;
        Ok(Some(snapshot))
    }

    fn save_zone(&self, snapshot: &ZoneSnapshot) -> Result<(), EngineError> {
        validate_snapshot(snapshot)?;
        let zone_id = snapshot.config.instance.to_string();
        let path = self.path_for(&zone_id)?;
        fs::create_dir_all(&self.directory).map_err(|error| {
            file_error(
                "zone checkpoint directory could not be created",
                &self.directory,
                &error,
            )
        })?;
        let body = serde_json::to_vec_pretty(snapshot).map_err(|error| {
            EngineError::new(
                ErrorCode::Internal,
                "the zone checkpoint could not be encoded",
            )
            .with_detail("error", error.to_string())
        })?;
        let temp = path.with_extension("tmp");
        fs::write(&temp, body)
            .map_err(|error| file_error("zone checkpoint could not be written", &temp, &error))?;
        fs::rename(&temp, &path)
            .map_err(|error| file_error("zone checkpoint could not be committed", &path, &error))
    }
}

#[derive(Debug, Clone)]
struct ZoneFence {
    owner: String,
    token: i64,
}

/// PostgreSQL checkpoints backed by `lynx.zone_snapshots`.
///
/// Each zone has a separate history keyed by its stable instance ID. Writes
/// require a bound lease and verify its fencing token in the same transaction
/// as the checkpoint insert.
pub struct PostgresZoneSnapshotStore {
    pool: PostgresPool,
    fences: Mutex<BTreeMap<String, ZoneFence>>,
}

impl std::fmt::Debug for PostgresZoneSnapshotStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresZoneSnapshotStore")
            .field(
                "bound_zones",
                &self.fences.lock().map(|fences| fences.len()).ok(),
            )
            .finish_non_exhaustive()
    }
}

impl PostgresZoneSnapshotStore {
    /// Create a store using the shared bounded pool after checking the schema.
    pub fn with_pool(pool: PostgresPool) -> Result<Self, EngineError> {
        let mut client = pool.acquire().map_err(|_| {
            postgres_unavailable("the PostgreSQL zone snapshot store is unavailable")
        })?;
        let status = crate::schema::inspect(&mut client)
            .map_err(|message| EngineError::new(ErrorCode::Internal, message))?;
        if !status.compatible() {
            return Err(EngineError::new(ErrorCode::Internal, status.diagnostic()));
        }
        Ok(Self {
            pool,
            fences: Mutex::new(BTreeMap::new()),
        })
    }

    /// Bind future writes for `zone_id` to its current lease fence.
    ///
    /// A later takeover increments the token, so stale writers fail even if
    /// their local authority loop has not observed lease loss yet.
    pub fn bind_zone_lease(
        &self,
        zone_id: &str,
        owner: &str,
        token: i64,
    ) -> Result<(), EngineError> {
        validate_zone_id(zone_id)?;
        if owner.is_empty() || owner.len() > 256 || token <= 0 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "the zone snapshot lease binding is invalid",
            ));
        }
        self.fences
            .lock()
            .map_err(|_| postgres_unavailable("the PostgreSQL zone snapshot store is unavailable"))?
            .insert(
                zone_id.to_owned(),
                ZoneFence {
                    owner: owner.to_owned(),
                    token,
                },
            );
        Ok(())
    }

    fn client(&self) -> Result<PooledConnection<Client>, EngineError> {
        self.pool
            .acquire()
            .map_err(|_| postgres_unavailable("the PostgreSQL zone snapshot store is unavailable"))
    }
}

impl ZoneSnapshotStore for PostgresZoneSnapshotStore {
    fn load_zone(&self, zone_id: &str) -> Result<Option<ZoneSnapshot>, EngineError> {
        validate_zone_id(zone_id)?;
        let mut client = self.client()?;
        let row = client
            .query_opt(
                "SELECT tick, schema_version, payload, payload_hash FROM lynx.zone_snapshots WHERE zone_id = $1 ORDER BY tick DESC, taken_at DESC LIMIT 1",
                &[&zone_id],
            )
            .map_err(|_| postgres_unavailable("the zone checkpoint could not be read"))?;
        let Some(row) = row else {
            return Ok(None);
        };
        let tick: i64 = row.get(0);
        let schema_version: i32 = row.get(1);
        let payload: Value = row.get(2);
        let payload_hash: String = row.get(3);
        let canonical = super::postgres::canonical_json(&payload)?;
        if engine_content::sha256_hex(canonical.as_bytes()) != payload_hash {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "the stored zone checkpoint failed its integrity check",
            ));
        }
        let snapshot: ZoneSnapshot = serde_json::from_value(payload).map_err(|error| {
            EngineError::new(
                ErrorCode::InvalidRequest,
                "the stored zone checkpoint payload is invalid",
            )
            .with_detail("error", error.to_string())
        })?;
        validate_snapshot(&snapshot)?;
        ensure_zone_match(zone_id, &snapshot)?;
        if i64::try_from(snapshot.tick.0).ok() != Some(tick)
            || i32::from(snapshot.schema_version.major) != schema_version
        {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "the stored zone checkpoint metadata does not match its payload",
            ));
        }
        Ok(Some(snapshot))
    }

    fn save_zone(&self, snapshot: &ZoneSnapshot) -> Result<(), EngineError> {
        validate_snapshot(snapshot)?;
        let zone_id = snapshot.config.instance.to_string();
        let tick = i64::try_from(snapshot.tick.0).map_err(|_| {
            EngineError::new(
                ErrorCode::InvalidRequest,
                "the zone checkpoint tick is out of range",
            )
        })?;
        let schema_version = i32::from(snapshot.schema_version.major);
        let payload = serde_json::to_value(snapshot).map_err(|error| {
            EngineError::new(
                ErrorCode::Internal,
                "the zone checkpoint could not be encoded",
            )
            .with_detail("error", error.to_string())
        })?;
        let canonical = super::postgres::canonical_json(&payload)?;
        let payload_hash = engine_content::sha256_hex(canonical.as_bytes());
        let fence = self
            .fences
            .lock()
            .map_err(|_| postgres_unavailable("the PostgreSQL zone snapshot store is unavailable"))?
            .get(&zone_id)
            .cloned()
            .ok_or_else(|| {
                EngineError::new(
                    ErrorCode::InvalidState,
                    "zone snapshot writes require a bound zone lease",
                )
            })?;
        let mut client = self.client()?;
        let mut tx = client
            .transaction()
            .map_err(|_| postgres_unavailable("the zone checkpoint transaction could not start"))?;
        let held = tx
            .query_opt(
                "SELECT 1 FROM lynx.zone_leases WHERE zone_id = $1 AND owner = $2 AND fencing_token = $3 AND expires_at > clock_timestamp() FOR UPDATE",
                &[&zone_id, &fence.owner, &fence.token],
            )
            .map_err(|_| postgres_unavailable("the zone snapshot lease could not be checked"))?;
        if held.is_none() {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "the zone lease is no longer authoritative",
            ));
        }
        tx.execute(
            "INSERT INTO lynx.zone_snapshots (zone_id, tick, schema_version, payload, payload_hash) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (zone_id, tick) DO UPDATE SET taken_at = clock_timestamp(), schema_version = EXCLUDED.schema_version, payload = EXCLUDED.payload, payload_hash = EXCLUDED.payload_hash",
            &[&zone_id, &tick, &schema_version, &payload, &payload_hash],
        )
        .map_err(|_| postgres_unavailable("the zone checkpoint could not be written"))?;
        tx.commit()
            .map_err(|_| postgres_unavailable("the zone checkpoint could not be committed"))
    }
}

impl ZoneHandoffStore for PostgresZoneSnapshotStore {
    fn commit_handoff(
        &self,
        source: &ZoneSnapshot,
        destination: &ZoneSnapshot,
        receipt: &ZoneHandoffReceipt,
    ) -> Result<ZoneHandoffReceipt, EngineError> {
        validate_handoff(source, destination, receipt)?;
        let source_record = encode_snapshot(source)?;
        let destination_record = encode_snapshot(destination)?;
        let receipt_payload = serde_json::to_value(receipt).map_err(|error| {
            EngineError::new(
                ErrorCode::Internal,
                "the zone handoff receipt could not be encoded",
            )
            .with_detail("error", error.to_string())
        })?;
        let receipt_canonical = super::postgres::canonical_json(&receipt_payload)?;
        let receipt_hash = engine_content::sha256_hex(receipt_canonical.as_bytes());
        let source_zone = source.config.instance.to_string();
        let destination_zone = destination.config.instance.to_string();
        let fences = self.fences.lock().map_err(|_| {
            postgres_unavailable("the PostgreSQL zone snapshot store is unavailable")
        })?;
        let mut zone_fences = [
            (
                source_zone.clone(),
                fences.get(&source_zone).cloned().ok_or_else(|| {
                    EngineError::new(
                        ErrorCode::InvalidState,
                        "handoff writes require a bound source zone lease",
                    )
                })?,
            ),
            (
                destination_zone.clone(),
                fences.get(&destination_zone).cloned().ok_or_else(|| {
                    EngineError::new(
                        ErrorCode::InvalidState,
                        "handoff writes require a bound destination zone lease",
                    )
                })?,
            ),
        ];
        zone_fences.sort_by(|left, right| left.0.cmp(&right.0));
        drop(fences);

        let mut client = self.client()?;
        let mut tx = client
            .transaction()
            .map_err(|_| postgres_unavailable("the zone handoff transaction could not start"))?;
        // Serialize retries even when they race from separate service workers.
        tx.query_one(
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            &[&receipt.request_key],
        )
        .map_err(|_| postgres_unavailable("the handoff request key could not be locked"))?;

        if let Some(existing) = tx
            .query_opt(
                "SELECT receipt, receipt_hash FROM lynx.zone_handoffs WHERE request_key = $1 FOR UPDATE",
                &[&receipt.request_key],
            )
            .map_err(|_| postgres_unavailable("the handoff receipt could not be read"))?
        {
            let stored_payload: Value = existing.get(0);
            let stored_hash: String = existing.get(1);
            let stored_canonical = super::postgres::canonical_json(&stored_payload)?;
            if engine_content::sha256_hex(stored_canonical.as_bytes()) != stored_hash {
                return Err(EngineError::new(
                    ErrorCode::InvalidRequest,
                    "the stored zone handoff receipt failed its integrity check",
                ));
            }
            let stored: ZoneHandoffReceipt = serde_json::from_value(stored_payload).map_err(|error| {
                EngineError::new(
                    ErrorCode::InvalidRequest,
                    "the stored zone handoff receipt is invalid",
                )
                .with_detail("error", error.to_string())
            })?;
            if stored != *receipt {
                return Err(EngineError::new(
                    ErrorCode::Conflict,
                    "handoff request key was already used with different inputs",
                ));
            }
            tx.commit().map_err(|_| {
                postgres_unavailable("the handoff retry transaction could not be committed")
            })?;
            return Ok(stored);
        }

        for (zone_id, fence) in &zone_fences {
            let held = tx
                .query_opt(
                    "SELECT 1 FROM lynx.zone_leases WHERE zone_id = $1 AND owner = $2 AND fencing_token = $3 AND expires_at > clock_timestamp() FOR UPDATE",
                    &[zone_id, &fence.owner, &fence.token],
                )
                .map_err(|_| postgres_unavailable("the zone handoff lease could not be checked"))?;
            if held.is_none() {
                return Err(EngineError::new(
                    ErrorCode::Conflict,
                    "a zone lease is no longer authoritative for the handoff",
                )
                .with_detail("zone_id", zone_id));
            }
        }

        for (snapshot, record) in [(source, &source_record), (destination, &destination_record)] {
            let zone_id = snapshot.config.instance.to_string();
            tx.execute(
                "INSERT INTO lynx.zone_snapshots (zone_id, tick, schema_version, payload, payload_hash) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (zone_id, tick) DO UPDATE SET taken_at = clock_timestamp(), schema_version = EXCLUDED.schema_version, payload = EXCLUDED.payload, payload_hash = EXCLUDED.payload_hash",
                &[
                    &zone_id,
                    &record.tick,
                    &record.schema_version,
                    &record.payload,
                    &record.payload_hash,
                ],
            )
            .map_err(|_| postgres_unavailable("a zone handoff checkpoint could not be written"))?;
        }
        tx.execute(
            "UPDATE lynx.resume_sessions SET zone_id = $1 WHERE principal_id = $2 AND zone_id = $3 AND connected AND NOT revoked",
            &[&destination_zone, &receipt.character.to_string(), &source_zone],
        )
        .map_err(|_| postgres_unavailable("the handoff resume route could not be updated"))?;
        tx.execute(
            "INSERT INTO lynx.zone_handoffs (request_key, source_zone_id, destination_zone_id, character_id, entity_id, receipt, receipt_hash) VALUES ($1, $2, $3, $4, $5, $6, $7)",
            &[
                &receipt.request_key,
                &source_zone,
                &destination_zone,
                &receipt.character.to_string(),
                &receipt.entity.to_string(),
                &receipt_payload,
                &receipt_hash,
            ],
        )
        .map_err(|_| postgres_unavailable("the zone handoff receipt could not be written"))?;
        tx.commit().map_err(|_| {
            postgres_unavailable("the zone handoff transaction could not be committed")
        })?;
        Ok(receipt.clone())
    }

    fn handoff_receipt(
        &self,
        request_key: &str,
    ) -> Result<Option<ZoneHandoffReceipt>, EngineError> {
        validate_request_key(request_key)?;
        let mut client = self.client()?;
        let Some(row) = client
            .query_opt(
                "SELECT receipt, receipt_hash FROM lynx.zone_handoffs WHERE request_key = $1",
                &[&request_key],
            )
            .map_err(|_| postgres_unavailable("the zone handoff receipt could not be read"))?
        else {
            return Ok(None);
        };
        let payload: Value = row.get(0);
        let hash: String = row.get(1);
        let canonical = super::postgres::canonical_json(&payload)?;
        if engine_content::sha256_hex(canonical.as_bytes()) != hash {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "the stored zone handoff receipt failed its integrity check",
            ));
        }
        let receipt = serde_json::from_value(payload).map_err(|error| {
            EngineError::new(
                ErrorCode::InvalidRequest,
                "the stored zone handoff receipt is invalid",
            )
            .with_detail("error", error.to_string())
        })?;
        Ok(Some(receipt))
    }
}

struct EncodedSnapshot {
    tick: i64,
    schema_version: i32,
    payload: Value,
    payload_hash: String,
}

fn encode_snapshot(snapshot: &ZoneSnapshot) -> Result<EncodedSnapshot, EngineError> {
    validate_snapshot(snapshot)?;
    let tick = i64::try_from(snapshot.tick.0).map_err(|_| {
        EngineError::new(
            ErrorCode::InvalidRequest,
            "the zone checkpoint tick is out of range",
        )
    })?;
    let payload = serde_json::to_value(snapshot).map_err(|error| {
        EngineError::new(
            ErrorCode::Internal,
            "the zone checkpoint could not be encoded",
        )
        .with_detail("error", error.to_string())
    })?;
    let canonical = super::postgres::canonical_json(&payload)?;
    Ok(EncodedSnapshot {
        tick,
        schema_version: i32::from(snapshot.schema_version.major),
        payload,
        payload_hash: engine_content::sha256_hex(canonical.as_bytes()),
    })
}

fn validate_handoff(
    source: &ZoneSnapshot,
    destination: &ZoneSnapshot,
    receipt: &ZoneHandoffReceipt,
) -> Result<(), EngineError> {
    validate_snapshot(source)?;
    validate_snapshot(destination)?;
    validate_request_key(&receipt.request_key)?;
    receipt.character.expect_kind(IdKind::Character)?;
    receipt.entity.expect_kind(IdKind::Entity)?;
    receipt.source_instance.expect_kind(IdKind::Zone)?;
    receipt.destination_instance.expect_kind(IdKind::Zone)?;
    if receipt.source_instance == receipt.destination_instance
        || source.config.instance != receipt.source_instance
        || destination.config.instance != receipt.destination_instance
        || source.config.instance == destination.config.instance
        || !receipt.destination.x.is_finite()
        || !receipt.destination.y.is_finite()
        || !receipt.destination.z.is_finite()
    {
        return Err(EngineError::new(
            ErrorCode::InvalidRequest,
            "the zone handoff snapshots do not match the receipt",
        ));
    }
    let source_has_character = source.entities.iter().any(|entity| {
        entity.id == receipt.entity
            || entity
                .mind
                .as_ref()
                .is_some_and(|mind| mind.player_character == Some(receipt.character))
    });
    let destination_entities = destination
        .entities
        .iter()
        .filter(|entity| {
            entity.id == receipt.entity
                && entity
                    .mind
                    .as_ref()
                    .is_some_and(|mind| mind.player_character == Some(receipt.character))
        })
        .count();
    if source_has_character || destination_entities != 1 {
        return Err(EngineError::new(
            ErrorCode::InvalidState,
            "the handoff snapshots do not contain exactly one destination actor",
        ));
    }
    Ok(())
}

fn validate_request_key(request_key: &str) -> Result<(), EngineError> {
    if (1..=128).contains(&request_key.len()) {
        Ok(())
    } else {
        Err(EngineError::new(
            ErrorCode::InvalidRequest,
            "handoff request key must contain 1 to 128 bytes",
        ))
    }
}

fn validate_snapshot(snapshot: &ZoneSnapshot) -> Result<(), EngineError> {
    validate_zone_id(&snapshot.config.instance.to_string())?;
    if snapshot.schema_version.major == 0 || snapshot.tick.0 > i64::MAX as u64 {
        return Err(EngineError::new(
            ErrorCode::InvalidRequest,
            "the zone checkpoint metadata is invalid",
        ));
    }
    Ok(())
}

fn ensure_zone_match(zone_id: &str, snapshot: &ZoneSnapshot) -> Result<(), EngineError> {
    if snapshot.config.instance.to_string() != zone_id {
        return Err(EngineError::new(
            ErrorCode::InvalidRequest,
            "the zone checkpoint belongs to a different zone instance",
        ));
    }
    Ok(())
}

fn validate_zone_id(zone_id: &str) -> Result<(), EngineError> {
    let valid = zone_id.strip_prefix("zone_").is_some_and(|suffix| {
        suffix.len() == 26
            && suffix.bytes().all(|byte| {
                byte.is_ascii_digit()
                    || (b'A'..=b'H').contains(&byte)
                    || (b'J'..=b'K').contains(&byte)
                    || (b'M'..=b'N').contains(&byte)
                    || (b'P'..=b'T').contains(&byte)
                    || (b'V'..=b'Z').contains(&byte)
            })
    });
    if valid {
        Ok(())
    } else {
        Err(EngineError::new(
            ErrorCode::InvalidRequest,
            "the zone snapshot ID is invalid",
        ))
    }
}

fn file_error(message: &'static str, path: &Path, error: &io::Error) -> EngineError {
    EngineError::new(ErrorCode::Internal, message)
        .with_detail("path", path.display().to_string())
        .with_detail("error", error.to_string())
}

fn postgres_unavailable(message: &'static str) -> EngineError {
    EngineError::new(ErrorCode::Internal, message)
}
