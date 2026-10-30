//! PostgreSQL-backed world snapshots.
//!
//! The runnable service stores one whole-world snapshot per `world_id` in
//! `lynx.world_snapshots`. The payload is the serialized [`WorldSnapshot`], so
//! the zone, calendar, and faction aggregates stay paired exactly as the file
//! store writes them. An upsert replaces the row atomically and keeps the
//! previous good world if the write fails.
//!
//! Connection settings come from the same `LYNX_PG_*` variables the setup and
//! migration scripts use. Only plain TCP is supported today: run PostgreSQL on
//! loopback (or a trusted private network) and terminate TLS in front of it
//! before exposing a remote database.

use std::fmt;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use engine_core::error::{EngineError, ErrorCode};
use postgres::{Client, Config, NoTls};
use serde_json::Value;

use super::{
    SnapshotStore, WorldSnapshot, ZoneEvent, ZoneEventInput, ZoneLease, ZoneLeaseStore,
    replay_world_snapshot_events,
};

/// PostgreSQL lease and event-log operations for one zone at a time.
pub struct PostgresZoneLeaseStore {
    client: Mutex<Client>,
}

impl fmt::Debug for PostgresZoneLeaseStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PostgresZoneLeaseStore")
            .finish_non_exhaustive()
    }
}

impl PostgresZoneLeaseStore {
    /// Connect after verifying that the configured database schema is current.
    pub fn from_env() -> Result<Self, EngineError> {
        Self::connect(&postgres_config_from_env()?)
    }

    /// Connect with an explicit PostgreSQL configuration.
    pub fn connect(config: &Config) -> Result<Self, EngineError> {
        let mut client = connect_with_retry(config).map_err(|_| {
            EngineError::new(
                ErrorCode::Internal,
                "the PostgreSQL zone store could not connect",
            )
        })?;
        let status = crate::schema::inspect(&mut client)
            .map_err(|message| EngineError::new(ErrorCode::Internal, message))?;
        if !status.compatible() {
            return Err(EngineError::new(ErrorCode::Internal, status.diagnostic()));
        }
        Ok(Self {
            client: Mutex::new(client),
        })
    }

    /// Acquire an absent/expired zone lease. Expired takeovers increment its token.
    pub fn acquire(
        &self,
        zone_id: &str,
        owner: &str,
        ttl: std::time::Duration,
    ) -> Result<Option<ZoneLease>, EngineError> {
        validate_zone_owner(zone_id, owner, ttl)?;
        let ttl_ms = i64::try_from(ttl.as_millis()).map_err(|_| invalid_lease())?;
        let mut client = self.client.lock().map_err(|_| store_unavailable())?;
        let row = client.query_opt(
            "INSERT INTO lynx.zone_leases (zone_id, owner, expires_at) VALUES ($1, $2, clock_timestamp() + ($3::bigint::double precision * interval '1 millisecond')) ON CONFLICT (zone_id) DO UPDATE SET owner = EXCLUDED.owner, acquired_at = clock_timestamp(), expires_at = EXCLUDED.expires_at, fencing_token = lynx.zone_leases.fencing_token + 1 WHERE lynx.zone_leases.expires_at <= clock_timestamp() RETURNING zone_id, owner, fencing_token, expires_at",
            &[&zone_id, &owner, &ttl_ms],
        ).map_err(|error| store_error("the zone lease could not be acquired", &error))?;
        Ok(row.map(|row| ZoneLease {
            zone_id: row.get(0),
            owner: row.get(1),
            fencing_token: row.get(2),
            expires_at: row.get(3),
        }))
    }

    /// Extend a still-live lease only if owner and fencing token still match.
    pub fn renew(
        &self,
        zone_id: &str,
        owner: &str,
        token: i64,
        ttl: std::time::Duration,
    ) -> Result<Option<ZoneLease>, EngineError> {
        validate_zone_owner(zone_id, owner, ttl)?;
        let ttl_ms = i64::try_from(ttl.as_millis()).map_err(|_| invalid_lease())?;
        let mut client = self.client.lock().map_err(|_| store_unavailable())?;
        let row = client.query_opt("UPDATE lynx.zone_leases SET expires_at = clock_timestamp() + ($4::bigint::double precision * interval '1 millisecond') WHERE zone_id = $1 AND owner = $2 AND fencing_token = $3 AND expires_at > clock_timestamp() RETURNING zone_id, owner, fencing_token, expires_at", &[&zone_id, &owner, &token, &ttl_ms]).map_err(|error| store_error("the zone lease could not be renewed", &error))?;
        Ok(row.map(|row| ZoneLease {
            zone_id: row.get(0),
            owner: row.get(1),
            fencing_token: row.get(2),
            expires_at: row.get(3),
        }))
    }

    /// Expire a lease without deleting its generation history.
    pub fn release(&self, zone_id: &str, owner: &str, token: i64) -> Result<bool, EngineError> {
        let mut client = self.client.lock().map_err(|_| store_unavailable())?;
        let count = client.execute("UPDATE lynx.zone_leases SET expires_at = clock_timestamp() WHERE zone_id = $1 AND owner = $2 AND fencing_token = $3 AND expires_at > clock_timestamp()", &[&zone_id, &owner, &token]).map_err(|error| store_error("the zone lease could not be released", &error))?;
        Ok(count == 1)
    }

    /// Append an event atomically with lease validation and gap-free sequencing.
    pub fn append_event(
        &self,
        zone_id: &str,
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
        if !valid_zone_id(zone_id)
            || owner.is_empty()
            || tick < 0
            || kind.is_empty()
            || kind.len() > 128
            || schema_version < 1
            || !payload.is_object()
        {
            return Err(invalid_lease());
        }
        let mut client = self.client.lock().map_err(|_| store_unavailable())?;
        let mut tx = client
            .transaction()
            .map_err(|error| store_error("the zone event could not be appended", &error))?;
        let held = tx.query_opt("SELECT 1 FROM lynx.zone_leases WHERE zone_id = $1 AND owner = $2 AND fencing_token = $3 AND expires_at > clock_timestamp() FOR UPDATE", &[&zone_id, &owner, &token]).map_err(|error| store_error("the zone lease could not be checked", &error))?;
        if held.is_none() {
            return Err(stale_lease());
        }
        let seq: i64 = tx
            .query_one(
                "SELECT COALESCE(MAX(sequence) + 1, 0) FROM lynx.zone_events WHERE zone_id = $1",
                &[&zone_id],
            )
            .map_err(|error| store_error("the zone event sequence could not be read", &error))?
            .get(0);
        tx.execute("INSERT INTO lynx.zone_events (zone_id, sequence, tick, occurred_at, kind, actor_id, payload, schema_version) VALUES ($1, $2, $3, clock_timestamp(), $4, $5, $6, $7)", &[&zone_id, &seq, &tick, &kind, &actor_id, payload, &schema_version]).map_err(|error| store_error("the zone event could not be appended", &error))?;
        tx.commit()
            .map_err(|error| store_error("the zone event could not be committed", &error))?;
        Ok(seq)
    }

    /// Fetch ordered events strictly after a sequence (use `-1` for all events).
    pub fn events_after(
        &self,
        zone_id: &str,
        sequence: i64,
    ) -> Result<Vec<ZoneEvent>, EngineError> {
        if !valid_zone_id(zone_id) {
            return Err(invalid_lease());
        }
        let mut client = self.client.lock().map_err(|_| store_unavailable())?;
        let rows = client.query("SELECT sequence, tick, occurred_at, kind, actor_id, payload, schema_version FROM lynx.zone_events WHERE zone_id = $1 AND sequence > $2 ORDER BY sequence", &[&zone_id, &sequence]).map_err(|error| store_error("the zone events could not be read", &error))?;
        Ok(rows
            .into_iter()
            .map(|r| ZoneEvent {
                sequence: r.get(0),
                tick: r.get(1),
                occurred_at: r.get(2),
                kind: r.get(3),
                actor_id: r.get(4),
                payload: r.get(5),
                schema_version: r.get(6),
            })
            .collect())
    }
}

fn valid_zone_id(value: &str) -> bool {
    value.strip_prefix("zone_").is_some_and(|suffix| {
        suffix.len() == 26
            && suffix.bytes().all(|b| {
                b.is_ascii_digit()
                    || (b'A'..=b'H').contains(&b)
                    || (b'J'..=b'K').contains(&b)
                    || (b'M'..=b'N').contains(&b)
                    || (b'P'..=b'T').contains(&b)
                    || (b'V'..=b'Z').contains(&b)
            })
    })
}

impl ZoneLeaseStore for PostgresZoneLeaseStore {
    fn acquire(
        &self,
        zone_id: &str,
        owner: &str,
        ttl: std::time::Duration,
    ) -> Result<Option<ZoneLease>, EngineError> {
        PostgresZoneLeaseStore::acquire(self, zone_id, owner, ttl)
    }

    fn renew(
        &self,
        zone_id: &str,
        owner: &str,
        token: i64,
        ttl: std::time::Duration,
    ) -> Result<Option<ZoneLease>, EngineError> {
        PostgresZoneLeaseStore::renew(self, zone_id, owner, token, ttl)
    }

    fn release(&self, zone_id: &str, owner: &str, token: i64) -> Result<bool, EngineError> {
        PostgresZoneLeaseStore::release(self, zone_id, owner, token)
    }

    fn append_event(
        &self,
        zone_id: &str,
        owner: &str,
        token: i64,
        event: ZoneEventInput<'_>,
    ) -> Result<i64, EngineError> {
        PostgresZoneLeaseStore::append_event(self, zone_id, owner, token, event)
    }

    fn events_after(&self, zone_id: &str, sequence: i64) -> Result<Vec<ZoneEvent>, EngineError> {
        PostgresZoneLeaseStore::events_after(self, zone_id, sequence)
    }
}

fn validate_zone_owner(
    zone: &str,
    owner: &str,
    ttl: std::time::Duration,
) -> Result<(), EngineError> {
    if !valid_zone_id(zone)
        || owner.is_empty()
        || owner.len() > 256
        || ttl.is_zero()
        || ttl.as_millis() > i64::MAX as u128
    {
        Err(invalid_lease())
    } else {
        Ok(())
    }
}
fn invalid_lease() -> EngineError {
    EngineError::new(
        ErrorCode::InvalidRequest,
        "the zone lease request is invalid",
    )
}
fn stale_lease() -> EngineError {
    EngineError::new(
        ErrorCode::Conflict,
        "the zone lease is no longer authoritative",
    )
}
fn store_unavailable() -> EngineError {
    EngineError::new(
        ErrorCode::Internal,
        "the PostgreSQL zone store is unavailable",
    )
}
fn store_error(message: &'static str, _error: &postgres::Error) -> EngineError {
    EngineError::new(ErrorCode::Internal, message)
}

const SELECT_SNAPSHOT: &str = "\
SELECT schema_major, schema_minor, saved_at_ms, payload, payload_hash \
FROM lynx.world_snapshots WHERE world_id = $1";

const UPSERT_SNAPSHOT: &str = "\
INSERT INTO lynx.world_snapshots \
    (world_id, schema_major, schema_minor, saved_at_ms, payload, payload_hash) \
VALUES ($1, $2, $3, $4, $5, $6) \
ON CONFLICT (world_id) DO UPDATE SET \
    schema_major = EXCLUDED.schema_major, \
    schema_minor = EXCLUDED.schema_minor, \
    saved_at_ms = EXCLUDED.saved_at_ms, \
    payload = EXCLUDED.payload, \
    payload_hash = EXCLUDED.payload_hash";

const SELECT_HELD_LEASE: &str = "\
SELECT 1 FROM lynx.zone_leases \
WHERE zone_id = $1 AND owner = $2 AND fencing_token = $3 \
    AND expires_at > clock_timestamp() \
FOR UPDATE";

const SELECT_ZONE_EVENTS: &str = "\
SELECT sequence, tick, occurred_at, kind, actor_id, payload, schema_version \
FROM lynx.zone_events WHERE zone_id = $1 ORDER BY sequence";

/// A snapshot store connection bound to one held zone lease.
#[derive(Debug, Clone)]
struct SnapshotFence {
    zone_id: String,
    owner: String,
    token: i64,
}

/// A [`SnapshotStore`] that keeps the whole world in PostgreSQL.
///
/// Clones are not provided: the store owns one connection behind a mutex, and
/// the service shares it through an `Arc`. A dropped connection is not retried;
/// the next save reports the failure to the caller.
pub struct PostgresSnapshotStore {
    client: Mutex<Client>,
    world_id: String,
    fence: Mutex<Option<SnapshotFence>>,
}

impl fmt::Debug for PostgresSnapshotStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The connection configuration contains a password, so never print it.
        f.debug_struct("PostgresSnapshotStore")
            .field("world_id", &self.world_id)
            .finish_non_exhaustive()
    }
}

impl PostgresSnapshotStore {
    /// Connect using `LYNX_PG_HOST`, `LYNX_PG_PORT`, `LYNX_PG_DATABASE`,
    /// `LYNX_PG_USER`, and `LYNX_PG_PASSWORD`.
    ///
    /// Host defaults to `127.0.0.1` and port to `5432`; the database, user, and
    /// password are required. `world_id` names the row this process owns.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::Internal`] for a missing variable, an unparsable
    /// port, or a failed connection, and [`ErrorCode::InvalidRequest`] for an
    /// invalid `world_id`.
    pub fn from_env(world_id: impl Into<String>) -> Result<Self, EngineError> {
        Self::connect(&postgres_config_from_env()?, world_id)
    }

    /// Connect with an explicit configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection or schema compatibility check fails.
    pub fn connect(config: &Config, world_id: impl Into<String>) -> Result<Self, EngineError> {
        let world_id = world_id.into();
        if !valid_world_id(&world_id) {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "the world snapshot ID is invalid",
            ));
        }
        let mut client = connect_with_retry(config).map_err(|_| {
            EngineError::new(
                ErrorCode::Internal,
                "the PostgreSQL snapshot store could not connect",
            )
        })?;
        let status = crate::schema::inspect(&mut client)
            .map_err(|message| EngineError::new(ErrorCode::Internal, message))?;
        if !status.compatible() {
            return Err(EngineError::new(ErrorCode::Internal, status.diagnostic()));
        }
        Ok(Self {
            client: Mutex::new(client),
            world_id,
            fence: Mutex::new(None),
        })
    }

    /// The row this store owns.
    #[must_use]
    pub fn world_id(&self) -> &str {
        &self.world_id
    }

    /// Reject later saves unless `zone_id` is still held by `owner` under
    /// `token`.
    ///
    /// The lease row is checked in the same transaction as the snapshot upsert,
    /// so a takeover fences the write even when this process has not noticed
    /// the loss yet. Bind the fence before the store is shared with the
    /// authority loop; it applies to every subsequent save.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] for an invalid zone, owner, or
    /// token, and [`ErrorCode::Internal`] if the fence lock is poisoned.
    pub fn bind_zone_lease(
        &self,
        zone_id: &str,
        owner: &str,
        token: i64,
    ) -> Result<(), EngineError> {
        if !valid_zone_id(zone_id) || owner.is_empty() || owner.len() > 256 || token <= 0 {
            return Err(invalid_lease());
        }
        *self.fence.lock().map_err(|_| store_unavailable())? = Some(SnapshotFence {
            zone_id: zone_id.to_owned(),
            owner: owner.to_owned(),
            token,
        });
        Ok(())
    }

    /// Remove the stored snapshot for this world.
    ///
    /// # Errors
    /// Returns an error if the delete fails.
    pub fn delete(&self) -> Result<bool, EngineError> {
        let mut client = self.client()?;
        let deleted = client
            .execute(
                "DELETE FROM lynx.world_snapshots WHERE world_id = $1",
                &[&self.world_id],
            )
            .map_err(|error| write_error(&error))?;
        Ok(deleted > 0)
    }

    fn client(&self) -> Result<MutexGuard<'_, Client>, EngineError> {
        self.client.lock().map_err(|_| {
            EngineError::new(
                ErrorCode::Internal,
                "the PostgreSQL snapshot connection is unavailable",
            )
        })
    }
}

/// Read the shared PostgreSQL configuration without exposing its values.
pub fn postgres_config_from_env() -> Result<Config, EngineError> {
    let host = environment_or("LYNX_PG_HOST", "127.0.0.1");
    if !is_loopback_host(&host) && !environment_is_true("LYNX_PG_ALLOW_INSECURE_REMOTE") {
        return Err(EngineError::new(
            ErrorCode::Internal,
            "remote PostgreSQL connections require TLS support or explicit insecure opt-in",
        ));
    }
    let port = environment_or("LYNX_PG_PORT", "5432")
        .parse::<u16>()
        .map_err(|error| {
            EngineError::new(ErrorCode::Internal, "LYNX_PG_PORT is not a valid port")
                .with_detail("error", error.to_string())
        })?;
    let database = required_environment("LYNX_PG_DATABASE")?;
    let user = required_environment("LYNX_PG_USER")?;
    let password = required_environment("LYNX_PG_PASSWORD")?;

    let mut config = Config::new();
    config
        .host(&host)
        .port(port)
        .dbname(&database)
        .user(&user)
        .password(&password)
        .application_name("lynx-world")
        .connect_timeout(Duration::from_secs(5))
        .options("-c statement_timeout=5000 -c lock_timeout=2000 -c idle_in_transaction_session_timeout=5000");
    Ok(config)
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

fn environment_is_true(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| value.eq_ignore_ascii_case("true"))
}

/// Retry connection establishment with a bounded exponential backoff. Queries
/// are not replayed because a lost response can make a write's outcome unknown.
fn connect_with_retry(config: &Config) -> Result<Client, postgres::Error> {
    const ATTEMPTS: usize = 3;
    let mut delay = Duration::from_millis(100);
    let mut last_error = None;
    for attempt in 0..ATTEMPTS {
        match config.connect(NoTls) {
            Ok(client) => return Ok(client),
            Err(error) => last_error = Some(error),
        }
        if attempt + 1 < ATTEMPTS {
            std::thread::sleep(delay);
            delay *= 2;
        }
    }
    Err(last_error.expect("at least one connection attempt"))
}

impl SnapshotStore for PostgresSnapshotStore {
    fn load(&self) -> Result<Option<WorldSnapshot>, EngineError> {
        let mut client = self.client()?;
        let row = client
            .query_opt(SELECT_SNAPSHOT, &[&self.world_id])
            .map_err(|error| query_error(&error))?;
        let Some(row) = row else {
            return Ok(None);
        };

        let payload: Value = row.get("payload");
        let stored_hash: String = row.get("payload_hash");
        let stored_schema_major: i32 = row.get("schema_major");
        let stored_schema_minor: i32 = row.get("schema_minor");
        let stored_saved_at_ms: i64 = row.get("saved_at_ms");
        let canonical = canonical_json(&payload)?;
        if crate::auth::hash_hex(&canonical) != stored_hash {
            return Err(snapshot_error(
                "the stored world snapshot failed its integrity check",
            ));
        }

        let snapshot: WorldSnapshot = serde_json::from_value(payload).map_err(|error| {
            snapshot_error("the stored world snapshot is not valid")
                .with_detail("error", error.to_string())
        })?;
        if !snapshot_metadata_matches(
            snapshot.schema_version,
            snapshot.saved_at_ms,
            stored_schema_major,
            stored_schema_minor,
            stored_saved_at_ms,
        ) {
            return Err(snapshot_error(
                "the stored world snapshot metadata does not match its payload",
            ));
        }
        snapshot.validate()?;

        // Stores created before transactional snapshot events remain readable.
        // Once a lease is bound, however, startup verifies that the latest
        // replayable event rebuilds exactly the row it is about to serve.
        let fence = self.fence.lock().map_err(|_| store_unavailable())?.clone();
        let Some(fence) = fence else {
            return Ok(Some(snapshot));
        };
        let events = read_zone_events(&mut client, &fence.zone_id)?;
        let Some(replayed) = replay_world_snapshot_events(&events, &self.world_id)? else {
            return Ok(Some(snapshot));
        };
        if replayed != snapshot {
            return Err(snapshot_error(
                "the replayed world snapshot does not match the stored snapshot",
            ));
        }
        Ok(Some(replayed))
    }

    fn save(&self, snapshot: &WorldSnapshot) -> Result<(), EngineError> {
        snapshot.validate()?;
        let payload = serde_json::to_value(snapshot).map_err(|error| encode_error(&error))?;
        let canonical = canonical_json(&payload)?;
        let payload_hash = crate::auth::hash_hex(&canonical);
        let schema_major = i32::from(snapshot.schema_version.major);
        let schema_minor = i32::from(snapshot.schema_version.minor);
        let params: [&(dyn postgres::types::ToSql + Sync); 6] = [
            &self.world_id,
            &schema_major,
            &schema_minor,
            &snapshot.saved_at_ms,
            &payload,
            &payload_hash,
        ];
        let fence = self.fence.lock().map_err(|_| store_unavailable())?.clone();

        let mut client = self.client()?;
        let Some(fence) = fence else {
            client
                .execute(UPSERT_SNAPSHOT, &params)
                .map_err(|error| write_error(&error))?;
            return Ok(());
        };
        let mut tx = client.transaction().map_err(|error| write_error(&error))?;
        let held = tx
            .query_opt(
                SELECT_HELD_LEASE,
                &[&fence.zone_id, &fence.owner, &fence.token],
            )
            .map_err(|error| write_error(&error))?;
        if held.is_none() {
            return Err(stale_lease());
        }
        tx.execute(UPSERT_SNAPSHOT, &params)
            .map_err(|error| write_error(&error))?;
        tx.commit().map_err(|error| write_error(&error))?;
        Ok(())
    }

    fn save_at_tick(&self, snapshot: &WorldSnapshot, tick: i64) -> Result<(), EngineError> {
        if tick < 0 {
            return Err(snapshot_error("the world snapshot tick is invalid"));
        }
        snapshot.validate()?;
        let payload = serde_json::to_value(snapshot).map_err(|error| encode_error(&error))?;
        let canonical = canonical_json(&payload)?;
        let payload_hash = crate::auth::hash_hex(&canonical);
        let schema_major = i32::from(snapshot.schema_version.major);
        let schema_minor = i32::from(snapshot.schema_version.minor);
        let params: [&(dyn postgres::types::ToSql + Sync); 6] = [
            &self.world_id,
            &schema_major,
            &schema_minor,
            &snapshot.saved_at_ms,
            &payload,
            &payload_hash,
        ];
        let fence = self.fence.lock().map_err(|_| store_unavailable())?.clone();
        let Some(fence) = fence else {
            return self.save(snapshot);
        };

        let mut client = self.client()?;
        let mut tx = client.transaction().map_err(|error| write_error(&error))?;
        let held = tx
            .query_opt(
                SELECT_HELD_LEASE,
                &[&fence.zone_id, &fence.owner, &fence.token],
            )
            .map_err(|error| write_error(&error))?;
        if held.is_none() {
            return Err(stale_lease());
        }
        tx.execute(UPSERT_SNAPSHOT, &params)
            .map_err(|error| write_error(&error))?;
        let sequence: i64 = tx
            .query_one(
                "SELECT COALESCE(MAX(sequence) + 1, 0) FROM lynx.zone_events WHERE zone_id = $1",
                &[&fence.zone_id],
            )
            .map_err(|error| write_error(&error))?
            .get(0);
        let event_payload = serde_json::json!({
            "world_id": self.world_id,
            "snapshot": payload,
            "snapshot_hash": payload_hash,
        });
        tx.execute(
            "INSERT INTO lynx.zone_events (zone_id, sequence, tick, occurred_at, kind, actor_id, payload, schema_version) VALUES ($1, $2, $3, clock_timestamp(), 'world.snapshot', NULL, $4, 1)",
            &[&fence.zone_id, &sequence, &tick, &event_payload],
        )
        .map_err(|error| write_error(&error))?;
        tx.commit().map_err(|error| write_error(&error))?;
        Ok(())
    }

    fn records_snapshot_events_transactionally(&self) -> bool {
        self.fence.lock().is_ok_and(|fence| fence.is_some())
    }
}

fn read_zone_events(client: &mut Client, zone_id: &str) -> Result<Vec<ZoneEvent>, EngineError> {
    let rows = client
        .query(SELECT_ZONE_EVENTS, &[&zone_id])
        .map_err(|error| query_error(&error))?;
    Ok(rows
        .into_iter()
        .map(|row| ZoneEvent {
            sequence: row.get(0),
            tick: row.get(1),
            occurred_at: row.get(2),
            kind: row.get(3),
            actor_id: row.get(4),
            payload: row.get(5),
            schema_version: row.get(6),
        })
        .collect())
}

fn environment_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn required_environment(name: &str) -> Result<String, EngineError> {
    std::env::var(name).map_err(|_| {
        EngineError::new(
            ErrorCode::Internal,
            "the PostgreSQL snapshot store is missing an environment variable",
        )
        .with_detail("variable", name)
    })
}

fn valid_world_id(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index > 0 && matches!(byte, b'.' | b'_' | b'-'))
        })
}

/// Serialize a JSON value deterministically.
///
/// `serde_json`'s map is a `BTreeMap` by default, so key order does not depend
/// on the order PostgreSQL returned from JSONB.
fn canonical_json(value: &Value) -> Result<String, EngineError> {
    serde_json::to_string(value).map_err(|error| encode_error(&error))
}

fn encode_error(error: &serde_json::Error) -> EngineError {
    EngineError::new(
        ErrorCode::Internal,
        "the world snapshot could not be encoded",
    )
    .with_detail("error", error.to_string())
}

fn snapshot_error(message: &'static str) -> EngineError {
    EngineError::new(ErrorCode::InvalidRequest, message)
}

fn snapshot_metadata_matches(
    version: engine_core::schema::SchemaVersion,
    saved_at_ms: i64,
    stored_major: i32,
    stored_minor: i32,
    stored_saved_at_ms: i64,
) -> bool {
    i32::from(version.major) == stored_major
        && i32::from(version.minor) == stored_minor
        && saved_at_ms == stored_saved_at_ms
}

fn query_error(_error: &postgres::Error) -> EngineError {
    EngineError::new(
        ErrorCode::Internal,
        "the world snapshot could not be read from PostgreSQL",
    )
}

fn write_error(_error: &postgres::Error) -> EngineError {
    EngineError::new(
        ErrorCode::Internal,
        "the world snapshot could not be written to PostgreSQL",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_ids_match_the_sql_constraint() {
        assert!(valid_world_id("demo"));
        assert!(valid_world_id("test-world_1"));
        assert!(!valid_world_id(""));
        assert!(!valid_world_id("-leading"));
        assert!(!valid_world_id("Upper"));
        assert!(!valid_world_id("with space"));
        assert!(!valid_world_id(&"a".repeat(65)));
    }

    #[test]
    fn canonical_json_is_independent_of_key_insertion_order() {
        let first: Value = serde_json::from_str(r#"{"b": 2, "a": {"d": 4, "c": 3}}"#).unwrap();
        let second: Value = serde_json::from_str(r#"{"a": {"c": 3, "d": 4}, "b": 2}"#).unwrap();
        assert_eq!(
            canonical_json(&first).unwrap(),
            canonical_json(&second).unwrap()
        );
    }

    #[test]
    fn snapshot_row_metadata_must_match_the_payload() {
        let version = engine_core::schema::SchemaVersion::new(1, 2);
        assert!(snapshot_metadata_matches(version, 123, 1, 2, 123));
        assert!(!snapshot_metadata_matches(version, 123, 2, 2, 123));
        assert!(!snapshot_metadata_matches(version, 123, 1, 3, 123));
        assert!(!snapshot_metadata_matches(version, 123, 1, 2, 124));
    }

    #[test]
    fn only_loopback_hosts_are_insecure_by_default() {
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("::1"));
        assert!(!is_loopback_host("db.internal"));
        assert!(!is_loopback_host("192.168.1.2"));
    }
}
