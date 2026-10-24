//! The authoritative zone: entity store, links, navigation, and budgets.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::sync::Arc;

use engine_core::error::{EngineError, ErrorCode};
use engine_core::id::{DefinitionId, IdKind, StableId};
use engine_core::inventory::{ItemRules, TransferLedger};
use engine_core::math::Vec3;
use engine_core::time::Tick;
use serde::{Deserialize, Serialize};

use crate::action::{ActionEffect, ActionKey, ActionResult};
use crate::entity::EntityRecord;
use crate::link::{EntityLink, LinkEvent, LinkGraph};
use crate::nav::{NavGrid, PathBudget};

/// Maximum remembered action results for idempotent retries.
pub const ACTION_CACHE_LIMIT: usize = 2_048;
/// Maximum remembered link-activation results.
pub const LINK_CACHE_LIMIT: usize = 1_024;
/// Maximum targets one activation may fan out to.
pub const MAX_LINK_TARGETS: usize = 32;

/// Work limits for one simulation tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TickBudget {
    /// Maximum actions processed in one tick.
    pub max_actions: usize,
    /// Maximum AI controller updates in one tick.
    pub max_ai_updates: usize,
    /// Maximum pathfinding expansions across the tick.
    pub max_path_expansions: usize,
}

impl Default for TickBudget {
    fn default() -> Self {
        Self {
            max_actions: 256,
            max_ai_updates: 128,
            max_path_expansions: 16_384,
        }
    }
}

impl TickBudget {
    /// Path budget derived from this tick budget.
    #[must_use]
    pub const fn path_budget(self) -> PathBudget {
        PathBudget {
            max_expansions: self.max_path_expansions,
            max_waypoints: 512,
        }
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
    pub fn spawn(&mut self, entity: EntityRecord) -> Result<StableId, EngineError> {
        entity.id.expect_kind(IdKind::Entity)?;
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
        Ok(removed)
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

    /// Advance the tick and refresh statistics.
    pub fn advance_tick(&mut self) -> TickStats {
        self.tick = self.tick.next();
        self.stats.tick = self.tick;
        self.stats.entity_count = self.entities.len();
        self.stats.controlled_entities = self
            .entities
            .values()
            .filter(|entity| entity.is_controlled())
            .count();
        self.stats.clone()
    }

    /// Current statistics.
    #[must_use]
    pub fn stats(&self) -> &TickStats {
        &self.stats
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
