//! Faction-aware perception and bounded ally assistance.

use engine_core::faction::{
    Alignment, AlignmentClass, FactionCatalog, Thresholds, resolve_alignment,
};
use engine_core::id::{DefinitionId, StableId};
use engine_core::math::Vec3;
use engine_core::time::{DailySchedule, Millis, Tick};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;

use crate::action::{ActionRequest, ActionResult, Verb};
use crate::entity::EntityRecord;
use crate::mind::Controller;
use crate::nav::{PartialPathPolicy, Path, ReplanPolicy, ReplanState};
use crate::zone::Zone;

/// Hard upper bound on behavior graph instructions executed for one AI per tick.
pub const MAX_AI_BEHAVIOR_STEPS_PER_DECISION: usize = 16;
/// Maximum behavior graph nodes accepted by the runtime binding.
pub const MAX_AI_BEHAVIOR_GRAPH_NODES: usize = 64;
/// Maximum route points accepted by a patrol instruction.
pub const MAX_AI_PATROL_ROUTE_POINTS: usize = 64;

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
    ///
    /// Scans start after the previous tick's last inspected record and wrap
    /// around in stable-ID order, so a low limit cannot permanently starve
    /// controllers that sort later.
    pub max_controller_candidates: usize,
    /// Maximum per-tick decisions that may reuse a confirmed hostile target
    /// from another AI in the same authored aggro group. Zero disables sharing.
    pub max_group_aggro_shares: usize,
    /// Simulation epoch for reputation expiry.
    pub epoch: Millis,
    /// Milliseconds per simulation tick.
    pub tick_ms: u32,
    /// Declarative behavior bindings. Content may assign stable behavior IDs
    /// to these bounded built-in semantics without changing the engine.
    pub behaviors: Vec<AiBehaviorDefinition>,
    /// Base NPC navigation speed; entity movement profiles apply multipliers.
    pub navigation_speed_m_per_s: f32,
    /// Retry cadence after a path fails or is blocked.
    pub navigation_replan: ReplanPolicy,
}

/// A server-approved behavior definition that can be populated from content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiBehaviorDefinition {
    pub id: DefinitionId,
    pub kind: AiBehaviorKind,
    /// Perception range for this behavior, in meters.
    pub range_m: f32,
    /// Optional recurring window in UTC simulation time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<DailySchedule>,
    /// Optional stable group key for same-tick hostile-target sharing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggro_group: Option<DefinitionId>,
    /// Optional directed instruction graph that overrides the legacy primitive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<AiBehaviorGraph>,
    /// Ordered looping route used by graph `patrol` instructions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patrol_route: Vec<Vec3>,
}

/// A validated graph of bounded AI instructions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiBehaviorGraph {
    /// First instruction evaluated each tick.
    pub entry: DefinitionId,
    /// Directed nodes; the interpreter also enforces a per-decision step cap.
    pub nodes: Vec<AiBehaviorNode>,
}

impl AiBehaviorGraph {
    /// Whether all IDs and transitions are valid within the runtime node cap.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        if self.nodes.is_empty() || self.nodes.len() > MAX_AI_BEHAVIOR_GRAPH_NODES {
            return false;
        }
        let ids: BTreeSet<_> = self.nodes.iter().map(|node| node.id.clone()).collect();
        ids.len() == self.nodes.len()
            && ids.contains(&self.entry)
            && self.nodes.iter().all(|node| {
                node.on_success
                    .as_ref()
                    .is_none_or(|next| ids.contains(next))
                    && node
                        .on_failure
                        .as_ref()
                        .is_none_or(|next| ids.contains(next))
                    && (node.instruction != AiBehaviorInstruction::Wait
                        || (node.on_success.is_none() && node.on_failure.is_none()))
            })
    }
}

/// One instruction node in an AI behavior graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiBehaviorNode {
    /// Stable node identifier.
    pub id: DefinitionId,
    /// Operation evaluated by this node.
    pub instruction: AiBehaviorInstruction,
    /// Next node when the operation succeeds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_success: Option<DefinitionId>,
    /// Next node when the operation fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_failure: Option<DefinitionId>,
}

/// Runtime operation supported by the bounded behavior graph interpreter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiBehaviorInstruction {
    PerceiveHostiles,
    AttackTarget,
    ChaseTarget,
    FleeTarget,
    InteractTarget,
    Patrol,
    Wait,
}

/// Bounded behavior primitives supported by the runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiBehaviorKind {
    Guard,
    /// Guard that follows visible hostile targets using bounded zone paths.
    GuardChase,
}

/// Last tick's bounded AI decisions, retained for server inspection.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AiTickReport {
    /// Tick on which these decisions ran.
    pub tick: Tick,
    /// Entity records scanned for controllers.
    pub controller_candidates: usize,
    /// Controllers evaluated in rotating stable-ID order.
    pub decisions: Vec<AiDecision>,
    /// Number of action attempts, including rejections.
    pub action_attempts: usize,
    /// Number of decisions that reused a target confirmed by an ally.
    pub group_aggro_shares: usize,
    /// Total unique player and AI action attempts admitted in this tick window.
    pub shared_action_attempts: usize,
    /// Remaining unique player and AI action attempts in this tick window.
    pub action_budget_remaining: usize,
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
    /// Decisions that could not reuse an available group target due to budget.
    pub group_aggro_share_budget_exhaustions: u64,
    /// Decisions that reached the behavior graph instruction cap.
    pub behavior_step_budget_exhaustions: u64,
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
    /// Whether the target came from another AI decision this tick.
    pub shared_target: bool,
    /// Whether this decision exhausted the shared player/AI action quota.
    pub action_budget_exhausted: bool,
    /// Graph instructions evaluated for this decision.
    pub behavior_nodes_executed: usize,
    /// Current route point for a patrol instruction, when applicable.
    pub patrol_waypoint: Option<usize>,
    /// Navigation outcome, if this decision requested or followed a path.
    pub navigation: Option<AiNavigationReport>,
    /// Stable decision diagnostic.
    pub reason: &'static str,
}

/// Bounded, inspectable navigation outcome for one AI decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AiNavigationReport {
    /// Current hostile target.
    pub target: StableId,
    /// Requested target position's navigation cell.
    pub goal_cell: (usize, usize),
    /// Short state code; no map or private entity information is included.
    pub status: &'static str,
    /// Whether the retained route reaches the requested goal cell.
    pub complete: bool,
    /// Expansions used to produce the current retained route.
    pub expansions: usize,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct AiNavigationState {
    target: Option<StableId>,
    goal_cell: Option<(usize, usize)>,
    path: Option<Path>,
    next_waypoint: usize,
    replan: ReplanState,
    abandoned: bool,
}

impl Zone {
    /// Enable or disable tick-driven AI. This is server configuration, not client input.
    pub fn set_ai_tick_policy(&mut self, policy: Option<AiTickPolicy>) {
        self.ai_policy = policy.map(std::sync::Arc::new);
    }

    /// Merge validated content behavior bindings into the installed policy.
    /// Matching IDs replace demo/runtime defaults; new IDs are available to
    /// controllers whose behavior ID is authored by the world composition.
    ///
    /// # Errors
    ///
    /// Returns an error when no AI policy is installed or a graph is invalid.
    pub fn merge_ai_behavior_bindings(
        &mut self,
        behaviors: Vec<AiBehaviorDefinition>,
    ) -> Result<(), engine_core::error::EngineError> {
        if behaviors.iter().any(|behavior| {
            behavior
                .graph
                .as_ref()
                .is_some_and(|graph| !graph.is_valid())
                || behavior.patrol_route.len() > MAX_AI_PATROL_ROUTE_POINTS
                || behavior.patrol_route.iter().any(|point| {
                    !point.x.is_finite() || !point.y.is_finite() || !point.z.is_finite()
                })
        }) {
            return Err(engine_core::error::EngineError::new(
                engine_core::error::ErrorCode::InvalidRequest,
                "content AI behavior bindings are invalid",
            ));
        }
        let policy = self.ai_policy.as_ref().ok_or_else(|| {
            engine_core::error::EngineError::new(
                engine_core::error::ErrorCode::InvalidState,
                "AI behavior bindings require an installed AI policy",
            )
        })?;
        let mut updated = policy.as_ref().clone();
        for behavior in behaviors {
            if let Some(existing) = updated
                .behaviors
                .iter_mut()
                .find(|existing| existing.id == behavior.id)
            {
                *existing = behavior;
            } else {
                updated.behaviors.push(behavior);
            }
        }
        self.ai_policy = Some(std::sync::Arc::new(updated));
        Ok(())
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
            report.shared_action_attempts = self.action_attempts_this_tick;
            report.action_budget_remaining = self.action_budget_remaining();
            self.ai_report = report;
            return;
        };
        self.ai_diagnostics.ticks = self.ai_diagnostics.ticks.saturating_add(1);
        // Bound selection work independently of the number of non-AI entities.
        // The scan starts after the previous tick's last inspected record and
        // wraps in stable-ID order, so low candidate or update limits cannot
        // permanently starve controllers that sort later.
        let update_limit = self.config.budget.max_ai_updates;
        let scan_limit = if update_limit == 0 {
            0
        } else {
            policy.max_controller_candidates
        };
        let mut controllers = Vec::new();
        let mut last_scanned = None;
        if scan_limit > 0 {
            let cursor = self.ai_scan_cursor;
            let mut scanned = 0;
            let mut consider = |entity: &EntityRecord| {
                scanned += 1;
                last_scanned = Some(entity.id);
                if let Some(crate::mind::Mind {
                    controller: Controller::Ai { behavior },
                    ..
                }) = &entity.mind
                {
                    controllers.push((entity.id, behavior.clone()));
                    if controllers.len() >= update_limit {
                        return false;
                    }
                }
                scanned < scan_limit
            };
            let mut stopped = false;
            if let Some(cursor) = cursor {
                for entity in self.entities.range(cursor..) {
                    if !consider(entity.1) {
                        stopped = true;
                        break;
                    }
                }
                if !stopped {
                    for entity in self.entities.range(..cursor) {
                        if !consider(entity.1) {
                            break;
                        }
                    }
                }
            } else {
                for entity in self.entities.values() {
                    if !consider(entity) {
                        break;
                    }
                }
            }
            report.controller_candidates = scanned;
            self.ai_scan_cursor = last_scanned.and_then(|last| {
                self.entities
                    .range((Bound::Excluded(last), Bound::Unbounded))
                    .next()
                    .map(|(id, _)| *id)
                    .or_else(|| self.entities.keys().next().copied())
            });
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
        let chase_behaviors: BTreeSet<_> = policy
            .behaviors
            .iter()
            .filter(|definition| definition.kind == AiBehaviorKind::GuardChase)
            .map(|definition| definition.id.clone())
            .collect();
        let active_chasers: BTreeSet<_> = controllers
            .iter()
            .filter(|(_, behavior)| chase_behaviors.contains(behavior))
            .map(|(actor, _)| *actor)
            .collect();
        self.ai_navigation
            .retain(|actor, _| active_chasers.contains(actor));
        let active_patrollers: BTreeSet<_> = controllers
            .iter()
            .filter(|(_, behavior)| {
                policy.behaviors.iter().any(|definition| {
                    definition.id == *behavior
                        && definition.graph.as_ref().is_some_and(|graph| {
                            graph.is_valid()
                                && graph
                                    .nodes
                                    .iter()
                                    .any(|node| node.instruction == AiBehaviorInstruction::Patrol)
                        })
                })
            })
            .map(|(actor, _)| *actor)
            .collect();
        self.ai_patrol_cursors
            .retain(|actor, _| active_patrollers.contains(actor));
        let elapsed = self.tick.0.saturating_mul(u64::from(policy.tick_ms));
        let now = Millis::new(
            policy
                .epoch
                .0
                .saturating_add(i64::try_from(elapsed).unwrap_or(i64::MAX)),
        );
        let mut group_targets = BTreeMap::new();
        for (actor, behavior) in controllers {
            let mut decision = AiDecision {
                actor,
                behavior,
                perception: None,
                target: None,
                action: None,
                shared_target: false,
                action_budget_exhausted: false,
                behavior_nodes_executed: 0,
                patrol_waypoint: None,
                navigation: None,
                reason: "unknown_behavior",
            };
            // The built-in behavior is deliberately explicit: unknown content does not attack.
            let definition = policy
                .behaviors
                .iter()
                .find(|definition| definition.id == decision.behavior)
                .filter(|definition| {
                    matches!(
                        definition.kind,
                        AiBehaviorKind::Guard | AiBehaviorKind::GuardChase
                    )
                });
            if let Some(definition) = definition
                && definition
                    .schedule
                    .is_some_and(|schedule| !schedule.contains(now))
            {
                decision.reason = "schedule_inactive";
            } else if let Some(definition) = definition.filter(|definition| {
                definition
                    .graph
                    .as_ref()
                    .is_some_and(|graph| graph.is_valid())
            }) {
                let graph = definition.graph.as_ref().expect("filtered graph exists");
                self.run_ai_behavior_graph(
                    actor,
                    definition,
                    graph,
                    &policy,
                    now,
                    &mut group_targets,
                    &mut report,
                    &mut decision,
                );
            } else if definition.is_some_and(|definition| definition.graph.is_some()) {
                decision.reason = "invalid_behavior_graph";
            } else if let Some(definition) = definition {
                let aggro_group = definition.aggro_group.clone();
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
                if decision.target.is_none()
                    && matches!(
                        perception.status,
                        PerceptionStatus::Complete
                            | PerceptionStatus::CandidateBudgetReached
                            | PerceptionStatus::LineOfSightBudgetReached
                    )
                    && let Some(group) = aggro_group.as_ref()
                    && let Some(target) = group_targets.get(group).copied()
                    && self.ai_target_is_hostile(
                        actor,
                        target,
                        &policy.factions,
                        policy.thresholds,
                        now,
                    )
                {
                    if report.group_aggro_shares < policy.max_group_aggro_shares {
                        decision.target = Some(target);
                        decision.shared_target = true;
                        report.group_aggro_shares += 1;
                    } else {
                        self.ai_diagnostics.group_aggro_share_budget_exhaustions = self
                            .ai_diagnostics
                            .group_aggro_share_budget_exhaustions
                            .saturating_add(1);
                    }
                }
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
                let chasing = policy
                    .behaviors
                    .iter()
                    .find(|definition| definition.id == decision.behavior)
                    .is_some_and(|definition| definition.kind == AiBehaviorKind::GuardChase);
                if !chasing {
                    self.ai_navigation.remove(&actor);
                }
                if chasing && decision.target.is_none() {
                    self.ai_navigation.remove(&actor);
                }
                if chasing
                    && decision
                        .target
                        .is_some_and(|target| self.ai_target_in_range(actor, target))
                {
                    self.ai_navigation.remove(&actor);
                }
                if chasing
                    && let Some(target) = decision.target
                    && !self.ai_target_in_range(actor, target)
                {
                    let navigation = self.navigate_ai_toward(
                        actor,
                        target,
                        policy.navigation_speed_m_per_s,
                        policy.tick_ms,
                        policy.navigation_replan,
                    );
                    decision.reason = navigation.status;
                    decision.navigation = Some(navigation);
                } else if let Some(target) = decision.target
                    && report.action_attempts < self.config.budget.max_actions
                    && self.action_budget_remaining() > 0
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
                        if result.is_applied()
                            && !decision.shared_target
                            && let Some(group) = aggro_group
                        {
                            group_targets.entry(group).or_insert(target);
                        }
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
            if decision.reason == "action_budget_reached" || decision.action_budget_exhausted {
                self.ai_diagnostics.action_budget_exhaustions = self
                    .ai_diagnostics
                    .action_budget_exhaustions
                    .saturating_add(1);
            }
            report.decisions.push(decision);
        }
        report.shared_action_attempts = self.action_attempts_this_tick;
        report.action_budget_remaining = self.action_budget_remaining();
        self.ai_report = report;
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn run_ai_behavior_graph(
        &mut self,
        actor: StableId,
        definition: &AiBehaviorDefinition,
        graph: &AiBehaviorGraph,
        policy: &AiTickPolicy,
        now: Millis,
        group_targets: &mut BTreeMap<DefinitionId, StableId>,
        report: &mut AiTickReport,
        decision: &mut AiDecision,
    ) {
        let mut current = Some(graph.entry.clone());
        let mut target = None;
        let mut perception = None;
        let mut perception_ran = false;
        let mut action_ran = false;
        let mut navigation_ran = false;
        decision.reason = "behavior_graph_complete";

        while let Some(node_id) = current {
            if decision.behavior_nodes_executed >= MAX_AI_BEHAVIOR_STEPS_PER_DECISION {
                decision.reason = "behavior_step_budget_reached";
                self.ai_diagnostics.behavior_step_budget_exhaustions = self
                    .ai_diagnostics
                    .behavior_step_budget_exhaustions
                    .saturating_add(1);
                break;
            }
            let Some(node) = graph.nodes.iter().find(|node| node.id == node_id) else {
                decision.reason = "invalid_behavior_graph";
                break;
            };
            let instruction = node.instruction;
            let on_success = node.on_success.clone();
            let on_failure = node.on_failure.clone();
            decision.behavior_nodes_executed += 1;

            if instruction == AiBehaviorInstruction::Wait {
                decision.reason = "behavior_wait";
                break;
            }

            let succeeded = match instruction {
                AiBehaviorInstruction::PerceiveHostiles => {
                    if !perception_ran {
                        perception = self.perceive_hostiles_bounded(
                            actor,
                            &policy.factions,
                            policy.thresholds,
                            definition.range_m,
                            now,
                            policy.perception,
                        );
                        perception_ran = true;
                        if let Some(perception) = perception.as_ref() {
                            target = perception.targets.first().map(|target| target.entity);
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
                        }
                        if target.is_none()
                            && perception.as_ref().is_some_and(|perception| {
                                matches!(
                                    perception.status,
                                    PerceptionStatus::Complete
                                        | PerceptionStatus::CandidateBudgetReached
                                        | PerceptionStatus::LineOfSightBudgetReached
                                )
                            })
                            && let Some(group) = definition.aggro_group.as_ref()
                            && let Some(shared) = group_targets.get(group).copied()
                            && self.ai_target_is_hostile(
                                actor,
                                shared,
                                &policy.factions,
                                policy.thresholds,
                                now,
                            )
                        {
                            if report.group_aggro_shares < policy.max_group_aggro_shares {
                                target = Some(shared);
                                decision.shared_target = true;
                                report.group_aggro_shares += 1;
                            } else {
                                self.ai_diagnostics.group_aggro_share_budget_exhaustions = self
                                    .ai_diagnostics
                                    .group_aggro_share_budget_exhaustions
                                    .saturating_add(1);
                            }
                        }
                        decision.reason = match perception.as_ref().map(|report| report.status) {
                            Some(PerceptionStatus::IneligibleObserver) => "ineligible_observer",
                            Some(PerceptionStatus::InvalidGeometry) => "invalid_geometry",
                            _ if target.is_some() => "target_acquired",
                            _ => "no_target",
                        };
                    }
                    target.is_some()
                }
                AiBehaviorInstruction::AttackTarget => {
                    if action_ran {
                        decision.reason = "action_already_attempted";
                        false
                    } else if target.is_none() {
                        decision.reason = "no_target";
                        false
                    } else if report.action_attempts >= self.config.budget.max_actions
                        || self.action_budget_remaining() == 0
                    {
                        decision.reason = "action_budget_reached";
                        decision.action_budget_exhausted = true;
                        false
                    } else {
                        let target = target.expect("checked target is present");
                        let request_id =
                            format!("ai:{}:{}:{}", self.config.instance, self.tick.0, actor);
                        if self.action_results.contains_key(&request_id) {
                            decision.reason = "request_id_conflict";
                            action_ran = true;
                            false
                        } else {
                            let request = ActionRequest::new(request_id, actor, Verb::Attack)
                                .on(target)
                                .at_tick(self.tick);
                            report.action_attempts += 1;
                            action_ran = true;
                            let result = self.apply_action(&request, now);
                            let succeeded = result.is_applied();
                            if succeeded
                                && !decision.shared_target
                                && let Some(group) = definition.aggro_group.as_ref()
                            {
                                group_targets.entry(group.clone()).or_insert(target);
                            }
                            decision.reason = if succeeded {
                                "applied"
                            } else {
                                "action_rejected"
                            };
                            decision.action = Some(result);
                            succeeded
                        }
                    }
                }
                AiBehaviorInstruction::ChaseTarget => {
                    if navigation_ran {
                        decision.reason = "navigation_already_attempted";
                        false
                    } else if target.is_none() {
                        decision.reason = "no_target";
                        false
                    } else {
                        let target = target.expect("checked target is present");
                        navigation_ran = true;
                        if self.ai_target_in_range(actor, target) {
                            self.ai_navigation.remove(&actor);
                            decision.reason = "target_in_interaction_range";
                            true
                        } else {
                            let navigation = self.navigate_ai_toward(
                                actor,
                                target,
                                policy.navigation_speed_m_per_s,
                                policy.tick_ms,
                                policy.navigation_replan,
                            );
                            let succeeded = matches!(
                                navigation.status,
                                "navigating" | "following_partial_path" | "arrived"
                            );
                            decision.reason = navigation.status;
                            decision.navigation = Some(navigation);
                            succeeded
                        }
                    }
                }
                AiBehaviorInstruction::FleeTarget => {
                    if navigation_ran {
                        decision.reason = "navigation_already_attempted";
                        false
                    } else if let Some(target) = target {
                        navigation_ran = true;
                        let succeeded = self.flee_ai_from(
                            actor,
                            target,
                            policy.navigation_speed_m_per_s,
                            policy.tick_ms,
                        );
                        decision.reason = if succeeded { "fleeing" } else { "flee_blocked" };
                        decision.navigation = Some(AiNavigationReport {
                            target,
                            goal_cell: self.entities.get(&actor).map_or((0, 0), |entity| {
                                self.nav.cell_of(entity.position()).unwrap_or((0, 0))
                            }),
                            status: decision.reason,
                            complete: false,
                            expansions: 0,
                        });
                        succeeded
                    } else {
                        decision.reason = "no_target";
                        false
                    }
                }
                AiBehaviorInstruction::InteractTarget => {
                    if action_ran {
                        decision.reason = "action_already_attempted";
                        false
                    } else if let Some(target) = target {
                        action_ran = true;
                        if report.action_attempts >= self.config.budget.max_actions
                            || self.action_budget_remaining() == 0
                        {
                            decision.reason = "action_budget_reached";
                            decision.action_budget_exhausted = true;
                            false
                        } else {
                            let request_id =
                                format!("ai:{}:{}:{}", self.config.instance, self.tick.0, actor);
                            if self.action_results.contains_key(&request_id) {
                                decision.reason = "request_id_conflict";
                                false
                            } else {
                                let request = ActionRequest::new(request_id, actor, Verb::Use)
                                    .on(target)
                                    .at_tick(self.tick);
                                report.action_attempts += 1;
                                let result = self.apply_action(&request, now);
                                let succeeded = result.is_applied();
                                decision.reason = if succeeded {
                                    "interaction_applied"
                                } else {
                                    "interaction_rejected"
                                };
                                decision.action = Some(result);
                                succeeded
                            }
                        }
                    } else {
                        decision.reason = "no_target";
                        false
                    }
                }
                AiBehaviorInstruction::Patrol => {
                    if navigation_ran {
                        decision.reason = "navigation_already_attempted";
                        false
                    } else {
                        navigation_ran = true;
                        let (reason, waypoint) = self.patrol_ai_route(
                            actor,
                            &definition.id,
                            &definition.patrol_route,
                            policy.navigation_speed_m_per_s,
                            policy.tick_ms,
                        );
                        decision.reason = reason;
                        decision.patrol_waypoint = waypoint;
                        reason == "patrolling" || reason == "patrol_waypoint_reached"
                    }
                }
                AiBehaviorInstruction::Wait => unreachable!("wait terminates before evaluation"),
            };
            decision.target = target;
            decision.perception = perception.clone();
            current = if succeeded { on_success } else { on_failure };
        }
    }

    fn ai_target_in_range(&self, actor: StableId, target: StableId) -> bool {
        let Some(actor) = self.entities.get(&actor) else {
            return false;
        };
        let Some(target) = self.entities.get(&target) else {
            return false;
        };
        target.interactable.as_ref().is_some_and(|interaction| {
            actor.position().distance_xz(target.position()) <= interaction.range_m
        })
    }

    fn flee_ai_from(
        &mut self,
        actor: StableId,
        target: StableId,
        speed_m_per_s: f32,
        tick_ms: u32,
    ) -> bool {
        let (actor_position, target_position, radius) =
            match (self.entities.get(&actor), self.entities.get(&target)) {
                (Some(actor_entity), Some(target_entity)) => (
                    actor_entity.position(),
                    target_entity.position(),
                    actor_entity
                        .movement_profile()
                        .map_or(0.0, |profile| profile.radius_m),
                ),
                _ => return false,
            };
        let dx = actor_position.x - target_position.x;
        let dz = actor_position.z - target_position.z;
        let distance = dx.hypot(dz);
        if !distance.is_finite() || distance <= f32::EPSILON {
            return false;
        }
        let step = f64::from(speed_m_per_s) * f64::from(tick_ms) / 1_000.0;
        if !step.is_finite() || step <= 0.0 {
            return false;
        }
        // The endpoint only establishes direction; move_ai_toward caps the
        // actual displacement by the actor's server-owned speed profile.
        let reach = (step + f64::from(radius) + 1.0).max(1.0);
        #[allow(clippy::cast_possible_truncation)]
        let destination = Vec3::new(
            (f64::from(actor_position.x) + f64::from(dx / distance) * reach) as f32,
            actor_position.y,
            (f64::from(actor_position.z) + f64::from(dz / distance) * reach) as f32,
        );
        self.move_ai_toward(actor, destination, speed_m_per_s, tick_ms)
    }

    fn patrol_ai_route(
        &mut self,
        actor: StableId,
        behavior: &DefinitionId,
        route: &[Vec3],
        speed_m_per_s: f32,
        tick_ms: u32,
    ) -> (&'static str, Option<usize>) {
        if route.len() < 2 || route.len() > MAX_AI_PATROL_ROUTE_POINTS {
            return ("invalid_patrol_route", None);
        }
        let Some(position) = self.entities.get(&actor).map(EntityRecord::position) else {
            return ("actor_missing", None);
        };
        let cursor = self
            .ai_patrol_cursors
            .entry(actor)
            .or_insert((behavior.clone(), 0));
        if cursor.0 != *behavior || cursor.1 >= route.len() {
            *cursor = (behavior.clone(), 0);
        }
        let mut waypoint = cursor.1;
        let arrival_distance = (self.nav.cell_size() * 0.25).max(0.1);
        if position.distance_xz(route[waypoint]) <= arrival_distance {
            waypoint = (waypoint + 1) % route.len();
            cursor.1 = waypoint;
        }
        let destination = route[waypoint];
        if self.move_ai_toward(actor, destination, speed_m_per_s, tick_ms) {
            ("patrolling", Some(waypoint))
        } else {
            ("patrol_blocked", Some(waypoint))
        }
    }

    fn ai_target_is_hostile(
        &self,
        actor: StableId,
        target: StableId,
        factions: &FactionCatalog,
        thresholds: Thresholds,
        now: Millis,
    ) -> bool {
        let Some(actor) = self.entities.get(&actor) else {
            return false;
        };
        let Some(target) = self
            .entities
            .get(&target)
            .filter(|target| target.is_alive())
        else {
            return false;
        };
        let (Some(actor_faction), Some(target_faction)) =
            (actor.faction.as_ref(), target.faction.as_ref())
        else {
            return false;
        };
        resolve_alignment(
            factions,
            &actor_faction.faction,
            &target_faction.faction,
            Some(&actor_faction.reputation),
            now,
            &[],
        )
        .class(thresholds)
            == AlignmentClass::Enemy
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one AI navigation decision keeps cache, retry, and movement accounting ordered"
    )]
    fn navigate_ai_toward(
        &mut self,
        actor: StableId,
        target: StableId,
        speed_m_per_s: f32,
        tick_ms: u32,
        replan_policy: ReplanPolicy,
    ) -> AiNavigationReport {
        let Some(actor_entity) = self.entities.get(&actor) else {
            return AiNavigationReport {
                target,
                goal_cell: (0, 0),
                status: "actor_missing",
                complete: false,
                expansions: 0,
            };
        };
        let Some(target_entity) = self.entities.get(&target) else {
            self.ai_navigation.remove(&actor);
            return AiNavigationReport {
                target,
                goal_cell: (0, 0),
                status: "target_missing",
                complete: false,
                expansions: 0,
            };
        };
        let actor_position = actor_entity.position();
        let target_position = target_entity.position();
        let interaction_range = target_entity
            .interactable
            .as_ref()
            .map_or(0.0, |interactable| interactable.range_m);
        let actor_radius = actor_entity
            .movement_profile()
            .map_or(0.0, |profile| profile.radius_m);
        let target_radius = target_entity
            .movement_profile()
            .map_or(0.0, |profile| profile.radius_m);
        let goal_distance = (interaction_range - self.nav.cell_size() * 0.75)
            .max(actor_radius + target_radius + 0.05);
        let direction_x = target_position.x - actor_position.x;
        let direction_z = target_position.z - actor_position.z;
        let length = (direction_x * direction_x + direction_z * direction_z).sqrt();
        if !length.is_finite() || length == 0.0 {
            return AiNavigationReport {
                target,
                goal_cell: self.nav.cell_of(target_position).unwrap_or((0, 0)),
                status: "invalid_geometry",
                complete: false,
                expansions: 0,
            };
        }
        let goal = Vec3::new(
            target_position.x - direction_x / length * goal_distance,
            actor_position.y,
            target_position.z - direction_z / length * goal_distance,
        );
        let Some(goal_cell) = self.nav.cell_of(goal) else {
            return AiNavigationReport {
                target,
                goal_cell: (0, 0),
                status: "goal_out_of_bounds",
                complete: false,
                expansions: 0,
            };
        };

        let mut state = self.ai_navigation.remove(&actor).unwrap_or_default();
        if state.target != Some(target) || state.goal_cell != Some(goal_cell) {
            state = AiNavigationState {
                target: Some(target),
                goal_cell: Some(goal_cell),
                ..AiNavigationState::default()
            };
        }
        let mut status = "navigating";
        if state.abandoned {
            status = "navigation_abandoned";
        } else if state.path.is_none() {
            if state.replan.ready(self.tick) {
                match self.find_path(actor_position, goal, PartialPathPolicy::Fail) {
                    Ok(path) => {
                        state.replan.on_success();
                        state.next_waypoint = 0;
                        state.path = Some(path);
                    }
                    Err(error) => {
                        if state.replan.on_failure(self.tick, replan_policy).is_none() {
                            state.abandoned = true;
                        }
                        status = match error {
                            crate::nav::PathError::BudgetExhausted => "navigation_budget_exhausted",
                            crate::nav::PathError::NoPath => "navigation_no_path",
                            crate::nav::PathError::Blocked => "navigation_blocked",
                            crate::nav::PathError::OutOfBounds => "navigation_out_of_bounds",
                        };
                    }
                }
            } else {
                status = "navigation_backoff";
            }
        }

        let (complete, expansions) = state
            .path
            .as_ref()
            .map_or((false, 0), |path| (path.complete, path.expansions));
        if let Some(path) = state.path.as_ref() {
            if state.next_waypoint < path.waypoints.len() {
                let waypoint = path.waypoints[state.next_waypoint];
                let current = self
                    .entities
                    .get(&actor)
                    .map_or(actor_position, EntityRecord::position);
                let dx = f64::from(waypoint.x) - f64::from(current.x);
                let dz = f64::from(waypoint.z) - f64::from(current.z);
                let distance = (dx * dx + dz * dz).sqrt();
                if distance < 0.001 {
                    state.next_waypoint += 1;
                }
            }
            if let Some(waypoint) = state
                .path
                .as_ref()
                .and_then(|path| path.waypoints.get(state.next_waypoint))
                .copied()
            {
                if self.move_ai_toward(actor, waypoint, speed_m_per_s, tick_ms) {
                    status = if complete {
                        "navigating"
                    } else {
                        "following_partial_path"
                    };
                } else {
                    state.path = None;
                    if state.replan.on_failure(self.tick, replan_policy).is_none() {
                        state.abandoned = true;
                    }
                    status = "navigation_blocked";
                }
            } else {
                state.path = None;
                if self.ai_target_in_range(actor, target) {
                    status = "arrived";
                    state.replan.on_success();
                } else {
                    if state.replan.on_failure(self.tick, replan_policy).is_none() {
                        state.abandoned = true;
                    }
                    status = if complete {
                        "goal_reached_out_of_range"
                    } else {
                        "partial_path_exhausted"
                    };
                }
            }
        }
        self.ai_navigation.insert(actor, state);
        AiNavigationReport {
            target,
            goal_cell,
            status,
            complete,
            expansions,
        }
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

#[cfg_attr(coverage_nightly, coverage(off))]
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
            max_path_requests: 0,
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
            max_group_aggro_shares: 10,
            epoch: Millis::new(0),
            tick_ms: 50,
            navigation_speed_m_per_s: 2.0,
            navigation_replan: ReplanPolicy::default(),
            behaviors: vec![AiBehaviorDefinition {
                id: DefinitionId::new("behavior.guard").unwrap(),
                kind: AiBehaviorKind::Guard,
                range_m: 8.0,
                schedule: None,
                aggro_group: None,
                graph: None,
                patrol_route: Vec::new(),
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
    fn guard_chase_uses_bounded_paths_and_moves_under_server_authority() {
        let mut zone = tick_zone(1, 1);
        zone.config.budget.max_path_expansions = 512;
        zone.config.budget.max_path_requests = 2;
        zone.path_budget = zone.config.budget.path_tick_budget();
        let mut policy = zone.ai_policy.as_ref().unwrap().as_ref().clone();
        policy.behaviors[0].kind = AiBehaviorKind::GuardChase;
        zone.set_ai_tick_policy(Some(policy));
        zone.spawn(ai_at(1, "faction.guards", 1.5, 1.5)).unwrap();
        let mut target = ai_at(2, "faction.bandits", 6.5, 1.5);
        target.mind = Some(Mind::player(id(99), "session"));
        target.interactable = Some(Interactable::new(2.0, [Verb::Attack]));
        zone.spawn(target).unwrap();

        zone.advance_tick();
        let first = &zone.ai_tick_report().decisions[0];
        assert_eq!(first.reason, "navigating");
        let navigation = first.navigation.unwrap();
        assert!(navigation.expansions > 0);
        assert!(navigation.complete);
        assert!(zone.entity(id(1)).unwrap().position().x > 1.5);
        assert_eq!(zone.path_diagnostics().requests, 1);

        for _ in 0..64 {
            if zone.ai_tick_report().decisions[0].action.is_some() {
                break;
            }
            zone.advance_tick();
        }
        let decision = &zone.ai_tick_report().decisions[0];
        assert_eq!(decision.reason, "applied");
        assert_eq!(
            zone.entity(id(2)).unwrap().health.as_ref().unwrap().current,
            99
        );
        assert!(
            zone.entity(id(1))
                .unwrap()
                .position()
                .distance_xz(Vec3::new(6.5, 0.0, 1.5))
                <= 2.0
        );
        assert_eq!(zone.path_diagnostics().requests, 1);
    }

    #[test]
    fn guard_chase_backs_off_on_no_path_and_retries_after_blocker_expiry() {
        let mut zone = tick_zone(1, 1);
        zone.config.budget.max_path_expansions = 512;
        zone.config.budget.max_path_requests = 2;
        zone.path_budget = zone.config.budget.path_tick_budget();
        let mut policy = zone.ai_policy.as_ref().unwrap().as_ref().clone();
        policy.behaviors[0].kind = AiBehaviorKind::GuardChase;
        zone.set_ai_tick_policy(Some(policy));
        zone.spawn(ai_at(1, "faction.guards", 1.5, 1.5)).unwrap();
        let mut target = ai_at(2, "faction.bandits", 1.5, 7.5);
        target.mind = Some(Mind::player(id(99), "session"));
        target.interactable = Some(Interactable::new(2.0, [Verb::Attack]));
        zone.spawn(target).unwrap();
        for x in 0..10 {
            assert!(zone.add_dynamic_obstacle(x, 4, Tick(5)));
        }

        zone.advance_tick();
        assert_eq!(
            zone.ai_tick_report().decisions[0].reason,
            "navigation_no_path"
        );
        zone.advance_tick();
        assert_eq!(
            zone.ai_tick_report().decisions[0].reason,
            "navigation_no_path"
        );
        zone.advance_tick();
        assert_eq!(
            zone.ai_tick_report().decisions[0].reason,
            "navigation_backoff"
        );
        zone.advance_tick();
        assert_eq!(
            zone.ai_tick_report().decisions[0].reason,
            "navigation_no_path"
        );
        zone.advance_tick();
        assert_eq!(zone.ai_tick_report().decisions[0].reason, "navigating");
        assert_eq!(zone.path_diagnostics().requests, 4);
        assert!(zone.entity(id(1)).unwrap().position().z > 1.5);
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
            schedule: None,
            aggro_group: None,
            graph: None,
            patrol_route: Vec::new(),
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
    fn bounded_controller_scan_rotates_across_all_ai_controllers() {
        let mut zone = tick_zone(0, 1);
        let mut policy = zone.ai_policy.as_ref().unwrap().as_ref().clone();
        policy.max_controller_candidates = 1;
        zone.set_ai_tick_policy(Some(policy));
        for (seed, z) in [(1, 2.5), (2, 3.5), (3, 4.5)] {
            zone.spawn(ai_at(seed, "faction.guards", 1.5, z)).unwrap();
        }

        let mut served = BTreeSet::new();
        for _ in 0..6 {
            zone.advance_tick();
            let report = zone.ai_tick_report();
            assert_eq!(report.decisions.len(), 1);
            assert_eq!(report.controller_candidates, 1);
            served.insert(report.decisions[0].actor);
        }
        assert_eq!(served, BTreeSet::from([id(1), id(2), id(3)]));
    }

    #[test]
    fn rotated_controller_scan_replays_deterministically() {
        let setup = || {
            let mut zone = tick_zone(0, 1);
            let mut policy = zone.ai_policy.as_ref().unwrap().as_ref().clone();
            policy.max_controller_candidates = 2;
            zone.set_ai_tick_policy(Some(policy));
            for (seed, z) in [(1, 2.5), (2, 3.5), (3, 4.5), (4, 5.5)] {
                zone.spawn(ai_at(seed, "faction.guards", 1.5, z)).unwrap();
            }
            zone
        };
        let mut left = setup();
        let mut right = setup();
        for tick in 0..9 {
            left.advance_tick();
            right.advance_tick();
            assert_eq!(
                left.ai_tick_report(),
                right.ai_tick_report(),
                "tick {tick} diverged"
            );
            assert_eq!(left.ai_diagnostics(), right.ai_diagnostics());
        }
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
