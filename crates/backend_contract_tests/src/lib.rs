//! Shared contract assertions for character persistence adapters.
//!
//! Adapters provide a test-local wrapper that supplies already-authenticated
//! owner credentials. The assertions deliberately never format credentials.

use game_api::{
    ApiError, Character, CharacterCreationKey, CharacterCreationReceipt, CharacterId,
    CreateCharacter,
};

/// Minimal test seam implemented by each adapter's test wrapper.
pub trait CharacterContract {
    /// Adapter-specific authenticated owner handle.
    type Credential;

    /// Create or replay one character creation intent.
    fn create(
        &self,
        credential: &Self::Credential,
        key: &CharacterCreationKey,
        request: &CreateCharacter,
    ) -> Result<CharacterCreationReceipt, ApiError>;

    /// Select one currently playable owned character.
    fn select(
        &self,
        credential: &Self::Credential,
        character: CharacterId,
    ) -> Result<Character, ApiError>;

    /// Tombstone one owned character.
    fn delete(&self, credential: &Self::Credential, character: CharacterId)
    -> Result<(), ApiError>;

    /// Record authoritative entry for one owned character.
    fn enter(
        &self,
        credential: &Self::Credential,
        character: CharacterId,
        zone_id: &str,
    ) -> Result<Character, ApiError>;

    /// Clear recorded entry for one owned character.
    fn leave(
        &self,
        credential: &Self::Credential,
        character: CharacterId,
    ) -> Result<Character, ApiError>;
}

/// A different, still-valid name for the same idempotency key.
///
/// Test callers may pass a name already at the 32-character limit, so appending
/// would make the conflicting request invalid and mask the key conflict.
fn different_name(name: &str) -> String {
    let mut characters: Vec<char> = name.chars().collect();
    if characters.len() < 32 {
        characters.push('x');
    } else if let Some(last) = characters.last_mut() {
        *last = if *last == 'x' { 'y' } else { 'x' };
    }
    characters.into_iter().collect()
}

/// Assert character semantics shared by memory and PostgreSQL adapters.
pub fn assert_character_contract<B: CharacterContract>(
    backend: &B,
    owner: &B::Credential,
    other: &B::Credential,
    zone_id: &str,
    name: &str,
) {
    let key = CharacterCreationKey::new("contract-create-1").expect("valid fixed test key");
    let request = CreateCharacter {
        name: name.to_owned(),
    };
    let receipt = backend
        .create(owner, &key, &request)
        .expect("owner creates character");
    assert_eq!(
        backend
            .create(owner, &key, &request)
            .expect("same key replays receipt"),
        receipt
    );
    let conflicting = CreateCharacter {
        name: different_name(name),
    };
    assert_ne!(conflicting.name, name);
    assert!(matches!(
        backend.create(owner, &key, &conflicting),
        Err(ApiError::Conflict(_))
    ));

    let selected = backend
        .select(owner, receipt.character_id)
        .expect("owner selects character");
    assert_eq!(selected.name, name);
    assert!(matches!(
        backend.select(other, receipt.character_id),
        Err(ApiError::NotFound(_))
    ));
    assert!(matches!(
        backend.delete(other, receipt.character_id),
        Err(ApiError::NotFound(_))
    ));

    let entered = backend
        .enter(owner, receipt.character_id, zone_id)
        .expect("owner enters routed zone");
    assert_eq!(entered.zone_id.as_deref(), Some(zone_id));
    assert!(matches!(
        backend.enter(other, receipt.character_id, zone_id),
        Err(ApiError::NotFound(_))
    ));
    assert_eq!(
        backend
            .leave(owner, receipt.character_id)
            .expect("owner leaves zone")
            .zone_id,
        None
    );

    backend
        .delete(owner, receipt.character_id)
        .expect("owner deletes character");
    assert!(matches!(
        backend.select(owner, receipt.character_id),
        Err(ApiError::NotFound(_))
    ));
    assert_eq!(
        backend
            .create(owner, &key, &request)
            .expect("deletion preserves creation receipt"),
        receipt
    );
}
