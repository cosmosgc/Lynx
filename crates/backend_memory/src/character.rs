//! Character management for the in-memory adapter.

use engine_core::{IdKind, StableId};
use game_api::{
    AccountId, ApiError, Character, CharacterCreationKey, CharacterCreationReceipt, CharacterId,
    CharacterService, CharacterStatus, CreateCharacter, EphemeralCharacterIntentService,
    SessionToken,
};

use crate::{MemoryBackend, State};

fn next_character_id(state: &mut State) -> Result<CharacterId, ApiError> {
    state.next_id = state
        .next_id
        .checked_add(1)
        .ok_or_else(|| ApiError::Unavailable("in-memory ID space exhausted".into()))?;
    CharacterId::new(StableId::from_parts(
        IdKind::Character,
        0,
        u128::from(state.next_id),
    ))
}

fn create_in_state(
    state: &mut State,
    account_id: AccountId,
    request: &CreateCharacter,
    character_limit: usize,
) -> Result<Character, ApiError> {
    request.validate()?;
    let owned = state
        .characters
        .values()
        .filter(|character| {
            character.account_id == account_id && character.status != CharacterStatus::Deleted
        })
        .count();
    if owned >= character_limit {
        return Err(ApiError::Conflict(format!(
            "character limit reached: {character_limit}"
        )));
    }
    let folded_name = request.name.to_lowercase();
    if state.characters.values().any(|character| {
        character.status != CharacterStatus::Deleted && character.name.to_lowercase() == folded_name
    }) {
        return Err(ApiError::Conflict(format!(
            "character name is already in use: {}",
            request.name
        )));
    }
    let id = next_character_id(state)?;
    let character = Character {
        id,
        account_id,
        name: request.name.clone(),
        zone_id: None,
        status: CharacterStatus::Active,
        created_at: None,
    };
    state.characters.insert(id, character.clone());
    Ok(character)
}

impl CharacterService for MemoryBackend {
    fn characters(&self, token: &SessionToken) -> Result<Vec<Character>, ApiError> {
        let state = self.lock_state()?;
        let account_id = Self::account_for_token(&state, token)?;
        Ok(state
            .characters
            .values()
            .filter(|character| {
                character.account_id == account_id && character.status != CharacterStatus::Deleted
            })
            .cloned()
            .collect())
    }

    fn create_character(
        &self,
        token: &SessionToken,
        request: &CreateCharacter,
    ) -> Result<Character, ApiError> {
        request.validate()?;
        let mut state = self.lock_state()?;
        let account_id = Self::account_for_token(&state, token)?;

        create_in_state(&mut state, account_id, request, self.character_limit)
    }

    fn delete_character(
        &self,
        token: &SessionToken,
        character_id: CharacterId,
    ) -> Result<(), ApiError> {
        let mut state = self.lock_state()?;
        let account_id = Self::account_for_token(&state, token)?;
        {
            let character = state
                .characters
                .get_mut(&character_id)
                .ok_or_else(|| ApiError::NotFound("character not found".into()))?;
            if character.account_id != account_id {
                return Err(ApiError::NotFound("character not found".into()));
            }
            if character.status == CharacterStatus::Deleted {
                return Ok(());
            }
            character.status = CharacterStatus::Deleted;
            character.zone_id = None;
        }
        state.inventory.remove(&character_id);
        Ok(())
    }
}

impl EphemeralCharacterIntentService for MemoryBackend {
    fn create_character_idempotent(
        &self,
        token: &SessionToken,
        key: &CharacterCreationKey,
        request: &CreateCharacter,
    ) -> Result<CharacterCreationReceipt, ApiError> {
        let mut state = self.lock_state()?;
        let account_id = Self::account_for_token(&state, token)?;
        let receipt_key = (account_id.clone(), key.as_str().to_owned());
        if let Some((original, receipt)) = state.character_receipts.get(&receipt_key) {
            return if original == request {
                Ok(receipt.clone())
            } else {
                Err(ApiError::Conflict(
                    "character creation key already used for a different request".into(),
                ))
            };
        }
        let character = create_in_state(&mut state, account_id, request, self.character_limit)?;
        let receipt = CharacterCreationReceipt {
            character_id: character.id,
        };
        state
            .character_receipts
            .insert(receipt_key, (request.clone(), receipt.clone()));
        Ok(receipt)
    }

    fn select_character(
        &self,
        token: &SessionToken,
        character_id: CharacterId,
    ) -> Result<Character, ApiError> {
        let state = self.lock_state()?;
        let account_id = Self::account_for_token(&state, token)?;
        state
            .characters
            .get(&character_id)
            .filter(|character| character.account_id == account_id && character.is_playable())
            .cloned()
            .ok_or_else(|| ApiError::NotFound("character not found".into()))
    }
}

#[cfg(test)]
mod tests {
    use game_api::{AuthService, InventoryService};

    use super::*;
    use crate::test_support::login;

    fn session(backend: &MemoryBackend, device: &str) -> game_api::Session {
        backend.authenticate_device(&login(device, true)).unwrap()
    }

    fn create(backend: &MemoryBackend, token: &SessionToken, name: &str) -> Character {
        backend
            .create_character(token, &CreateCharacter { name: name.into() })
            .unwrap()
    }

    #[test]
    fn characters_are_created_listed_and_deleted() {
        let backend = MemoryBackend::new();
        let session = session(&backend, "device-1");
        let character = create(&backend, &session.token, "Aria");
        assert_eq!(character.account_id, session.account_id);
        assert!(character.is_playable());
        assert_eq!(
            backend.characters(&session.token).unwrap(),
            vec![character.clone()]
        );

        backend
            .delete_character(&session.token, character.id)
            .unwrap();
        assert_eq!(backend.characters(&session.token).unwrap(), Vec::new());
        assert!(matches!(
            backend.inventory(&session.token, character.id),
            Err(ApiError::NotFound(_))
        ));
    }

    #[test]
    fn names_are_unique_case_insensitively_and_freed_on_delete() {
        let backend = MemoryBackend::new();
        let session = session(&backend, "device-1");
        let first = create(&backend, &session.token, "Aria");
        assert!(matches!(
            backend.create_character(
                &session.token,
                &CreateCharacter {
                    name: "aria".into()
                }
            ),
            Err(ApiError::Conflict(_))
        ));
        backend.delete_character(&session.token, first.id).unwrap();
        assert!(
            backend
                .create_character(
                    &session.token,
                    &CreateCharacter {
                        name: "ARIA".into()
                    }
                )
                .is_ok()
        );
    }

    #[test]
    fn character_limits_and_ownership_are_enforced() {
        let backend = MemoryBackend::new().with_character_limit(1);
        let owner = session(&backend, "device-1");
        let other = session(&backend, "device-2");
        let character = create(&backend, &owner.token, "Aria");
        assert!(matches!(
            backend.create_character(
                &owner.token,
                &CreateCharacter {
                    name: "Bram".into()
                }
            ),
            Err(ApiError::Conflict(_))
        ));
        assert!(matches!(
            backend.delete_character(&other.token, character.id),
            Err(ApiError::NotFound(_))
        ));
        assert_eq!(backend.characters(&owner.token).unwrap().len(), 1);
    }

    #[test]
    fn intents_are_account_scoped_replayed_and_survive_deletion() {
        let backend = MemoryBackend::new().with_character_limit(1);
        let owner = session(&backend, "device-1");
        let other = session(&backend, "device-2");
        let key = CharacterCreationKey::new("same-key").unwrap();
        let request = CreateCharacter {
            name: "Aria".into(),
        };
        let receipt = backend
            .create_character_idempotent(&owner.token, &key, &request)
            .unwrap();
        let owner_again = session(&backend, "device-1");
        assert_eq!(
            backend
                .create_character_idempotent(&owner_again.token, &key, &request)
                .unwrap(),
            receipt
        );
        assert!(matches!(
            backend.create_character_idempotent(
                &owner.token,
                &key,
                &CreateCharacter {
                    name: "aria".into()
                }
            ),
            Err(ApiError::Conflict(_))
        ));
        assert!(matches!(
            backend.create_character_idempotent(
                &owner.token,
                &key,
                &CreateCharacter {
                    name: String::new()
                }
            ),
            Err(ApiError::Conflict(_))
        ));
        let other_receipt = backend
            .create_character_idempotent(
                &other.token,
                &key,
                &CreateCharacter {
                    name: "Bram".into(),
                },
            )
            .unwrap();
        assert_ne!(receipt, other_receipt);
        assert!(matches!(
            backend.select_character(&other.token, receipt.character_id),
            Err(ApiError::NotFound(_))
        ));
        assert_eq!(
            backend
                .select_character(&owner.token, receipt.character_id)
                .unwrap()
                .name,
            "Aria"
        );
        backend
            .delete_character(&owner.token, receipt.character_id)
            .unwrap();
        create(&backend, &owner.token, "Aria");
        assert_eq!(
            backend
                .create_character_idempotent(&owner.token, &key, &request)
                .unwrap(),
            receipt
        );
        assert!(matches!(
            backend.select_character(&owner.token, receipt.character_id),
            Err(ApiError::NotFound(_))
        ));
        backend.logout(&owner.token).unwrap();
        assert!(matches!(
            backend.create_character_idempotent(&owner.token, &key, &request),
            Err(ApiError::Unauthenticated)
        ));
        assert!(matches!(
            backend.select_character(&owner.token, receipt.character_id),
            Err(ApiError::Unauthenticated)
        ));
    }

    #[test]
    fn failed_creation_does_not_bind_key_and_selection_checks_current_status() {
        let backend = MemoryBackend::new();
        let owner = session(&backend, "device-1");
        let key = CharacterCreationKey::new("key").unwrap();
        assert!(matches!(
            backend.create_character_idempotent(
                &owner.token,
                &key,
                &CreateCharacter {
                    name: String::new()
                }
            ),
            Err(ApiError::InvalidRequest(_))
        ));
        let request = CreateCharacter {
            name: "Aria".into(),
        };
        let receipt = backend
            .create_character_idempotent(&owner.token, &key, &request)
            .unwrap();
        backend
            .lock_state()
            .unwrap()
            .characters
            .get_mut(&receipt.character_id)
            .unwrap()
            .status = CharacterStatus::Suspended;
        assert!(matches!(
            backend.select_character(&owner.token, receipt.character_id),
            Err(ApiError::NotFound(_))
        ));
        assert_eq!(
            backend
                .create_character_idempotent(&owner.token, &key, &request)
                .unwrap(),
            receipt
        );
    }

    #[test]
    fn selection_hides_unavailable_characters_and_rechecks_status() {
        let backend = MemoryBackend::new();
        let owner = session(&backend, "selection-owner");
        let other = session(&backend, "selection-other");
        let active = create(&backend, &owner.token, "Aria");
        let suspended = create(&backend, &owner.token, "Bram");
        let deleted = create(&backend, &owner.token, "Cora");
        backend
            .lock_state()
            .unwrap()
            .characters
            .get_mut(&suspended.id)
            .unwrap()
            .status = CharacterStatus::Suspended;
        backend.delete_character(&owner.token, deleted.id).unwrap();
        let missing = CharacterId::new(StableId::from_parts(IdKind::Character, 0, 0)).unwrap();
        assert_eq!(
            backend.select_character(&owner.token, active.id).unwrap(),
            active
        );
        let missing_error = backend.select_character(&owner.token, missing).unwrap_err();
        assert!(matches!(missing_error, ApiError::NotFound(_)));
        for (token, id) in [
            (&other.token, active.id),
            (&other.token, suspended.id),
            (&other.token, deleted.id),
            (&owner.token, suspended.id),
            (&owner.token, deleted.id),
        ] {
            assert_eq!(
                backend.select_character(token, id).unwrap_err().to_string(),
                missing_error.to_string()
            );
        }
        backend
            .lock_state()
            .unwrap()
            .characters
            .get_mut(&suspended.id)
            .unwrap()
            .status = CharacterStatus::Active;
        assert!(backend.select_character(&owner.token, suspended.id).is_ok());
        backend.logout(&owner.token).unwrap();
        assert!(matches!(
            backend.select_character(&owner.token, missing),
            Err(ApiError::Unauthenticated)
        ));
        assert!(matches!(
            backend.select_character(&owner.token, active.id),
            Err(ApiError::Unauthenticated)
        ));
    }

    #[test]
    fn concurrent_retries_create_only_once() {
        let backend = MemoryBackend::new();
        let owner = session(&backend, "device-1");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(16));
        let threads: Vec<_> = (0..16)
            .map(|_| {
                let backend = backend.clone();
                let token = owner.token.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    backend
                        .create_character_idempotent(
                            &token,
                            &CharacterCreationKey::new("concurrent").unwrap(),
                            &CreateCharacter {
                                name: "Aria".into(),
                            },
                        )
                        .unwrap()
                })
            })
            .collect();
        let receipts: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert!(receipts.iter().all(|receipt| receipt == &receipts[0]));
        assert_eq!(backend.characters(&owner.token).unwrap().len(), 1);
    }

    #[test]
    fn receipts_are_shared_by_clones_but_not_new_instances() {
        let backend = MemoryBackend::new();
        let clone = backend.clone();
        let owner = session(&backend, "device-1");
        let key = CharacterCreationKey::new("key").unwrap();
        let request = CreateCharacter {
            name: "Aria".into(),
        };
        let receipt = backend
            .create_character_idempotent(&owner.token, &key, &request)
            .unwrap();
        drop(backend);
        assert_eq!(
            clone
                .create_character_idempotent(&owner.token, &key, &request)
                .unwrap(),
            receipt
        );

        let fresh = MemoryBackend::new();
        assert!(fresh.lock_state().unwrap().character_receipts.is_empty());
        assert!(matches!(
            fresh.create_character_idempotent(&owner.token, &key, &request),
            Err(ApiError::Unauthenticated)
        ));
        let fresh_owner = session(&fresh, "device-1");
        fresh
            .create_character_idempotent(&fresh_owner.token, &key, &request)
            .unwrap();
        assert_eq!(fresh.characters(&fresh_owner.token).unwrap().len(), 1);
    }
}
