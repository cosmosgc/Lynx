//! Faction-aware perception and bounded ally assistance.

use engine_core::faction::{
    Alignment, AlignmentClass, FactionCatalog, Thresholds, resolve_alignment,
};
use engine_core::id::{DefinitionId, StableId};
use engine_core::math::Vec3;
use engine_core::time::{Millis, Tick};
use serde::{Deserialize, Serialize};

use crate::action::{ActionRequest, ActionResult, Verb};
use crate::entity::EntityRecord;
use crate::mind::Controller;
use crate::zone::Zone;

/// Server-owned, opt-in policy for the built-in guard behavior.
/// Simulation time is `epoch + tick * tick_ms`, not wall-clock time.
#[derive(Debug, Clone)]
pub struct AiTickPolicy {
    /// Directional faction alignments used for perception.
    pub factions: FactionCatalog,
    /// Alignment classification thresholds.
    pub thresholds: Thresholds,
    /// Guard perception range in meters.
    pub range_m: f32,
    /// Per-guard perception limits.
    pub perception: PerceptionBudget,
    /// Maximum entity records scanned for controllers each tick.
    pub max_controller_candidates: usize,
    /// Simulation epoch for reputation expiry.
    pub epoch: Millis,
    /// Milliseconds per simulation tick.
    pub tick_ms: u32,
    /// Declarative behavior bindings. Content may assign stable behavior IDs
    /// to these bounded built-in semantics without changing the engine.
    pub behaviors: Vec<AiBehaviorDefinition>,
}

/// A server-approved behavior definition that can be populated from content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiBehaviorDefinition {
    pub id: DefinitionId,
    pub kind: AiBehaviorKind,
    /// Perception range for this behavior, in meters.
    pub range_m: f32,
}

/// Bounded behavior primitives supported by the runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiBehaviorKind {
    Guard,
}

/// Last tick's bounded AI decisions, retained for server inspection.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AiTickReport {
    /// Tick on which these decisions ran.
    pub tick: Tick,
    /// Entity records scanned for controllers.
    pub controller_candidates: usize,
    /// Controllers evaluated in entity-ID order.
    pub decisions: Vec<AiDecision>,
    /// Number of action attempts, including rejections.
    pub action_attempts: usize,
}

/// Fixed-size cumulative diagnostics for tick-driven AI. All counters saturate.
/// Controller limit counters mean the limit was reached with unscanned entity
/// records remaining; they do not assert that another AI controller was skipped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AiDiagnostics {
    /// Enabled AI ticks.
    pub ticks: u64,
    /// Controller decisions evaluated.
    pub decisions: u64,
    /// Ticks ending at the controller scan limit with records remaining.
    pub controller_scan_limit_reached: u64,
    /// Ticks ending at the AI update limit with records remaining.
    pub update_limit_reached: u64,
    /// Decisions whose perception reached its candidate limit.
    pub perception_candidate_limit_reached: u64,
    /// Decisions whose perception ran out of line-of-sight checks.
    pub perception_los_limit_reached: u64,
    /// Decisions with a target but no remaining action budget.
    pub action_budget_exhaustions: u64,
}

/// A guard decision or an explicit reason for not acting.
#[derive(Debug, Clone, PartialEq)]
pub struct AiDecision {
    /// Acting entity.
    pub actor: StableId,
    /// Behavior identifier on the mind.
    pub behavior: DefinitionId,
    /// Perception report when the behavior was recognized.
    pub perception: Option<PerceptionReport>,
    /// Selected target, if one was perceived.
    pub target: Option<StableId>,
    /// Action pipeline result, if dispatched.
    pub action: Option<ActionResult>,
    /// Stable decision diagnostic.
    pub reason: &'static str,
}

impl Zone {
    /// Enable or disable tick-driven AI. This is server configuration, not client input.
    pub fn set_ai_tick_policy(&mut self, policy: Option<AiTickPolicy>) {
        self.ai_policy = policy.map(std::sync::Arc::new);
    }

    /// Inspect the most recent AI tick.
    #[must_use]
    pub fn ai_tick_report(&self) -> &AiTickReport {
        &self.ai_report
    }

    /// Cumulative tick-driven AI diagnostics since creation or the last take.
    #[must_use]
    pub fn ai_diagnostics(&self) -> AiDiagnostics {
        self.ai_diagnostics
    }

    /// Return and reset cumulative AI diagnostics without affecting simulation.
    pub fn take_ai_diagnostics(&mut self) -> AiDiagnostics {
        std::mem::take(&mut self.ai_diagnostics)
    }

    #[allow(clippy::too_many_lines)] // Keep decision and accounting order together.
    pub(crate) fn run_ai_tick(&mut self) {
        let mut report = AiTickReport {
            tick: self.tick,
            ..AiTickReport::default()
        };
        let Some(policy) = self.ai_policy.clone() else {
            self.ai_report = report;
            return;
        };
        self.ai_diagnostics.ticks = self.ai_diagnostics.ticks.saturating_add(1);
        // Bound selection work independently of the number of non-AI entities.
        let mut controllers = Vec::new();
        for entity in self
            .entities
            .values()
            .take(if self.config.budget.max_ai_updates == 0 {
                0
            } else {
                policy.max_controller_candidates
            })
        {
            report.controller_candidates += 1;
            if let Some(crate::mind::Mind {
                controller: Controller::Ai { behavior },
                ..
            }) = &entity.mind
            {
                controllers.push((entity.id, behavior.clone()));
                if controllers.len() == self.config.budget.max_ai_updates {
                    break;
                }
            }
        }
        if report.controller_candidates < self.entities.len() {
            if controllers.len() == self.config.budget.max_ai_updates {
                self.ai_diagnostics.update_limit_reached =
                    self.ai_diagnostics.update_limit_reached.saturating_add(1);
            } else if report.controller_candidates == policy.max_controller_candidates {
                self.ai_diagnostics.controller_scan_limit_reached = self
                    .ai_diagnostics
                    .controller_scan_limit_reached
                    .saturating_add(1);
            }
        }
        let elapsed = self.tick.0.saturating_mul(u64::from(policy.tick_ms));
        let now = Millis::new(
            policy
                .epoch
                .0
                .saturating_add(i64::try_from(elapsed).unwrap_or(i64::MAX)),
        );
        for (actor, behavior) in controllers {
            let mut decision = AiDecision {
                actor,
                behavior,
                perception: None,
                target: None,
                action: None,
                reason: "unknown_behavior",
            };
            // The built-in behavior is deliberately explicit: unknown content does not attack.
            if let Some(definition) = policy
                .behaviors
                .iter()
                .find(|definition| definition.id == decision.behavior)
                .filter(|definition| definition.kind == AiBehaviorKind::Guard)
            {
                let perception = self
                    .perceive_hostiles_bounded(
                        actor,
                        &policy.factions,
                        policy.thresholds,
                        definition.range_m,
                        now,
                        policy.perception,
                    )
                    .expect("selected actor exists");
                decision.target = perception.targets.first().map(|target| target.entity);
                match perception.status {
                    PerceptionStatus::CandidateBudgetReached => {
                        self.ai_diagnostics.perception_candidate_limit_reached = self
                            .ai_diagnostics
                            .perception_candidate_limit_reached
                            .saturating_add(1);
                    }
                    PerceptionStatus::LineOfSightBudgetReached => {
                        self.ai_diagnostics.perception_los_limit_reached = self
                            .ai_diagnostics
                            .perception_los_limit_reached
                            .saturating_add(1);
                    }
                    _ => {}
                }
                decision.reason = match perception.status {
                    PerceptionStatus::IneligibleObserver => "ineligible_observer",
                    PerceptionStatus::InvalidGeometry => "invalid_geometry",
                    _ if decision.target.is_none() => "no_target",
                    _ => "action_budget_reached",
                };
                decision.perception = Some(perception);
                if let Some(target) = decision.target
                    && report.action_attempts < self.config.budget.max_actions
                {
                    let request_id =
                        format!("ai:{}:{}:{}", self.config.instance, self.tick.0, actor);
                    // Never let a cached client request be mistaken for this decision.
                    if self.action_results.contains_key(&request_id) {
                        decision.reason = "request_id_conflict";
                    } else {
                        let request = ActionRequest::new(request_id, actor, Verb::Attack)
                            .on(target)
                            .at_tick(self.tick);
                        report.action_attempts += 1;
                        let result = self.apply_action(&request, now);
                        decision.reason = if result.is_applied() {
                            "applied"
                        } else {
                            "action_rejected"
                        };
                        decision.action = Some(result);
                    }
                }
            }
            self.ai_diagnostics.decisions = self.ai_diagnostics.decisions.saturating_add(1);
            if decision.reason == "action_budget_reached" {
                self.ai_diagnostics.action_budget_exhaustions = self
                    .ai_diagnostics
                    .action_budget_exhaustions
                    .saturating_add(1);
            }
            report.decisions.push(decision);
        }
        self.ai_report = report;
    }
}

/// A bounded request for nearby allies to assist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelpRequest {
    /// Entity asking for help.
    pub requester: StableId,
    /// Requester position, used for range checks.
    pub position: Vec3,
    /// Requester faction.
    pub faction: DefinitionId,
    /// Maximum horizontal range in meters.
    pub range_m: f32,
    /// Maximum number of responders.
    pub max_responders: usize,
    /// Current time for reputation expiry.
    pub now: Millis,
}

impl HelpRequest {
    /// Create a request with a default limit of three responders.
    #[must_use]
    pub fn new(
        requester: StableId,
        position: Vec3,
        faction: DefinitionId,
        range_m: f32,
        now: Millis,
    ) -> Self {
        Self {
            requester,
            position,
            faction,
            range_m,
            max_responders: 3,
            now,
        }
    }
}

/// Why a candidate did not respond.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelpSkipped {
    /// Candidate entity.
    pub entity: StableId,
    /// Stable reason code.
    pub reason: String,
}

/// The bounded result of an assistance query.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelpSelection {
    /// Selected responders, nearest first.
    pub responders: Vec<StableId>,
    /// Number of candidates inspected.
    pub considered: usize,
    /// Skipped candidates and reasons, for AI debugging.
    pub skipped: Vec<HelpSkipped>,
}

/// Select nearby AI entities whose effective alignment treats the requester as
/// a friend.
///
/// Eligibility requires: alive, AI/scripted control, a faction, range, line of
/// sight, and a friendly alignment resolved from the candidate's perspective.
#[must_use]
pub fn select_helpers<'a>(
    request: &HelpRequest,
    entities: impl IntoIterator<Item = &'a EntityRecord>,
    line_of_sight: impl Fn(Vec3, Vec3) -> bool,
    catalog: &FactionCatalog,
    thresholds: Thresholds,
) -> HelpSelection {
    let mut selection = HelpSelection::default();
    let mut candidates = Vec::new();

    for entity in entities {
        selection.considered += 1;
        if entity.id == request.requester {
            continue;
        }
        if !entity.is_alive() {
            selection.skipped.push(skip(entity.id, "dead"));
            continue;
        }
        let Some(mind) = &entity.mind else {
            selection.skipped.push(skip(entity.id, "no_mind"));
            continue;
        };
        if !matches!(
            mind.controller,
            Controller::Ai { .. } | Controller::Scripted { .. }
        ) {
            selection.skipped.push(skip(entity.id, "not_ai"));
            continue;
        }
        let Some(membership) = &entity.faction else {
            selection.skipped.push(skip(entity.id, "no_faction"));
            continue;
        };
        let distance = request.position.distance_xz(entity.position());
        if distance > request.range_m {
            selection.skipped.push(skip(entity.id, "out_of_range"));
            continue;
        }
        if !line_of_sight(request.position, entity.position()) {
            selection.skipped.push(skip(entity.id, "no_line_of_sight"));
            continue;
        }
        let alignment = resolve_alignment(
            catalog,
            &membership.faction,
            &request.faction,
            Some(&membership.reputation),
            request.now,
            &[],
        );
        if alignment.class(thresholds) != AlignmentClass::Friend {
            selection.skipped.push(skip(entity.id, "not_friendly"));
            continue;
        }
        candidates.push((distance, entity.id));
    }

    candidates.sort_by(|(distance_a, id_a), (distance_b, id_b)| {
        distance_a
            .total_cmp(distance_b)
            .then_with(|| id_a.cmp(id_b))
    });
    selection.responders = candidates
        .into_iter()
        .take(request.max_responders)
        .map(|(_, id)| id)
        .collect();
    selection
}

/// A hostile entity perceived by a controller.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PerceivedTarget {
    /// Perceived entity.
    pub entity: StableId,
    /// Horizontal distance in meters.
    pub distance_m: f32,
    /// Effective alignment from the observer's perspective.
    pub alignment: Alignment,
}

/// Per-query limits supplied by the server, not by content or a client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerceptionBudget {
    /// Maximum candidate records inspected, including the observer.
    pub max_candidates: usize,
    /// Maximum line-of-sight checks.
    pub max_line_of_sight: usize,
    /// Maximum targets returned.
    pub max_targets: usize,
}

/// Why a bounded perception query stopped or could not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PerceptionStatus {
    /// The supplied candidate iterator was exhausted.
    Complete,
    /// The candidate limit was reached; more candidates may remain.
    CandidateBudgetReached,
    /// A candidate needed a line-of-sight check after that budget was spent.
    LineOfSightBudgetReached,
    /// The observer is dead, lacks a faction, or is not AI-controlled.
    IneligibleObserver,
    /// Range is negative or non-finite, or the observer's position is non-finite.
    InvalidGeometry,
}

/// Inspectable output from a bounded perception query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PerceptionReport {
    /// Nearest discovered enemies, with entity id as the tie breaker.
    pub targets: Vec<PerceivedTarget>,
    /// Candidate records actually inspected.
    pub candidates_examined: usize,
    /// Line-of-sight checks actually performed.
    pub line_of_sight_checks: usize,
    /// Completion or failure diagnostic.
    pub status: PerceptionStatus,
}

/// Perceive enemies with hard candidate, line-of-sight, and output limits.
///
/// Candidates must be supplied in a stable order for replay. When a budget is
/// reached, the results cover only the inspected prefix and therefore need not
/// contain the globally nearest enemies. No lookahead is performed to detect
/// whether the candidate limit coincides exactly with the end of the iterator.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn perceive_hostiles_bounded<'a>(
    observer: &EntityRecord,
    entities: impl IntoIterator<Item = &'a EntityRecord>,
    line_of_sight: impl Fn(Vec3, Vec3) -> bool,
    catalog: &FactionCatalog,
    thresholds: Thresholds,
    range_m: f32,
    now: Millis,
    budget: PerceptionBudget,
) -> PerceptionReport {
    let mut report = PerceptionReport {
        targets: Vec::new(),
        candidates_examined: 0,
        line_of_sight_checks: 0,
        status: PerceptionStatus::Complete,
    };
    let Some(membership) = observer.faction.as_ref().filter(|_| {
        observer.is_alive()
            && observer
                .mind
                .as_ref()
                .is_some_and(|mind| matches!(mind.controller, Controller::Ai { .. }))
    }) else {
        report.status = PerceptionStatus::IneligibleObserver;
        return report;
    };
    let position = observer.position();
    if !range_m.is_finite() || range_m < 0.0 || !finite_position(position) {
        report.status = PerceptionStatus::InvalidGeometry;
        return report;
    }

    let mut entities = entities.into_iter();
    while report.candidates_examined < budget.max_candidates {
        let Some(entity) = entities.next() else {
            return finish_perception(report, budget.max_targets);
        };
        report.candidates_examined += 1;
        if entity.id == observer.id || !entity.is_alive() || !finite_position(entity.position()) {
            continue;
        }
        let Some(other) = &entity.faction else {
            continue;
        };
        let distance = position.distance_xz(entity.position());
        if !distance.is_finite() || distance > range_m {
            continue;
        }
        let alignment = resolve_alignment(
            catalog,
            &membership.faction,
            &other.faction,
            Some(&membership.reputation),
            now,
            &[],
        );
        if alignment.class(thresholds) != AlignmentClass::Enemy {
            continue;
        }
        if report.line_of_sight_checks == budget.max_line_of_sight {
            report.status = PerceptionStatus::LineOfSightBudgetReached;
            return finish_perception(report, budget.max_targets);
        }
        report.line_of_sight_checks += 1;
        if line_of_sight(position, entity.position()) {
            report.targets.push(PerceivedTarget {
                entity: entity.id,
                distance_m: distance,
                alignment,
            });
        }
    }
    report.status = PerceptionStatus::CandidateBudgetReached;
    finish_perception(report, budget.max_targets)
}

fn finite_position(position: Vec3) -> bool {
    position.x.is_finite() && position.y.is_finite() && position.z.is_finite()
}

fn finish_perception(mut report: PerceptionReport, max_targets: usize) -> PerceptionReport {
    report.targets.sort_by(|a, b| {
        a.distance_m
            .total_cmp(&b.distance_m)
            .then_with(|| a.entity.cmp(&b.entity))
    });
    report.targets.truncate(max_targets);
    report
}

/// Perceive hostile entities within range and line of sight, nearest first.
#[must_use]
pub fn perceive_hostiles<'a>(
    observer: &EntityRecord,
    entities: impl IntoIterator<Item = &'a EntityRecord>,
    line_of_sight: impl Fn(Vec3, Vec3) -> bool,
    catalog: &FactionCatalog,
    thresholds: Thresholds,
    range_m: f32,
    now: Millis,
) -> Vec<PerceivedTarget> {
    let Some(membership) = &observer.faction else {
        return Vec::new();
    };

    let mut targets: Vec<PerceivedTarget> = entities
        .into_iter()
        .filter(|entity| entity.id != observer.id && entity.is_alive())
        .filter_map(|entity| {
            let other = entity.faction.as_ref()?;
            let distance = observer.position().distance_xz(entity.position());
            if distance > range_m || !line_of_sight(observer.position(), entity.position()) {
                return None;
            }
            let alignment = resolve_alignment(
                catalog,
                &membership.faction,
                &other.faction,
                Some(&membership.reputation),
                now,
                &[],
            );
            (alignment.class(thresholds) == AlignmentClass::Enemy).then_some(PerceivedTarget {
                entity: entity.id,
                distance_m: distance,
                alignment,
            })
        })
        .collect();

    targets.sort_by(|a, b| {
        a.distance_m
            .total_cmp(&b.distance_m)
            .then_with(|| a.entity.cmp(&b.entity))
    });
    targets
}

impl Zone {
    /// Bounded hostile perception in stable entity-id order using the zone nav grid.
    /// Returns `None` when the observer does not exist.
    #[must_use]
    pub fn perceive_hostiles_bounded(
        &self,
        observer_id: StableId,
        catalog: &FactionCatalog,
        thresholds: Thresholds,
        range_m: f32,
        now: Millis,
        budget: PerceptionBudget,
    ) -> Option<PerceptionReport> {
        self.entities.get(&observer_id).map(|observer| {
            perceive_hostiles_bounded(
                observer,
                self.entities.values(),
                |from, to| self.nav.line_of_sight(from, to),
                catalog,
                thresholds,
                range_m,
                now,
                budget,
            )
        })
    }

    /// Select nearby allies willing to help, using this zone's nav grid for
    /// line of sight.
    #[must_use]
    pub fn select_helpers(
        &self,
        catalog: &FactionCatalog,
        thresholds: Thresholds,
        request: &HelpRequest,
    ) -> HelpSelection {
        select_helpers(
            request,
            self.entities.values(),
            |from, to| self.nav.line_of_sight(from, to),
            catalog,
            thresholds,
        )
    }

    /// Perceive hostile entities using this zone's nav grid.
    #[must_use]
    pub fn perceive_hostiles(
        &self,
        observer_id: StableId,
        catalog: &FactionCatalog,
        thresholds: Thresholds,
        range_m: f32,
        now: Millis,
    ) -> Vec<PerceivedTarget> {
        self.entities
            .get(&observer_id)
            .map_or_else(Vec::new, |observer| {
                perceive_hostiles(
                    observer,
                    self.entities.values(),
                    |from, to| self.nav.line_of_sight(from, to),
                    catalog,
                    thresholds,
                    range_m,
                    now,
                )
            })
    }
}

fn skip(entity: StableId, reason: &str) -> HelpSkipped {
    HelpSkipped {
        entity,
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::id::IdKind;
    use engine_core::inventory::{Inventory, UniformItemRules};
    use engine_core::schema::PersistencePolicy;

    use crate::entity::{FactionMembership, Health, Interactable};
    use crate::mind::Mind;
    use crate::nav::NavGrid;
    use crate::zone::{TickBudget, ZoneConfig};
    use std::sync::Arc;

    fn id(seed: u128) -> StableId {
        StableId::from_parts(IdKind::Entity, 1, seed)
    }

    fn faction(name: &str) -> DefinitionId {
        DefinitionId::new(name).unwrap()
    }

    fn ai_at(seed: u128, faction_name: &str, x: f32, z: f32) -> EntityRecord {
        EntityRecord::new(
            id(seed),
            DefinitionId::new("npc.guard").unwrap(),
            Vec3::new(x, 0.0, z),
        )
        .with_health(100)
        .with_mind(Mind::ai(DefinitionId::new("behavior.guard").unwrap()))
        .with_faction(FactionMembership::new(faction(faction_name)))
        .with_interactable(Interactable::new(2.0, std::iter::empty()))
        .with_persistence(PersistencePolicy::PersistentEntity)
        .with_inventory(Inventory::with_capacity(4).unwrap())
    }

    fn catalog() -> FactionCatalog {
        let mut catalog = FactionCatalog::new();
        let guards = faction("faction.guards");
        let bandits = faction("faction.bandits");
        catalog.set_bidirectional(guards.clone(), guards.clone(), Alignment::new(80));
        catalog.set_bidirectional(bandits.clone(), bandits.clone(), Alignment::new(60));
        catalog.set(guards.clone(), bandits.clone(), Alignment::new(-90));
        catalog.set(bandits.clone(), guards.clone(), Alignment::new(-90));
        catalog
    }

    fn tick_zone(max_actions: usize, max_updates: usize) -> Zone {
        let config = ZoneConfig::new(
            DefinitionId::new("zone.ai_test").unwrap(),
            StableId::from_parts(IdKind::Zone, 1, 1),
            Vec3::ZERO,
            Vec3::new(10.0, 1.0, 10.0),
        )
        .unwrap()
        .with_budget(TickBudget {
            max_actions,
            max_ai_updates: max_updates,
            max_path_expansions: 0,
        });
        let nav = NavGrid::new(10, 10, 1.0, Vec3::ZERO).unwrap();
        let mut zone = Zone::new(config, nav, Arc::new(UniformItemRules::new(10))).unwrap();
        zone.set_ai_tick_policy(Some(AiTickPolicy {
            factions: catalog(),
            thresholds: Thresholds::default(),
            range_m: 8.0,
            perception: PerceptionBudget {
                max_candidates: 10,
                max_line_of_sight: 10,
                max_targets: 1,
            },
            max_controller_candidates: 10,
            epoch: Millis::new(0),
            tick_ms: 50,
            behaviors: vec![AiBehaviorDefinition {
                id: DefinitionId::new("behavior.guard").unwrap(),
                kind: AiBehaviorKind::Guard,
                range_m: 8.0,
            }],
        }));
        zone
    }

    #[test]
    fn tick_guards_use_validated_actions_and_replay_deterministically() {
        let setup = || {
            let mut zone = tick_zone(1, 2);
            zone.spawn(ai_at(1, "faction.guards", 1.5, 1.5)).unwrap();
            zone.spawn(ai_at(2, "faction.guards", 1.5, 2.5)).unwrap();
            let mut target = ai_at(3, "faction.bandits", 2.5, 1.5);
            target.mind = Some(Mind::player(id(99), "session"));
            target.interactable = Some(Interactable::new(2.0, [Verb::Attack]));
            zone.spawn(target).unwrap();
            zone
        };
        let mut a = setup();
        let mut b = setup();
        // This is the same tick entrypoint used by the gateway.
        a.advance_movement_tick(50, 2.0);
        b.advance_movement_tick(50, 2.0);
        assert_eq!(a.ai_tick_report(), b.ai_tick_report());
        let report = a.ai_tick_report();
        assert_eq!(report.action_attempts, 1);
        assert_eq!(report.decisions.len(), 2);
        assert_eq!(report.decisions[0].reason, "applied");
        assert_eq!(report.decisions[1].reason, "action_budget_reached");
        assert_eq!(
            a.entity(id(3)).unwrap().health.as_ref().unwrap().current,
            99
        );
        assert_eq!(a.entity(id(3)), b.entity(id(3)));
        assert_eq!(a.ai_diagnostics(), b.ai_diagnostics());
        assert_eq!(a.ai_diagnostics().decisions, 2);
        assert_eq!(a.ai_diagnostics().action_budget_exhaustions, 1);
        assert_eq!(a.ai_diagnostics().update_limit_reached, 1);
        assert_eq!(a.take_ai_diagnostics().decisions, 2);
        assert_eq!(a.ai_diagnostics(), AiDiagnostics::default());
        assert_eq!(a.entity(id(3)), b.entity(id(3)));
    }

    #[test]
    fn policy_binds_content_behavior_ids_to_bounded_guard_semantics() {
        let mut zone = tick_zone(2, 1);
        let content_id = DefinitionId::new("behavior.zone_sentinel").unwrap();
        let mut policy = zone.ai_policy.as_ref().unwrap().as_ref().clone();
        policy.behaviors = vec![AiBehaviorDefinition {
            id: content_id.clone(),
            kind: AiBehaviorKind::Guard,
            range_m: 0.5,
        }];
        zone.set_ai_tick_policy(Some(policy));
        let mut sentinel = ai_at(1, "faction.guards", 1.5, 1.5);
        sentinel.mind = Some(Mind::ai(content_id));
        zone.spawn(sentinel).unwrap();
        let mut enemy = ai_at(2, "faction.bandits", 2.5, 1.5);
        enemy.mind = None;
        enemy.interactable = Some(Interactable::new(2.0, [Verb::Attack]));
        zone.spawn(enemy).unwrap();

        zone.advance_tick();
        let decision = &zone.ai_tick_report().decisions[0];
        assert_eq!(decision.behavior.as_str(), "behavior.zone_sentinel");
        assert_eq!(decision.reason, "no_target");
        assert_eq!(decision.perception.as_ref().unwrap().targets.len(), 0);
        assert_eq!(
            zone.entity(id(2)).unwrap().health.as_ref().unwrap().current,
            100
        );
    }

    #[test]
    fn tick_respects_control_limits_and_action_rejection() {
        let mut zone = tick_zone(2, 2);
        let mut player = ai_at(1, "faction.guards", 1.5, 1.5);
        player.mind = Some(Mind::player(id(99), "session"));
        zone.spawn(player).unwrap();
        let mut guard = ai_at(2, "faction.guards", 1.5, 2.5);
        guard
            .mind
            .as_mut()
            .unwrap()
            .capabilities
            .remove(&crate::mind::ControlCapability::Attack);
        zone.spawn(guard).unwrap();
        let mut target = ai_at(3, "faction.bandits", 2.5, 2.5);
        target.mind = None;
        target.interactable = Some(Interactable::new(2.0, [Verb::Attack]));
        zone.spawn(target).unwrap();
        zone.advance_tick();
        let report = zone.ai_tick_report();
        assert_eq!(report.decisions.len(), 1);
        assert_eq!(report.decisions[0].actor, id(2));
        assert_eq!(report.decisions[0].reason, "action_rejected");
        assert!(
            report.decisions[0]
                .action
                .as_ref()
                .unwrap()
                .error()
                .is_some()
        );
        assert_eq!(
            zone.entity(id(3)).unwrap().health.as_ref().unwrap().current,
            100
        );
        let mut policy = zone.ai_policy.as_ref().unwrap().as_ref().clone();
        policy.max_controller_candidates = 1;
        zone.set_ai_tick_policy(Some(policy));
        zone.advance_tick();
        assert_eq!(zone.ai_tick_report().controller_candidates, 1);
        assert_eq!(zone.ai_tick_report().decisions, [] as [AiDecision; 0]);
    }

    #[test]
    fn control_transfer_and_zero_budgets_prevent_ai_actions() {
        let mut zone = tick_zone(0, 1);
        zone.spawn(ai_at(1, "faction.guards", 1.5, 1.5)).unwrap();
        let mut target = ai_at(2, "faction.bandits", 2.5, 1.5);
        target.mind = None;
        target.interactable = Some(Interactable::new(2.0, [Verb::Attack]));
        zone.spawn(target).unwrap();
        zone.advance_tick();
        assert_eq!(zone.ai_tick_report().action_attempts, 0);
        assert_eq!(
            zone.ai_tick_report().decisions[0].reason,
            "action_budget_reached"
        );
        zone.entity_mut(id(1)).unwrap().mind = Some(Mind::player(id(99), "session"));
        zone.config.budget.max_actions = 1;
        zone.advance_tick();
        assert_eq!(zone.ai_tick_report().decisions, [] as [AiDecision; 0]);
        assert_eq!(
            zone.entity(id(2)).unwrap().health.as_ref().unwrap().current,
            100
        );
        zone.config.budget.max_ai_updates = 0;
        zone.advance_tick();
        assert_eq!(zone.ai_tick_report().controller_candidates, 0);
    }

    #[test]
    fn unknown_behavior_and_exhausted_perception_are_inspectable() {
        let mut zone = tick_zone(2, 2);
        let mut unknown = ai_at(1, "faction.guards", 1.5, 1.5);
        unknown.mind = Some(Mind::ai(DefinitionId::new("behavior.unknown").unwrap()));
        zone.spawn(unknown).unwrap();
        zone.spawn(ai_at(2, "faction.guards", 1.5, 2.5)).unwrap();
        let mut policy = zone.ai_policy.as_ref().unwrap().as_ref().clone();
        policy.perception.max_candidates = 0;
        zone.set_ai_tick_policy(Some(policy));
        zone.advance_tick();
        let decisions = &zone.ai_tick_report().decisions;
        assert_eq!(decisions[0].reason, "unknown_behavior");
        assert_eq!(
            decisions[1].perception.as_ref().unwrap().status,
            PerceptionStatus::CandidateBudgetReached
        );
        assert_eq!(zone.ai_diagnostics().perception_candidate_limit_reached, 1);
        assert_eq!(zone.ai_tick_report().action_attempts, 0);
    }

    #[test]
    fn only_friendly_allies_in_range_respond() {
        let guards = faction("faction.guards");
        let requester = id(1);
        let near = ai_at(2, "faction.guards", 2.0, 0.0);
        let far = ai_at(3, "faction.guards", 50.0, 0.0);
        let bandit = ai_at(4, "faction.bandits", 1.0, 0.0);
        let entities = vec![near, far, bandit];

        let request = HelpRequest::new(requester, Vec3::ZERO, guards, 10.0, Millis::new(10));
        let selection = select_helpers(
            &request,
            &entities,
            |_, _| true,
            &catalog(),
            Thresholds::default(),
        );

        assert_eq!(selection.responders, vec![id(2)]);
        assert!(selection.considered >= 3);
        assert!(
            selection
                .skipped
                .iter()
                .any(|s| s.entity == id(3) && s.reason == "out_of_range")
        );
        assert!(
            selection
                .skipped
                .iter()
                .any(|s| s.entity == id(4) && s.reason == "not_friendly")
        );
    }

    #[test]
    fn responders_are_capped_and_sorted_by_distance() {
        let guards = faction("faction.guards");
        let entities = vec![
            ai_at(5, "faction.guards", 5.0, 0.0),
            ai_at(2, "faction.guards", 2.0, 0.0),
            ai_at(3, "faction.guards", 3.0, 0.0),
        ];
        let mut request = HelpRequest::new(id(1), Vec3::ZERO, guards, 10.0, Millis::new(10));
        request.max_responders = 2;
        let selection = select_helpers(
            &request,
            &entities,
            |_, _| true,
            &catalog(),
            Thresholds::default(),
        );
        assert_eq!(selection.responders, vec![id(2), id(3)]);
    }

    #[test]
    fn line_of_sight_filters_responders() {
        let guards = faction("faction.guards");
        let entities = vec![ai_at(2, "faction.guards", 2.0, 0.0)];
        let request = HelpRequest::new(id(1), Vec3::ZERO, guards, 10.0, Millis::new(10));
        let selection = select_helpers(
            &request,
            &entities,
            |_, _| false,
            &catalog(),
            Thresholds::default(),
        );
        assert_eq!(selection.responders, Vec::<StableId>::new());
        assert_eq!(selection.skipped[0].reason, "no_line_of_sight");
    }

    #[test]
    fn dead_and_non_ai_entities_do_not_respond() {
        let guards = faction("faction.guards");
        let mut dead = ai_at(2, "faction.guards", 1.0, 0.0);
        dead.health = Some(Health {
            current: 0,
            max: 10,
        });
        let mut player = ai_at(3, "faction.guards", 1.0, 0.0);
        player.mind = Some(Mind::player(id(99), "session"));

        let request = HelpRequest::new(id(1), Vec3::ZERO, guards, 10.0, Millis::new(10));
        let selection = select_helpers(
            &request,
            &[dead, player],
            |_, _| true,
            &catalog(),
            Thresholds::default(),
        );
        assert_eq!(selection.responders, Vec::<StableId>::new());
        assert!(selection.skipped.iter().any(|s| s.reason == "dead"));
        assert!(selection.skipped.iter().any(|s| s.reason == "not_ai"));
    }

    #[test]
    fn perception_returns_hostiles_nearest_first() {
        let guard = ai_at(1, "faction.guards", 0.0, 0.0);
        let near = ai_at(2, "faction.bandits", 2.0, 0.0);
        let far = ai_at(3, "faction.bandits", 6.0, 0.0);
        let friend = ai_at(4, "faction.guards", 1.0, 0.0);
        let entities = vec![guard.clone(), near, far, friend];

        let targets = perceive_hostiles(
            &guard,
            &entities,
            |_, _| true,
            &catalog(),
            Thresholds::default(),
            10.0,
            Millis::new(10),
        );
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].entity, id(2));
        assert_eq!(targets[1].entity, id(3));
    }

    #[test]
    fn bounded_perception_limits_work_and_reports_partial_results() {
        use std::cell::Cell;
        let guard = ai_at(1, "faction.guards", 0.0, 0.0);
        let entities = vec![
            ai_at(2, "faction.bandits", 4.0, 0.0),
            ai_at(3, "faction.bandits", 2.0, 0.0),
            ai_at(4, "faction.bandits", 1.0, 0.0),
        ];
        let checks = Cell::new(0);
        let run = || {
            perceive_hostiles_bounded(
                &guard,
                &entities,
                |_, _| {
                    checks.set(checks.get() + 1);
                    true
                },
                &catalog(),
                Thresholds::default(),
                10.0,
                Millis::new(10),
                PerceptionBudget {
                    max_candidates: 3,
                    max_line_of_sight: 2,
                    max_targets: 1,
                },
            )
        };
        let report = run();
        assert_eq!(report.status, PerceptionStatus::LineOfSightBudgetReached);
        assert_eq!(report.candidates_examined, 3);
        assert_eq!(report.line_of_sight_checks, 2);
        assert_eq!(checks.get(), 2);
        assert_eq!(
            report.targets.iter().map(|t| t.entity).collect::<Vec<_>>(),
            vec![id(3)]
        );
        assert_eq!(report, run());
    }

    #[test]
    fn bounded_perception_rejects_player_control_and_invalid_geometry() {
        let mut guard = ai_at(1, "faction.guards", 0.0, 0.0);
        let enemy = ai_at(2, "faction.bandits", 1.0, 0.0);
        let budget = PerceptionBudget {
            max_candidates: 5,
            max_line_of_sight: 5,
            max_targets: 5,
        };
        guard.mind = Some(Mind::player(id(99), "session"));
        let run = |observer: &EntityRecord, range| {
            perceive_hostiles_bounded(
                observer,
                [&enemy],
                |_, _| panic!("unexpected LOS"),
                &catalog(),
                Thresholds::default(),
                range,
                Millis::new(0),
                budget,
            )
        };
        let report = run(&guard, 10.0);
        assert_eq!(report.status, PerceptionStatus::IneligibleObserver);
        assert_eq!(report.candidates_examined, 0);
        guard.mind = Some(Mind::ai(DefinitionId::new("behavior.guard").unwrap()));
        assert_eq!(
            run(&guard, f32::NAN).status,
            PerceptionStatus::InvalidGeometry
        );
    }

    #[test]
    fn bounded_perception_candidate_limit_never_reads_ahead() {
        let guard = ai_at(1, "faction.guards", 0.0, 0.0);
        let enemy = ai_at(2, "faction.bandits", 1.0, 0.0);
        let entities =
            std::iter::once(&enemy).chain(std::iter::from_fn(|| panic!("read beyond budget")));
        let report = perceive_hostiles_bounded(
            &guard,
            entities,
            |_, _| true,
            &catalog(),
            Thresholds::default(),
            10.0,
            Millis::new(0),
            PerceptionBudget {
                max_candidates: 1,
                max_line_of_sight: 1,
                max_targets: 1,
            },
        );
        assert_eq!(report.status, PerceptionStatus::CandidateBudgetReached);
        assert_eq!(report.targets[0].entity, id(2));
    }
}
