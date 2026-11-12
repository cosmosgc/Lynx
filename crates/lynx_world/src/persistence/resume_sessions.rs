//! PostgreSQL adapter for the gateway's durable resume-session contract.

use backend_nakama::postgres_pool::PostgresPool;
use engine_core::error::{EngineError, ErrorCode};
use postgres::{Config, Row};
use world_gateway::{ResumeSessionRecord, ResumeSessionStore, TokenDigest};

/// PostgreSQL-backed durable resume session store.
///
/// It leases connections from a bounded pool. Each token rotation is one
/// conditional SQL `UPDATE` executed in a transaction, so competing uses
/// serialize on the row and only one can consume the prior digest.
pub struct PostgresResumeSessionStore {
    pool: PostgresPool,
}

impl std::fmt::Debug for PostgresResumeSessionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresResumeSessionStore")
            .finish_non_exhaustive()
    }
}

impl PostgresResumeSessionStore {
    /// Connect using the supplied PostgreSQL configuration.
    pub fn connect(config: &Config) -> Result<Self, EngineError> {
        Self::with_pool(backend_nakama::postgres_pool::pool(
            config.clone(),
            8,
            std::time::Duration::from_secs(2),
        ))
    }

    /// Create a store that leases connections from the shared bounded pool.
    pub fn with_pool(pool: PostgresPool) -> Result<Self, EngineError> {
        pool.acquire().map_err(|_| store_unavailable())?;
        Ok(Self { pool })
    }

    /// Connect using the world service's configured database.
    pub fn connect_from_env() -> Result<Self, EngineError> {
        let config = super::postgres_config_from_env()?;
        Self::connect(&config)
    }
}

impl ResumeSessionStore for PostgresResumeSessionStore {
    fn create(&self, record: &ResumeSessionRecord) -> Result<(), EngineError> {
        if !record.connected
            || record.expires_at_ms.is_some()
            || record.revoked
            || record.generation < 0
        {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "invalid initial resume-session record",
            ));
        }
        let mut client = self.pool.acquire().map_err(|_| store_poisoned())?;
        client.execute(
            "INSERT INTO lynx.resume_sessions (session_id, zone_id, account_id, principal_id, actor_id, token_digest, expires_at_ms, connected, generation, revoked, staff) VALUES ($1, $2, $3, $4, $5, $6, NULL, true, $7, false, $8)",
            &[&record.session_id, &record.zone_id, &record.account_id, &record.principal_id, &record.actor_id, &&record.token_digest.as_bytes()[..], &record.generation, &record.staff],
        ).map_err(store_error)?;
        Ok(())
    }

    fn lookup_token(&self, token: TokenDigest) -> Result<Option<ResumeSessionRecord>, EngineError> {
        let mut client = self.pool.acquire().map_err(|_| store_poisoned())?;
        let row = client
            .query_opt(
                "SELECT session_id, zone_id, account_id, principal_id, actor_id, token_digest, expires_at_ms, connected, generation, revoked, staff FROM lynx.resume_sessions WHERE token_digest = $1",
                &[&&token.as_bytes()[..]],
            )
            .map_err(store_error)?;
        row.as_ref().map(decode_record).transpose()
    }

    fn detach_stale_connected(
        &self,
        zone_id: &str,
        owner: &str,
        fencing_token: i64,
        expires_at_ms: i64,
    ) -> Result<u64, EngineError> {
        let mut client = self.pool.acquire().map_err(|_| store_poisoned())?;
        let mut tx = client.transaction().map_err(store_error)?;
        let held = tx.query_opt(
            "SELECT 1 FROM lynx.zone_leases WHERE zone_id = $1 AND owner = $2 AND fencing_token = $3 AND expires_at > clock_timestamp() FOR UPDATE",
            &[&zone_id, &owner, &fencing_token],
        ).map_err(store_error)?;
        if held.is_none() {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "the zone lease is no longer authoritative",
            ));
        }
        let changed = tx.execute(
            "UPDATE lynx.resume_sessions SET connected = false, expires_at_ms = $1, generation = generation + 1 WHERE zone_id = $2 AND connected AND NOT revoked",
            &[&expires_at_ms, &zone_id],
        ).map_err(store_error)?;
        tx.commit().map_err(store_error)?;
        Ok(changed)
    }

    fn detach(
        &self,
        session_id: &str,
        expected_generation: i64,
        expires_at_ms: i64,
    ) -> Result<bool, EngineError> {
        let mut client = self.pool.acquire().map_err(|_| store_poisoned())?;
        let changed = client.execute(
            "UPDATE lynx.resume_sessions SET connected = false, expires_at_ms = $3, generation = generation + 1 WHERE session_id = $1 AND generation = $2 AND connected AND NOT revoked",
            &[&session_id, &expected_generation, &expires_at_ms],
        ).map_err(store_error)?;
        Ok(changed == 1)
    }

    fn consume_and_rotate(
        &self,
        current: TokenDigest,
        next: TokenDigest,
        now_ms: i64,
    ) -> Result<Option<ResumeSessionRecord>, EngineError> {
        if current == next {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "resume token rotation must use a fresh token",
            ));
        }
        let mut client = self.pool.acquire().map_err(|_| store_poisoned())?;
        let mut tx = client.transaction().map_err(store_error)?;
        let row = tx.query_opt(
            "SELECT session_id, zone_id, account_id, principal_id, actor_id, token_digest, expires_at_ms, connected, generation, revoked, staff FROM lynx.resume_sessions WHERE token_digest = $1 FOR UPDATE",
            &[&&current.as_bytes()[..]],
        ).map_err(store_error)?;
        let Some(row) = row else {
            tx.commit().map_err(store_error)?;
            return Ok(None);
        };
        let mut record = decode_record(&row)?;
        if record.revoked {
            tx.commit().map_err(store_error)?;
            return Ok(None);
        }
        if record.connected {
            // The caller still has a live transport for this credential, or a
            // peer process does. Report the documented conflict rather than
            // treating the token as unknown and leaving the state ambiguous.
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "the session is still connected",
            ));
        }
        if record.expires_at_ms.is_none_or(|expiry| expiry <= now_ms) {
            tx.commit().map_err(store_error)?;
            return Ok(None);
        }
        let changed = tx.execute(
            "UPDATE lynx.resume_sessions SET token_digest = $2, expires_at_ms = NULL, connected = true, generation = generation + 1 WHERE session_id = $1 AND token_digest = $3 AND NOT connected AND NOT revoked",
            &[&record.session_id, &&next.as_bytes()[..], &&current.as_bytes()[..]],
        ).map_err(store_error)?;
        if changed != 1 {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "the durable resume claim was lost",
            ));
        }
        tx.commit().map_err(store_error)?;
        // Return the consumed detached metadata: callers validate the old
        // expiry and fence before reconstructing process-local state.
        record.token_digest = next;
        record.generation = record.generation.saturating_add(1);
        Ok(Some(record))
    }

    fn revoke(&self, session_id: &str, expected_generation: i64) -> Result<bool, EngineError> {
        let mut client = self.pool.acquire().map_err(|_| store_poisoned())?;
        let changed = client.execute(
            "UPDATE lynx.resume_sessions SET revoked = true, connected = false, expires_at_ms = NULL, generation = generation + 1 WHERE session_id = $1 AND generation = $2 AND NOT revoked",
            &[&session_id, &expected_generation],
        ).map_err(store_error)?;
        Ok(changed == 1)
    }

    fn revoke_principal(&self, principal_id: &str) -> Result<u64, EngineError> {
        let mut client = self.pool.acquire().map_err(|_| store_poisoned())?;
        client
            .execute(
                "UPDATE lynx.resume_sessions SET revoked = true, connected = false, expires_at_ms = NULL, generation = generation + 1 WHERE principal_id = $1 AND NOT revoked",
                &[&principal_id],
            )
            .map_err(store_error)
    }

    fn sweep_expired(&self, now_ms: i64) -> Result<u64, EngineError> {
        let mut client = self.pool.acquire().map_err(|_| store_poisoned())?;
        client.execute(
            "DELETE FROM lynx.resume_sessions WHERE revoked OR (NOT connected AND expires_at_ms <= $1)",
            &[&now_ms],
        ).map_err(store_error)
    }
}

fn decode_record(row: &Row) -> Result<ResumeSessionRecord, EngineError> {
    let digest: Vec<u8> = row.get("token_digest");
    let token_digest = TokenDigest::from_bytes(&digest)
        .ok_or_else(|| store_invalid("stored token digest has an invalid length"))?;
    Ok(ResumeSessionRecord {
        session_id: row.get("session_id"),
        zone_id: row.get("zone_id"),
        account_id: row.get("account_id"),
        principal_id: row.get("principal_id"),
        actor_id: row.get("actor_id"),
        staff: row.get("staff"),
        token_digest,
        expires_at_ms: row.get("expires_at_ms"),
        connected: row.get("connected"),
        generation: row.get("generation"),
        revoked: row.get("revoked"),
    })
}

fn store_error(_error: postgres::Error) -> EngineError {
    EngineError::new(
        ErrorCode::Internal,
        "the resume-session store operation failed",
    )
}

fn store_poisoned() -> EngineError {
    EngineError::new(
        ErrorCode::Internal,
        "the resume-session store connection is unavailable",
    )
}

fn store_unavailable() -> EngineError {
    EngineError::new(
        ErrorCode::Internal,
        "the resume-session store connection could not be established",
    )
}

fn store_invalid(message: &'static str) -> EngineError {
    EngineError::new(ErrorCode::InvalidRequest, message)
}
