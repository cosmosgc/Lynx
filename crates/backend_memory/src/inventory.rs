//! Inventory persistence for the in-memory adapter.

use std::collections::BTreeSet;

use engine_core::StableId;
use engine_core::inventory::TransferReceipt;
use game_api::{
    ApiError, CharacterId, CharacterStatus, InventoryService, InventorySlot, InventoryStore,
    SessionToken, TransferInventoryUpdate, VersionedInventory, validate_slots,
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
        Ok(self.load_versioned_inventory(character_id)?.slots)
    }

    fn load_versioned_inventory(
        &self,
        character_id: CharacterId,
    ) -> Result<VersionedInventory, ApiError> {
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
        Ok(VersionedInventory {
            version: state
                .inventory_versions
                .get(&character_id)
                .copied()
                .unwrap_or(0),
            slots,
        })
    }

    fn replace_inventory(
        &self,
        character_id: CharacterId,
        slots: &[InventorySlot],
    ) -> Result<(), ApiError> {
        validate_slots(slots)?;
        let mut state = self.lock_state()?;
        replace_inventory_locked(&mut state, character_id, None, slots).map(|_| ())
    }

    fn replace_inventory_if_version(
        &self,
        character_id: CharacterId,
        expected: i64,
        slots: &[InventorySlot],
    ) -> Result<i64, ApiError> {
        validate_slots(slots)?;
        let mut state = self.lock_state()?;
        replace_inventory_locked(&mut state, character_id, Some(expected), slots)
    }

    #[allow(clippy::too_many_lines)] // The lock holds the complete in-memory transfer commit.
    fn commit_transfer(
        &self,
        key: &str,
        receipt: &TransferReceipt,
        source: TransferInventoryUpdate<'_>,
        destination: TransferInventoryUpdate<'_>,
    ) -> Result<TransferReceipt, ApiError> {
        if key.is_empty()
            || key.len() > 256
            || receipt.key != key
            || receipt.quantity == 0
            || source.character_id == destination.character_id
        {
            return Err(ApiError::InvalidRequest(
                "transfer receipt is invalid".into(),
            ));
        }
        if source.expected_version < 0 || destination.expected_version < 0 {
            return Err(ApiError::InvalidRequest(
                "inventory version is invalid".into(),
            ));
        }
        validate_slots(source.slots)?;
        validate_slots(destination.slots)?;
        let mut state = self.lock_state()?;
        if let Some((applied_source, applied_destination, applied)) =
            state.inventory_transfer_receipts.get(key)
        {
            return if *applied_source == source.character_id
                && *applied_destination == destination.character_id
                && *applied == *receipt
            {
                Ok(applied.clone())
            } else {
                Err(ApiError::Conflict(
                    "idempotency key was used for another transfer".into(),
                ))
            };
        }
        for (character_id, expected) in [
            (source.character_id, source.expected_version),
            (destination.character_id, destination.expected_version),
        ] {
            state
                .characters
                .get(&character_id)
                .filter(|character| character.status != CharacterStatus::Deleted)
                .ok_or_else(|| ApiError::NotFound("character not found".into()))?;
            let current = state
                .inventory_versions
                .get(&character_id)
                .copied()
                .unwrap_or(0);
            if current != expected {
                return Err(ApiError::Conflict("inventory version changed".into()));
            }
        }
        let source_instances: BTreeSet<_> = source
            .slots
            .iter()
            .filter_map(|slot| slot.stack.instance)
            .collect();
        if destination
            .slots
            .iter()
            .filter_map(|slot| slot.stack.instance)
            .any(|id| source_instances.contains(&id))
        {
            return Err(ApiError::Conflict(
                "item instance appears in both inventories".into(),
            ));
        }
        let other_instances: BTreeSet<_> = state
            .inventory
            .iter()
            .filter(|(owner, _)| {
                **owner != source.character_id && **owner != destination.character_id
            })
            .flat_map(|(_, slots)| slots.iter())
            .filter_map(|slot| slot.stack.instance)
            .collect();
        if source
            .slots
            .iter()
            .chain(destination.slots)
            .filter_map(|slot| slot.stack.instance)
            .any(|instance| other_instances.contains(&instance))
        {
            return Err(ApiError::Conflict(
                "item instance is owned by another inventory".into(),
            ));
        }
        let source_next = source
            .expected_version
            .checked_add(1)
            .ok_or_else(|| ApiError::Unavailable("inventory version exhausted".into()))?;
        let destination_next = destination
            .expected_version
            .checked_add(1)
            .ok_or_else(|| ApiError::Unavailable("inventory version exhausted".into()))?;
        state
            .inventory
            .insert(source.character_id, source.slots.to_vec());
        state
            .inventory
            .insert(destination.character_id, destination.slots.to_vec());
        state
            .inventory_versions
            .insert(source.character_id, source_next);
        state
            .inventory_versions
            .insert(destination.character_id, destination_next);
        state.inventory_transfer_receipts.insert(
            key.to_owned(),
            (
                source.character_id,
                destination.character_id,
                receipt.clone(),
            ),
        );
        Ok(receipt.clone())
    }
}

fn replace_inventory_locked(
    state: &mut State,
    character_id: CharacterId,
    expected: Option<i64>,
    slots: &[InventorySlot],
) -> Result<i64, ApiError> {
    let character = state
        .characters
        .get(&character_id)
        .ok_or_else(|| ApiError::NotFound("character not found".into()))?;
    if character.status == CharacterStatus::Deleted {
        return Err(ApiError::NotFound("character not found".into()));
    }

    let version = state
        .inventory_versions
        .get(&character_id)
        .copied()
        .unwrap_or(0);
    if expected.is_some_and(|expected| expected != version) {
        return Err(ApiError::Conflict("inventory version changed".into()));
    }

    let taken = instances_in_other_inventories(state, character_id);
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
    let next = version
        .checked_add(1)
        .ok_or_else(|| ApiError::Unavailable("inventory version exhausted".into()))?;
    state.inventory_versions.insert(character_id, next);
    Ok(next)
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
    fn inventory_versions_reject_stale_replacements_atomically() {
        let backend = MemoryBackend::new();
        let (_token, character_id) = character(&backend, "versioned");
        let initial = backend.load_versioned_inventory(character_id).unwrap();
        assert_eq!(initial.version, 0);
        let slots = vec![InventorySlot {
            slot: 0,
            stack: stack("item.torch", 2),
        }];
        assert_eq!(
            backend
                .replace_inventory_if_version(character_id, initial.version, &slots)
                .unwrap(),
            1
        );
        let stale = backend.replace_inventory_if_version(character_id, 0, &[]);
        assert!(matches!(stale, Err(ApiError::Conflict(_))));
        let current = backend.load_versioned_inventory(character_id).unwrap();
        assert_eq!(current.version, 1);
        assert_eq!(current.slots, slots);
    }

    #[test]
    fn transfer_commit_replays_receipts_without_replacing_inventories_twice() {
        let backend = MemoryBackend::new();
        let (_token, source) = character(&backend, "source");
        let (_token, destination) = character(&backend, "destination");
        let old_source = vec![InventorySlot {
            slot: 0,
            stack: stack("item.ore", 5),
        }];
        let new_source = vec![InventorySlot {
            slot: 0,
            stack: stack("item.ore", 2),
        }];
        let new_destination = vec![InventorySlot {
            slot: 0,
            stack: stack("item.ore", 3),
        }];
        backend.replace_inventory(source, &old_source).unwrap();
        let receipt = TransferReceipt {
            key: "transfer-once".into(),
            source: StableId::from_parts(engine_core::IdKind::Entity, 0, 1),
            source_slot: 0,
            destination: StableId::from_parts(engine_core::IdKind::Entity, 0, 2),
            definition: DefinitionId::new("item.ore").unwrap(),
            quantity: 3,
            destination_slots: vec![0],
        };
        let applied = backend
            .commit_transfer(
                &receipt.key,
                &receipt,
                TransferInventoryUpdate {
                    character_id: source,
                    expected_version: 1,
                    slots: &new_source,
                },
                TransferInventoryUpdate {
                    character_id: destination,
                    expected_version: 0,
                    slots: &new_destination,
                },
            )
            .unwrap();
        assert_eq!(applied, receipt);
        let replay = backend
            .commit_transfer(
                &receipt.key,
                &receipt,
                TransferInventoryUpdate {
                    character_id: source,
                    expected_version: 0,
                    slots: &old_source,
                },
                TransferInventoryUpdate {
                    character_id: destination,
                    expected_version: 0,
                    slots: &[],
                },
            )
            .unwrap();
        assert_eq!(replay, receipt);
        assert_eq!(backend.load_versioned_inventory(source).unwrap().version, 2);
        assert_eq!(
            backend.load_versioned_inventory(source).unwrap().slots,
            new_source
        );
        assert_eq!(
            backend.load_versioned_inventory(destination).unwrap().slots,
            new_destination
        );
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
