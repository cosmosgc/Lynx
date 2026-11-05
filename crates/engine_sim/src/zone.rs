//! The authoritative zone: entity store, links, navigation, and budgets.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use engine_core::error::{EngineError, ErrorCode};
use engine_core::id::{DefinitionId, IdKind, StableId};
use engine_core::inventory::{ItemRules, TransferLedger};
use engine_core::math::Vec3;
use engine_core::time::Tick;
use serde::{Deserialize, Serialize};

use crate::action::{ActionEffect, ActionKey, ActionResult};
use crate::ai::{AiDiagnostics, AiNavigationState, AiTickPolicy, AiTickReport};
use crate::combat::{CombatAuditEvent, CombatRules, DamageType};
use crate::entity::{EntityRecord, TimedDamageOverTime, TimedShield, TimedSlow};
use crate::link::{EntityLink, LinkEvent, LinkGraph};
use crate::mind::Controller;
use crate::movement::{MovementIntent, MovementSequence};
use crate::nav::{
    NavGrid, PartialPathPolicy, Path, PathBudget, PathDiagnostics, PathError, PathTickBudget,
};

/// Maximum remembered action results for idempotent retries.
pub const ACTION_CACHE_LIMIT: usize = 2_048;
/// Maximum remembered link-activation results.
pub const LINK_CACHE_LIMIT: usize = 1_024;
/// Maximum targets one activation may fan out to.
pub const MAX_LINK_TARGETS: usize = 32;
/// Maximum retained server-only movement diagnostics.
pub const MOVEMENT_DIAGNOSTIC_LIMIT: usize = 256;
/// Maximum concurrent timed damage effects or shields on one entity.
pub const TIMED_COMBAT_EFFECT_LIMIT: usize = 8;
/// Maximum retained server-only combat audit entries per zone.
pub const COMBAT_AUDIT_LIMIT: usize = 512;

/// Work limits for one simulation tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TickBudget {
    /// Maximum actions processed in one tick.
    pub max_actions: usize,
    /// Maximum AI controller updates in one tick.
    pub max_ai_updates: usize,
    /// Maximum pathfinding expansions across the tick.
    pub max_path_expansions: usize,
    /// Maximum pathfinding requests across the tick.
    #[serde(default = "default_path_requests")]
    pub max_path_requests: usize,
}

fn default_path_requests() -> usize {
    32
}

impl Default for TickBudget {
    fn default() -> Self {
        Self {
            max_actions: 256,
            max_ai_updates: 128,
            max_path_expansions: 16_384,
            max_path_requests: 32,
        }
    }
}

impl TickBudget {
    /// Path budget for one request, capped by the tick expansion allowance.
    ///
    /// Prefer [`Zone::find_path`], which enforces the cumulative tick budget.
    #[must_use]
    pub const fn path_budget(self) -> PathBudget {
        PathBudget {
            max_expansions: self.max_path_expansions,
            max_waypoints: 512,
        }
    }

    /// Cumulative per-tick pathfinding budget derived from this tick budget.
    #[must_use]
    pub const fn path_tick_budget(self) -> PathTickBudget {
        PathTickBudget::new(self.max_path_expansions, 512, self.max_path_requests)
    }
}

/// Observable counters for the current tick.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TickStats {
    /// The tick these stats describe.
    pub tick: Tick,
    /// Live entity count.
    pub entity_count: usize,
    /// Entities with an active controller.
    pub controlled_entities: usize,
    /// Actions applied since the zone started.
    pub applied_actions: u64,
    /// Link activations since the zone started.
    pub link_activations: u64,
}

/// Wall-clock measurements of work phases in the most recently completed
/// authoritative tick. These diagnostics never enter snapshots or simulation
/// decisions, so timing variability cannot affect deterministic outcomes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TickPhaseTimings {
    /// Expiration/application of timed effects and due respawns.
    pub effects_ns: u128,
    /// Queued movement validation and application, excluding the other tick phases.
    pub movement_ns: u128,
    /// Navigation blocker expiry and per-tick path-budget reset.
    pub navigation_ns: u128,
    /// Tick-driven AI decisions and action attempts.
    pub ai_ns: u128,
    /// Final entity and controller counters.
    pub statistics_ns: u128,
}

/// Static configuration for a zone instance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZoneConfig {
    /// Authored zone definition.
    pub definition: DefinitionId,
    /// Durable zone instance ID.
    pub instance: StableId,
    /// Minimum world corner.
    pub min: Vec3,
    /// Maximum world corner.
    pub max: Vec3,
    /// Maximum live entities.
    pub max_entities: usize,
    /// Per-tick work limits.
    pub budget: TickBudget,
    /// Versioned combat formula and faction policy for this zone.
    #[serde(default)]
    pub combat: CombatRules,
}

impl ZoneConfig {
    /// Create a configuration with default limits.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] when the instance ID is not a zone
    /// ID or the bounds are inverted.
    pub fn new(
        definition: DefinitionId,
        instance: StableId,
        min: Vec3,
        max: Vec3,
    ) -> Result<Self, EngineError> {
        instance.expect_kind(IdKind::Zone)?;
        if max.x <= min.x || max.z <= min.z {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "zone maximum bounds must exceed minimum bounds",
            ));
        }
        Ok(Self {
            definition,
            instance,
            min,
            max,
            max_entities: 1_000,
            budget: TickBudget::default(),
            combat: CombatRules::default(),
        })
    }

    /// Override the entity limit.
    #[must_use]
    pub fn with_max_entities(mut self, max_entities: usize) -> Self {
        self.max_entities = max_entities;
        self
    }

    /// Override the tick budget.
    #[must_use]
    pub const fn with_budget(mut self, budget: TickBudget) -> Self {
        self.budget = budget;
        self
    }

    /// Override the versioned combat rules for this zone.
    #[must_use]
    pub fn with_combat(mut self, combat: CombatRules) -> Self {
        self.combat = combat;
        self
    }

    /// Whether a world position is inside the zone.
    #[must_use]
    pub fn contains(&self, position: Vec3) -> bool {
        position.x >= self.min.x
            && position.x <= self.max.x
            && position.z >= self.min.z
            && position.z <= self.max.z
            && position.y >= self.min.y
            && position.y <= self.max.y
    }
}

/// A non-fatal structural problem, reported for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Stable code, for example `link.missing_target`.
    pub code: String,
    /// Affected entity, when known.
    pub entity: Option<StableId>,
    /// Human-readable explanation.
    pub message: String,
}

/// The authoritative simulation state for one zone instance.
pub struct Zone {
    pub(crate) config: ZoneConfig,
    pub(crate) entities: BTreeMap<StableId, EntityRecord>,
    pub(crate) links: LinkGraph,
    pub(crate) nav: NavGrid,
    pub(crate) rules: Arc<dyn ItemRules + Send + Sync>,
    pub(crate) transfers: TransferLedger,
    pub(crate) action_results: BTreeMap<String, ActionResult>,
    pub(crate) action_order: VecDeque<String>,
    pub(crate) cooldowns: BTreeMap<ActionKey, Tick>,
    pub(crate) link_results: BTreeMap<String, Vec<ActionEffect>>,
    pub(crate) link_order: VecDeque<String>,
    pub(crate) link_cooldowns: BTreeMap<(StableId, StableId, LinkEvent), Tick>,
    pub(crate) tick: Tick,
    pub(crate) stats: TickStats,
    pub(crate) phase_timings: TickPhaseTimings,
    pub(crate) movement_pending: BTreeMap<StableId, MovementIntent>,
    pub(crate) movement_sequences: BTreeMap<StableId, MovementSequence>,
    pub(crate) movement_diagnostics: VecDeque<Diagnostic>,
    pub(crate) combat_audit: VecDeque<CombatAuditEvent>,
    pub(crate) respawned_this_tick: Vec<StableId>,
    pub(crate) movement_resets: BTreeSet<StableId>,
    pub(crate) path_budget: PathTickBudget,
    pub(crate) path_diagnostics: PathDiagnostics,
    pub(crate) dynamic_blockers: BTreeMap<(usize, usize), Tick>,
    pub(crate) ai_policy: Option<Arc<AiTickPolicy>>,
    pub(crate) ai_report: AiTickReport,
    pub(crate) ai_diagnostics: AiDiagnostics,
    pub(crate) ai_navigation: BTreeMap<StableId, AiNavigationState>,
}

impl fmt::Debug for Zone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Zone")
            .field("definition", &self.config.definition)
            .field("instance", &self.config.instance)
            .field("tick", &self.tick)
            .field("entities", &self.entities.len())
            .field("links", &self.links.len())
            .finish_non_exhaustive()
    }
}

impl Zone {
    /// Create an empty zone.
    ///
    /// # Errors
    ///
    /// Returns an error when the configuration is invalid or the navigation
    /// grid does not cover the zone bounds.
    pub fn new(
        config: ZoneConfig,
        nav: NavGrid,
        rules: Arc<dyn ItemRules + Send + Sync>,
    ) -> Result<Self, EngineError> {
        if config.max_entities == 0 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "zone maximum entities must be at least one",
            ));
        }
        config.combat.validate()?;
        let grid_max = nav.extent();
        if nav.origin().x > config.min.x
            || nav.origin().z > config.min.z
            || grid_max.x < config.max.x
            || grid_max.z < config.max.z
        {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "navigation grid must cover the zone bounds",
            )
            .with_detail("zone", config.definition.to_string()));
        }
        let path_budget = config.budget.path_tick_budget();
        Ok(Self {
            config,
            entities: BTreeMap::new(),
            links: LinkGraph::new(),
            nav,
            rules,
            transfers: TransferLedger::new(),
            action_results: BTreeMap::new(),
            action_order: VecDeque::new(),
            cooldowns: BTreeMap::new(),
            link_results: BTreeMap::new(),
            link_order: VecDeque::new(),
            link_cooldowns: BTreeMap::new(),
            tick: Tick::ZERO,
            stats: TickStats::default(),
            phase_timings: TickPhaseTimings::default(),
            movement_pending: BTreeMap::new(),
            movement_sequences: BTreeMap::new(),
            movement_diagnostics: VecDeque::new(),
            combat_audit: VecDeque::new(),
            respawned_this_tick: Vec::new(),
            movement_resets: BTreeSet::new(),
            path_budget,
            path_diagnostics: PathDiagnostics::default(),
            dynamic_blockers: BTreeMap::new(),
            ai_policy: None,
            ai_report: AiTickReport::default(),
            ai_diagnostics: AiDiagnostics::default(),
            ai_navigation: BTreeMap::new(),
        })
    }

    /// Static configuration.
    #[must_use]
    pub fn config(&self) -> &ZoneConfig {
        &self.config
    }

    /// Current tick.
    #[must_use]
    pub fn tick(&self) -> Tick {
        self.tick
    }

    /// Navigation grid.
    #[must_use]
    pub fn nav(&self) -> &NavGrid {
        &self.nav
    }

    /// Mutable navigation grid, for dynamic obstacles and authoring tools.
    pub fn nav_mut(&mut self) -> &mut NavGrid {
        &mut self.nav
    }

    /// Temporarily block one navigation cell until `expires_at`.
    ///
    /// The expiry tick is exclusive: the obstacle is active while
    /// `self.tick < expires_at` and is removed when that tick is reached.
    /// Re-adding a cell extends its lifetime but never shortens it. Static
    /// blocked cells cannot be registered as dynamic obstacles.
    ///
    /// Returns `false` for an out-of-bounds or statically blocked cell, or an
    /// expiry tick that is not in the future.
    pub fn add_dynamic_obstacle(&mut self, x: usize, y: usize, expires_at: Tick) -> bool {
        if !self.nav.in_bounds(x, y) || self.nav.is_blocked(x, y) || expires_at <= self.tick {
            return false;
        }
        self.dynamic_blockers
            .entry((x, y))
            .and_modify(|current| *current = (*current).max(expires_at))
            .or_insert(expires_at);
        true
    }

    /// Whether a cell currently has a temporary dynamic obstacle.
    #[must_use]
    pub fn has_dynamic_obstacle(&self, x: usize, y: usize) -> bool {
        self.dynamic_blockers.contains_key(&(x, y))
    }

    /// Find a path using the zone's cumulative per-tick expansion and request
    /// allowances. Callers should preserve `ReplanState` on failure and honor
    /// its backoff before requesting another path.
    pub fn find_path(
        &mut self,
        from: Vec3,
        to: Vec3,
        policy: PartialPathPolicy,
    ) -> Result<Path, PathError> {
        self.nav.find_path_in_tick_with_blockers(
            from,
            to,
            policy,
            &self.dynamic_blockers,
            &mut self.path_budget,
            &mut self.path_diagnostics,
        )
    }

    /// Cumulative navigation diagnostics since creation or the last take.
    #[must_use]
    pub const fn path_diagnostics(&self) -> PathDiagnostics {
        self.path_diagnostics
    }

    /// Take and reset cumulative navigation diagnostics.
    pub fn take_path_diagnostics(&mut self) -> PathDiagnostics {
        self.path_diagnostics.take()
    }

    /// Remaining pathfinding work for the current tick.
    #[must_use]
    pub const fn path_budget_remaining(&self) -> (usize, usize) {
        (
            self.path_budget.remaining_expansions(),
            self.path_budget.remaining_requests(),
        )
    }

    /// All entities in stable-ID order.
    pub fn entities(&self) -> impl Iterator<Item = &EntityRecord> {
        self.entities.values()
    }

    /// Number of live entities.
    #[must_use]
    pub fn entity_count(&self) -> usize {
        self.entities.len()
    }

    /// Look up an entity.
    #[must_use]
    pub fn entity(&self, id: StableId) -> Option<&EntityRecord> {
        self.entities.get(&id)
    }

    /// Mutably look up an entity.
    pub fn entity_mut(&mut self, id: StableId) -> Option<&mut EntityRecord> {
        self.entities.get_mut(&id)
    }

    /// The authored link graph.
    #[must_use]
    pub fn links(&self) -> &LinkGraph {
        &self.links
    }

    /// Spawn an entity.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] for a non-entity ID,
    /// [`ErrorCode::OutOfRange`] for a position outside the zone,
    /// [`ErrorCode::Conflict`] for a duplicate ID, and
    /// [`ErrorCode::CapacityExceeded`] when the zone is full.
    pub fn spawn(&mut self, mut entity: EntityRecord) -> Result<StableId, EngineError> {
        entity.id.expect_kind(IdKind::Entity)?;
        if entity.stun_until.is_some_and(|until| until <= self.tick) {
            entity.stun_until = None;
        }
        if let Some(slow) = entity.slow {
            if slow.until <= self.tick {
                entity.slow = None;
            } else {
                slow.validate()?;
            }
        }
        if entity.damage_over_time.len() > TIMED_COMBAT_EFFECT_LIMIT
            || entity.shields.len() > TIMED_COMBAT_EFFECT_LIMIT
        {
            return Err(EngineError::new(
                ErrorCode::CapacityExceeded,
                "entity timed combat effect limit exceeded",
            ));
        }
        for effect in &entity.damage_over_time {
            effect.source.expect_kind(IdKind::Entity)?;
            if effect.damage_per_tick <= 0
                || effect.interval_ticks == 0
                || effect.until <= self.tick
            {
                return Err(EngineError::new(
                    ErrorCode::InvalidRequest,
                    "invalid or expired periodic damage effect",
                ));
            }
        }
        for shield in &entity.shields {
            shield.source.expect_kind(IdKind::Entity)?;
            if shield.remaining <= 0 || shield.until <= self.tick {
                return Err(EngineError::new(
                    ErrorCode::InvalidRequest,
                    "invalid or expired shield",
                ));
            }
        }
        if let Some(movement) = entity.movement {
            movement.validate()?;
        }
        if let Some(policy) = entity.respawn_policy {
            let navigable_safe_point = self.nav.cell_of(policy.position).is_some_and(|cell| {
                !self.nav.is_blocked(cell.0, cell.1) && !self.dynamic_blockers.contains_key(&cell)
            });
            if entity.health.is_none()
                || policy.delay_ticks == 0
                || !policy.position.x.is_finite()
                || !policy.position.y.is_finite()
                || !policy.position.z.is_finite()
                || !self.config.contains(policy.position)
                || !navigable_safe_point
            {
                return Err(EngineError::new(
                    ErrorCode::InvalidRequest,
                    "respawn policy requires health, positive delay, and an in-zone unblocked navigation cell",
                ));
            }
        }
        if entity.respawn_at.is_some_and(|_| {
            entity.health.is_none_or(super::entity::Health::is_alive)
                || entity.respawn_policy.is_none()
        }) {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "respawn schedule requires a dead entity with a respawn policy",
            ));
        }
        if self.entities.contains_key(&entity.id) {
            return Err(
                EngineError::new(ErrorCode::Conflict, "entity id is already in this zone")
                    .with_detail("entity", entity.id.to_string()),
            );
        }
        if !self.config.contains(entity.position()) {
            return Err(EngineError::new(
                ErrorCode::OutOfRange,
                "entity position is outside the zone bounds",
            )
            .with_detail("entity", entity.id.to_string())
            .with_detail("zone", self.config.definition.to_string()));
        }
        if self.entities.len() >= self.config.max_entities {
            return Err(
                EngineError::new(ErrorCode::CapacityExceeded, "zone entity limit reached")
                    .with_detail("max_entities", self.config.max_entities.to_string()),
            );
        }
        let id = entity.id;
        self.entities.insert(id, entity);
        Ok(id)
    }

    /// Remove an entity and every link touching it.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::NotFound`] when the entity is absent.
    pub fn despawn(&mut self, id: StableId) -> Result<EntityRecord, EngineError> {
        let removed = self
            .entities
            .remove(&id)
            .ok_or_else(|| EngineError::not_found("entity", id))?;
        self.links.remove_for(id);
        self.movement_pending.remove(&id);
        self.movement_sequences.remove(&id);
        self.ai_navigation.remove(&id);
        Ok(removed)
    }

    /// Move one persistent, living entity into another authoritative zone.
    ///
    /// Route authorization and a durable handoff receipt belong to the world
    /// coordinator. This method handles the simulation-state transaction: it
    /// validates the destination cell, rebases timed state onto the target
    /// zone's tick, preserves compatible actor cooldowns, and rolls back the
    /// destination spawn if source removal fails. Entities with zone-local links
    /// or cooldown targets absent from the destination are refused rather than
    /// silently losing behavior.
    ///
    /// # Errors
    /// Returns an error for identical zones, missing/non-persistent/dead
    /// entities, zone-local links, invalid or occupied arrivals, incompatible
    /// cooldown targets, capacity failure, or a tick-rebase overflow.
    #[allow(clippy::too_many_lines)] // Keep the handoff validation and rollback transaction together.
    pub fn transfer_entity_to(
        &mut self,
        destination: &mut Zone,
        id: StableId,
        position: Vec3,
    ) -> Result<(), EngineError> {
        if self.config.instance == destination.config.instance {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "a zone handoff requires distinct zone instances",
            ));
        }
        let entity = self
            .entities
            .get(&id)
            .ok_or_else(|| EngineError::not_found("handoff entity", id))?;
        if entity.persistence != engine_core::schema::PersistencePolicy::PersistentEntity
            || entity.health.is_some_and(|health| !health.is_alive())
        {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "only living persistent entities may be handed off",
            ));
        }
        if self
            .links
            .links()
            .iter()
            .any(|link| link.source == id || link.target == id)
        {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "an entity with zone-local links cannot be handed off",
            ));
        }
        let Some(cell) = destination.nav.cell_of(position) else {
            return Err(EngineError::new(
                ErrorCode::OutOfRange,
                "handoff arrival is outside destination navigation",
            ));
        };
        if !position.x.is_finite()
            || !position.y.is_finite()
            || !position.z.is_finite()
            || !destination.config.contains(position)
            || destination.nav.is_blocked(cell.0, cell.1)
            || destination.has_dynamic_obstacle(cell.0, cell.1)
            || destination
                .entities
                .values()
                .any(|other| destination.nav.cell_of(other.position()) == Some(cell))
        {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "handoff arrival is blocked or occupied",
            ));
        }

        let source_tick = self.tick;
        let destination_tick = destination.tick;
        let mut transferred = entity.clone();
        transferred.transform.position = position;
        if let Some(mind) = transferred.mind.as_mut() {
            mind.controller = Controller::None;
        }
        if let Some(policy) = transferred.respawn_policy.as_mut() {
            policy.position = position;
        }
        transferred.stun_until =
            rebase_optional_tick(transferred.stun_until, source_tick, destination_tick)?;
        if let Some(mut slow) = transferred.slow {
            slow.until = rebase_tick(slow.until, source_tick, destination_tick)?;
            transferred.slow = (slow.until > destination_tick).then_some(slow);
        }
        transferred
            .damage_over_time
            .retain(|effect| effect.until > source_tick);
        for effect in &mut transferred.damage_over_time {
            effect.next_tick = rebase_tick(effect.next_tick, source_tick, destination_tick)?;
            effect.until = rebase_tick(effect.until, source_tick, destination_tick)?;
        }
        transferred
            .shields
            .retain(|shield| shield.until > source_tick);
        for shield in &mut transferred.shields {
            shield.until = rebase_tick(shield.until, source_tick, destination_tick)?;
        }

        let action_cooldowns = self
            .cooldowns
            .iter()
            .filter(|((actor, _, _), until)| *actor == id && **until > source_tick)
            .map(|(key, until)| {
                if key.2.is_some_and(|target| destination.entity(target).is_none()) {
                    return Err(EngineError::new(
                        ErrorCode::InvalidState,
                        "handoff cannot preserve a cooldown for an entity outside the destination zone",
                    ));
                }
                Ok((key.clone(), rebase_tick(*until, source_tick, destination_tick)?))
            })
            .collect::<Result<Vec<_>, EngineError>>()?;

        destination.spawn(transferred)?;
        if let Err(error) = self.despawn(id) {
            let _ = destination.despawn(id);
            return Err(error);
        }
        for (key, until) in action_cooldowns {
            self.cooldowns.remove(&key);
            destination
                .cooldowns
                .entry(key)
                .and_modify(|current| *current = (*current).max(until))
                .or_insert(until);
        }
        destination.movement_resets.insert(id);
        Ok(())
    }

    /// Add a validated link.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::NotFound`] when either endpoint is missing, or the
    /// graph's structural errors.
    pub fn add_link(&mut self, link: EntityLink) -> Result<(), EngineError> {
        if !self.entities.contains_key(&link.source) {
            return Err(EngineError::not_found("link source", link.source));
        }
        if !self.entities.contains_key(&link.target) {
            return Err(EngineError::not_found("link target", link.target));
        }
        self.links.add(link)
    }

    /// Whether two positions have line of sight under the nav grid.
    #[must_use]
    pub fn line_of_sight(&self, from: Vec3, to: Vec3) -> bool {
        self.nav.line_of_sight(from, to)
    }

    /// Apply a server-owned timed stun. Reapplication keeps the later expiry;
    /// durations never add and client action payloads cannot call this method.
    ///
    /// # Errors
    /// Rejects zero/overflowing durations and missing or dead targets.
    pub fn apply_stun(&mut self, target: StableId, duration_ticks: u64) -> Result<(), EngineError> {
        let until = self
            .tick
            .0
            .checked_add(duration_ticks)
            .filter(|_| duration_ticks > 0)
            .ok_or_else(|| EngineError::new(ErrorCode::InvalidRequest, "invalid stun duration"))?;
        let entity = self
            .entities
            .get_mut(&target)
            .ok_or_else(|| EngineError::not_found("stun target", target))?;
        if !entity.is_alive() {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "cannot stun a dead entity",
            ));
        }
        entity.stun_until = Some(entity.stun_until.unwrap_or(Tick(until)).max(Tick(until)));
        Ok(())
    }

    /// Cleanse only the timed stun; permanent locomotion restrictions remain.
    ///
    /// # Errors
    /// Returns an error if the target does not exist.
    pub fn cleanse_stun(&mut self, target: StableId) -> Result<(), EngineError> {
        let entity = self
            .entities
            .get_mut(&target)
            .ok_or_else(|| EngineError::not_found("stun target", target))?;
        entity.stun_until = None;
        Ok(())
    }

    /// Apply a timed slow. Reapplication uses the strongest multiplier and
    /// latest expiry; these dimensions do not compound.
    ///
    /// # Errors
    /// Rejects zero/overflowing durations, invalid multipliers, and missing or
    /// dead targets.
    pub fn apply_slow(
        &mut self,
        target: StableId,
        duration_ticks: u64,
        multiplier: f32,
    ) -> Result<(), EngineError> {
        let until = self
            .tick
            .0
            .checked_add(duration_ticks)
            .filter(|_| duration_ticks > 0)
            .ok_or_else(|| EngineError::new(ErrorCode::InvalidRequest, "invalid slow duration"))?;
        let incoming = TimedSlow {
            until: Tick(until),
            multiplier,
        };
        incoming.validate()?;
        let entity = self
            .entities
            .get_mut(&target)
            .ok_or_else(|| EngineError::not_found("slow target", target))?;
        if !entity.is_alive() {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "cannot slow a dead entity",
            ));
        }
        entity.slow = Some(entity.slow.map_or(incoming, |active| TimedSlow {
            until: active.until.max(incoming.until),
            multiplier: active.multiplier.min(incoming.multiplier),
        }));
        Ok(())
    }

    /// Remove the timed slow without changing the base movement profile.
    ///
    /// # Errors
    /// Returns an error if the target does not exist.
    pub fn cleanse_slow(&mut self, target: StableId) -> Result<(), EngineError> {
        let entity = self
            .entities
            .get_mut(&target)
            .ok_or_else(|| EngineError::not_found("slow target", target))?;
        entity.slow = None;
        Ok(())
    }

    /// Apply a bounded, server-owned periodic damage effect.
    /// Reapplication from the same source and school refreshes the expiry and
    /// keeps the stronger potency without resetting an already-running cadence.
    pub fn apply_damage_over_time(
        &mut self,
        target: StableId,
        source: StableId,
        duration_ticks: u64,
        interval_ticks: u64,
        damage_per_tick: i32,
        damage_type: DamageType,
    ) -> Result<(), EngineError> {
        source.expect_kind(IdKind::Entity)?;
        if !self.entities.contains_key(&source) {
            return Err(EngineError::not_found("damage-over-time source", source));
        }
        if duration_ticks == 0 || interval_ticks == 0 || damage_per_tick <= 0 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "periodic damage requires positive duration, interval, and damage",
            ));
        }
        let until = self.tick.0.checked_add(duration_ticks).ok_or_else(|| {
            EngineError::new(ErrorCode::InvalidRequest, "invalid effect duration")
        })?;
        let next_tick = self.tick.0.checked_add(interval_ticks).ok_or_else(|| {
            EngineError::new(ErrorCode::InvalidRequest, "invalid effect interval")
        })?;
        let entity = self
            .entities
            .get_mut(&target)
            .ok_or_else(|| EngineError::not_found("damage-over-time target", target))?;
        if !entity.is_alive() {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "cannot apply periodic damage to a dead entity",
            ));
        }
        if let Some(active) = entity
            .damage_over_time
            .iter_mut()
            .find(|active| active.source == source && active.damage_type == damage_type)
        {
            active.until = active.until.max(Tick(until));
            active.damage_per_tick = active.damage_per_tick.max(damage_per_tick);
            active.interval_ticks = active.interval_ticks.min(interval_ticks);
            return Ok(());
        }
        if entity.damage_over_time.len() >= TIMED_COMBAT_EFFECT_LIMIT {
            return Err(EngineError::new(
                ErrorCode::CapacityExceeded,
                "target has reached its periodic damage effect limit",
            ));
        }
        entity.damage_over_time.push(TimedDamageOverTime {
            source,
            damage_type,
            damage_per_tick,
            interval_ticks,
            next_tick: Tick(next_tick),
            until: Tick(until),
        });
        entity
            .damage_over_time
            .sort_by_key(|effect| (effect.source, effect.damage_type));
        Ok(())
    }

    /// Apply a finite shield; independent sources stack up to the per-entity cap.
    /// Reapplication from the same source refreshes expiry and keeps the larger
    /// of its current capacity and incoming capacity without adding them.
    pub fn apply_shield(
        &mut self,
        target: StableId,
        source: StableId,
        duration_ticks: u64,
        capacity: i32,
    ) -> Result<(), EngineError> {
        source.expect_kind(IdKind::Entity)?;
        if !self.entities.contains_key(&source) {
            return Err(EngineError::not_found("shield source", source));
        }
        if duration_ticks == 0 || capacity <= 0 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "shield requires positive duration and capacity",
            ));
        }
        let until = self.tick.0.checked_add(duration_ticks).ok_or_else(|| {
            EngineError::new(ErrorCode::InvalidRequest, "invalid shield duration")
        })?;
        let entity = self
            .entities
            .get_mut(&target)
            .ok_or_else(|| EngineError::not_found("shield target", target))?;
        if !entity.is_alive() {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "cannot shield a dead entity",
            ));
        }
        if let Some(active) = entity
            .shields
            .iter_mut()
            .find(|active| active.source == source)
        {
            active.remaining = active.remaining.max(capacity);
            active.until = active.until.max(Tick(until));
            return Ok(());
        }
        if entity.shields.len() >= TIMED_COMBAT_EFFECT_LIMIT {
            return Err(EngineError::new(
                ErrorCode::CapacityExceeded,
                "target has reached its shield limit",
            ));
        }
        entity.shields.push(TimedShield {
            source,
            remaining: capacity,
            until: Tick(until),
        });
        Ok(())
    }

    /// Clear periodic damage effects without changing other statuses.
    pub fn cleanse_damage_over_time(&mut self, target: StableId) -> Result<(), EngineError> {
        let entity = self
            .entities
            .get_mut(&target)
            .ok_or_else(|| EngineError::not_found("cleanse target", target))?;
        entity.damage_over_time.clear();
        Ok(())
    }

    /// Read the bounded, private combat audit ring. It is never replicated.
    #[must_use]
    pub fn combat_audit(&self) -> &VecDeque<CombatAuditEvent> {
        &self.combat_audit
    }

    /// Take entity IDs respawned by the last tick, for gateway control rebinding
    /// and client reconciliation resets.
    pub fn take_respawned_entities(&mut self) -> Vec<StableId> {
        std::mem::take(&mut self.respawned_this_tick)
    }

    /// Take pending authoritative movement discontinuities for private delivery.
    pub fn take_movement_resets(&mut self) -> BTreeSet<StableId> {
        std::mem::take(&mut self.movement_resets)
    }

    /// Advance the tick and refresh statistics. Timed effects expire before AI
    /// or queued movement can act on the new tick.
    pub fn advance_tick(&mut self) -> TickStats {
        self.phase_timings = TickPhaseTimings::default();
        self.tick = self.tick.next();
        self.respawned_this_tick.clear();
        let effects_started = Instant::now();
        let mut due_damage = Vec::new();
        for entity in self.entities.values_mut() {
            if entity.stun_until.is_some_and(|until| until <= self.tick) {
                entity.stun_until = None;
            }
            if entity.slow.is_some_and(|slow| slow.until <= self.tick) {
                entity.slow = None;
            }
            entity
                .damage_over_time
                .retain(|effect| effect.until > self.tick);
            for effect in &mut entity.damage_over_time {
                if effect.next_tick <= self.tick {
                    due_damage.push((
                        effect.source,
                        entity.id,
                        effect.damage_per_tick,
                        effect.damage_type,
                    ));
                    effect.next_tick = Tick(self.tick.0.saturating_add(effect.interval_ticks));
                }
            }
            entity
                .shields
                .retain(|shield| shield.until > self.tick && shield.remaining > 0);
        }
        for (source, target, damage, damage_type) in due_damage {
            let _ = self.apply_combat_damage(source, target, damage, damage_type);
        }
        for entity in self.entities.values_mut() {
            if entity.respawn_at.is_none_or(|at| at > self.tick) {
                continue;
            }
            let Some(policy) = entity.respawn_policy else {
                continue;
            };
            let safe_cell = self.nav.cell_of(policy.position);
            if safe_cell.is_none_or(|cell| {
                self.nav.is_blocked(cell.0, cell.1) || self.dynamic_blockers.contains_key(&cell)
            }) {
                // Temporary blockers do not cancel death or relocate the
                // character to a client-selected point; retry the authored
                // safe point on the next authoritative tick.
                continue;
            }
            if let Some(health) = entity.health.as_mut() {
                health.current = health.max;
            }
            entity.transform.position = policy.position;
            entity.respawn_at = None;
            entity.stun_until = None;
            entity.slow = None;
            entity.damage_over_time.clear();
            entity.shields.clear();
            self.respawned_this_tick.push(entity.id);
            self.movement_resets.insert(entity.id);
        }
        self.phase_timings.effects_ns = effects_started.elapsed().as_nanos();
        let navigation_started = Instant::now();
        let blocker_count = self.dynamic_blockers.len();
        self.dynamic_blockers
            .retain(|_, expires_at| *expires_at > self.tick);
        if self.dynamic_blockers.len() != blocker_count {
            for state in self.ai_navigation.values_mut() {
                *state = AiNavigationState::default();
            }
        }
        self.path_budget.reset();
        self.phase_timings.navigation_ns = navigation_started.elapsed().as_nanos();
        let ai_started = Instant::now();
        self.run_ai_tick();
        self.phase_timings.ai_ns = ai_started.elapsed().as_nanos();
        let statistics_started = Instant::now();
        self.stats.tick = self.tick;
        self.stats.entity_count = self.entities.len();
        self.stats.controlled_entities = self
            .entities
            .values()
            .filter(|entity| entity.is_controlled())
            .count();
        self.phase_timings.statistics_ns = statistics_started.elapsed().as_nanos();
        self.stats.clone()
    }

    /// Current statistics.
    #[must_use]
    pub fn stats(&self) -> &TickStats {
        &self.stats
    }

    /// Timings of the last tick's phases, kept outside deterministic state.
    #[must_use]
    pub fn tick_phase_timings(&self) -> TickPhaseTimings {
        self.phase_timings
    }

    /// Validate structural invariants and report non-fatal problems.
    #[must_use]
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        let mut diagnostics: Vec<Diagnostic> = self
            .links
            .diagnose(|id| self.entities.contains_key(&id))
            .into_iter()
            .map(|diagnostic| Diagnostic {
                code: diagnostic.code,
                entity: diagnostic.target,
                message: diagnostic.message,
            })
            .collect();
        diagnostics.extend(self.movement_diagnostics.iter().cloned());

        for entity in self.entities.values() {
            if !self.config.contains(entity.position()) {
                diagnostics.push(Diagnostic {
                    code: "entity.out_of_bounds".into(),
                    entity: Some(entity.id),
                    message: "entity is outside the zone bounds".into(),
                });
            }
            if !entity.is_alive() && entity.is_controlled() {
                diagnostics.push(Diagnostic {
                    code: "entity.dead_but_controlled".into(),
                    entity: Some(entity.id),
                    message: "a dead entity still has an active controller".into(),
                });
            }
            for key in entity.components.keys() {
                if entity
                    .components
                    .get(key)
                    .is_some_and(|component| component.version.major == 0)
                {
                    diagnostics.push(Diagnostic {
                        code: "component.unversioned".into(),
                        entity: Some(entity.id),
                        message: format!("component {key} has major version zero"),
                    });
                }
            }
        }
        diagnostics
    }

    /// Activate every enabled link from `source` carrying `event`.
    ///
    /// Repeating the same request ID returns the original effects without
    /// applying them twice. Missing targets and locked doors are skipped and
    /// remain visible through [`Zone::diagnostics`].
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] for an empty request ID,
    /// [`ErrorCode::NotFound`] when the source is missing,
    /// [`ErrorCode::InvalidState`] when the source is dead, and
    /// [`ErrorCode::CapacityExceeded`] when one activation fans out too far.
    pub fn activate_links(
        &mut self,
        request_id: &str,
        source: StableId,
        event: LinkEvent,
    ) -> Result<Vec<ActionEffect>, EngineError> {
        if request_id.is_empty() {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "link activation requires a request id",
            ));
        }
        if let Some(cached) = self.link_results.get(request_id) {
            return Ok(cached.clone());
        }
        let source_entity = self
            .entities
            .get(&source)
            .ok_or_else(|| EngineError::not_found("link source", source))?;
        if !source_entity.is_alive() {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "a dead entity cannot activate links",
            )
            .with_detail("entity", source.to_string()));
        }
        let source_entity_id = source_entity.id;

        let candidates: Vec<EntityLink> = self
            .links
            .links()
            .iter()
            .filter(|link| {
                link.source == source_entity_id && link.carries(event) && !link.cross_zone
            })
            .cloned()
            .collect();
        if candidates.len() > MAX_LINK_TARGETS {
            return Err(EngineError::new(
                ErrorCode::CapacityExceeded,
                "link activation fan-out exceeds the per-request limit",
            )
            .with_detail("targets", candidates.len().to_string())
            .with_detail("max", MAX_LINK_TARGETS.to_string()));
        }

        let mut effects = Vec::new();
        let mut activated_targets = BTreeSet::new();
        for link in candidates {
            if !activated_targets.insert(link.target) {
                continue;
            }
            let key = (link.source, link.target, event);
            if self
                .link_cooldowns
                .get(&key)
                .is_some_and(|until| self.tick < *until)
            {
                continue;
            }
            if !self.entities.contains_key(&link.target) {
                continue;
            }

            let mut applied = true;
            if let Some(target) = self.entities.get_mut(&link.target)
                && let Some(door) = target.door.as_mut()
            {
                if door.locked {
                    applied = false;
                } else {
                    door.open = !door.open;
                    effects.push(ActionEffect::DoorToggled {
                        target: link.target,
                        open: door.open,
                    });
                }
            }
            if applied {
                effects.push(ActionEffect::Activated {
                    source: link.source,
                    target: link.target,
                    link_type: link.link_type.clone(),
                });
                if link.cooldown_ticks > 0 {
                    self.link_cooldowns
                        .insert(key, Tick(self.tick.0.saturating_add(link.cooldown_ticks)));
                }
                self.stats.link_activations += 1;
            }
        }

        self.link_results
            .insert(request_id.to_owned(), effects.clone());
        self.link_order.push_back(request_id.to_owned());
        while self.link_order.len() > LINK_CACHE_LIMIT {
            if let Some(oldest) = self.link_order.pop_front() {
                self.link_results.remove(&oldest);
            }
        }
        Ok(effects)
    }
}

fn rebase_tick(value: Tick, source: Tick, destination: Tick) -> Result<Tick, EngineError> {
    let remaining = value.0.saturating_sub(source.0);
    destination
        .0
        .checked_add(remaining)
        .map(Tick)
        .ok_or_else(|| EngineError::new(ErrorCode::InvalidRequest, "handoff tick overflow"))
}

fn rebase_optional_tick(
    value: Option<Tick>,
    source: Tick,
    destination: Tick,
) -> Result<Option<Tick>, EngineError> {
    value
        .filter(|until| *until > source)
        .map(|until| rebase_tick(until, source, destination))
        .transpose()
}
