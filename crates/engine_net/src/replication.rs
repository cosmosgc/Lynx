//! Replication categories, updates, and latest-wins batching.
//!
//! The server never broadcasts a whole zone. Each update declares what kind of
//! state it carries, which determines delivery guarantees and who may receive
//! it. Movement and ambient state are unreliable and latest-wins; inventories,
//! trades, chat, and ownership changes are reliable and ordered.

use std::collections::{BTreeMap, BTreeSet};

use engine_core::error::{EngineError, ErrorCode};
use engine_core::id::{DefinitionId, StableId};
use engine_core::time::Tick;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::envelope::check_json_limits;

/// Maximum updates carried by one batch.
pub const MAX_UPDATES_PER_BATCH: usize = 4096;

/// Transport guarantee for a replication category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// Ordered and acknowledged. Loss or reordering is a session fault.
    Reliable,
    /// May be dropped or reordered; newer updates supersede older ones.
    Unreliable,
}

impl Delivery {
    /// Whether delivery is acknowledged and ordered.
    #[must_use]
    pub const fn is_reliable(self) -> bool {
        matches!(self, Self::Reliable)
    }
}

/// Who may receive an update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Recipient {
    /// The session that caused the update.
    Session,
    /// Clients with the entity in their interest set.
    Zone,
    /// The owning account or character only.
    Owner,
    /// A recipient allowed by the object's access policy.
    Authorized,
    /// Staff roles only.
    Staff,
}

/// Classification of replicated data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplicationCategory {
    /// Inventory, trade, chat, ownership, or quest events.
    ReliableEvent,
    /// Position, rotation, and velocity snapshots.
    StateSnapshot,
    /// Nearby entities and zone objects filtered by interest.
    InterestManaged,
    /// Private inventory and control leases.
    OwnerOnly,
    /// Placed objects and housing details with an access policy.
    PropertyScoped,
    /// Diagnostics and AI state for staff.
    Admin,
}

impl ReplicationCategory {
    /// Delivery guarantee for this category.
    #[must_use]
    pub const fn delivery(self) -> Delivery {
        match self {
            Self::StateSnapshot | Self::InterestManaged => Delivery::Unreliable,
            Self::ReliableEvent | Self::OwnerOnly | Self::PropertyScoped | Self::Admin => {
                Delivery::Reliable
            }
        }
    }

    /// Whether updates must arrive in sequence order.
    #[must_use]
    pub const fn ordered(self) -> bool {
        self.delivery().is_reliable()
    }

    /// Whether an update must name the entity it concerns.
    #[must_use]
    pub const fn requires_entity(self) -> bool {
        matches!(
            self,
            Self::StateSnapshot | Self::InterestManaged | Self::OwnerOnly | Self::PropertyScoped
        )
    }

    /// Default audience for this category.
    #[must_use]
    pub const fn recipient(self) -> Recipient {
        match self {
            Self::ReliableEvent => Recipient::Session,
            Self::StateSnapshot | Self::InterestManaged => Recipient::Zone,
            Self::OwnerOnly => Recipient::Owner,
            Self::PropertyScoped => Recipient::Authorized,
            Self::Admin => Recipient::Staff,
        }
    }
}

/// One replicated change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplicationUpdate {
    /// What kind of data this update carries.
    pub category: ReplicationCategory,
    /// Entity the update concerns, when the category requires one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<StableId>,
    /// Definition that produced the update, for client-side content lookup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition: Option<DefinitionId>,
    /// Authoritative tick at which the change happened.
    pub tick: Tick,
    /// Per-entity sequence for latest-wins ordering.
    #[serde(default)]
    pub sequence: u64,
    /// Component delta or event data.
    #[serde(default = "empty_payload")]
    pub payload: Value,
}

impl ReplicationUpdate {
    /// Create an update with an empty payload.
    #[must_use]
    pub fn new(category: ReplicationCategory, tick: Tick) -> Self {
        Self {
            category,
            entity: None,
            definition: None,
            tick,
            sequence: 0,
            payload: empty_payload(),
        }
    }

    /// Attach the affected entity.
    #[must_use]
    pub fn with_entity(mut self, entity: StableId) -> Self {
        self.entity = Some(entity);
        self
    }

    /// Attach the source definition.
    #[must_use]
    pub fn with_definition(mut self, definition: DefinitionId) -> Self {
        self.definition = Some(definition);
        self
    }

    /// Attach a per-entity sequence.
    #[must_use]
    pub fn with_sequence(mut self, sequence: u64) -> Self {
        self.sequence = sequence;
        self
    }

    /// Attach event or component data.
    #[must_use]
    pub fn with_payload(mut self, payload: Value) -> Self {
        self.payload = payload;
        self
    }

    /// Validate the update against the batch tick it belongs to.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] when a required entity is missing
    /// or the update claims a tick after its batch, and
    /// [`ErrorCode::CapacityExceeded`] when the payload exceeds JSON limits.
    pub fn validate(&self, batch_tick: Tick) -> Result<(), EngineError> {
        if self.category.requires_entity() && self.entity.is_none() {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "replication update requires an entity",
            )
            .with_detail("category", category_name(self.category)));
        }
        if self.tick > batch_tick {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "replication update is from the future",
            )
            .with_detail("tick", self.tick.0.to_string())
            .with_detail("batch_tick", batch_tick.0.to_string()));
        }
        check_json_limits(&self.payload, "payload")
    }
}

fn empty_payload() -> Value {
    Value::Object(Map::new())
}

fn category_name(category: ReplicationCategory) -> &'static str {
    match category {
        ReplicationCategory::ReliableEvent => "reliable_event",
        ReplicationCategory::StateSnapshot => "state_snapshot",
        ReplicationCategory::InterestManaged => "interest_managed",
        ReplicationCategory::OwnerOnly => "owner_only",
        ReplicationCategory::PropertyScoped => "property_scoped",
        ReplicationCategory::Admin => "admin",
    }
}

/// Updates emitted for one authoritative tick.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReplicationBatch {
    /// Authoritative tick shared by the batch.
    pub tick: Tick,
    /// Updates in emission order.
    pub updates: Vec<ReplicationUpdate>,
}

impl ReplicationBatch {
    /// Create an empty batch for a tick.
    #[must_use]
    pub fn new(tick: Tick) -> Self {
        Self {
            tick,
            updates: Vec::new(),
        }
    }

    /// Validate and append an update.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`ReplicationUpdate::validate`] and
    /// [`ErrorCode::CapacityExceeded`] when the batch is full.
    pub fn push(&mut self, update: ReplicationUpdate) -> Result<(), EngineError> {
        update.validate(self.tick)?;
        if self.updates.len() >= MAX_UPDATES_PER_BATCH {
            return Err(
                EngineError::new(ErrorCode::CapacityExceeded, "replication batch is full")
                    .with_detail("max_updates", MAX_UPDATES_PER_BATCH.to_string()),
            );
        }
        self.updates.push(update);
        Ok(())
    }

    /// Validate the whole batch, including updates inserted through the public
    /// field.
    ///
    /// # Errors
    ///
    /// Returns the first invalid update's error or
    /// [`ErrorCode::CapacityExceeded`] when the batch holds too many updates.
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.updates.len() > MAX_UPDATES_PER_BATCH {
            return Err(EngineError::new(
                ErrorCode::CapacityExceeded,
                "replication batch is too large",
            )
            .with_detail("updates", self.updates.len().to_string())
            .with_detail("max_updates", MAX_UPDATES_PER_BATCH.to_string()));
        }
        for update in &self.updates {
            update.validate(self.tick)?;
        }
        Ok(())
    }

    /// Number of updates.
    #[must_use]
    pub fn len(&self) -> usize {
        self.updates.len()
    }

    /// Whether the batch has no updates.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.updates.is_empty()
    }

    /// Reliable updates, in emission order.
    pub fn reliable(&self) -> impl Iterator<Item = &ReplicationUpdate> {
        self.updates
            .iter()
            .filter(|update| update.category.delivery().is_reliable())
    }

    /// Unreliable updates, in emission order.
    pub fn unreliable(&self) -> impl Iterator<Item = &ReplicationUpdate> {
        self.updates
            .iter()
            .filter(|update| !update.category.delivery().is_reliable())
    }

    /// Drop state snapshots that are superseded by a newer sequence or tick for
    /// the same entity. Other categories are never collapsed.
    pub fn retain_latest_state(&mut self) {
        let mut latest: BTreeMap<StableId, (u64, Tick, usize)> = BTreeMap::new();
        for (index, update) in self.updates.iter().enumerate() {
            if update.category != ReplicationCategory::StateSnapshot {
                continue;
            }
            let Some(entity) = update.entity else {
                continue;
            };
            let candidate = (update.sequence, update.tick, index);
            latest
                .entry(entity)
                .and_modify(|current| {
                    if candidate > *current {
                        *current = candidate;
                    }
                })
                .or_insert(candidate);
        }

        let keep: BTreeSet<usize> = latest.values().map(|(_, _, index)| *index).collect();
        let mut index = 0;
        self.updates.retain(|update| {
            let current = index;
            index += 1;
            update.category != ReplicationCategory::StateSnapshot || keep.contains(&current)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::id::{IdGenerator, IdKind};
    use serde_json::json;

    fn entity(seed: u128) -> StableId {
        engine_core::id::DeterministicIdGenerator::new(seed).generate(IdKind::Entity)
    }

    #[test]
    fn categories_declare_delivery_and_audience() {
        let cases = [
            (
                ReplicationCategory::ReliableEvent,
                Delivery::Reliable,
                Recipient::Session,
            ),
            (
                ReplicationCategory::StateSnapshot,
                Delivery::Unreliable,
                Recipient::Zone,
            ),
            (
                ReplicationCategory::InterestManaged,
                Delivery::Unreliable,
                Recipient::Zone,
            ),
            (
                ReplicationCategory::OwnerOnly,
                Delivery::Reliable,
                Recipient::Owner,
            ),
            (
                ReplicationCategory::PropertyScoped,
                Delivery::Reliable,
                Recipient::Authorized,
            ),
            (
                ReplicationCategory::Admin,
                Delivery::Reliable,
                Recipient::Staff,
            ),
        ];
        for (category, delivery, recipient) in cases {
            assert_eq!(category.delivery(), delivery);
            assert_eq!(category.ordered(), delivery.is_reliable());
            assert_eq!(category.recipient(), recipient);
        }
    }

    #[test]
    fn authoritative_categories_require_an_entity() {
        let batch_tick = Tick(10);
        let update = ReplicationUpdate::new(ReplicationCategory::StateSnapshot, batch_tick);
        assert_eq!(
            update.validate(batch_tick).unwrap_err().code(),
            ErrorCode::InvalidRequest
        );
        assert!(update.with_entity(entity(1)).validate(batch_tick).is_ok());
        assert!(
            ReplicationUpdate::new(ReplicationCategory::ReliableEvent, batch_tick)
                .validate(batch_tick)
                .is_ok()
        );
    }

    #[test]
    fn future_updates_are_rejected() {
        let mut batch = ReplicationBatch::new(Tick(5));
        let update = ReplicationUpdate::new(ReplicationCategory::ReliableEvent, Tick(6));
        assert_eq!(
            batch.push(update).unwrap_err().code(),
            ErrorCode::InvalidRequest
        );
        assert!(batch.is_empty());
    }

    #[test]
    fn batches_enforce_their_update_limit() {
        let tick = Tick(1);
        let update = ReplicationUpdate::new(ReplicationCategory::ReliableEvent, tick);
        let batch = ReplicationBatch {
            tick,
            updates: vec![update; MAX_UPDATES_PER_BATCH + 1],
        };
        assert_eq!(
            batch.validate().unwrap_err().code(),
            ErrorCode::CapacityExceeded
        );
    }

    #[test]
    fn latest_state_snapshots_win_per_entity() {
        let tick = Tick(7);
        let first = entity(1);
        let second = entity(2);
        let mut batch = ReplicationBatch::new(tick);
        batch
            .push(
                ReplicationUpdate::new(ReplicationCategory::StateSnapshot, tick)
                    .with_entity(first)
                    .with_sequence(1)
                    .with_payload(json!({ "x": 1 })),
            )
            .unwrap();
        batch
            .push(
                ReplicationUpdate::new(ReplicationCategory::StateSnapshot, tick)
                    .with_entity(second)
                    .with_sequence(1),
            )
            .unwrap();
        batch
            .push(
                ReplicationUpdate::new(ReplicationCategory::StateSnapshot, tick)
                    .with_entity(first)
                    .with_sequence(3)
                    .with_payload(json!({ "x": 3 })),
            )
            .unwrap();
        batch
            .push(ReplicationUpdate::new(
                ReplicationCategory::ReliableEvent,
                tick,
            ))
            .unwrap();

        assert_eq!(batch.reliable().count(), 1);
        assert_eq!(batch.unreliable().count(), 3);

        batch.retain_latest_state();
        assert_eq!(batch.len(), 3);
        let snapshots: Vec<_> = batch
            .updates
            .iter()
            .filter(|update| update.category == ReplicationCategory::StateSnapshot)
            .collect();
        assert_eq!(snapshots.len(), 2);
        let kept = snapshots
            .iter()
            .find(|update| update.entity.as_ref() == Some(&first))
            .unwrap();
        assert_eq!(kept.sequence, 3);
        assert_eq!(kept.payload, json!({ "x": 3 }));
    }

    #[test]
    fn batches_round_trip_through_json() {
        let tick = Tick(12);
        let mut batch = ReplicationBatch::new(tick);
        batch
            .push(
                ReplicationUpdate::new(ReplicationCategory::StateSnapshot, tick)
                    .with_entity(entity(9))
                    .with_sequence(4)
                    .with_definition(DefinitionId::new("entity.door").unwrap())
                    .with_payload(json!({ "open": true })),
            )
            .unwrap();
        let text = serde_json::to_string(&batch).unwrap();
        let parsed: ReplicationBatch = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed, batch);
        assert!(parsed.validate().is_ok());
    }
}
