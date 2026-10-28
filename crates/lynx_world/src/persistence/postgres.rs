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

use engine_core::error::{EngineError, ErrorCode};
use postgres::{Client, Config, NoTls};
use serde_json::Value;

use super::{SnapshotStore, WorldSnapshot};

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

/// A [`SnapshotStore`] that keeps the whole world in PostgreSQL.
///
/// Clones are not provided: the store owns one connection behind a mutex, and
/// the service shares it through an `Arc`. A dropped connection is not retried;
/// the next save reports the failure to the caller.
pub struct PostgresSnapshotStore {
    client: Mutex<Client>,
    world_id: String,
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
        let host = environment_or("LYNX_PG_HOST", "127.0.0.1");
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
            .application_name("lynx-world");
        Self::connect(&config, world_id)
    }

    /// Connect with an explicit configuration.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] for an invalid `world_id` and
    /// [`ErrorCode::Internal`] when the connection fails.
    pub fn connect(config: &Config, world_id: impl Into<String>) -> Result<Self, EngineError> {
        let world_id = world_id.into();
        if !valid_world_id(&world_id) {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "the world snapshot ID is invalid",
            )
            .with_detail("world_id", &world_id));
        }
        let client = config.connect(NoTls).map_err(|error| {
            EngineError::new(
                ErrorCode::Internal,
                "the PostgreSQL snapshot store could not connect",
            )
            .with_detail("error", error.to_string())
        })?;
        Ok(Self {
            client: Mutex::new(client),
            world_id,
        })
    }

    /// The row this store owns.
    #[must_use]
    pub fn world_id(&self) -> &str {
        &self.world_id
    }

    /// Remove the stored snapshot for this world.
    ///
    /// Startup does not call this: `--fresh` overwrites the row before serving.
    /// It exists so operators and tests can return a world to a true
    /// no-snapshot state.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::Internal`] when the delete fails.
    pub fn delete(&self) -> Result<bool, EngineError> {
        let mut client = self.client()?;
        let deleted = client
            .execute(
                "DELETE FROM lynx.world_snapshots WHERE world_id = $1",
                &[&self.world_id],
            )
            .map_err(write_error)?;
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

impl SnapshotStore for PostgresSnapshotStore {
    fn load(&self) -> Result<Option<WorldSnapshot>, EngineError> {
        let mut client = self.client()?;
        let row = client
            .query_opt(SELECT_SNAPSHOT, &[&self.world_id])
            .map_err(query_error)?;
        let Some(row) = row else {
            return Ok(None);
        };

        let payload: Value = row.get("payload");
        let stored_hash: String = row.get("payload_hash");
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
        snapshot.validate()?;
        Ok(Some(snapshot))
    }

    fn save(&self, snapshot: &WorldSnapshot) -> Result<(), EngineError> {
        snapshot.validate()?;
        let payload = serde_json::to_value(snapshot).map_err(encode_error)?;
        let canonical = canonical_json(&payload)?;
        let payload_hash = crate::auth::hash_hex(&canonical);
        let schema_major = i32::from(snapshot.schema_version.major);
        let schema_minor = i32::from(snapshot.schema_version.minor);

        let mut client = self.client()?;
        client
            .execute(
                UPSERT_SNAPSHOT,
                &[
                    &self.world_id,
                    &schema_major,
                    &schema_minor,
                    &snapshot.saved_at_ms,
                    &payload,
                    &payload_hash,
                ],
            )
            .map_err(write_error)?;
        Ok(())
    }
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
    serde_json::to_string(value).map_err(encode_error)
}

fn encode_error(error: serde_json::Error) -> EngineError {
    EngineError::new(
        ErrorCode::Internal,
        "the world snapshot could not be encoded",
    )
    .with_detail("error", error.to_string())
}

fn snapshot_error(message: &'static str) -> EngineError {
    EngineError::new(ErrorCode::InvalidRequest, message)
}

fn query_error(error: postgres::Error) -> EngineError {
    EngineError::new(
        ErrorCode::Internal,
        "the world snapshot could not be read from PostgreSQL",
    )
    .with_detail("error", error.to_string())
}

fn write_error(error: postgres::Error) -> EngineError {
    EngineError::new(
        ErrorCode::Internal,
        "the world snapshot could not be written to PostgreSQL",
    )
    .with_detail("error", error.to_string())
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
}
