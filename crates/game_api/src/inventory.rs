//! Persistence-facing inventory contracts.
//!
//! Inventory mutations are authoritative simulation results. Clients never
//! call [`InventoryStore`]; the world service validates an action first and
//! persists the resulting snapshot. [`InventoryService`] is the read-only view
//! exposed to the owning account.

use std::collections::{BTreeMap, BTreeSet};

use engine_core::IdKind;
use engine_core::inventory::{ItemStack, MAX_SLOTS};
use serde::{Deserialize, Serialize};

use crate::character::CharacterId;
use crate::service::{ApiError, SessionToken};

/// One occupied inventory slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventorySlot {
    /// Zero-based slot index, below [`MAX_SLOTS`].
    pub slot: u16,
    /// Item definition, quantity, and optional unique instance.
    pub stack: ItemStack,
}

impl InventorySlot {
    /// Validate one slot without checking cross-slot uniqueness.
    pub fn validate(&self) -> Result<(), ApiError> {
        if usize::from(self.slot) >= MAX_SLOTS {
            return Err(ApiError::InvalidRequest(format!(
                "inventory slot must be below {MAX_SLOTS}"
            )));
        }
        if self.stack.quantity == 0 {
            return Err(ApiError::InvalidRequest(
                "inventory stack quantity must be at least one".into(),
            ));
        }
        if let Some(instance) = self.stack.instance {
            if instance.kind() != IdKind::Item {
                return Err(ApiError::InvalidRequest(
                    "inventory instance must be an item identifier".into(),
                ));
            }
            if self.stack.quantity != 1 {
                return Err(ApiError::InvalidRequest(
                    "a unique item stack must contain exactly one item".into(),
                ));
            }
        }
        Ok(())
    }
}

/// Validate a complete inventory snapshot.
///
/// Rejects invalid slots, duplicate slot indices, and the same unique item
/// instance appearing twice.
pub fn validate_slots(slots: &[InventorySlot]) -> Result<(), ApiError> {
    let mut by_slot = BTreeSet::new();
    let mut instances = BTreeMap::new();
    for slot in slots {
        slot.validate()?;
        if !by_slot.insert(slot.slot) {
            return Err(ApiError::Conflict(format!(
                "duplicate inventory slot: {}",
                slot.slot
            )));
        }
        if let Some(instance) = slot.stack.instance
            && instances.insert(instance, slot.slot).is_some()
        {
            return Err(ApiError::Conflict(format!(
                "item instance appears in multiple slots: {instance}"
            )));
        }
    }
    Ok(())
}

/// Read-only inventory access for the owning account.
pub trait InventoryService {
    /// Return the authenticated account's inventory for one of its characters.
    ///
    /// Adapters must verify that `character_id` belongs to the caller.
    fn inventory(
        &self,
        token: &SessionToken,
        character_id: CharacterId,
    ) -> Result<Vec<InventorySlot>, ApiError>;
}

/// Trusted persistence access used by the authoritative world service.
///
/// This trait is not a client contract. Callers must have validated the
/// mutation through the simulation and must not expose these methods over a
/// player-facing endpoint.
pub trait InventoryStore {
    /// Load the last persisted snapshot for a character.
    fn load_inventory(&self, character_id: CharacterId) -> Result<Vec<InventorySlot>, ApiError>;

    /// Replace a character's persisted snapshot atomically.
    fn replace_inventory(
        &self,
        character_id: CharacterId,
        slots: &[InventorySlot],
    ) -> Result<(), ApiError>;
}
