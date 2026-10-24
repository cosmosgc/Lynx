//! Interest management: which entities an observer is allowed to know about.
//!
//! The world gateway never sends a whole zone to every client. It maintains one
//! [`InterestSet`] per session and filters each [`ReplicationBatch`] for the
//! receiving session. Selection starts with horizontal distance and explicit
//! visibility, then applies the category's audience rule.
//!
//! Suspicious input fails closed: an update that is missing the entity or
//! session it needs is dropped instead of falling back to a zone broadcast.

use std::collections::{BTreeMap, BTreeSet};

use engine_core::error::{EngineError, ErrorCode};
use engine_core::id::StableId;
use engine_core::math::Vec3;
use serde::{Deserialize, Serialize};

use crate::replication::{Recipient, ReplicationBatch, ReplicationUpdate, SessionId};

/// Default horizontal view distance in meters.
pub const DEFAULT_VIEW_RADIUS_M: f32 = 64.0;

/// Largest accepted horizontal view distance in meters.
pub const MAX_VIEW_RADIUS_M: f32 = 2_048.0;

/// Maximum entities retained in one observer's interest set.
pub const MAX_INTEREST_ENTITIES: usize = 512;

/// Default hysteresis margin used when an entity leaves a view.
///
/// With a radius of 64 m and the default 0.125 margin, an entity enters at
/// 64 m and leaves at 72 m, so a player pacing across the boundary does not
/// cause a stream of appear/disappear messages.
pub const DEFAULT_EXIT_MARGIN: f32 = 0.125;

/// Tuning for perspective selection and hysteresis.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct InterestConfig {
    /// Extra fraction of the view radius applied before an entity leaves.
    pub exit_margin: f32,
    /// Maximum entities kept in one observer's set.
    pub max_entities: usize,
}

impl Default for InterestConfig {
    fn default() -> Self {
        Self {
            exit_margin: DEFAULT_EXIT_MARGIN,
            max_entities: MAX_INTEREST_ENTITIES,
        }
    }
}

impl InterestConfig {
    /// Validate custom tuning.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] when the margin is not finite or
    /// outside `0.0..=1.0`, or when the entity limit is zero or above
    /// [`MAX_INTEREST_ENTITIES`].
    pub fn new(exit_margin: f32, max_entities: usize) -> Result<Self, EngineError> {
        if !exit_margin.is_finite() || !(0.0..=1.0).contains(&exit_margin) {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "interest exit margin must be between 0.0 and 1.0",
            )
            .with_detail("exit_margin", exit_margin.to_string()));
        }
        if max_entities == 0 || max_entities > MAX_INTEREST_ENTITIES {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "interest entity limit is out of range",
            )
            .with_detail("max_entities", max_entities.to_string())
            .with_detail("max_allowed", MAX_INTEREST_ENTITIES.to_string()));
        }
        Ok(Self {
            exit_margin,
            max_entities,
        })
    }
}

/// An authenticated session asking for a view of the zone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterestObserver {
    /// World session receiving the view.
    pub session: SessionId,
    /// Character or entity principal controlled by the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<StableId>,
    /// Current horizontal position of the observer.
    pub position: Vec3,
    /// Horizontal distance at which new entities become visible.
    pub view_radius_m: f32,
    /// Whether the session may receive admin state.
    #[serde(default)]
    pub staff: bool,
}

impl InterestObserver {
    /// An observer with the default view distance and no privileges.
    #[must_use]
    pub fn new(session: SessionId, position: Vec3) -> Self {
        Self {
            session,
            principal: None,
            position,
            view_radius_m: DEFAULT_VIEW_RADIUS_M,
            staff: false,
        }
    }

    /// Record the controlled principal.
    #[must_use]
    pub fn with_principal(mut self, principal: StableId) -> Self {
        self.principal = Some(principal);
        self
    }

    /// Override the view distance.
    #[must_use]
    pub fn with_view_radius(mut self, view_radius_m: f32) -> Self {
        self.view_radius_m = view_radius_m;
        self
    }

    /// Grant staff visibility.
    #[must_use]
    pub const fn with_staff(mut self, staff: bool) -> Self {
        self.staff = staff;
        self
    }

    fn validate(&self) -> Result<(), EngineError> {
        if !self.position.x.is_finite()
            || !self.position.y.is_finite()
            || !self.position.z.is_finite()
        {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "observer position must be finite",
            )
            .with_detail("session", self.session.to_string()));
        }
        if !self.view_radius_m.is_finite()
            || self.view_radius_m <= 0.0
            || self.view_radius_m > MAX_VIEW_RADIUS_M
        {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "observer view radius is out of range",
            )
            .with_detail("session", self.session.to_string())
            .with_detail("max_radius_m", MAX_VIEW_RADIUS_M.to_string()));
        }
        Ok(())
    }
}

/// Explicit visibility class of a replicated entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// Visible to every observer in range.
    Public,
    /// Visible to the recorded owner and staff.
    Hidden,
    /// Visible only to the recorded owner.
    OwnerOnly,
    /// Visible only to staff sessions.
    StaffOnly,
}

/// One entity in the authoritative view used to compute interest sets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterestEntity {
    /// Authoritative entity ID.
    pub entity: StableId,
    /// Current horizontal position.
    pub position: Vec3,
    /// Explicit visibility class.
    pub visibility: Visibility,
    /// Owning character or property, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<StableId>,
    /// Principals allowed by a property access policy.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub authorized: BTreeSet<StableId>,
}

impl InterestEntity {
    /// A public entity at a position.
    #[must_use]
    pub fn new(entity: StableId, position: Vec3) -> Self {
        Self {
            entity,
            position,
            visibility: Visibility::Public,
            owner: None,
            authorized: BTreeSet::new(),
        }
    }

    /// Override the visibility class.
    #[must_use]
    pub const fn with_visibility(mut self, visibility: Visibility) -> Self {
        self.visibility = visibility;
        self
    }

    /// Record the owner.
    #[must_use]
    pub const fn with_owner(mut self, owner: StableId) -> Self {
        self.owner = Some(owner);
        self
    }

    /// Allow a principal under a property access policy.
    #[must_use]
    pub fn authorize(mut self, principal: StableId) -> Self {
        self.authorized.insert(principal);
        self
    }

    /// Whether the entity's visibility class admits this observer.
    #[must_use]
    pub fn is_visible_to(&self, observer: &InterestObserver) -> bool {
        self.is_visible_to_principal(observer.principal, observer.staff)
    }

    /// Whether the visibility class admits a principal with an optional staff
    /// role, without requiring a position.
    #[must_use]
    pub fn is_visible_to_principal(&self, principal: Option<StableId>, staff: bool) -> bool {
        let is_owner = self.owner.is_some_and(|owner| principal == Some(owner));
        match self.visibility {
            Visibility::Public => true,
            Visibility::Hidden => is_owner || staff,
            Visibility::OwnerOnly => is_owner,
            Visibility::StaffOnly => staff,
        }
    }
}

/// The entities one observer currently knows about.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InterestSet(BTreeSet<StableId>);

impl InterestSet {
    /// An empty view.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the view contains an entity.
    #[must_use]
    pub fn contains(&self, entity: &StableId) -> bool {
        self.0.contains(entity)
    }

    /// Add an entity to the view.
    pub fn insert(&mut self, entity: StableId) -> bool {
        self.0.insert(entity)
    }

    /// Remove an entity from the view.
    pub fn remove(&mut self, entity: &StableId) -> bool {
        self.0.remove(entity)
    }

    /// Number of visible entities.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the observer sees no entities.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Iterate in stable ID order.
    pub fn iter(&self) -> impl Iterator<Item = StableId> + '_ {
        self.0.iter().copied()
    }
}

impl FromIterator<StableId> for InterestSet {
    fn from_iter<T: IntoIterator<Item = StableId>>(iter: T) -> Self {
        Self(iter.into_iter().collect())
    }
}

/// The result of refreshing one observer's view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterestUpdate {
    /// Complete current view.
    pub set: InterestSet,
    /// Entities that became visible, in stable ID order.
    pub added: Vec<StableId>,
    /// Entities that became invisible, in stable ID order.
    pub removed: Vec<StableId>,
}

impl InterestUpdate {
    /// Whether nothing entered or left the view.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

/// The authoritative entity positions used to compute interest.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InterestWorld {
    entities: BTreeMap<StableId, InterestEntity>,
}

impl InterestWorld {
    /// An empty world view.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace an entity's interest record.
    pub fn upsert(&mut self, entity: InterestEntity) -> Option<InterestEntity> {
        self.entities.insert(entity.entity, entity)
    }

    /// Remove an entity, returning its previous record.
    pub fn remove(&mut self, entity: StableId) -> Option<InterestEntity> {
        self.entities.remove(&entity)
    }

    /// Look up an entity.
    #[must_use]
    pub fn get(&self, entity: StableId) -> Option<&InterestEntity> {
        self.entities.get(&entity)
    }

    /// Whether the world contains an entity.
    #[must_use]
    pub fn contains(&self, entity: StableId) -> bool {
        self.entities.contains_key(&entity)
    }

    /// Number of indexed entities.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entities.len()
    }

    /// Whether the index is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    /// Iterate in stable ID order.
    pub fn iter(&self) -> impl Iterator<Item = &InterestEntity> {
        self.entities.values()
    }
}

/// Per-session views with entry/exit hysteresis.
#[derive(Debug, Clone)]
pub struct InterestManager {
    config: InterestConfig,
    sets: BTreeMap<SessionId, InterestSet>,
}

impl InterestManager {
    /// Create a manager with the supplied tuning.
    #[must_use]
    pub fn new(config: InterestConfig) -> Self {
        Self {
            config,
            sets: BTreeMap::new(),
        }
    }

    /// Current tuning.
    #[must_use]
    pub const fn config(&self) -> InterestConfig {
        self.config
    }

    /// Refresh one observer and report appear/disappear changes.
    ///
    /// Entities already in the view leave only beyond the exit radius, while
    /// new entities must be inside the entry radius. When the entity cap is
    /// reached, the nearest entities win and ties break on stable ID.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] when the observer position or view
    /// radius is not finite or is out of range.
    pub fn refresh(
        &mut self,
        observer: &InterestObserver,
        world: &InterestWorld,
    ) -> Result<InterestUpdate, EngineError> {
        observer.validate()?;
        let previous = self
            .sets
            .get(&observer.session)
            .cloned()
            .unwrap_or_default();
        let exit_radius =
            (observer.view_radius_m * (1.0 + self.config.exit_margin)).min(MAX_VIEW_RADIUS_M);

        let mut candidates: Vec<(f32, StableId)> = world
            .entities
            .values()
            .filter(|entity| entity.is_visible_to(observer))
            .filter_map(|entity| {
                let distance = observer.position.distance_xz(entity.position);
                let limit = if previous.contains(&entity.entity) {
                    exit_radius
                } else {
                    observer.view_radius_m
                };
                (distance <= limit).then_some((distance, entity.entity))
            })
            .collect();
        candidates.sort_by(|left, right| {
            left.0
                .total_cmp(&right.0)
                .then_with(|| left.1.cmp(&right.1))
        });
        candidates.truncate(self.config.max_entities);

        let set: InterestSet = candidates.iter().map(|(_, entity)| *entity).collect();
        let added = set.0.difference(&previous.0).copied().collect();
        let removed = previous.0.difference(&set.0).copied().collect();
        self.sets.insert(observer.session.clone(), set.clone());
        Ok(InterestUpdate {
            set,
            added,
            removed,
        })
    }

    /// The last computed view for a session.
    #[must_use]
    pub fn set(&self, session: &SessionId) -> Option<&InterestSet> {
        self.sets.get(session)
    }

    /// Forget a disconnected session.
    pub fn remove_session(&mut self, session: &SessionId) -> Option<InterestSet> {
        self.sets.remove(session)
    }

    /// Number of tracked sessions.
    #[must_use]
    pub fn session_count(&self) -> usize {
        self.sets.len()
    }
}

impl Default for InterestManager {
    fn default() -> Self {
        Self::new(InterestConfig::default())
    }
}

/// Everything needed to decide what one session may receive.
#[derive(Debug, Clone, Copy)]
pub struct ReplicationAudience<'a> {
    /// Receiving session.
    pub session: &'a SessionId,
    /// Principal controlled by the receiving session.
    pub principal: Option<StableId>,
    /// Whether the session may receive admin state.
    pub staff: bool,
    /// The session's current interest set.
    pub interest: &'a InterestSet,
    /// Entity index used for owner and property checks.
    pub world: &'a InterestWorld,
}

impl<'a> ReplicationAudience<'a> {
    /// Build an audience with no owner or staff privileges.
    #[must_use]
    pub fn new(
        session: &'a SessionId,
        interest: &'a InterestSet,
        world: &'a InterestWorld,
    ) -> Self {
        Self {
            session,
            principal: None,
            staff: false,
            interest,
            world,
        }
    }

    /// Record the controlled principal.
    #[must_use]
    pub const fn with_principal(mut self, principal: StableId) -> Self {
        self.principal = Some(principal);
        self
    }

    /// Grant staff audience.
    #[must_use]
    pub const fn with_staff(mut self, staff: bool) -> Self {
        self.staff = staff;
        self
    }
}

/// Filter a batch down to the updates one session may receive.
///
/// The returned batch keeps emission order, then collapses superseded state
/// snapshots. Updates that cannot be attributed to an allowed audience are
/// dropped, never broadcast.
#[must_use]
pub fn filter_batch(
    batch: &ReplicationBatch,
    audience: &ReplicationAudience<'_>,
) -> ReplicationBatch {
    let mut filtered = ReplicationBatch::new(batch.tick);
    filtered.updates = batch
        .updates
        .iter()
        .filter(|update| admits(update, audience))
        .cloned()
        .collect();
    filtered.retain_latest_state();
    filtered
}

fn admits(update: &ReplicationUpdate, audience: &ReplicationAudience<'_>) -> bool {
    match update.category.recipient() {
        Recipient::Session => update.session.as_ref() == Some(audience.session),
        Recipient::Zone => update.entity.is_some_and(|entity| {
            audience.interest.contains(&entity)
                && audience.world.get(entity).is_some_and(|entity| {
                    entity.is_visible_to_principal(audience.principal, audience.staff)
                })
        }),
        Recipient::Owner => update
            .entity
            .and_then(|entity| audience.world.get(entity))
            .is_some_and(|entity| owned_by(entity, audience.principal)),
        Recipient::Authorized => update
            .entity
            .and_then(|entity| audience.world.get(entity))
            .is_some_and(|entity| {
                audience.staff
                    || owned_by(entity, audience.principal)
                    || audience
                        .principal
                        .is_some_and(|principal| entity.authorized.contains(&principal))
            }),
        Recipient::Staff => audience.staff,
    }
}

fn owned_by(entity: &InterestEntity, principal: Option<StableId>) -> bool {
    entity.owner.is_some_and(|owner| principal == Some(owner))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replication::ReplicationCategory;
    use engine_core::id::IdKind;
    use engine_core::time::Tick;
    use serde_json::json;

    fn entity(seed: u128) -> StableId {
        StableId::from_parts(IdKind::Entity, 1, seed)
    }

    fn character(seed: u128) -> StableId {
        StableId::from_parts(IdKind::Character, 1, seed)
    }

    fn session(value: &str) -> SessionId {
        SessionId::new(value).unwrap()
    }

    fn observer(seed: u128) -> InterestObserver {
        InterestObserver::new(session("session-1"), Vec3::ZERO).with_principal(character(seed))
    }

    fn new_batch(tick: Tick) -> ReplicationBatch {
        ReplicationBatch::new(tick)
    }

    fn snapshot(batch: &mut ReplicationBatch, target: StableId, sequence: u64) {
        let tick = batch.tick;
        batch
            .push(
                ReplicationUpdate::new(ReplicationCategory::StateSnapshot, tick)
                    .with_entity(target)
                    .with_sequence(sequence)
                    .with_payload(json!({ "sequence": sequence })),
            )
            .unwrap();
    }

    #[test]
    fn config_and_observer_validation_reject_bad_values() {
        assert!(InterestConfig::new(0.5, 10).is_ok());
        for margin in [-0.1, 1.5, f32::NAN, f32::INFINITY] {
            assert_eq!(
                InterestConfig::new(margin, 10).unwrap_err().code(),
                ErrorCode::InvalidRequest
            );
        }
        assert!(InterestConfig::new(0.1, 0).is_err());
        assert!(InterestConfig::new(0.1, MAX_INTEREST_ENTITIES + 1).is_err());

        let mut bad = observer(1).with_view_radius(0.0);
        assert!(
            InterestManager::default()
                .refresh(&bad, &InterestWorld::new())
                .is_err()
        );
        bad = bad.with_view_radius(f32::NAN);
        assert!(
            InterestManager::default()
                .refresh(&bad, &InterestWorld::new())
                .is_err()
        );
        bad = observer(1).with_view_radius(MAX_VIEW_RADIUS_M + 1.0);
        assert!(
            InterestManager::default()
                .refresh(&bad, &InterestWorld::new())
                .is_err()
        );

        let mut invalid_position = observer(1);
        invalid_position.position = Vec3::new(f32::NAN, 0.0, 0.0);
        assert!(
            InterestManager::default()
                .refresh(&invalid_position, &InterestWorld::new())
                .is_err()
        );
    }

    #[test]
    fn entities_enter_at_the_radius_and_leave_with_hysteresis() {
        let mut manager = InterestManager::new(InterestConfig::new(0.25, 32).unwrap());
        let mut world = InterestWorld::new();
        let target = entity(2);
        world.upsert(InterestEntity::new(target, Vec3::new(11.0, 0.0, 0.0)));
        let observer = observer(1).with_view_radius(10.0);

        let first = manager.refresh(&observer, &world).unwrap();
        assert!(first.set.is_empty());

        world.upsert(InterestEntity::new(target, Vec3::new(10.0, 0.0, 0.0)));
        let entered = manager.refresh(&observer, &world).unwrap();
        assert!(entered.set.contains(&target));
        assert_eq!(entered.added, vec![target]);
        assert_eq!(entered.removed, Vec::new());

        world.upsert(InterestEntity::new(target, Vec3::new(12.0, 0.0, 0.0)));
        let lingering = manager.refresh(&observer, &world).unwrap();
        assert!(lingering.set.contains(&target));
        assert!(lingering.is_empty());

        world.upsert(InterestEntity::new(target, Vec3::new(13.0, 0.0, 0.0)));
        let left = manager.refresh(&observer, &world).unwrap();
        assert!(!left.set.contains(&target));
        assert_eq!(left.removed, vec![target]);
    }

    #[test]
    fn visibility_classes_and_staff_privileges_are_respected() {
        let principal = character(1);
        let other = character(2);
        let public = entity(10);
        let hidden_mine = entity(11);
        let hidden_other = entity(12);
        let owner_only = entity(13);
        let staff_only = entity(14);
        let mut world = InterestWorld::new();
        world.upsert(InterestEntity::new(public, Vec3::ZERO));
        world.upsert(
            InterestEntity::new(hidden_mine, Vec3::ZERO)
                .with_visibility(Visibility::Hidden)
                .with_owner(principal),
        );
        world.upsert(
            InterestEntity::new(hidden_other, Vec3::ZERO)
                .with_visibility(Visibility::Hidden)
                .with_owner(other),
        );
        world.upsert(
            InterestEntity::new(owner_only, Vec3::ZERO)
                .with_visibility(Visibility::OwnerOnly)
                .with_owner(principal),
        );
        world.upsert(
            InterestEntity::new(staff_only, Vec3::ZERO).with_visibility(Visibility::StaffOnly),
        );

        let mut manager = InterestManager::default();
        let player = manager.refresh(&observer(1), &world).unwrap().set;
        assert!(player.contains(&public));
        assert!(player.contains(&hidden_mine));
        assert!(!player.contains(&hidden_other));
        assert!(player.contains(&owner_only));
        assert!(!player.contains(&staff_only));

        let staff = manager
            .refresh(
                &InterestObserver::new(session("staff-1"), Vec3::ZERO).with_staff(true),
                &world,
            )
            .unwrap()
            .set;
        assert!(staff.contains(&public));
        assert!(staff.contains(&hidden_mine));
        assert!(staff.contains(&hidden_other));
        assert!(!staff.contains(&owner_only));
        assert!(staff.contains(&staff_only));
    }

    #[test]
    fn the_nearest_entities_win_when_the_cap_is_reached() {
        let mut world = InterestWorld::new();
        let far = entity(3);
        let near = entity(1);
        let middle = entity(2);
        world.upsert(InterestEntity::new(far, Vec3::new(9.0, 0.0, 0.0)));
        world.upsert(InterestEntity::new(near, Vec3::new(1.0, 0.0, 0.0)));
        world.upsert(InterestEntity::new(middle, Vec3::new(5.0, 0.0, 0.0)));

        let mut manager = InterestManager::new(InterestConfig::new(0.0, 2).unwrap());
        let set = manager.refresh(&observer(1), &world).unwrap().set;
        assert_eq!(set.iter().collect::<Vec<_>>(), vec![near, middle]);
    }

    #[test]
    fn filtering_applies_category_audience_rules() {
        let tick = Tick(4);
        let visible = entity(1);
        let hidden = entity(2);
        let owned = entity(3);
        let property = entity(4);
        let admin = entity(5);
        let principal = character(9);
        let session_a = session("session-a");
        let session_b = session("session-b");

        let mut world = InterestWorld::new();
        world.upsert(InterestEntity::new(visible, Vec3::ZERO));
        world.upsert(InterestEntity::new(hidden, Vec3::new(1_000.0, 0.0, 0.0)));
        world.upsert(InterestEntity::new(owned, Vec3::ZERO).with_owner(principal));
        world.upsert(InterestEntity::new(property, Vec3::ZERO).authorize(principal));
        world.upsert(InterestEntity::new(admin, Vec3::ZERO));

        let interest: InterestSet = [visible].into_iter().collect();
        let mut batch = new_batch(tick);
        snapshot(&mut batch, visible, 2);
        snapshot(&mut batch, visible, 3);
        snapshot(&mut batch, hidden, 1);
        batch
            .push(
                ReplicationUpdate::new(ReplicationCategory::ReliableEvent, tick)
                    .with_session(session_a.clone()),
            )
            .unwrap();
        batch
            .push(
                ReplicationUpdate::new(ReplicationCategory::ReliableEvent, tick)
                    .with_session(session_b.clone()),
            )
            .unwrap();
        batch
            .push(ReplicationUpdate::new(ReplicationCategory::OwnerOnly, tick).with_entity(owned))
            .unwrap();
        batch
            .push(
                ReplicationUpdate::new(ReplicationCategory::PropertyScoped, tick)
                    .with_entity(property),
            )
            .unwrap();
        batch
            .push(ReplicationUpdate::new(ReplicationCategory::Admin, tick).with_entity(admin))
            .unwrap();

        let audience =
            ReplicationAudience::new(&session_a, &interest, &world).with_principal(principal);
        let filtered = filter_batch(&batch, &audience);
        assert_eq!(filtered.len(), 4);
        let snapshots: Vec<_> = filtered
            .updates
            .iter()
            .filter(|update| update.category == ReplicationCategory::StateSnapshot)
            .collect();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].sequence, 3, "latest snapshot wins");
        assert!(filtered.updates.iter().any(|update| {
            update.category == ReplicationCategory::ReliableEvent
                && update.session.as_ref() == Some(&session_a)
        }));
        assert!(filtered.updates.iter().any(|update| {
            update.category == ReplicationCategory::OwnerOnly && update.entity == Some(owned)
        }));
        assert!(filtered.updates.iter().any(|update| {
            update.category == ReplicationCategory::PropertyScoped
                && update.entity == Some(property)
        }));
        assert!(
            !filtered
                .updates
                .iter()
                .any(|update| update.category == ReplicationCategory::Admin)
        );

        let staff = filter_batch(&batch, &audience.with_staff(true));
        assert!(
            staff
                .updates
                .iter()
                .any(|update| update.category == ReplicationCategory::Admin)
        );
    }

    #[test]
    fn filtering_fails_closed_for_unattributable_updates() {
        let tick = Tick(1);
        let target = entity(1);
        let session_a = session("session-a");
        let principal = character(9);
        let other = character(8);
        let interest = InterestSet::new();
        let mut world = InterestWorld::new();
        world.upsert(InterestEntity::new(target, Vec3::ZERO).with_owner(other));
        let audience =
            ReplicationAudience::new(&session_a, &interest, &world).with_principal(principal);

        let mut batch = new_batch(tick);
        // Bypass validation to model a malformed or hostile batch.
        batch.updates.push(
            ReplicationUpdate::new(ReplicationCategory::ReliableEvent, tick).with_entity(target),
        );
        assert!(filter_batch(&batch, &audience).is_empty());

        let mut batch = new_batch(tick);
        batch.updates.push(
            ReplicationUpdate::new(ReplicationCategory::OwnerOnly, tick)
                .with_entity(target)
                .with_session(session_a.clone()),
        );
        assert!(
            filter_batch(&batch, &audience).is_empty(),
            "naming a session does not bypass the owner check"
        );

        let mut batch = new_batch(tick);
        batch.updates.push(
            ReplicationUpdate::new(ReplicationCategory::PropertyScoped, tick).with_entity(target),
        );
        assert!(filter_batch(&batch, &audience).is_empty());
    }

    #[test]
    fn stale_interest_sets_do_not_leak_removed_or_hidden_entities() {
        let tick = Tick(1);
        let removed = entity(1);
        let hidden = entity(2);
        let session_a = session("session-a");
        let principal = character(9);
        let other = character(8);
        let interest: InterestSet = [removed, hidden].into_iter().collect();
        let mut world = InterestWorld::new();
        world.upsert(
            InterestEntity::new(hidden, Vec3::ZERO)
                .with_visibility(Visibility::OwnerOnly)
                .with_owner(other),
        );
        let audience =
            ReplicationAudience::new(&session_a, &interest, &world).with_principal(principal);

        let mut batch = new_batch(tick);
        snapshot(&mut batch, removed, 1);
        snapshot(&mut batch, hidden, 1);

        assert_eq!(
            filter_batch(&batch, &audience).len(),
            0,
            "a cached interest set cannot outlive authoritative visibility"
        );
    }

    #[test]
    fn interest_updates_round_trip_through_json() {
        let update = InterestUpdate {
            set: [entity(1), entity(2)].into_iter().collect(),
            added: vec![entity(2)],
            removed: Vec::new(),
        };
        let text = serde_json::to_string(&update).unwrap();
        assert_eq!(
            serde_json::from_str::<InterestUpdate>(&text).unwrap(),
            update
        );
    }

    #[test]
    fn managers_forget_disconnected_sessions() {
        let mut manager = InterestManager::default();
        let world = InterestWorld::new();
        manager.refresh(&observer(1), &world).unwrap();
        let session = observer(1).session;
        assert_eq!(manager.session_count(), 1);
        assert!(manager.remove_session(&session).is_some());
        assert_eq!(manager.session_count(), 0);
        assert!(manager.set(&session).is_none());
    }
}
