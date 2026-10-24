//! Safe client-facing snapshots and per-session delivery envelopes.
//!
//! Zone snapshots intentionally expose only fields every observer in range may
//! know: identity, prototype, transform, health, interaction surface, door
//! state, and owner. Inventories, minds, tags, and extension components stay
//! out of zone replication and travel only in owner-scoped updates.

use std::collections::BTreeSet;

use engine_core::id::{DefinitionId, StableId};
use engine_core::inventory::Inventory;
use engine_core::math::Vec3;
use engine_net::interest::{InterestEntity, InterestUpdate, Visibility};
use engine_net::replication::{ReplicationBatch, SessionId};
use engine_sim::action::Verb;
use engine_sim::entity::EntityRecord;
use engine_sim::mind::Controller;
use serde::{Deserialize, Serialize};

/// Tag that marks an entity as visible only to its owner and staff.
pub const VISIBILITY_HIDDEN_TAG: &str = "visibility.hidden";

/// Tag that marks an entity as visible only to its owner.
pub const VISIBILITY_OWNER_ONLY_TAG: &str = "visibility.owner_only";

/// Tag that marks an entity as visible only to staff sessions.
pub const VISIBILITY_STAFF_ONLY_TAG: &str = "visibility.staff_only";

/// Hit points safe to replicate to nearby observers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthSnapshot {
    /// Current hit points.
    pub current: i32,
    /// Maximum hit points.
    pub max: i32,
}

/// Door state safe to replicate to nearby observers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoorSnapshot {
    /// Whether the door is open.
    pub open: bool,
    /// Whether the door is locked.
    pub locked: bool,
}

/// The advertised interaction surface of an entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionSnapshot {
    /// Verbs the entity currently advertises, in stable order.
    pub verbs: Vec<Verb>,
    /// Maximum interaction distance in meters.
    pub range_m: f32,
    /// Whether interaction is currently enabled.
    pub enabled: bool,
}

/// The public state of one entity at an authoritative tick.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntitySnapshot {
    /// Authoritative entity ID.
    pub entity: StableId,
    /// Authored prototype.
    pub prototype: DefinitionId,
    /// World position in meters.
    pub position: Vec3,
    /// Facing angle in degrees.
    pub yaw_degrees: f32,
    /// Hit points, when damageable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<HealthSnapshot>,
    /// Interaction surface, when usable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interaction: Option<InteractionSnapshot>,
    /// Door state, when the entity is a door.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub door: Option<DoorSnapshot>,
    /// Owning character or property, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<StableId>,
}

impl EntitySnapshot {
    /// Capture the publicly replicable state of an entity.
    #[must_use]
    pub fn capture(entity: &EntityRecord) -> Self {
        Self {
            entity: entity.id,
            prototype: entity.prototype.clone(),
            position: entity.transform.position,
            yaw_degrees: entity.transform.yaw_degrees,
            health: entity.health.map(|health| HealthSnapshot {
                current: health.current,
                max: health.max,
            }),
            interaction: entity
                .interactable
                .as_ref()
                .map(|interactable| InteractionSnapshot {
                    verbs: interactable.verbs.iter().cloned().collect(),
                    range_m: interactable.range_m,
                    enabled: interactable.enabled,
                }),
            door: entity.door.as_ref().map(|door| DoorSnapshot {
                open: door.open,
                locked: door.locked,
            }),
            owner: entity.owner,
        }
    }
}

/// Owner-scoped state that never travels to other observers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OwnerState {
    /// The owner's full inventory.
    pub inventory: Inventory,
}

/// Everything the transport should send to one session for one tick.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionDelivery {
    /// Receiving session.
    pub session: SessionId,
    /// Authoritative tick shared by the delivery.
    pub tick: engine_core::time::Tick,
    /// Entities that entered or left the session's view.
    pub interest: InterestUpdate,
    /// Replication updates after audience filtering.
    pub batch: ReplicationBatch,
}

impl SessionDelivery {
    /// Whether there is anything to send.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.interest.is_empty() && self.batch.is_empty()
    }
}

/// Build the interest index entry for an authoritative entity.
///
/// Player-controlled entities are owned by their character principal even when
/// the record carries no explicit owner. Visibility is authored with tags until
/// a first-class visibility component exists.
#[must_use]
pub(crate) fn interest_entity(entity: &EntityRecord) -> InterestEntity {
    let visibility = if entity.tags.contains(VISIBILITY_OWNER_ONLY_TAG) {
        Visibility::OwnerOnly
    } else if entity.tags.contains(VISIBILITY_STAFF_ONLY_TAG) {
        Visibility::StaffOnly
    } else if entity.tags.contains(VISIBILITY_HIDDEN_TAG) {
        Visibility::Hidden
    } else {
        Visibility::Public
    };

    let mut interest =
        InterestEntity::new(entity.id, entity.position()).with_visibility(visibility);
    let owner = entity
        .owner
        .or_else(|| entity.mind.as_ref().and_then(player_character));
    if let Some(owner) = owner {
        interest = interest.with_owner(owner);
    }
    interest
}

fn player_character(mind: &engine_sim::mind::Mind) -> Option<StableId> {
    match &mind.controller {
        Controller::Player { character, .. } => Some(*character),
        _ => None,
    }
}

/// Canonical visibility tags, for diagnostics and authoring tools.
#[must_use]
pub fn visibility_tags() -> BTreeSet<&'static str> {
    BTreeSet::from([
        VISIBILITY_HIDDEN_TAG,
        VISIBILITY_OWNER_ONLY_TAG,
        VISIBILITY_STAFF_ONLY_TAG,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::id::IdKind;

    fn entity(seed: u128) -> StableId {
        StableId::from_parts(IdKind::Entity, 1, seed)
    }

    fn character(seed: u128) -> StableId {
        StableId::from_parts(IdKind::Character, 1, seed)
    }

    fn record(seed: u128) -> EntityRecord {
        EntityRecord::new(
            entity(seed),
            DefinitionId::new("prop.crate").unwrap(),
            Vec3::new(2.0, 0.0, 3.0),
        )
        .with_tag(VISIBILITY_OWNER_ONLY_TAG)
        .owned_by(character(9))
    }

    #[test]
    fn snapshots_expose_only_public_fields() {
        let record = record(1);
        let snapshot = EntitySnapshot::capture(&record);
        assert_eq!(snapshot.entity, record.id);
        assert_eq!(snapshot.owner, Some(character(9)));
        let json = serde_json::to_value(&snapshot).unwrap();
        assert!(json.get("inventory").is_none());
        assert!(json.get("mind").is_none());
        assert!(json.get("tags").is_none());
    }

    #[test]
    fn interest_entries_use_tags_and_controller_ownership() {
        let hidden = interest_entity(&record(1));
        assert_eq!(hidden.visibility, Visibility::OwnerOnly);
        assert_eq!(hidden.owner, Some(character(9)));

        let player = EntityRecord::new(
            entity(2),
            DefinitionId::new("character.player").unwrap(),
            Vec3::ZERO,
        )
        .with_mind(engine_sim::mind::Mind::player(character(5), "session-1"));
        let entry = interest_entity(&player);
        assert_eq!(entry.visibility, Visibility::Public);
        assert_eq!(entry.owner, Some(character(5)));

        let staff_only = EntityRecord::new(
            entity(3),
            DefinitionId::new("prop.lever").unwrap(),
            Vec3::ZERO,
        )
        .with_tag(VISIBILITY_STAFF_ONLY_TAG);
        assert_eq!(
            interest_entity(&staff_only).visibility,
            Visibility::StaffOnly
        );

        let hidden = EntityRecord::new(
            entity(4),
            DefinitionId::new("prop.trap").unwrap(),
            Vec3::ZERO,
        )
        .with_tag(VISIBILITY_HIDDEN_TAG);
        assert_eq!(interest_entity(&hidden).visibility, Visibility::Hidden);
    }
}
