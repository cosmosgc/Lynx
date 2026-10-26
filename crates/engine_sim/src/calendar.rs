//! The authoritative calendar command layer.
//!
//! Calendar lifecycle and enrollment are world-scoped operations, not
//! zone-entity verbs, so they run through their own command stage beside the
//! zone action pipeline. Requests carry intent only; `WorldCalendar` and
//! `FactionWorld` remain the sole authorities, every transition and enrollment
//! is validated against them, and a repeated request ID returns the original
//! result instead of applying the command twice.
//!
//! This module does not persist anything. After an applied command the caller
//! must commit the new [`WorldCalendar`] and [`FactionWorld`] snapshots
//! together through the trusted [`game_api::CalendarStore`] and
//! [`game_api::FactionStore`] seams, so a failed commit leaves no aggregate
//! behind.

use std::collections::{BTreeMap, VecDeque};

use engine_core::error::{EngineError, ErrorCode};
use engine_core::faction::{FactionWorld, TerritoryChange};
use engine_core::id::{DefinitionId, StableId};
use engine_core::time::Millis;
use engine_core::world_event::{ConflictOutcome, WorldCalendar, WorldEventStatus};
use serde::{Deserialize, Serialize};

/// Longest accepted calendar request ID, in bytes.
pub const MAX_CALENDAR_REQUEST_ID_LEN: usize = 128;

/// How many recent calendar results are retained for replay protection.
pub const CALENDAR_ACTION_CACHE_LIMIT: usize = 2_048;

/// A world-scoped calendar intent.
///
/// Enrollment is a participant intent. Every lifecycle operation is a
/// staff-level world operation; the calendar never accepts lifecycle changes
/// from an ordinary session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CalendarAction {
    /// Join one side of an open event.
    Enroll {
        /// Event to join.
        event: DefinitionId,
        /// Requested side.
        side: DefinitionId,
    },
    /// Publish a draft with its fixed schedule.
    Schedule {
        /// Event to publish.
        event: DefinitionId,
    },
    /// Open enrollment for a scheduled event.
    Open {
        /// Event to open.
        event: DefinitionId,
    },
    /// Start the conflict and contest its target territory.
    Activate {
        /// Event to activate.
        event: DefinitionId,
    },
    /// Pause an active conflict; requires a reason.
    Suspend {
        /// Event to pause.
        event: DefinitionId,
        /// Audit reason.
        reason: String,
    },
    /// Resume a suspended conflict.
    Resume {
        /// Event to resume.
        event: DefinitionId,
    },
    /// Cancel an event without applying any result; requires a reason.
    Cancel {
        /// Event to cancel.
        event: DefinitionId,
        /// Audit reason.
        reason: String,
    },
    /// Apply the deterministic result after the scheduled end.
    Resolve {
        /// Event to resolve.
        event: DefinitionId,
    },
}

/// A client- or operator-issued calendar intent.
///
/// Trusted code builds this from an authenticated session: the actor is
/// resolved server-side, `principal` carries the server-verified character,
/// and `staff` comes from session claims. A client can never set these to
/// impersonate another participant or a staff member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarActionRequest {
    /// Caller-scoped ID used for deduplication and replies.
    pub request_id: String,
    /// Acting character or AI entity.
    pub actor: StableId,
    /// Server-resolved principal, when the session provides one.
    pub principal: Option<StableId>,
    /// Whether the session carries a staff role. Set by the server only.
    pub staff: bool,
    /// Requested calendar intent.
    pub action: CalendarAction,
}

impl CalendarActionRequest {
    /// Start a request.
    #[must_use]
    pub fn new(request_id: impl Into<String>, actor: StableId, action: CalendarAction) -> Self {
        Self {
            request_id: request_id.into(),
            actor,
            principal: None,
            staff: false,
            action,
        }
    }

    /// Record the authenticated principal.
    #[must_use]
    pub fn by(mut self, principal: StableId) -> Self {
        self.principal = Some(principal);
        self
    }

    /// Mark the request as issued by a staff session.
    #[must_use]
    pub const fn with_staff(mut self) -> Self {
        self.staff = true;
        self
    }
}

/// A validated calendar state change produced by a command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CalendarEffect {
    /// A participant joined one side.
    Enrolled {
        /// Event joined.
        event: DefinitionId,
        /// Side joined.
        side: DefinitionId,
        /// Character or AI entity that enrolled.
        participant: StableId,
        /// The participant's faction at enrollment, when known.
        faction: Option<DefinitionId>,
    },
    /// The event moved to a new lifecycle status.
    StatusChanged {
        /// Event that changed.
        event: DefinitionId,
        /// New status.
        status: WorldEventStatus,
    },
    /// A conflict resolved with its deterministic result.
    Resolved {
        /// Event that resolved.
        event: DefinitionId,
        /// Whether the attacker captured or the defender retained.
        outcome: ConflictOutcome,
        /// Faction that won.
        winning_faction: DefinitionId,
        /// The captured territory, present only for an attacker victory.
        territory_change: Option<TerritoryChange>,
    },
}

/// Whether a calendar command applied or was rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CalendarActionOutcome {
    /// The command was applied and produced effects.
    Applied {
        /// Ordered effects for replication and UI.
        effects: Vec<CalendarEffect>,
    },
    /// The command failed validation.
    Rejected {
        /// Structured failure.
        error: EngineError,
    },
}

/// The complete server response to a calendar command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarActionResult {
    /// Echo of the request ID.
    pub request_id: String,
    /// Acting entity.
    pub actor: StableId,
    /// Requested intent.
    pub action: CalendarAction,
    /// Outcome.
    pub outcome: CalendarActionOutcome,
}

impl CalendarActionResult {
    /// Whether the command was applied.
    #[must_use]
    pub fn is_applied(&self) -> bool {
        matches!(self.outcome, CalendarActionOutcome::Applied { .. })
    }

    /// The applied effects, or an empty slice.
    #[must_use]
    pub fn effects(&self) -> &[CalendarEffect] {
        match &self.outcome {
            CalendarActionOutcome::Applied { effects } => effects,
            CalendarActionOutcome::Rejected { .. } => &[],
        }
    }

    /// The rejection, if any.
    #[must_use]
    pub fn error(&self) -> Option<&EngineError> {
        match &self.outcome {
            CalendarActionOutcome::Rejected { error } => Some(error),
            CalendarActionOutcome::Applied { .. } => None,
        }
    }
}

/// Replay-protected executor for world-scoped calendar commands.
///
/// The executor owns no aggregates; the caller supplies the authoritative
/// calendar and faction world for every command and persists both snapshots
/// after an applied result.
#[derive(Debug, Clone, Default)]
pub struct CalendarCommands {
    results: BTreeMap<String, CalendarActionResult>,
    order: VecDeque<String>,
}

impl CalendarCommands {
    /// Create an empty command stage.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of retained results.
    #[must_use]
    pub fn len(&self) -> usize {
        self.results.len()
    }

    /// Whether no results are retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.results.is_empty()
    }

    /// Validate and apply a calendar command.
    ///
    /// A repeated `request_id` returns the original result and never applies
    /// the command twice.
    pub fn execute(
        &mut self,
        calendar: &mut WorldCalendar,
        factions: &mut FactionWorld,
        request: &CalendarActionRequest,
        now: Millis,
    ) -> CalendarActionResult {
        if let Some(previous) = self.results.get(&request.request_id) {
            return previous.clone();
        }
        let outcome = match Self::apply(calendar, factions, request, now) {
            Ok(effects) => CalendarActionOutcome::Applied { effects },
            Err(error) => CalendarActionOutcome::Rejected { error },
        };
        let result = CalendarActionResult {
            request_id: request.request_id.clone(),
            actor: request.actor,
            action: request.action.clone(),
            outcome,
        };
        self.remember(&result);
        result
    }

    /// Whether a request ID already has a cached result.
    ///
    /// A caller that must commit applied state may use this to skip
    /// persistence for replayed requests: a cached result was effectively
    /// applied once already.
    #[must_use]
    pub fn contains(&self, request_id: &str) -> bool {
        self.results.contains_key(request_id)
    }

    fn remember(&mut self, result: &CalendarActionResult) {
        self.results
            .insert(result.request_id.clone(), result.clone());
        self.order.push_back(result.request_id.clone());
        while self.order.len() > CALENDAR_ACTION_CACHE_LIMIT {
            if let Some(oldest) = self.order.pop_front() {
                self.results.remove(&oldest);
            }
        }
    }

    fn apply(
        calendar: &mut WorldCalendar,
        factions: &mut FactionWorld,
        request: &CalendarActionRequest,
        now: Millis,
    ) -> Result<Vec<CalendarEffect>, EngineError> {
        validate_request(request)?;
        match &request.action {
            CalendarAction::Enroll { event, side } => {
                let enrollment = calendar.enroll(factions, event, request.actor, side, now)?;
                Ok(vec![CalendarEffect::Enrolled {
                    event: enrollment.event.clone(),
                    side: enrollment.side.clone(),
                    participant: enrollment.participant,
                    faction: enrollment.faction.clone(),
                }])
            }
            CalendarAction::Resolve { event } => {
                let resolution = calendar.resolve(factions, event, request.actor, now)?;
                Ok(vec![CalendarEffect::Resolved {
                    event: resolution.event.clone(),
                    outcome: resolution.outcome,
                    winning_faction: resolution.winning_faction.clone(),
                    territory_change: resolution.territory_change.clone(),
                }])
            }
            lifecycle => {
                let transition = match lifecycle {
                    CalendarAction::Schedule { event } => {
                        calendar.schedule(factions, event, request.actor, now)?
                    }
                    CalendarAction::Open { event } => calendar.open(event, request.actor, now)?,
                    CalendarAction::Activate { event } => {
                        calendar.activate(factions, event, request.actor, now)?
                    }
                    CalendarAction::Suspend { event, reason } => {
                        calendar.suspend(factions, event, request.actor, now, reason)?
                    }
                    CalendarAction::Resume { event } => {
                        calendar.resume(factions, event, request.actor, now)?
                    }
                    CalendarAction::Cancel { event, reason } => {
                        calendar.cancel(factions, event, request.actor, now, reason)?
                    }
                    CalendarAction::Enroll { .. } | CalendarAction::Resolve { .. } => {
                        unreachable!("handled above")
                    }
                };
                Ok(vec![CalendarEffect::StatusChanged {
                    event: transition.event.clone(),
                    status: transition.to,
                }])
            }
        }
    }
}

/// Validate request shape and authority before touching the aggregates.
fn validate_request(request: &CalendarActionRequest) -> Result<(), EngineError> {
    if request.request_id.is_empty() || request.request_id.len() > MAX_CALENDAR_REQUEST_ID_LEN {
        return Err(EngineError::new(
            ErrorCode::InvalidRequest,
            "a calendar command requires a bounded request id",
        )
        .with_detail("min_bytes", 1.to_string())
        .with_detail("max_bytes", MAX_CALENDAR_REQUEST_ID_LEN.to_string()));
    }
    if let Some(principal) = request.principal
        && principal != request.actor
    {
        return Err(EngineError::new(
            ErrorCode::PermissionDenied,
            "the principal does not control this character",
        )
        .with_detail("actor", request.actor.to_string()));
    }
    if !matches!(request.action, CalendarAction::Enroll { .. }) && !request.staff {
        return Err(EngineError::new(
            ErrorCode::PermissionDenied,
            "calendar lifecycle operations require a staff session",
        )
        .with_detail("actor", request.actor.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::faction::{
        Faction, FactionDefinition, GovernancePolicy, PowerRules, TerritoryRules,
    };
    use engine_core::id::IdKind;
    use engine_core::time::DurationMs;
    use engine_core::world_event::{
        CancellationPolicy, ConflictRule, EnrollmentRules, EventSchedule, EventSide, EventSideRole,
        WORLD_EVENT_SCHEMA_VERSION, WorldEvent,
    };

    fn id(value: &str) -> DefinitionId {
        DefinitionId::new(value).unwrap()
    }

    fn actor(seed: u128) -> StableId {
        StableId::from_parts(IdKind::Character, 0, seed)
    }

    fn world() -> (FactionWorld, DefinitionId, DefinitionId) {
        let mut world = FactionWorld::new(TerritoryRules {
            claim_protection: DurationMs::new(0),
            capture_cooldown: DurationMs::new(0),
            minimum_capture_power: 0,
            ..TerritoryRules::default()
        });
        let guards = id("faction.guards");
        let bandits = id("faction.bandits");
        for (faction, founder, name) in [
            (guards.clone(), actor(1), "Guards"),
            (bandits.clone(), actor(2), "Bandits"),
        ] {
            world
                .add_faction(
                    Faction::new(
                        FactionDefinition::new(faction, name),
                        GovernancePolicy::default(),
                        PowerRules::default(),
                        founder,
                        Millis::new(0),
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        world.register_territory(id("zone.town_square")).unwrap();
        world
            .claim_territory(
                "claim-1",
                actor(1),
                &guards,
                &id("zone.town_square"),
                Millis::new(0),
            )
            .unwrap();
        world
            .invite(
                actor(2),
                &bandits,
                actor(3),
                engine_core::faction::FactionRole::Member,
                Millis::new(1),
            )
            .unwrap();
        (world, guards, bandits)
    }

    fn event() -> WorldEvent {
        WorldEvent {
            id: id("event.siege"),
            title: "Siege of the square".into(),
            description: None,
            schedule: EventSchedule {
                enrollment_opens_at: Millis::new(10),
                enrollment_closes_at: Millis::new(20),
                starts_at: Millis::new(20),
                ends_at: Millis::new(30),
            },
            target_zone: id("zone.town_square"),
            sides: vec![
                EventSide {
                    id: id("side.attackers"),
                    role: EventSideRole::Attacker,
                    display_name: "Bandits".into(),
                    faction: id("faction.bandits"),
                },
                EventSide {
                    id: id("side.defenders"),
                    role: EventSideRole::Defender,
                    display_name: "Guards".into(),
                    faction: id("faction.guards"),
                },
            ],
            enrollment: EnrollmentRules::default(),
            conflict: ConflictRule::default(),
            cancellation: CancellationPolicy::AnyTimeBeforeResolution,
            status: WorldEventStatus::Draft,
            schema_version: WORLD_EVENT_SCHEMA_VERSION,
        }
    }

    fn draft() -> (WorldCalendar, DefinitionId) {
        let mut calendar = WorldCalendar::new();
        let event = event();
        let id = event.id.clone();
        calendar.create(event, actor(9), Millis::new(1)).unwrap();
        (calendar, id)
    }

    fn opened(world: &mut FactionWorld) -> (WorldCalendar, DefinitionId) {
        let (mut calendar, id) = draft();
        calendar
            .schedule(world, &id, actor(9), Millis::new(2))
            .unwrap();
        calendar.open(&id, actor(9), Millis::new(10)).unwrap();
        (calendar, id)
    }

    fn enroll_request(request_id: &str, participant: u128, side: &str) -> CalendarActionRequest {
        CalendarActionRequest::new(
            request_id,
            actor(participant),
            CalendarAction::Enroll {
                event: id("event.siege"),
                side: id(side),
            },
        )
        .by(actor(participant))
    }

    fn lifecycle_request(
        request_id: &str,
        actor_seed: u128,
        action: CalendarAction,
        staff: bool,
    ) -> CalendarActionRequest {
        let mut request =
            CalendarActionRequest::new(request_id, actor(actor_seed), action).by(actor(actor_seed));
        request.staff = staff;
        request
    }

    #[test]
    fn lifecycle_operations_require_a_staff_session() {
        let (mut world, _, _) = world();
        let (mut calendar, event_id) = draft();
        let mut commands = CalendarCommands::new();

        let rejected = commands.execute(
            &mut calendar,
            &mut world,
            &lifecycle_request(
                "r1",
                1,
                CalendarAction::Schedule {
                    event: event_id.clone(),
                },
                false,
            ),
            Millis::new(2),
        );
        assert_eq!(
            rejected.error().unwrap().code(),
            ErrorCode::PermissionDenied
        );
        assert_eq!(
            calendar.event(&event_id).unwrap().status,
            WorldEventStatus::Draft
        );

        let applied = commands.execute(
            &mut calendar,
            &mut world,
            &lifecycle_request(
                "r2",
                9,
                CalendarAction::Schedule {
                    event: event_id.clone(),
                },
                true,
            ),
            Millis::new(2),
        );
        assert!(applied.is_applied());
        assert_eq!(
            calendar.event(&event_id).unwrap().status,
            WorldEventStatus::Scheduled
        );
    }

    #[test]
    fn enrollment_replays_the_same_request_and_rejects_side_switches() {
        let (mut world, _, _) = world();
        let (mut calendar, event_id) = opened(&mut world);
        let mut commands = CalendarCommands::new();

        let applied = commands.execute(
            &mut calendar,
            &mut world,
            &enroll_request("r1", 2, "side.attackers"),
            Millis::new(11),
        );
        assert!(applied.is_applied());
        assert_eq!(
            applied.effects(),
            &[CalendarEffect::Enrolled {
                event: event_id.clone(),
                side: id("side.attackers"),
                participant: actor(2),
                faction: Some(id("faction.bandits")),
            }]
        );

        let replay = commands.execute(
            &mut calendar,
            &mut world,
            &enroll_request("r1", 2, "side.attackers"),
            Millis::new(12),
        );
        assert_eq!(replay, applied);
        assert_eq!(calendar.enrollments(&event_id).count(), 1);

        let switched = commands.execute(
            &mut calendar,
            &mut world,
            &enroll_request("r2", 2, "side.defenders"),
            Millis::new(12),
        );
        assert_eq!(switched.error().unwrap().code(), ErrorCode::Conflict);
        assert_eq!(calendar.enrollments(&event_id).count(), 1);
    }

    #[test]
    fn enrollment_cannot_impersonate_another_participant() {
        let (mut world, _, _) = world();
        let (mut calendar, _) = opened(&mut world);
        let mut commands = CalendarCommands::new();

        let request = CalendarActionRequest::new(
            "r1",
            actor(2),
            CalendarAction::Enroll {
                event: id("event.siege"),
                side: id("side.attackers"),
            },
        )
        .by(actor(3));
        let rejected = commands.execute(&mut calendar, &mut world, &request, Millis::new(11));
        assert_eq!(
            rejected.error().unwrap().code(),
            ErrorCode::PermissionDenied
        );
        assert_eq!(calendar.enrollments(&id("event.siege")).count(), 0);
    }

    #[test]
    fn staff_drives_the_full_lifecycle_to_a_capture() {
        let (mut world, _, _) = world();
        let (mut calendar, event_id) = opened(&mut world);
        let mut commands = CalendarCommands::new();

        for (request_id, participant, side) in [
            ("e1", 2u128, "side.attackers"),
            ("e2", 3, "side.attackers"),
            ("e3", 1, "side.defenders"),
        ] {
            assert!(
                commands
                    .execute(
                        &mut calendar,
                        &mut world,
                        &enroll_request(request_id, participant, side),
                        Millis::new(11),
                    )
                    .is_applied()
            );
        }

        for (request_id, action, expected) in [
            (
                "a1",
                CalendarAction::Activate {
                    event: event_id.clone(),
                },
                WorldEventStatus::Active,
            ),
            (
                "a2",
                CalendarAction::Suspend {
                    event: event_id.clone(),
                    reason: "storm".into(),
                },
                WorldEventStatus::Suspended,
            ),
            (
                "a3",
                CalendarAction::Resume {
                    event: event_id.clone(),
                },
                WorldEventStatus::Active,
            ),
        ] {
            let applied = commands.execute(
                &mut calendar,
                &mut world,
                &lifecycle_request(request_id, 9, action, true),
                Millis::new(20),
            );
            assert!(applied.is_applied());
            assert_eq!(
                applied.effects(),
                &[CalendarEffect::StatusChanged {
                    event: event_id.clone(),
                    status: expected,
                }]
            );
        }

        let resolved = commands.execute(
            &mut calendar,
            &mut world,
            &lifecycle_request(
                "a4",
                9,
                CalendarAction::Resolve {
                    event: event_id.clone(),
                },
                true,
            ),
            Millis::new(30),
        );
        assert!(resolved.is_applied());
        let CalendarEffect::Resolved {
            event: resolved_event,
            outcome,
            winning_faction,
            territory_change,
        } = &resolved.effects()[0]
        else {
            unreachable!("a resolved command yields one Resolved effect")
        };
        assert_eq!(resolved_event, &event_id);
        assert_eq!(outcome, &ConflictOutcome::AttackerVictory);
        assert_eq!(winning_faction, &id("faction.bandits"));
        let change = territory_change.as_ref().unwrap();
        assert_eq!(change.key, "event.siege:result");
        assert_eq!(change.zone, id("zone.town_square"));
        assert_eq!(
            change.action,
            engine_core::faction::TerritoryChangeKind::Capture
        );
        assert_eq!(change.from, Some(id("faction.guards")));
        assert_eq!(change.to, Some(id("faction.bandits")));
        assert_eq!(
            calendar.event(&event_id).unwrap().status,
            WorldEventStatus::Resolved
        );
        calendar.validate_against(&world).unwrap();
    }

    #[test]
    fn cancelling_an_event_never_applies_its_result() {
        let (mut world, _, _) = world();
        let (mut calendar, event_id) = opened(&mut world);
        let mut commands = CalendarCommands::new();

        assert!(
            commands
                .execute(
                    &mut calendar,
                    &mut world,
                    &enroll_request("e1", 2, "side.attackers"),
                    Millis::new(11),
                )
                .is_applied()
        );
        assert!(
            commands
                .execute(
                    &mut calendar,
                    &mut world,
                    &lifecycle_request(
                        "a1",
                        9,
                        CalendarAction::Activate {
                            event: event_id.clone()
                        },
                        true,
                    ),
                    Millis::new(20),
                )
                .is_applied()
        );
        assert!(
            commands
                .execute(
                    &mut calendar,
                    &mut world,
                    &lifecycle_request(
                        "a2",
                        9,
                        CalendarAction::Cancel {
                            event: event_id.clone(),
                            reason: "rain".into(),
                        },
                        true,
                    ),
                    Millis::new(21),
                )
                .is_applied()
        );
        assert!(!world.territory(&id("zone.town_square")).unwrap().contested);

        let rejected = commands.execute(
            &mut calendar,
            &mut world,
            &lifecycle_request(
                "a3",
                9,
                CalendarAction::Resolve {
                    event: event_id.clone(),
                },
                true,
            ),
            Millis::new(30),
        );
        assert_eq!(rejected.error().unwrap().code(), ErrorCode::InvalidState);
        assert!(calendar.resolution(&event_id).is_none());
        assert_eq!(
            world.territory(&id("zone.town_square")).unwrap().owner,
            Some(id("faction.guards"))
        );
    }

    #[test]
    fn rejections_are_cached_and_leave_the_world_unchanged() {
        let (mut world, _, _) = world();
        let (mut calendar, event_id) = draft();
        let mut commands = CalendarCommands::new();

        let rejected = commands.execute(
            &mut calendar,
            &mut world,
            &lifecycle_request(
                "r1",
                9,
                CalendarAction::Resolve {
                    event: event_id.clone(),
                },
                true,
            ),
            Millis::new(2),
        );
        assert_eq!(rejected.error().unwrap().code(), ErrorCode::InvalidState);

        let replay = commands.execute(
            &mut calendar,
            &mut world,
            &lifecycle_request(
                "r1",
                9,
                CalendarAction::Resolve {
                    event: event_id.clone(),
                },
                true,
            ),
            Millis::new(3),
        );
        assert_eq!(replay, rejected);
        assert_eq!(calendar.transitions().len(), 1);
        assert_eq!(commands.len(), 1);
    }

    #[test]
    fn the_replay_cache_is_bounded() {
        let (mut world, _, _) = world();
        let (mut calendar, _) = opened(&mut world);
        let mut commands = CalendarCommands::new();

        for sequence in 0..=CALENDAR_ACTION_CACHE_LIMIT {
            let applied = commands.execute(
                &mut calendar,
                &mut world,
                &enroll_request(&format!("r{sequence}"), 2, "side.attackers"),
                Millis::new(11),
            );
            assert!(applied.is_applied());
        }
        assert_eq!(commands.len(), CALENDAR_ACTION_CACHE_LIMIT);

        let retried = commands.execute(
            &mut calendar,
            &mut world,
            &enroll_request("r0", 2, "side.attackers"),
            Millis::new(12),
        );
        assert!(retried.is_applied());
        assert_eq!(commands.len(), CALENDAR_ACTION_CACHE_LIMIT);
        assert_eq!(calendar.enrollments(&id("event.siege")).count(), 1);
    }
}
