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
}
