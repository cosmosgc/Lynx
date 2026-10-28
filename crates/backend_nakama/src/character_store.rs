//! Account-scoped access to Lynx-owned character records.
//!
//! This is a repository, not an authentication service. The caller must first
//! validate the session and resolve its account through a trusted identity
//! mapping; accepting an account ID directly from a client is unsafe.

use std::sync::Mutex;

use engine_core::IdKind;
use engine_core::id::{IdGenerator, StableId, SystemIdGenerator};
use engine_core::time::Millis;
use game_api::{AccountId, ApiError, Character, CharacterId, CharacterStatus, CreateCharacter};
use postgres::{Client, Row};

const COLUMNS: &str = "character_id, name, zone_id, status, \
    (extract(epoch FROM created_at) * 1000)::bigint AS created_ms";

/// Character repository using a caller-provided PostgreSQL connection.
///
/// This does not validate tokens, create accounts, or enter
/// the world. Connections are serialized and are not automatically reconnected.
pub struct PostgresCharacterStore {
    client: Mutex<Client>,
}

impl std::fmt::Debug for PostgresCharacterStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresCharacterStore")
            .finish_non_exhaustive()
    }
}

impl PostgresCharacterStore {
    /// Wrap an already established connection to the migrated Lynx database.
    #[must_use]
    pub fn new(client: Client) -> Self {
        Self {
            client: Mutex::new(client),
        }
    }

    /// List the trusted account's non-deleted characters in stable order.
    pub fn characters(&self, account: &AccountId) -> Result<Vec<Character>, ApiError> {
        let mut client = self.client.lock().map_err(|_| unavailable())?;
        let sql = format!(
            "SELECT {COLUMNS} FROM lynx.characters \
            WHERE account_id = $1 AND deleted_at IS NULL ORDER BY character_id"
        );
        client
            .query(&sql, &[&account.as_str()])
            .map_err(|_| unavailable())?
            .iter()
            .map(|row| decode(row, account))
            .collect()
    }

    /// Resolve an owned, active character for selection without changing state.
    /// Missing, deleted, suspended, and foreign characters are indistinguishable.
    pub fn select_character(
        &self,
        account: &AccountId,
        id: CharacterId,
    ) -> Result<Character, ApiError> {
        let mut client = self.client.lock().map_err(|_| unavailable())?;
        let sql = format!(
            "SELECT {COLUMNS} FROM lynx.characters \
            WHERE account_id = $1 AND character_id = $2 AND deleted_at IS NULL"
        );
        let row = client
            .query_opt(&sql, &[&account.as_str(), &id.to_string()])
            .map_err(|_| unavailable())?
            .ok_or_else(|| ApiError::NotFound("character not found".into()))?;
        let character = decode(&row, account)?;
        if !character.is_playable() {
            return Err(ApiError::NotFound("character not found".into()));
        }
        Ok(character)
    }

    /// Tombstone an owned character, including a suspended one.
    ///
    /// Repeating a deletion of an owned tombstone succeeds without changing it
    /// or writing another audit event. Missing and foreign IDs are indistinguishable.
    /// This retains related records; it does not remove a character from a live
    /// world or release property. Callers must coordinate those operations.
    /// All character writers must lock the account row before changing state.
    pub fn delete_character(&self, account: &AccountId, id: CharacterId) -> Result<(), ApiError> {
        let mut client = self.client.lock().map_err(|_| unavailable())?;
        let mut tx = client.transaction().map_err(|_| unavailable())?;
        if tx
            .query_opt(
                "SELECT account_id FROM lynx.accounts WHERE account_id = $1 FOR UPDATE",
                &[&account.as_str()],
            )
            .map_err(|_| unavailable())?
            .is_none()
        {
            return Err(ApiError::NotFound("character not found".into()));
        }
        let id = id.to_string();
        let changed = tx.execute(
            "UPDATE lynx.characters SET status = 'deleted', deleted_at = now() WHERE account_id = $1 AND character_id = $2 AND deleted_at IS NULL",
            &[&account.as_str(), &id],
        ).map_err(|_| unavailable())?;
        if changed == 0 {
            if tx.query_opt(
                "SELECT 1 FROM lynx.characters WHERE account_id = $1 AND character_id = $2 AND deleted_at IS NOT NULL",
                &[&account.as_str(), &id],
            ).map_err(|_| unavailable())?.is_none() {
                return Err(ApiError::NotFound("character not found".into()));
            }
        } else {
            tx.execute(
                "INSERT INTO lynx.audit_events (occurred_at, actor_kind, action, target_id, outcome, correlation_id) VALUES (now(), 'system', 'character.delete', $1, 'succeeded', $1)",
                &[&id],
            ).map_err(|_| unavailable())?;
        }
        tx.commit().map_err(|_| unavailable())?;
        Ok(())
    }

    /// Record entry of an owned, active character into a server-resolved zone.
    ///
    /// The caller must route the character before calling this method. It does
    /// not create a live world session or accept a client-selected account.
    pub fn enter_character(
        &self,
        account: &AccountId,
        id: CharacterId,
        zone_id: &str,
    ) -> Result<Character, ApiError> {
        validate_zone_id(zone_id)?;
        self.update_character_zone(account, id, Some(zone_id), "character.enter_world")
    }

    /// Clear the recorded zone for an owned, active character.
    pub fn leave_character(
        &self,
        account: &AccountId,
        id: CharacterId,
    ) -> Result<Character, ApiError> {
        self.update_character_zone(account, id, None, "character.leave_world")
    }

    /// Create a character for a trusted account, or return the original receipt.
    ///
    /// `key` is a non-secret, account-scoped retry identifier, not a session
    /// token. Reusing it for a different request or operation is a conflict.
    /// All character creators must lock the account row before checking the
    /// eight-character limit. Receipts must be retained for the retry lifetime.
    pub fn create_character(
        &self,
        account: &AccountId,
        key: &str,
        request: &CreateCharacter,
    ) -> Result<Character, ApiError> {
        request.validate()?;
        if key.is_empty() || key.chars().count() > 128 || key.chars().any(char::is_control) {
            return Err(ApiError::InvalidRequest("invalid idempotency key".into()));
        }
        let mut client = self.client.lock().map_err(|_| unavailable())?;
        let mut tx = client.transaction().map_err(|_| unavailable())?;
        if tx
            .query_opt(
                "SELECT account_id FROM lynx.accounts WHERE account_id = $1 FOR UPDATE",
                &[&account.as_str()],
            )
            .map_err(|_| unavailable())?
            .is_none()
        {
            return Err(ApiError::NotFound("account not found".into()));
        }
        if let Some(row) = tx.query_opt(
            "SELECT operation, result::text FROM lynx.idempotency_keys WHERE scope = $1 AND key = $2",
            &[&account.as_str(), &key],
        ).map_err(|_| unavailable())? {
            let operation: String = row.try_get(0).map_err(|_| unavailable())?;
            if operation != "character.create" {
                return Err(ApiError::Conflict("idempotency key already used".into()));
            }
            let receipt: String = row.try_get(1).map_err(|_| unavailable())?;
            let character: Character = serde_json::from_str(&receipt).map_err(|_| unavailable())?;
            if character.account_id != *account || character.name != request.name {
                return Err(ApiError::Conflict("idempotency key already used".into()));
            }
            tx.commit().map_err(|_| unavailable())?;
            return Ok(character);
        }
        let count: i64 = tx
            .query_one(
                "SELECT count(*) FROM lynx.characters WHERE account_id = $1 AND deleted_at IS NULL",
                &[&account.as_str()],
            )
            .map_err(|_| unavailable())?
            .get(0);
        if count >= 8 {
            return Err(ApiError::Conflict("character limit reached".into()));
        }
        let id = CharacterId::new(SystemIdGenerator::new().generate(IdKind::Character))?;
        let sql = format!(
            "INSERT INTO lynx.characters (character_id, account_id, name) VALUES ($1, $2, $3) RETURNING {COLUMNS}"
        );
        let row = tx
            .query_one(&sql, &[&id.to_string(), &account.as_str(), &request.name])
            .map_err(
                |error| match error.as_db_error().and_then(|e| e.constraint()) {
                    Some("characters_name_unique") => {
                        ApiError::Conflict("character name is already in use".into())
                    }
                    _ => unavailable(),
                },
            )?;
        let character = decode(&row, account)?;
        let receipt = serde_json::to_string(&character).map_err(|_| unavailable())?;
        tx.execute(
            "INSERT INTO lynx.idempotency_keys (scope, key, operation, result) VALUES ($1, $2, 'character.create', $3::text::jsonb)",
            &[&account.as_str(), &key, &receipt],
        ).map_err(|_| unavailable())?;
        tx.execute(
            "INSERT INTO lynx.audit_events (occurred_at, actor_kind, action, target_id, outcome, correlation_id) VALUES (now(), 'system', 'character.create', $1, 'succeeded', $2)",
            &[&id.to_string(), &key],
        ).map_err(|_| unavailable())?;
        tx.commit().map_err(|_| unavailable())?;
        Ok(character)
    }

    fn update_character_zone(
        &self,
        account: &AccountId,
        id: CharacterId,
        zone_id: Option<&str>,
        action: &str,
    ) -> Result<Character, ApiError> {
        let mut client = self.client.lock().map_err(|_| unavailable())?;
        let mut tx = client.transaction().map_err(|_| unavailable())?;
        if tx
            .query_opt(
                "SELECT account_id FROM lynx.accounts WHERE account_id = $1 FOR UPDATE",
                &[&account.as_str()],
            )
            .map_err(|_| unavailable())?
            .is_none()
        {
            return Err(ApiError::NotFound("character not found".into()));
        }
        let id_text = id.to_string();
        let sql = format!(
            "SELECT {COLUMNS} FROM lynx.characters \
             WHERE account_id = $1 AND character_id = $2 AND deleted_at IS NULL FOR UPDATE"
        );
        let row = tx
            .query_opt(&sql, &[&account.as_str(), &id_text])
            .map_err(|_| unavailable())?
            .ok_or_else(|| ApiError::NotFound("character not found".into()))?;
        let current = decode(&row, account)?;
        if !current.is_playable() {
            return Err(ApiError::NotFound("character not found".into()));
        }
        if current.zone_id.as_deref() == zone_id {
            tx.commit().map_err(|_| unavailable())?;
            return Ok(current);
        }
        let sql = format!(
            "UPDATE lynx.characters SET zone_id = $3 WHERE account_id = $1 AND character_id = $2 \
             RETURNING {COLUMNS}"
        );
        let row = tx
            .query_one(&sql, &[&account.as_str(), &id_text, &zone_id])
            .map_err(|_| unavailable())?;
        let character = decode(&row, account)?;
        tx.execute(
            "INSERT INTO lynx.audit_events (occurred_at, actor_kind, actor_id, action, target_id, outcome, correlation_id) VALUES (now(), 'character', $1, $2, $1, 'succeeded', $3)",
            &[&id_text, &action, &format!("{action}:{id_text}")],
        )
        .map_err(|_| unavailable())?;
        tx.commit().map_err(|_| unavailable())?;
        Ok(character)
    }
}

fn validate_zone_id(zone_id: &str) -> Result<(), ApiError> {
    StableId::parse(zone_id)
        .and_then(|id| id.expect_kind(IdKind::Zone))
        .map_err(|_| ApiError::InvalidRequest("invalid zone identifier".into()))?;
    Ok(())
}

fn unavailable() -> ApiError {
    // Database errors and connection settings can contain sensitive information.
    ApiError::Unavailable("character store unavailable".into())
}

fn decode(row: &Row, account: &AccountId) -> Result<Character, ApiError> {
    let id: String = row.try_get("character_id").map_err(|_| unavailable())?;
    let status: String = row.try_get("status").map_err(|_| unavailable())?;
    let status = match status.as_str() {
        "active" => CharacterStatus::Active,
        "suspended" => CharacterStatus::Suspended,
        _ => return Err(unavailable()),
    };
    Ok(Character {
        id: CharacterId::parse(&id).map_err(|_| unavailable())?,
        account_id: account.clone(),
        name: row.try_get("name").map_err(|_| unavailable())?,
        zone_id: row.try_get("zone_id").map_err(|_| unavailable())?,
        status,
        created_at: Some(Millis::new(
            row.try_get("created_ms").map_err(|_| unavailable())?,
        )),
    })
}
