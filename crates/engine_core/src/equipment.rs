//! Equipment slots and authoritative equipped-item state.
//!
//! Equipment is server-owned durable state. Slot compatibility comes from
//! content through [`ItemRules`](crate::inventory::ItemRules); this module only
//! stores validated single-item stacks per slot and never decides what an item
//! may do.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{EngineError, ErrorCode};
use crate::inventory::{ItemRules, ItemStack};

/// The bounded set of equipment slots the engine understands.
///
/// The set is deliberately small and stable: adding a slot is a protocol and
/// persisted-state change, so it needs its own review instead of free-form
/// authored slot names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EquipSlot {
    /// Weapons and tools held in the primary hand.
    MainHand,
    /// Shields, parrying tools, or secondary weapons.
    OffHand,
    /// Helmets and hats.
    Head,
    /// Body armour and clothing.
    Chest,
    /// Gloves and gauntlets.
    Hands,
    /// Leggings and greaves.
    Legs,
    /// Boots and shoes.
    Feet,
}

impl EquipSlot {
    /// Every slot, in display order.
    pub const ALL: [Self; 7] = [
        Self::MainHand,
        Self::OffHand,
        Self::Head,
        Self::Chest,
        Self::Hands,
        Self::Legs,
        Self::Feet,
    ];

    /// The canonical wire and content representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MainHand => "main_hand",
            Self::OffHand => "off_hand",
            Self::Head => "head",
            Self::Chest => "chest",
            Self::Hands => "hands",
            Self::Legs => "legs",
            Self::Feet => "feet",
        }
    }

    /// Parse a wire or content slot name.
    ///
    /// Unknown names are rejected: an unrecognised slot must never silently
    /// become a different one.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|slot| slot.as_str() == value)
    }
}

/// Validated equipped items keyed by slot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Equipment {
    #[serde(default)]
    slots: BTreeMap<EquipSlot, ItemStack>,
}

impl Equipment {
    /// Empty equipment.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Read one slot.
    #[must_use]
    pub fn slot(&self, slot: EquipSlot) -> Option<&ItemStack> {
        self.slots.get(&slot)
    }

    /// Every equipped slot, in slot order.
    pub fn entries(&self) -> impl Iterator<Item = (EquipSlot, &ItemStack)> {
        self.slots.iter().map(|(slot, stack)| (*slot, stack))
    }

    /// Whether nothing is equipped.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Number of equipped slots.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Equip one item, returning whatever occupied the slot before.
    ///
    /// The caller owns the returned stack and must put it back into an
    /// inventory; this type never drops items.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidState`] for a stack of more than one item,
    /// [`ErrorCode::Unsupported`] when the item's content rules do not allow
    /// this slot, and [`ErrorCode::Conflict`] when the same unique instance is
    /// already equipped elsewhere.
    pub fn equip(
        &mut self,
        slot: EquipSlot,
        stack: ItemStack,
        rules: &dyn ItemRules,
    ) -> Result<Option<ItemStack>, EngineError> {
        if stack.quantity != 1 {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "only single items can be equipped",
            )
            .with_detail("quantity", stack.quantity.to_string()));
        }
        let allowed = rules.equip_slot(&stack.definition);
        if allowed != Some(slot) {
            return Err(EngineError::new(
                ErrorCode::Unsupported,
                "item cannot be equipped in this slot",
            )
            .with_detail("item", stack.definition.to_string())
            .with_detail("slot", slot.as_str())
            .with_detail("allowed_slot", allowed.map_or("none", EquipSlot::as_str)));
        }
        if let Some(instance) = stack.instance
            && self
                .slots
                .iter()
                .any(|(other, equipped)| *other != slot && equipped.instance == Some(instance))
        {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "the same item instance is already equipped",
            )
            .with_detail("instance", instance.to_string()));
        }
        Ok(self.slots.insert(slot, stack))
    }

    /// Remove and return the item in `slot`.
    pub fn unequip(&mut self, slot: EquipSlot) -> Option<ItemStack> {
        self.slots.remove(&slot)
    }

    /// Re-check deserialized equipment against structural rules only.
    ///
    /// Use this when content rules are unavailable, such as on a client that
    /// only displays replicated owner state.
    ///
    /// # Errors
    ///
    /// Returns an error for a multi-item stack or a unique instance equipped
    /// twice.
    pub fn validate_structure(&self) -> Result<(), EngineError> {
        let mut seen = BTreeMap::new();
        for (slot, stack) in &self.slots {
            if stack.quantity != 1 {
                return Err(EngineError::new(
                    ErrorCode::InvalidRequest,
                    "equipment stacks must contain exactly one item",
                )
                .with_detail("slot", slot.as_str()));
            }
            if let Some(instance) = stack.instance
                && seen.insert(instance, *slot).is_some()
            {
                return Err(EngineError::new(
                    ErrorCode::Conflict,
                    "the same item instance is equipped in multiple slots",
                )
                .with_detail("instance", instance.to_string()));
            }
        }
        Ok(())
    }

    /// Re-check deserialized equipment against content and structural rules.
    ///
    /// # Errors
    ///
    /// Returns an error for a multi-item stack, an item whose content slot does
    /// not match its stored slot, or a unique instance equipped twice.
    pub fn validate(&self, rules: &dyn ItemRules) -> Result<(), EngineError> {
        self.validate_structure()?;
        for (slot, stack) in &self.slots {
            let allowed = rules.equip_slot(&stack.definition);
            if allowed != Some(*slot) {
                return Err(EngineError::new(
                    ErrorCode::InvalidRequest,
                    "equipped item does not belong in its stored slot",
                )
                .with_detail("item", stack.definition.to_string())
                .with_detail("slot", slot.as_str())
                .with_detail("allowed_slot", allowed.map_or("none", EquipSlot::as_str)));
            }
        }
        Ok(())
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{DefinitionId, IdKind, StableId};
    use crate::inventory::TableItemRules;

    fn item(name: &str) -> DefinitionId {
        DefinitionId::new(name).unwrap()
    }

    fn rules() -> TableItemRules {
        TableItemRules::new(10)
            .with_item(item("item.sword"), 1, Some(EquipSlot::MainHand))
            .with_item(item("item.shield"), 1, Some(EquipSlot::OffHand))
            .with_item(item("item.torch"), 5, None)
    }

    #[test]
    fn slot_names_round_trip_and_reject_unknown_values() {
        for slot in EquipSlot::ALL {
            assert_eq!(EquipSlot::parse(slot.as_str()), Some(slot));
        }
        assert_eq!(EquipSlot::parse("tail"), None);
        assert_eq!(EquipSlot::parse(""), None);
    }

    #[test]
    fn equipping_swaps_only_within_the_authored_slot() {
        let rules = rules();
        let sword = item("item.sword");
        let mut equipment = Equipment::new();
        assert!(equipment.is_empty());

        let first = ItemStack::unique(sword.clone(), StableId::from_parts(IdKind::Item, 1, 1));
        assert_eq!(
            equipment.equip(EquipSlot::MainHand, first, &rules).unwrap(),
            None
        );
        let second = ItemStack::unique(sword.clone(), StableId::from_parts(IdKind::Item, 1, 2));
        let replaced = equipment
            .equip(EquipSlot::MainHand, second, &rules)
            .unwrap()
            .expect("the previous item is returned");
        assert_eq!(
            replaced.instance,
            Some(StableId::from_parts(IdKind::Item, 1, 1))
        );
        assert_eq!(equipment.len(), 1);
        equipment.validate(&rules).unwrap();

        let shield = ItemStack::unique(
            item("item.shield"),
            StableId::from_parts(IdKind::Item, 1, 3),
        );
        assert_eq!(
            equipment
                .equip(EquipSlot::MainHand, shield, &rules)
                .unwrap_err()
                .code(),
            ErrorCode::Unsupported
        );
        assert!(equipment.slot(EquipSlot::OffHand).is_none());
    }

    #[test]
    fn stacks_and_duplicate_instances_are_rejected() {
        let rules = rules();
        let mut equipment = Equipment::new();
        let sword = item("item.sword");
        let stack = ItemStack::new(sword.clone(), 2);
        assert_eq!(
            equipment
                .equip(EquipSlot::MainHand, stack, &rules)
                .unwrap_err()
                .code(),
            ErrorCode::InvalidState
        );

        let instance = StableId::from_parts(IdKind::Item, 2, 9);
        equipment
            .equip(
                EquipSlot::MainHand,
                ItemStack::unique(sword, instance),
                &rules,
            )
            .unwrap();
        // The same instance cannot appear in a second slot even though the
        // stored definition would not allow it there either.
        let duplicate = ItemStack::unique(item("item.torch"), instance);
        assert!(
            equipment
                .equip(EquipSlot::OffHand, duplicate, &rules)
                .is_err()
        );
    }

    #[test]
    fn validation_rejects_items_from_the_wrong_slot_and_bad_stacks() {
        let rules = rules();
        let mut equipment = Equipment::new();
        equipment
            .equip(
                EquipSlot::MainHand,
                ItemStack::unique(item("item.sword"), StableId::from_parts(IdKind::Item, 3, 1)),
                &rules,
            )
            .unwrap();

        let encoded = serde_json::to_string(&equipment).unwrap();
        let decoded: Equipment = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, equipment);
        decoded.validate(&rules).unwrap();

        let instance = StableId::from_parts(IdKind::Item, 3, 2);
        let wrong_slot = serde_json::json!({
            "slots": {
                "head": {"definition": "item.sword", "quantity": 1, "instance": instance.to_string()}
            }
        });
        let wrong_slot: Equipment = serde_json::from_value(wrong_slot).unwrap();
        assert_eq!(
            wrong_slot.validate(&rules).unwrap_err().code(),
            ErrorCode::InvalidRequest
        );

        let bad_stack = serde_json::json!({
            "slots": {
                "main_hand": {"definition": "item.sword", "quantity": 2}
            }
        });
        let bad_stack: Equipment = serde_json::from_value(bad_stack).unwrap();
        assert_eq!(
            bad_stack.validate(&rules).unwrap_err().code(),
            ErrorCode::InvalidRequest
        );
    }

    #[test]
    fn unequipping_returns_the_item() {
        let rules = rules();
        let mut equipment = Equipment::new();
        let sword = ItemStack::unique(item("item.sword"), StableId::from_parts(IdKind::Item, 4, 1));
        let expected = sword.clone();
        equipment.equip(EquipSlot::MainHand, sword, &rules).unwrap();
        assert_eq!(equipment.unequip(EquipSlot::MainHand), Some(expected));
        assert!(equipment.is_empty());
        assert_eq!(equipment.unequip(EquipSlot::MainHand), None);
    }
}
