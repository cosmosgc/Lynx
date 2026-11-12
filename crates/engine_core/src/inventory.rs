//! Inventory stacks, atomic transfers, and idempotency.
//!
//! Item conservation is a required invariant: a transfer either moves the
//! exact quantity or leaves both inventories untouched.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{EngineError, ErrorCode};
use crate::id::{DefinitionId, IdKind, StableId};

/// The largest inventory the engine accepts.
pub const MAX_SLOTS: usize = 512;

/// A quantity of an item. Unique items carry an item-instance ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemStack {
    /// Authored item definition.
    pub definition: DefinitionId,
    /// Number of items in the stack, always at least one.
    pub quantity: u32,
    /// Item instance for unique, indivisible items.
    pub instance: Option<StableId>,
}

impl ItemStack {
    /// A stackable quantity.
    #[must_use]
    pub fn new(definition: DefinitionId, quantity: u32) -> Self {
        Self {
            definition,
            quantity,
            instance: None,
        }
    }

    /// A unique item instance.
    #[must_use]
    pub fn unique(definition: DefinitionId, instance: StableId) -> Self {
        Self {
            definition,
            quantity: 1,
            instance: Some(instance),
        }
    }

    /// Whether this stack represents a unique item and cannot be split.
    #[must_use]
    pub fn is_unique(&self) -> bool {
        self.instance.is_some()
    }
}

/// Per-item stacking rules. Load these from content in production.
pub trait ItemRules {
    /// Maximum quantity in one slot. Must be at least one.
    fn max_stack(&self, definition: &DefinitionId) -> u32;
}

/// The same stack limit for every item. Intended for tests and prototypes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UniformItemRules {
    /// Maximum quantity per slot.
    pub max_stack: u32,
}

impl UniformItemRules {
    /// Create uniform rules with a limit of at least one.
    #[must_use]
    pub const fn new(max_stack: u32) -> Self {
        Self {
            max_stack: if max_stack == 0 { 1 } else { max_stack },
        }
    }
}

impl Default for UniformItemRules {
    fn default() -> Self {
        Self { max_stack: 99 }
    }
}

impl ItemRules for UniformItemRules {
    fn max_stack(&self, _definition: &DefinitionId) -> u32 {
        self.max_stack
    }
}

/// Slots changed by a successful add.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddReport {
    /// Slots that received items, in application order.
    pub slots: Vec<usize>,
    /// Quantity added.
    pub quantity: u32,
}

/// A fixed-capacity, slot-based inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inventory {
    capacity: usize,
    slots: Vec<Option<ItemStack>>,
}

impl Inventory {
    /// Create an empty inventory.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] unless `capacity` is in
    /// `1..=512`.
    pub fn with_capacity(capacity: usize) -> Result<Self, EngineError> {
        if capacity == 0 || capacity > MAX_SLOTS {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "inventory capacity must be between 1 and 512",
            )
            .with_detail("capacity", capacity.to_string()));
        }
        Ok(Self {
            capacity,
            slots: vec![None; capacity],
        })
    }

    /// Rebuild an inventory from saved slots, validating stack and ID rules.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty inventory, too many slots, zero
    /// quantities, over-limit stacks, or item-instance IDs of the wrong kind.
    pub fn from_slots(
        slots: Vec<Option<ItemStack>>,
        rules: &dyn ItemRules,
    ) -> Result<Self, EngineError> {
        let capacity = slots.len();
        if capacity == 0 || capacity > MAX_SLOTS {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "inventory capacity must be between 1 and 512",
            )
            .with_detail("capacity", capacity.to_string()));
        }
        let mut seen_instances = BTreeMap::new();
        for (index, slot) in slots.iter().enumerate() {
            let Some(stack) = slot else { continue };
            if stack.quantity == 0 {
                return Err(EngineError::new(
                    ErrorCode::InvalidRequest,
                    "inventory stack quantity must be at least one",
                )
                .with_detail("slot", index.to_string()));
            }
            let limit = rules.max_stack(&stack.definition);
            if stack.quantity > limit {
                return Err(EngineError::new(
                    ErrorCode::CapacityExceeded,
                    "inventory stack exceeds the item's stack limit",
                )
                .with_detail("slot", index.to_string())
                .with_detail("quantity", stack.quantity.to_string())
                .with_detail("max_stack", limit.to_string()));
            }
            if let Some(instance) = stack.instance {
                instance.expect_kind(IdKind::Item)?;
                if stack.quantity != 1 {
                    return Err(EngineError::new(
                        ErrorCode::InvalidState,
                        "a unique item stack must have quantity one",
                    )
                    .with_detail("slot", index.to_string()));
                }
                if seen_instances.insert(instance, index).is_some() {
                    return Err(EngineError::new(
                        ErrorCode::Conflict,
                        "the same item instance appears in multiple slots",
                    )
                    .with_detail("instance", instance.to_string()));
                }
            }
        }
        Ok(Self { capacity, slots })
    }

    /// Re-check a deserialized inventory against the same rules
    /// [`Inventory::from_slots`] enforces.
    ///
    /// Snapshots deserialize their inventory directly, bypassing the
    /// constructor, so restore paths call this before adopting the state.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty inventory, too many slots, zero
    /// quantities, over-limit stacks, or item-instance IDs of the wrong kind.
    pub fn validate(&self, rules: &dyn ItemRules) -> Result<(), EngineError> {
        Self::from_slots(self.slots.clone(), rules).map(|_| ())
    }

    /// Number of slots.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Read a slot.
    #[must_use]
    pub fn slot(&self, index: usize) -> Option<&ItemStack> {
        self.slots.get(index).and_then(Option::as_ref)
    }

    /// Number of occupied slots.
    #[must_use]
    pub fn occupied_slots(&self) -> usize {
        self.slots.iter().filter(|slot| slot.is_some()).count()
    }

    /// Every occupied slot with its index.
    pub fn entries(&self) -> impl Iterator<Item = (usize, &ItemStack)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| slot.as_ref().map(|stack| (index, stack)))
    }

    /// Total quantity of `definition` across all slots.
    #[must_use]
    pub fn total_quantity(&self, definition: &DefinitionId) -> u64 {
        self.entries()
            .filter(|(_, stack)| &stack.definition == definition)
            .map(|(_, stack)| u64::from(stack.quantity))
            .sum()
    }

    /// Whether the whole stack fits without mutating the inventory.
    #[must_use]
    pub fn can_accept(&self, stack: &ItemStack, rules: &dyn ItemRules) -> bool {
        self.plan_add(stack, rules).is_some()
    }

    /// Add a stack, merging into existing stacks first and then using empty
    /// slots. The operation is atomic: on failure nothing changes.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] for a zero or over-limit incoming
    /// stack, and [`ErrorCode::CapacityExceeded`] when there is not enough
    /// room.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "accepting a whole stack expresses the deposit operation"
    )]
    pub fn try_add(
        &mut self,
        stack: ItemStack,
        rules: &dyn ItemRules,
    ) -> Result<AddReport, EngineError> {
        if stack.quantity == 0 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "cannot add an empty stack",
            ));
        }
        let limit = rules.max_stack(&stack.definition);
        if limit == 0 {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "item rules report a zero stack limit",
            )
            .with_detail("item", stack.definition.to_string()));
        }
        if stack.quantity > limit {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "incoming stack exceeds the item's stack limit",
            )
            .with_detail("quantity", stack.quantity.to_string())
            .with_detail("max_stack", limit.to_string()));
        }

        let plan = self.plan_add(&stack, rules).ok_or_else(|| {
            EngineError::new(
                ErrorCode::CapacityExceeded,
                "inventory has no room for the stack",
            )
            .with_detail("item", stack.definition.to_string())
            .with_detail("quantity", stack.quantity.to_string())
        })?;

        let mut remaining = stack.quantity;
        let mut slots = Vec::new();
        for (index, amount) in plan {
            if amount == 0 {
                continue;
            }
            match &mut self.slots[index] {
                Some(existing) => {
                    existing.quantity += amount;
                }
                empty => {
                    *empty = Some(ItemStack {
                        definition: stack.definition.clone(),
                        quantity: amount,
                        instance: stack.instance,
                    });
                }
            }
            remaining -= amount;
            slots.push(index);
        }
        debug_assert_eq!(remaining, 0, "add plan must consume the whole stack");
        Ok(AddReport {
            slots,
            quantity: stack.quantity,
        })
    }

    /// Remove `quantity` from `slot`, returning the removed stack.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::NotFound`] for an empty or missing slot,
    /// [`ErrorCode::InvalidRequest`] for zero or excessive quantities, and
    /// [`ErrorCode::InvalidState`] when splitting a unique item.
    pub fn remove(&mut self, slot: usize, quantity: u32) -> Result<ItemStack, EngineError> {
        if quantity == 0 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "cannot remove a zero quantity",
            ));
        }
        let index = slot;
        let existing = self
            .slots
            .get_mut(index)
            .ok_or_else(|| EngineError::not_found("inventory slot", index))?
            .as_mut()
            .ok_or_else(|| EngineError::not_found("inventory slot", index))?;

        if existing.quantity < quantity {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "cannot remove more than the slot contains",
            )
            .with_detail("slot", index.to_string())
            .with_detail("available", existing.quantity.to_string())
            .with_detail("requested", quantity.to_string()));
        }
        if existing.is_unique() && quantity != existing.quantity {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "unique item stacks cannot be split",
            )
            .with_detail("slot", index.to_string()));
        }

        if quantity == existing.quantity {
            let removed = self.slots[index].take().expect("slot was checked above");
            Ok(removed)
        } else {
            existing.quantity -= quantity;
            Ok(ItemStack {
                definition: existing.definition.clone(),
                quantity,
                instance: existing.instance,
            })
        }
    }

    /// Split part of a stack into a specified empty slot atomically.
    ///
    /// The source must remain non-empty; moving the whole stack is a slot move,
    /// not a split. Unique items cannot be split.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] for identical slots, zero
    /// quantity, or a quantity that consumes the entire source; [`ErrorCode::NotFound`]
    /// for a missing slot; and [`ErrorCode::Conflict`] if the destination is
    /// occupied. Unique-item splits return [`ErrorCode::InvalidState`].
    pub fn split_into_slot(
        &mut self,
        source_slot: usize,
        destination_slot: usize,
        quantity: u32,
    ) -> Result<ItemStack, EngineError> {
        if source_slot == destination_slot || quantity == 0 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "a split requires distinct slots and a positive quantity",
            ));
        }
        let destination = self
            .slots
            .get(destination_slot)
            .ok_or_else(|| EngineError::not_found("inventory slot", destination_slot))?;
        if destination.is_some() {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "split destination slot is occupied",
            )
            .with_detail("slot", destination_slot.to_string()));
        }
        let source = self
            .slot(source_slot)
            .ok_or_else(|| EngineError::not_found("inventory slot", source_slot))?;
        if source.is_unique() {
            return Err(
                EngineError::new(ErrorCode::InvalidState, "unique items cannot be split")
                    .with_detail("slot", source_slot.to_string()),
            );
        }
        if quantity >= source.quantity {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "a split must leave at least one item in the source slot",
            )
            .with_detail("slot", source_slot.to_string())
            .with_detail("available", source.quantity.to_string())
            .with_detail("requested", quantity.to_string()));
        }

        let mut updated = self.clone();
        let split = updated.remove(source_slot, quantity)?;
        updated.slots[destination_slot] = Some(split.clone());
        *self = updated;
        Ok(split)
    }

    /// Merge `quantity` from one stack into another compatible stack atomically.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] for identical slots, zero, or an
    /// excessive quantity; [`ErrorCode::NotFound`] for a missing slot;
    /// [`ErrorCode::InvalidState`] for unique or different item definitions;
    /// and [`ErrorCode::CapacityExceeded`] when the destination would exceed
    /// its item stack limit.
    pub fn merge_into_slot(
        &mut self,
        source_slot: usize,
        destination_slot: usize,
        quantity: u32,
        rules: &dyn ItemRules,
    ) -> Result<ItemStack, EngineError> {
        if source_slot == destination_slot || quantity == 0 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "a merge requires distinct slots and a positive quantity",
            ));
        }
        let source = self
            .slot(source_slot)
            .cloned()
            .ok_or_else(|| EngineError::not_found("inventory slot", source_slot))?;
        let destination = self
            .slot(destination_slot)
            .cloned()
            .ok_or_else(|| EngineError::not_found("inventory slot", destination_slot))?;
        if source.is_unique() || destination.is_unique() {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "unique items cannot be merged",
            ));
        }
        if source.definition != destination.definition {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "only stacks of the same item can be merged",
            ));
        }
        if quantity > source.quantity {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "cannot merge more than the source stack contains",
            )
            .with_detail("available", source.quantity.to_string())
            .with_detail("requested", quantity.to_string()));
        }
        let limit = rules.max_stack(&destination.definition);
        if limit == 0 {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "item rules report a zero stack limit",
            ));
        }
        let merged_quantity = destination.quantity.checked_add(quantity).ok_or_else(|| {
            EngineError::new(
                ErrorCode::CapacityExceeded,
                "merged stack exceeds the item's stack limit",
            )
        })?;
        if merged_quantity > limit {
            return Err(EngineError::new(
                ErrorCode::CapacityExceeded,
                "merged stack exceeds the item's stack limit",
            )
            .with_detail("max_stack", limit.to_string()));
        }

        let mut updated = self.clone();
        if quantity == source.quantity {
            updated.slots[source_slot] = None;
        } else {
            updated.slots[source_slot]
                .as_mut()
                .expect("source slot was checked above")
                .quantity -= quantity;
        }
        updated.slots[destination_slot]
            .as_mut()
            .expect("destination slot was checked above")
            .quantity = merged_quantity;
        *self = updated;
        Ok(ItemStack::new(source.definition, quantity))
    }

    fn plan_add(&self, stack: &ItemStack, rules: &dyn ItemRules) -> Option<Vec<(usize, u32)>> {
        if stack.quantity == 0 {
            return None;
        }
        let limit = rules.max_stack(&stack.definition);
        if limit == 0 || stack.quantity > limit {
            return None;
        }

        let mut remaining = stack.quantity;
        let mut plan = Vec::new();

        if stack.instance.is_none() {
            for (index, slot) in self.slots.iter().enumerate() {
                if remaining == 0 {
                    break;
                }
                if let Some(existing) = slot
                    && existing.definition == stack.definition
                    && existing.instance.is_none()
                {
                    let space = limit.saturating_sub(existing.quantity);
                    if space > 0 {
                        let amount = space.min(remaining);
                        plan.push((index, amount));
                        remaining -= amount;
                    }
                }
            }
        }

        for (index, slot) in self.slots.iter().enumerate() {
            if remaining == 0 {
                break;
            }
            if slot.is_none() {
                let amount = remaining.min(limit);
                plan.push((index, amount));
                remaining -= amount;
            }
        }

        (remaining == 0).then_some(plan)
    }
}

/// The recorded outcome of a transfer, reused for idempotent retries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferReceipt {
    /// Idempotency key supplied by the caller.
    pub key: String,
    /// Source inventory.
    pub source: StableId,
    /// Source slot.
    pub source_slot: usize,
    /// Destination inventory.
    pub destination: StableId,
    /// Item definition moved.
    pub definition: DefinitionId,
    /// Quantity moved.
    pub quantity: u32,
    /// Destination slots that received items.
    pub destination_slots: Vec<usize>,
}

/// Remembers applied transfer keys so retries cannot duplicate items.
#[derive(Debug, Clone, Default)]
pub struct TransferLedger {
    applied: BTreeMap<String, TransferReceipt>,
}

impl TransferLedger {
    /// An empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Transfer `quantity` from one inventory to another exactly once per key.
    ///
    /// A replay with the same key returns the original receipt without
    /// touching either inventory. A failed transfer records nothing, so the
    /// caller may retry with the same key.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] for an empty key or invalid
    /// quantity, [`ErrorCode::NotFound`] for a missing source slot, and
    /// [`ErrorCode::CapacityExceeded`] when the destination cannot fit the
    /// stack.
    #[expect(
        clippy::too_many_arguments,
        reason = "the explicit source/destination endpoints keep the ledger API auditable"
    )]
    pub fn transfer(
        &mut self,
        key: &str,
        source: &mut Inventory,
        source_id: StableId,
        source_slot: usize,
        destination: &mut Inventory,
        destination_id: StableId,
        quantity: u32,
        rules: &dyn ItemRules,
    ) -> Result<TransferReceipt, EngineError> {
        if key.is_empty() {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "a transfer requires an idempotency key",
            ));
        }
        if let Some(receipt) = self.applied.get(key) {
            return Ok(receipt.clone());
        }
        if quantity == 0 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "cannot transfer a zero quantity",
            ));
        }
        source_id.expect_kind(IdKind::Entity)?;
        destination_id.expect_kind(IdKind::Entity)?;

        let mut source_after = source.clone();
        let mut destination_after = destination.clone();
        let removed = source_after.remove(source_slot, quantity)?;
        let definition = removed.definition.clone();
        let report = destination_after.try_add(removed, rules)?;

        *source = source_after;
        *destination = destination_after;

        let receipt = TransferReceipt {
            key: key.to_owned(),
            source: source_id,
            source_slot,
            destination: destination_id,
            definition,
            quantity,
            destination_slots: report.slots,
        };
        self.applied.insert(key.to_owned(), receipt.clone());
        Ok(receipt)
    }

    /// Look up a previously applied receipt.
    #[must_use]
    pub fn receipt(&self, key: &str) -> Option<&TransferReceipt> {
        self.applied.get(key)
    }

    /// Number of applied transfer keys.
    #[must_use]
    pub fn applied_count(&self) -> usize {
        self.applied.len()
    }

    /// All applied receipts. The order is unspecified.
    pub fn applied(&self) -> impl Iterator<Item = &TransferReceipt> {
        self.applied.values()
    }

    /// Adopt receipts recorded before a restart.
    ///
    /// Used when restoring a snapshot so retries that were already applied
    /// before the restart still return the original receipt. A later receipt
    /// with the same key overwrites the earlier one, mirroring a fresh
    /// application.
    pub fn adopt(&mut self, receipts: impl IntoIterator<Item = TransferReceipt>) {
        for receipt in receipts {
            self.applied.insert(receipt.key.clone(), receipt);
        }
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn item(name: &str) -> DefinitionId {
        DefinitionId::new(name).unwrap()
    }

    fn inventory(capacity: usize) -> Inventory {
        Inventory::with_capacity(capacity).unwrap()
    }

    fn definition_strategy() -> impl Strategy<Value = DefinitionId> {
        prop_oneof![Just(item("item.torch")), Just(item("item.rock"))]
    }

    fn inventory_strategy() -> impl Strategy<Value = Inventory> {
        prop::collection::vec(
            prop_oneof![
                Just(None),
                (definition_strategy(), 1u32..=10).prop_map(|(definition, quantity)| {
                    Some(ItemStack::new(definition, quantity))
                }),
                (definition_strategy(), 0u128..64).prop_map(|(definition, seed)| {
                    Some(ItemStack::unique(
                        definition,
                        StableId::from_parts(IdKind::Item, 1, seed),
                    ))
                }),
            ],
            1..=4,
        )
        .prop_map(|slots| {
            let slots = slots
                .into_iter()
                .enumerate()
                .map(|(index, slot)| match slot {
                    Some(stack) if stack.is_unique() => Some(ItemStack::unique(
                        stack.definition,
                        StableId::from_parts(
                            IdKind::Item,
                            1,
                            u128::try_from(index).expect("a slot index fits in u128"),
                        ),
                    )),
                    other => other,
                })
                .collect();
            Inventory::from_slots(slots, &UniformItemRules::new(10)).unwrap()
        })
    }

    #[test]
    fn adding_merges_then_uses_empty_slots() {
        let rules = UniformItemRules::new(10);
        let mut bag = inventory(3);
        let torch = item("item.torch");

        let report = bag
            .try_add(ItemStack::new(torch.clone(), 8), &rules)
            .unwrap();
        assert_eq!(report.slots, vec![0]);
        let report = bag
            .try_add(ItemStack::new(torch.clone(), 7), &rules)
            .unwrap();
        assert_eq!(report.slots, vec![0, 1]);
        assert_eq!(bag.slot(0).unwrap().quantity, 10);
        assert_eq!(bag.slot(1).unwrap().quantity, 5);
        assert_eq!(bag.total_quantity(&torch), 15);
    }

    #[test]
    fn adding_is_atomic_when_capacity_runs_out() {
        let rules = UniformItemRules::new(10);
        let mut bag = inventory(1);
        bag.try_add(ItemStack::new(item("item.torch"), 6), &rules)
            .unwrap();
        let error = bag
            .try_add(ItemStack::new(item("item.torch"), 5), &rules)
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::CapacityExceeded);
        assert_eq!(bag.slot(0).unwrap().quantity, 6);
        assert_eq!(bag.occupied_slots(), 1);
    }

    #[test]
    fn stack_limits_are_enforced() {
        let rules = UniformItemRules::new(5);
        let mut bag = inventory(2);
        assert_eq!(
            bag.try_add(ItemStack::new(item("item.torch"), 6), &rules)
                .unwrap_err()
                .code(),
            ErrorCode::InvalidRequest
        );
        assert_eq!(
            bag.try_add(ItemStack::new(item("item.torch"), 0), &rules)
                .unwrap_err()
                .code(),
            ErrorCode::InvalidRequest
        );
    }

    #[test]
    fn unique_items_do_not_merge() {
        let rules = UniformItemRules::new(10);
        let mut bag = inventory(2);
        let sword = item("item.sword");
        let first = StableId::from_parts(IdKind::Item, 1, 1);
        let second = StableId::from_parts(IdKind::Item, 1, 2);

        bag.try_add(ItemStack::unique(sword.clone(), first), &rules)
            .unwrap();
        bag.try_add(ItemStack::unique(sword, second), &rules)
            .unwrap();
        assert_eq!(bag.occupied_slots(), 2);
        assert_eq!(
            bag.remove(0, 1).unwrap().instance.unwrap().to_string(),
            first.to_string()
        );
        assert_eq!(
            bag.remove(1, 1).unwrap().instance.unwrap().to_string(),
            second.to_string()
        );
    }

    #[test]
    fn removing_partial_and_whole_stacks_works() {
        let rules = UniformItemRules::new(10);
        let mut bag = inventory(2);
        let torch = item("item.torch");
        bag.try_add(ItemStack::new(torch.clone(), 9), &rules)
            .unwrap();

        let removed = bag.remove(0, 4).unwrap();
        assert_eq!(removed.quantity, 4);
        assert_eq!(bag.slot(0).unwrap().quantity, 5);

        assert_eq!(
            bag.remove(0, 6).unwrap_err().code(),
            ErrorCode::InvalidRequest
        );
        assert_eq!(
            bag.remove(0, 0).unwrap_err().code(),
            ErrorCode::InvalidRequest
        );
        assert_eq!(bag.remove(1, 1).unwrap_err().code(), ErrorCode::NotFound);
        assert_eq!(bag.remove(9, 1).unwrap_err().code(), ErrorCode::NotFound);

        assert_eq!(bag.remove(0, 5).unwrap().quantity, 5);
        assert!(bag.slot(0).is_none());
    }

    #[test]
    fn split_and_merge_preserve_quantities_and_fail_atomically() {
        let rules = UniformItemRules::new(10);
        let torch = item("item.torch");
        let mut bag = Inventory::from_slots(
            vec![
                Some(ItemStack::new(torch.clone(), 8)),
                Some(ItemStack::new(torch.clone(), 4)),
                None,
            ],
            &rules,
        )
        .unwrap();

        bag.split_into_slot(0, 2, 3).unwrap();
        assert_eq!(bag.slot(0).unwrap().quantity, 5);
        assert_eq!(bag.slot(2).unwrap().quantity, 3);
        bag.merge_into_slot(2, 1, 2, &rules).unwrap();
        assert_eq!(bag.slot(1).unwrap().quantity, 6);
        assert_eq!(bag.slot(2).unwrap().quantity, 1);
        assert_eq!(bag.total_quantity(&torch), 12);

        let before = bag.clone();
        assert_eq!(
            bag.split_into_slot(0, 1, 1).unwrap_err().code(),
            ErrorCode::Conflict
        );
        assert_eq!(
            bag.merge_into_slot(0, 1, 5, &rules).unwrap_err().code(),
            ErrorCode::CapacityExceeded
        );
        assert_eq!(
            bag.merge_into_slot(0, 2, 0, &rules).unwrap_err().code(),
            ErrorCode::InvalidRequest
        );
        assert_eq!(bag, before);
    }

    #[test]
    fn transfers_conserve_items_and_record_a_receipt() {
        let rules = UniformItemRules::new(10);
        let mut source = inventory(2);
        let mut destination = inventory(2);
        let torch = item("item.torch");
        source
            .try_add(ItemStack::new(torch.clone(), 10), &rules)
            .unwrap();
        destination
            .try_add(ItemStack::new(torch.clone(), 3), &rules)
            .unwrap();

        let mut ledger = TransferLedger::new();
        let source_id = StableId::from_parts(IdKind::Entity, 1, 1);
        let destination_id = StableId::from_parts(IdKind::Entity, 1, 2);
        let receipt = ledger
            .transfer(
                "trade-1",
                &mut source,
                source_id,
                0,
                &mut destination,
                destination_id,
                6,
                &rules,
            )
            .unwrap();

        assert_eq!(receipt.quantity, 6);
        assert_eq!(receipt.definition, torch);
        assert_eq!(source.total_quantity(&torch), 4);
        assert_eq!(destination.total_quantity(&torch), 9);
        assert_eq!(destination.slot(0).unwrap().quantity, 9);
        assert_eq!(ledger.applied_count(), 1);
    }

    #[test]
    fn replayed_transfers_do_not_duplicate_items() {
        let rules = UniformItemRules::new(10);
        let mut source = inventory(2);
        let mut destination = inventory(2);
        let torch = item("item.torch");
        source
            .try_add(ItemStack::new(torch.clone(), 5), &rules)
            .unwrap();
        let mut ledger = TransferLedger::new();
        let source_id = StableId::from_parts(IdKind::Entity, 2, 1);
        let destination_id = StableId::from_parts(IdKind::Entity, 2, 2);

        let first = ledger
            .transfer(
                "trade-42",
                &mut source,
                source_id,
                0,
                &mut destination,
                destination_id,
                2,
                &rules,
            )
            .unwrap();
        let replay = ledger
            .transfer(
                "trade-42",
                &mut source,
                source_id,
                0,
                &mut destination,
                destination_id,
                2,
                &rules,
            )
            .unwrap();

        assert_eq!(first, replay);
        assert_eq!(source.total_quantity(&torch), 3);
        assert_eq!(destination.total_quantity(&torch), 2);
    }

    #[test]
    fn failed_transfers_leave_both_inventories_unchanged_and_do_not_record() {
        let rules = UniformItemRules::new(10);
        let mut source = inventory(1);
        let mut destination = inventory(1);
        let torch = item("item.torch");
        let rock = item("item.rock");
        source
            .try_add(ItemStack::new(torch.clone(), 5), &rules)
            .unwrap();
        destination
            .try_add(ItemStack::new(rock, 10), &rules)
            .unwrap();

        let mut ledger = TransferLedger::new();
        let error = ledger
            .transfer(
                "trade-full",
                &mut source,
                StableId::from_parts(IdKind::Entity, 3, 1),
                0,
                &mut destination,
                StableId::from_parts(IdKind::Entity, 3, 2),
                5,
                &rules,
            )
            .unwrap_err();

        assert_eq!(error.code(), ErrorCode::CapacityExceeded);
        assert_eq!(source.total_quantity(&torch), 5);
        assert_eq!(ledger.applied_count(), 0);
        assert!(ledger.receipt("trade-full").is_none());
    }

    #[test]
    fn transfer_requires_a_key() {
        let rules = UniformItemRules::default();
        let mut source = inventory(1);
        let mut destination = inventory(1);
        source
            .try_add(ItemStack::new(item("item.torch"), 1), &rules)
            .unwrap();
        let mut ledger = TransferLedger::new();
        let error = ledger
            .transfer(
                "",
                &mut source,
                StableId::from_parts(IdKind::Entity, 4, 1),
                0,
                &mut destination,
                StableId::from_parts(IdKind::Entity, 4, 2),
                1,
                &rules,
            )
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidRequest);
    }

    #[test]
    fn from_slots_validates_persisted_data() {
        let rules = UniformItemRules::new(5);
        let torch = item("item.torch");
        let duplicate = StableId::from_parts(IdKind::Item, 5, 5);

        let too_large = Inventory::from_slots(vec![Some(ItemStack::new(torch.clone(), 6))], &rules);
        assert_eq!(too_large.unwrap_err().code(), ErrorCode::CapacityExceeded);

        let duplicated = Inventory::from_slots(
            vec![
                Some(ItemStack::unique(torch.clone(), duplicate)),
                Some(ItemStack::unique(torch.clone(), duplicate)),
            ],
            &rules,
        );
        assert_eq!(duplicated.unwrap_err().code(), ErrorCode::Conflict);

        let valid =
            Inventory::from_slots(vec![None, Some(ItemStack::new(torch, 5))], &rules).unwrap();
        assert_eq!(valid.occupied_slots(), 1);
    }

    proptest! {
        #[test]
        fn random_transfers_conserve_totals_and_are_atomic_and_idempotent(
            source in inventory_strategy(),
            destination in inventory_strategy(),
            source_slot in 0usize..8,
            quantity in 1u32..=10,
        ) {
            let rules = UniformItemRules::new(10);
            let mut source = source;
            let mut destination = destination;
            let source_before = source.clone();
            let destination_before = destination.clone();
            let source_id = StableId::from_parts(IdKind::Entity, 1, 1);
            let destination_id = StableId::from_parts(IdKind::Entity, 1, 2);
            let definitions = [item("item.torch"), item("item.rock")];
            let totals_before: Vec<u64> = definitions
                .iter()
                .map(|definition| {
                    source.total_quantity(definition) + destination.total_quantity(definition)
                })
                .collect();
            let mut ledger = TransferLedger::new();

            match ledger.transfer(
                "property-transfer",
                &mut source,
                source_id,
                source_slot,
                &mut destination,
                destination_id,
                quantity,
                &rules,
            ) {
                Ok(receipt) => {
                    for (definition, total_before) in definitions.iter().zip(totals_before) {
                        prop_assert_eq!(
                            source.total_quantity(definition)
                                + destination.total_quantity(definition),
                            total_before,
                            "a successful transfer conserves each definition"
                        );
                    }
                    prop_assert_eq!(receipt.quantity, quantity);

                    let source_after = source.clone();
                    let destination_after = destination.clone();
                    let replay = ledger
                        .transfer(
                            "property-transfer",
                            &mut source,
                            source_id,
                            source_slot,
                            &mut destination,
                            destination_id,
                            quantity,
                            &rules,
                        )
                        .unwrap();
                    prop_assert_eq!(replay, receipt);
                    prop_assert_eq!(source, source_after);
                    prop_assert_eq!(destination, destination_after);
                }
                Err(error) => {
                    prop_assert!(matches!(
                        error.code(),
                        ErrorCode::NotFound
                            | ErrorCode::InvalidRequest
                            | ErrorCode::CapacityExceeded
                    ));
                    prop_assert_eq!(source, source_before);
                    prop_assert_eq!(destination, destination_before);
                    prop_assert_eq!(ledger.applied_count(), 0);
                }
            }
        }

        #[test]
        fn random_inventories_round_trip_through_json_and_revalidate(
            inventory in inventory_strategy()
        ) {
            let rules = UniformItemRules::new(10);
            let encoded = serde_json::to_string(&inventory).unwrap();
            let decoded: Inventory = serde_json::from_str(&encoded).unwrap();
            prop_assert_eq!(&decoded, &inventory);
            decoded.validate(&rules).unwrap();
        }
    }
}
