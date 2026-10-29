//! Entity records composed from optional, typed components.

use std::collections::{BTreeMap, BTreeSet};

use engine_core::error::{EngineError, ErrorCode};
use engine_core::faction::ReputationBook;
use engine_core::id::{DefinitionId, StableId};
use engine_core::inventory::Inventory;
use engine_core::math::Vec3;
use engine_core::schema::{ComponentInstance, ComponentKey, PersistencePolicy};
use serde::{Deserialize, Serialize};

use crate::action::Verb;
use crate::combat::CombatStats;
use crate::mind::Mind;
use crate::minigame::Minigame;

/// Position and facing in the zone.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    /// World position in meters.
    pub position: Vec3,
    /// Facing angle in degrees, clockwise from north.
    pub yaw_degrees: f32,
}

impl Transform {
    /// A transform at `position` facing north.
    #[must_use]
    pub const fn new(position: Vec3) -> Self {
        Self {
            position,
            yaw_degrees: 0.0,
        }
    }

    /// Set the facing angle.
    #[must_use]
    pub const fn with_yaw(mut self, yaw_degrees: f32) -> Self {
        self.yaw_degrees = yaw_degrees;
        self
    }
}

/// Hit points shared by creatures, machines, and destructible objects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health {
    /// Current hit points.
    pub current: i32,
    /// Maximum hit points.
    pub max: i32,
}

impl Health {
    /// Create a full-health component. `max` is clamped to at least one.
    #[must_use]
    pub fn new(max: i32) -> Self {
        let max = max.max(1);
        Self { current: max, max }
    }

    /// Whether the entity is alive.
    #[must_use]
    pub fn is_alive(self) -> bool {
        self.current > 0
    }

    /// Apply damage and return the amount actually dealt.
    pub fn damage(&mut self, amount: i32) -> i32 {
        let applied = amount.max(0).min(self.current.max(0));
        self.current -= applied;
        applied
    }
}

/// Who may use an interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionPermission {
    /// Anyone in range.
    Public,
    /// Only the recorded owner.
    Owner,
    /// The owner, or anyone when unowned.
    OwnerOrPublic,
    /// Only server processes.
    ServerOnly,
}

/// The interaction surface of an entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Interactable {
    /// Supported verbs. Availability is derived from this set and state.
    pub verbs: BTreeSet<Verb>,
    /// Maximum horizontal interaction distance in meters.
    pub range_m: f32,
    /// Whether a clear line of sight is required.
    pub requires_line_of_sight: bool,
    /// Whether interaction is currently enabled.
    pub enabled: bool,
    /// Permission rule.
    pub permission: InteractionPermission,
    /// Server ticks before the same actor may repeat the verb.
    pub cooldown_ticks: u64,
}

impl Interactable {
    /// An enabled, public interaction surface with no cooldown.
    #[must_use]
    pub fn new(range_m: f32, verbs: impl IntoIterator<Item = Verb>) -> Self {
        Self {
            verbs: verbs.into_iter().collect(),
            range_m,
            requires_line_of_sight: true,
            enabled: true,
            permission: InteractionPermission::Public,
            cooldown_ticks: 0,
        }
    }

    /// Require ownership for use.
    #[must_use]
    pub fn owned(mut self) -> Self {
        self.permission = InteractionPermission::OwnerOrPublic;
        self
    }

    /// Disable line-of-sight checks. Useful for terminals seen through glass.
    #[must_use]
    pub const fn without_line_of_sight(mut self) -> Self {
        self.requires_line_of_sight = false;
        self
    }

    /// Set the per-actor cooldown.
    #[must_use]
    pub const fn with_cooldown(mut self, ticks: u64) -> Self {
        self.cooldown_ticks = ticks;
        self
    }

    /// Set the permission rule.
    #[must_use]
    pub const fn with_permission(mut self, permission: InteractionPermission) -> Self {
        self.permission = permission;
        self
    }

    /// Whether the surface advertises `verb`.
    #[must_use]
    pub fn supports(&self, verb: &Verb) -> bool {
        self.verbs.contains(verb)
    }
}

/// An entity-provided interface, described declaratively.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiProvider {
    /// Panel schema the client knows how to render.
    pub schema: DefinitionId,
    /// Title shown in the panel header.
    pub title: String,
}

impl UiProvider {
    /// Create a UI provider.
    #[must_use]
    pub fn new(schema: DefinitionId, title: impl Into<String>) -> Self {
        Self {
            schema,
            title: title.into(),
        }
    }
}

/// A simple openable door. Game-specific state lives in a component because
/// the reference slice needs it; other games can replace it with their own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Door {
    /// Whether the door is open.
    pub open: bool,
    /// Whether the door is locked.
    pub locked: bool,
    /// Item definition that unlocks it, if any.
    pub key: Option<DefinitionId>,
}

impl Door {
    /// A closed, unlocked door.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            open: false,
            locked: false,
            key: None,
        }
    }

    /// A locked door opened by `key`.
    #[must_use]
    pub fn locked_with(key: DefinitionId) -> Self {
        Self {
            open: false,
            locked: true,
            key: Some(key),
        }
    }
}

impl Default for Door {
    fn default() -> Self {
        Self::new()
    }
}

/// Faction identity plus character-specific reputation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactionMembership {
    /// The entity's own faction.
    pub faction: DefinitionId,
    /// Modifiers this entity holds toward other factions.
    pub reputation: ReputationBook,
}

/// Server-owned condition that determines whether an entity can locomote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MovementStatus {
    /// The entity may move subject to control, navigation, and collision rules.
    #[default]
    Mobile,
    /// The entity is immobilized by an authoritative effect.
    Stunned,
}

/// Authoritative movement collision and status inputs for an entity.
///
/// This component is never populated from a movement request. Controlled
/// entities without it use [`Self::default`] for backwards-compatible player
/// occupancy; static entities must opt in to block movement.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MovementProfile {
    /// Horizontal collision radius in world meters.
    pub radius_m: f32,
    /// Whether this entity occupies a navigation cell and blocks body overlap.
    pub occupies: bool,
    /// Server-owned speed multiplier for statuses, equipment, and content.
    pub speed_multiplier: f32,
    /// Server-owned locomotion status.
    pub status: MovementStatus,
}

impl MovementProfile {
    /// Create a mobile, occupying collision body.
    #[must_use]
    pub const fn new(radius_m: f32) -> Self {
        Self {
            radius_m,
            occupies: true,
            speed_multiplier: 1.0,
            status: MovementStatus::Mobile,
        }
    }

    /// Validate content or persisted movement inputs.
    ///
    /// # Errors
    /// Returns [`ErrorCode::InvalidRequest`] for a non-finite/negative radius
    /// or non-finite, negative speed multiplier.
    pub fn validate(self) -> Result<(), EngineError> {
        if !self.radius_m.is_finite() || self.radius_m < 0.0 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "movement collision radius must be finite and non-negative",
            ));
        }
        if !self.speed_multiplier.is_finite() || self.speed_multiplier < 0.0 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "movement speed multiplier must be finite and non-negative",
            ));
        }
        Ok(())
    }

    /// Whether this profile permits movement.
    #[must_use]
    pub const fn can_move(self) -> bool {
        matches!(self.status, MovementStatus::Mobile) && self.speed_multiplier > 0.0
    }
}

impl Default for MovementProfile {
    fn default() -> Self {
        Self::new(0.35)
    }
}

impl FactionMembership {
    /// Join a faction with no reputation overrides.
    #[must_use]
    pub fn new(faction: DefinitionId) -> Self {
        Self {
            faction,
            reputation: ReputationBook::new(),
        }
    }
}

/// A live, mutable entity instance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityRecord {
    /// Durable entity identity.
    pub id: StableId,
    /// Authored prototype this instance came from.
    pub prototype: DefinitionId,
    /// Position and facing.
    pub transform: Transform,
    /// Hit points, when damageable.
    pub health: Option<Health>,
    /// Server-owned, resolved combat inputs, when this entity may attack.
    pub combat: Option<CombatStats>,
    /// Interaction surface, when usable.
    pub interactable: Option<Interactable>,
    /// Controller slot.
    pub mind: Option<Mind>,
    /// Server-owned collision and locomotion status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub movement: Option<MovementProfile>,
    /// Inventory, when the entity can hold items.
    pub inventory: Option<Inventory>,
    /// Faction identity and reputation.
    pub faction: Option<FactionMembership>,
    /// Door state, when the entity is a door.
    pub door: Option<Door>,
    /// Entity-provided UI, when it can open an interface.
    pub ui: Option<UiProvider>,
    /// Server-authoritative minigame attached to the interface, if any.
    pub minigame: Option<Minigame>,
    /// Owning character or property, when applicable.
    pub owner: Option<StableId>,
    /// Authoring and query tags.
    pub tags: BTreeSet<String>,
    /// Persistence lifetime.
    pub persistence: PersistencePolicy,
    /// Schema-validated extension components.
    pub components: BTreeMap<ComponentKey, ComponentInstance>,
}

impl EntityRecord {
    /// Start an entity from a prototype at a position.
    #[must_use]
    pub fn new(id: StableId, prototype: DefinitionId, position: Vec3) -> Self {
        Self {
            id,
            prototype,
            transform: Transform::new(position),
            health: None,
            combat: None,
            interactable: None,
            mind: None,
            movement: None,
            inventory: None,
            faction: None,
            door: None,
            ui: None,
            minigame: None,
            owner: None,
            tags: BTreeSet::new(),
            persistence: PersistencePolicy::Transient,
            components: BTreeMap::new(),
        }
    }

    /// Attach hit points.
    #[must_use]
    pub fn with_health(mut self, max: i32) -> Self {
        self.health = Some(Health::new(max));
        self
    }

    /// Attach server-owned resolved combat inputs.
    #[must_use]
    pub const fn with_combat(mut self, combat: CombatStats) -> Self {
        self.combat = Some(combat);
        self
    }

    /// Attach an interaction surface.
    #[must_use]
    pub fn with_interactable(mut self, interactable: Interactable) -> Self {
        self.interactable = Some(interactable);
        self
    }

    /// Attach a mind.
    #[must_use]
    pub fn with_mind(mut self, mind: Mind) -> Self {
        self.mind = Some(mind);
        self
    }

    /// Attach authoritative collision and locomotion state.
    #[must_use]
    pub const fn with_movement(mut self, movement: MovementProfile) -> Self {
        self.movement = Some(movement);
        self
    }

    /// Effective body for collision and occupancy.
    ///
    /// Controlled entities receive the conservative default body so legacy
    /// player snapshots cannot pass through one another. Static entities opt in
    /// by carrying an explicit profile.
    #[must_use]
    pub fn movement_profile(&self) -> Option<MovementProfile> {
        self.movement
            .or_else(|| self.mind.as_ref().map(|_| MovementProfile::default()))
    }

    /// Attach an inventory.
    #[must_use]
    pub fn with_inventory(mut self, inventory: Inventory) -> Self {
        self.inventory = Some(inventory);
        self
    }

    /// Attach faction membership.
    #[must_use]
    pub fn with_faction(mut self, faction: FactionMembership) -> Self {
        self.faction = Some(faction);
        self
    }

    /// Attach door state.
    #[must_use]
    pub fn with_door(mut self, door: Door) -> Self {
        self.door = Some(door);
        self
    }

    /// Attach an entity-provided UI.
    #[must_use]
    pub fn with_ui(mut self, ui: UiProvider) -> Self {
        self.ui = Some(ui);
        self
    }

    /// Attach a server-authoritative minigame.
    #[must_use]
    pub fn with_minigame(mut self, minigame: Minigame) -> Self {
        self.minigame = Some(minigame);
        self
    }

    /// Record an owner.
    #[must_use]
    pub fn owned_by(mut self, owner: StableId) -> Self {
        self.owner = Some(owner);
        self
    }

    /// Add a tag.
    #[must_use]
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.insert(tag.into());
        self
    }

    /// Set the persistence policy.
    #[must_use]
    pub const fn with_persistence(mut self, policy: PersistencePolicy) -> Self {
        self.persistence = policy;
        self
    }

    /// Add a schema-validated extension component.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::Conflict`] when the component key is already
    /// present.
    pub fn with_component(mut self, component: ComponentInstance) -> Result<Self, EngineError> {
        if self.components.contains_key(&component.key) {
            return Err(
                EngineError::new(ErrorCode::Conflict, "entity already has this component")
                    .with_detail("entity", self.id.to_string())
                    .with_detail("component", component.key.to_string()),
            );
        }
        self.components.insert(component.key.clone(), component);
        Ok(self)
    }

    /// The entity's current position.
    #[must_use]
    pub fn position(&self) -> Vec3 {
        self.transform.position
    }

    /// Whether the entity is alive. Entities without health are alive.
    #[must_use]
    pub fn is_alive(&self) -> bool {
        self.health.is_none_or(Health::is_alive)
    }

    /// Whether the entity currently has an active controller.
    #[must_use]
    pub fn is_controlled(&self) -> bool {
        self.mind
            .as_ref()
            .is_some_and(|mind| mind.controller != crate::mind::Controller::None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::id::IdKind;

    fn entity() -> EntityRecord {
        EntityRecord::new(
            StableId::from_parts(IdKind::Entity, 1, 1),
            DefinitionId::new("prop.crate").unwrap(),
            Vec3::ZERO,
        )
    }

    #[test]
    fn health_clamps_damage_and_reports_death() {
        let mut health = Health::new(5);
        assert!(health.is_alive());
        assert_eq!(health.damage(2), 2);
        assert_eq!(health.current, 3);
        assert_eq!(health.damage(99), 3);
        assert!(!health.is_alive());
        assert_eq!(health.damage(-5), 0);
    }

    #[test]
    fn entities_without_health_are_alive() {
        assert!(entity().is_alive());
        assert!(!entity().is_controlled());
    }

    #[test]
    fn builder_attaches_components_and_rejects_duplicates() {
        let key = ComponentKey::new("game.lock").unwrap();
        let component = ComponentInstance::new(
            key.clone(),
            engine_core::schema::SchemaVersion::new(1, 0),
            engine_core::schema::ComponentData::new(),
        );
        let entity = entity().with_component(component.clone()).unwrap();
        assert!(entity.components.contains_key(&key));

        let error = entity.with_component(component).unwrap_err();
        assert_eq!(error.code(), ErrorCode::Conflict);
    }
}
