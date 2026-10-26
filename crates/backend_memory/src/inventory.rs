//! Inventory persistence for the in-memory adapter.

use std::collections::BTreeSet;

use engine_core::StableId;
use game_api::{
    ApiError, CharacterId, CharacterStatus, InventoryService, InventorySlot, InventoryStore,
    SessionToken, validate_slots,
};

use crate::{MemoryBackend, State};

fn slot_order(slots: &mut [InventorySlot]) {
    slots.sort_by_key(|slot| slot.slot);
}

fn instances_in_other_inventories(state: &State, character_id: CharacterId) -> BTreeSet<StableId> {
    state
        .inventory
        .iter()
        .filter(|(owner, _)| **owner != character_id)
        .flat_map(|(_, slots)| slots.iter())
        .filter_map(|slot| slot.stack.instance)
        .collect()
}

impl InventoryService for MemoryBackend {
    fn inventory(
        &self,
        token: &SessionToken,
        character_id: CharacterId,
    ) -> Result<Vec<InventorySlot>, ApiError> {
        let state = self.lock_state()?;
        let account_id = Self::account_for_token(&state, token)?;
        let character = state
            .characters
            .get(&character_id)
            .ok_or_else(|| ApiError::NotFound("character not found".into()))?;
        if character.account_id != account_id || character.status == CharacterStatus::Deleted {
            return Err(ApiError::NotFound("character not found".into()));
        }
        let mut slots = state
            .inventory
            .get(&character_id)
            .cloned()
            .unwrap_or_default();
        slot_order(&mut slots);
        Ok(slots)
    }
}

impl InventoryStore for MemoryBackend {
    fn load_inventory(&self, character_id: CharacterId) -> Result<Vec<InventorySlot>, ApiError> {
        let state = self.lock_state()?;
        let character = state
            .characters
            .get(&character_id)
            .ok_or_else(|| ApiError::NotFound("character not found".into()))?;
        if character.status == CharacterStatus::Deleted {
            return Err(ApiError::NotFound("character not found".into()));
        }
        let mut slots = state
            .inventory
            .get(&character_id)
            .cloned()
            .unwrap_or_default();
        slot_order(&mut slots);
        Ok(slots)
    }

    fn replace_inventory(
        &self,
        character_id: CharacterId,
        slots: &[InventorySlot],
    ) -> Result<(), ApiError> {
        validate_slots(slots)?;
        let mut state = self.lock_state()?;
        let character = state
            .characters
            .get(&character_id)
            .ok_or_else(|| ApiError::NotFound("character not found".into()))?;
        if character.status == CharacterStatus::Deleted {
            return Err(ApiError::NotFound("character not found".into()));
        }

        let taken = instances_in_other_inventories(&state, character_id);
        if let Some(instance) = slots
            .iter()
            .filter_map(|slot| slot.stack.instance)
            .find(|instance| taken.contains(instance))
        {
            return Err(ApiError::Conflict(format!(
                "item instance is owned by another inventory: {instance}"
            )));
        }

        state.inventory.insert(character_id, slots.to_vec());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use engine_core::IdKind;
    use engine_core::id::DefinitionId;
    use engine_core::inventory::ItemStack;
    use game_api::{AuthService, CharacterService, CreateCharacter};

    use super::*;
    use crate::test_support::login;

    fn character(backend: &MemoryBackend, device: &str) -> (SessionToken, CharacterId) {
        let session = backend.authenticate_device(&login(device, true)).unwrap();
        let character = backend
            .create_character(
                &session.token,
                &CreateCharacter {
                    name: format!("Name-{device}"),
                },
            )
            .unwrap();
        (session.token, character.id)
    }

    fn stack(name: &str, quantity: u32) -> ItemStack {
        ItemStack::new(DefinitionId::new(name).unwrap(), quantity)
    }

    #[test]
    fn inventory_round_trips_for_the_owner() {
        let backend = MemoryBackend::new();
        let (token, character_id) = character(&backend, "device-1");
        let slots = vec![
            InventorySlot {
                slot: 1,
                stack: stack("item.torch", 3),
            },
            InventorySlot {
                slot: 0,
                stack: stack("item.key", 1),
            },
        ];
        backend.replace_inventory(character_id, &slots).unwrap();
        let loaded = backend.inventory(&token, character_id).unwrap();
        assert_eq!(loaded[0].slot, 0);
        assert_eq!(loaded[1].slot, 1);
        assert_eq!(backend.load_inventory(character_id).unwrap(), loaded);
    }

    #[test]
    fn another_account_cannot_read_the_inventory() {
        let backend = MemoryBackend::new();
        let (_token, character_id) = character(&backend, "device-1");
        let other = backend
            .authenticate_device(&login("device-2", true))
            .unwrap();
        assert!(matches!(
            backend.inventory(&other.token, character_id),
            Err(ApiError::NotFound(_))
        ));
    }

    #[test]
    fn invalid_and_duplicate_slots_are_rejected() {
        let backend = MemoryBackend::new();
        let (_token, character_id) = character(&backend, "device-1");
        assert!(matches!(
            backend.replace_inventory(
                character_id,
                &[InventorySlot {
                    slot: 0,
                    stack: stack("item.torch", 0),
                }]
            ),
            Err(ApiError::InvalidRequest(_))
        ));
        assert!(matches!(
            backend.replace_inventory(
                character_id,
                &[
                    InventorySlot {
                        slot: 0,
                        stack: stack("item.torch", 1),
                    },
                    InventorySlot {
                        slot: 0,
                        stack: stack("item.key", 1),
                    },
                ]
            ),
            Err(ApiError::Conflict(_))
        ));
    }

    #[test]
    fn unique_instances_cannot_be_owned_twice() {
        let backend = MemoryBackend::new();
        let (_token, first) = character(&backend, "device-1");
        let (_other, second) = character(&backend, "device-2");
        let instance = StableId::from_parts(IdKind::Item, 1, 1);
        let slot = InventorySlot {
            slot: 0,
            stack: ItemStack::unique(DefinitionId::new("item.key").unwrap(), instance),
        };
        backend
            .replace_inventory(first, std::slice::from_ref(&slot))
            .unwrap();
        assert!(matches!(
            backend.replace_inventory(second, &[slot]),
            Err(ApiError::Conflict(_))
        ));
    }
}
