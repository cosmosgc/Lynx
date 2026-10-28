//! Character records and account-scoped character operations.
//!
//! Character records belong to Lynx-owned persistence, not to the platform
//! account provider. Adapters must scope every operation to the authenticated
//! account; a client never chooses an owner.

use std::fmt;
use std::str::FromStr;

use engine_core::time::Millis;
use engine_core::{IdKind, StableId};
use serde::{Deserialize, Serialize};

use crate::service::{AccountId, ApiError, CreateCharacter, SessionToken};

/// A validated character identifier.
///
/// The inner [`StableId`] always has the `character` namespace, so an entity,
/// zone, item, or property ID cannot be passed where a character is expected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CharacterId(StableId);

impl CharacterId {
    /// Wrap a stable ID after checking its namespace.
    pub fn new(value: StableId) -> Result<Self, ApiError> {
        value
            .expect_kind(IdKind::Character)
            .map(Self)
            .map_err(|_| ApiError::InvalidRequest("expected a character identifier".into()))
    }

    /// Parse a `chr_`-prefixed identifier.
    pub fn parse(value: &str) -> Result<Self, ApiError> {
        let id = StableId::parse(value)
            .map_err(|_| ApiError::InvalidRequest("invalid character identifier".into()))?;
        Self::new(id)
    }

    /// The underlying stable ID.
    #[must_use]
    pub const fn as_stable(self) -> StableId {
        self.0
    }
}

impl fmt::Display for CharacterId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for CharacterId {
    type Err = ApiError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::parse(input)
    }
}

impl TryFrom<String> for CharacterId {
    type Error = ApiError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<CharacterId> for String {
    fn from(value: CharacterId) -> Self {
        value.0.to_string()
    }
}

/// Lifecycle state of a character record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CharacterStatus {
    /// Usable for play.
    Active,
    /// Retained but blocked from entering the world.
    Suspended,
    /// Tombstoned; the name may be reused.
    Deleted,
}

/// A persisted character safe to return to its owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Character {
    /// Stable character identifier.
    pub id: CharacterId,
    /// Account that owns the character.
    pub account_id: AccountId,
    /// Player-facing character name.
    pub name: String,
    /// Last zone, when the character has entered the world.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone_id: Option<String>,
    /// Current lifecycle state.
    pub status: CharacterStatus,
    /// Creation time, when the adapter records one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<Millis>,
}

impl Character {
    /// Whether the character may enter the world.
    #[must_use]
    pub const fn is_playable(&self) -> bool {
        matches!(self.status, CharacterStatus::Active)
    }
}

/// Account-scoped character management.
///
/// Creation and deletion are player intents, but the adapter owns identifier
/// generation, name uniqueness, and per-account limits.
pub trait CharacterService {
    /// List the authenticated account's non-deleted characters.
    fn characters(&self, token: &SessionToken) -> Result<Vec<Character>, ApiError>;

    /// Create a character owned by the authenticated account.
    fn create_character(
        &self,
        token: &SessionToken,
        request: &CreateCharacter,
    ) -> Result<Character, ApiError>;

    /// Delete a character owned by the authenticated account.
    ///
    /// Adapters should tombstone the record rather than discard its audit
    /// history, and must free the name for reuse.
    fn delete_character(
        &self,
        token: &SessionToken,
        character_id: CharacterId,
    ) -> Result<(), ApiError>;
}

/// Character-bound world-presence operations.
///
/// A routing service resolves the destination zone from trusted server state
/// before calling this interface. The client cannot choose an account or infer
/// a character from its account: every operation authenticates the token and
/// checks that the supplied character is currently owned and playable.
pub trait CharacterWorldService: CharacterService {
    /// Record entry of an owned, playable character into a server-resolved zone.
    ///
    /// This durable selection/presence seam does not create a gateway session;
    /// the world edge owns live session admission and capacity.
    fn enter_character(
        &self,
        token: &SessionToken,
        character_id: CharacterId,
        zone_id: &str,
    ) -> Result<Character, ApiError>;

    /// Clear the recorded zone for an owned character leaving the world.
    fn leave_character(
        &self,
        token: &SessionToken,
        character_id: CharacterId,
    ) -> Result<Character, ApiError>;
}

/// A non-secret client-generated key identifying one character creation intent.
///
/// Use a fresh, sufficiently unique key for each intent and reuse it only when
/// retrying that intent. This key is not a credential and must not contain a
/// session token or other secret. Its scope is the authenticated account and
/// character creation operation, not the session or character name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct CharacterCreationKey(String);

impl CharacterCreationKey {
    /// Validate a key of 1–128 ASCII letters, digits, underscores or hyphens.
    pub fn new(value: impl Into<String>) -> Result<Self, ApiError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 128
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        {
            return Err(ApiError::InvalidRequest(
                "invalid character creation key".into(),
            ));
        }
        Ok(Self(value))
    }

    /// Borrow the non-secret retry key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for CharacterCreationKey {
    type Error = ApiError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// Evidence of a successful creation, not the character's current state.
///
/// Its durability is determined by the service contract that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterCreationReceipt {
    /// Identifier assigned to the original creation intent.
    pub character_id: CharacterId,
}

/// Optional stronger character operations; existing implementations need not
/// implement this trait. It provides no fallback to non-idempotent creation.
pub trait CharacterIntentService: CharacterService {
    /// Create once for an authenticated account and return its creation receipt.
    ///
    /// The adapter must authenticate each call and atomically bind the account,
    /// key, request and receipt to the creation, including concurrent retries.
    /// Repeating the same request with the same account and key returns the
    /// original receipt, even if the character has since changed or been
    /// deleted. Reusing that key with a different request returns
    /// [`ApiError::Conflict`]. A key used by another account is independent.
    ///
    /// Receipts and their request bindings must survive process restarts and be
    /// retained for a documented duration. The implementation must document
    /// that duration and its behavior after expiry; callers cannot assume a
    /// retry is safe beyond the retention window. An ambiguous transport error
    /// should be retried with the same key, not a fresh one.
    fn create_character_idempotent(
        &self,
        token: &SessionToken,
        key: &CharacterCreationKey,
        request: &CreateCharacter,
    ) -> Result<CharacterCreationReceipt, ApiError>;

    /// Resolve a currently playable character owned by the authenticated account.
    ///
    /// This checks current ownership and status rather than trusting a creation
    /// receipt. Authenticate each call before resolving the character. Return
    /// [`ApiError::NotFound`] with the same message for missing, deleted,
    /// suspended, and foreign characters, so their existence and status are not
    /// disclosed by selection. Return only an owned, active character. This
    /// operation does not itself enter the world or promise to persist a
    /// selected-character preference; its result is a point-in-time check.
    fn select_character(
        &self,
        token: &SessionToken,
        character_id: CharacterId,
    ) -> Result<Character, ApiError>;
}

/// Explicitly non-durable character intents for tests and offline development.
///
/// This is a separate opt-in contract, not an implementation of
/// [`CharacterIntentService`]. It offers the same account scoping, atomicity,
/// replay, conflict and selection semantics while its state is retained, but
/// provides no restart-durability guarantee. Implementations must document when
/// their state is lost. Callers must not rely on retries across that boundary;
/// production code requiring restart-safe retries must instead require
/// [`CharacterIntentService`]. There is no fallback to non-idempotent creation.
pub trait EphemeralCharacterIntentService: CharacterService {
    /// Create once and replay the receipt while the implementation retains it.
    ///
    /// Authentication, account/key/request binding, concurrent retries, and
    /// conflicts follow [`CharacterIntentService::create_character_idempotent`],
    /// except that receipts and bindings need not survive a restart.
    fn create_character_idempotent(
        &self,
        token: &SessionToken,
        key: &CharacterCreationKey,
        request: &CreateCharacter,
    ) -> Result<CharacterCreationReceipt, ApiError>;

    /// Resolve a playable owned character using current state, as in
    /// [`CharacterIntentService::select_character`].
    fn select_character(
        &self,
        token: &SessionToken,
        character_id: CharacterId,
    ) -> Result<Character, ApiError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn character_id() -> CharacterId {
        CharacterId::new(StableId::from_parts(IdKind::Character, 1, 7)).unwrap()
    }

    #[test]
    fn character_ids_are_namespace_checked() {
        let id = character_id();
        assert_eq!(CharacterId::parse(&id.to_string()).unwrap(), id);
        assert!(CharacterId::parse("ent_00000000000000000000000000").is_err());
        assert!(CharacterId::parse("not-an-id").is_err());
        assert!(CharacterId::new(StableId::from_parts(IdKind::Entity, 1, 7)).is_err());
        let encoded = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<CharacterId>(&encoded).unwrap(), id);
    }

    #[test]
    fn status_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&CharacterStatus::Suspended).unwrap(),
            "\"suspended\""
        );
    }

    #[test]
    fn creation_key_validation_and_serde() {
        let key = CharacterCreationKey::new("intent_123-abc").unwrap();
        assert_eq!(key.as_str(), "intent_123-abc");
        let json = serde_json::to_string(&key).unwrap();
        assert_eq!(
            serde_json::from_str::<CharacterCreationKey>(&json).unwrap(),
            key
        );
        for invalid in ["", "has space", "secret\n", "é"] {
            assert!(CharacterCreationKey::new(invalid).is_err());
            assert!(
                serde_json::from_str::<CharacterCreationKey>(
                    &serde_json::to_string(invalid).unwrap()
                )
                .is_err()
            );
        }
        assert!(CharacterCreationKey::new("a".repeat(128)).is_ok());
        assert!(CharacterCreationKey::new("a".repeat(129)).is_err());
    }

    #[test]
    fn creation_receipt_round_trips() {
        let receipt = CharacterCreationReceipt {
            character_id: character_id(),
        };
        let json = serde_json::to_string(&receipt).unwrap();
        assert_eq!(
            serde_json::from_str::<CharacterCreationReceipt>(&json).unwrap(),
            receipt
        );
        assert!(
            serde_json::from_str::<CharacterCreationReceipt>(
                r#"{"character_id":"ent_00000000000000000000000000"}"#
            )
            .is_err()
        );
    }
}
