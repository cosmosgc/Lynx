//! Character management for the in-memory adapter.

use engine_core::{IdKind, StableId};
use game_api::{
    ApiError, Character, CharacterId, CharacterService, CharacterStatus, CreateCharacter,
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

        let owned = state
            .characters
            .values()
            .filter(|character| {
                character.account_id == account_id && character.status != CharacterStatus::Deleted
            })
            .count();
        if owned >= self.character_limit {
            return Err(ApiError::Conflict(format!(
                "character limit reached: {}",
                self.character_limit
            )));
        }

        let folded_name = request.name.to_lowercase();
        if state.characters.values().any(|character| {
            character.status != CharacterStatus::Deleted
                && character.name.to_lowercase() == folded_name
        }) {
            return Err(ApiError::Conflict(format!(
                "character name is already in use: {}",
                request.name
            )));
        }

        let id = next_character_id(&mut state)?;
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
}
