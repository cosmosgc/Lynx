//! The durable calendar aggregate and its cross-aggregate conflict resolution.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::error::{EngineError, ErrorCode};
use crate::faction::{FactionWorld, TerritoryChange};
use crate::id::{DefinitionId, StableId};
use crate::schema::SchemaVersion;
use crate::time::Millis;

use super::conflict::{ConflictOutcome, ConflictResolution, ConflictSideResult};
use super::{
    CALENDAR_SCHEMA_VERSION, ConflictMetric, EventEnrollment, EventSide, EventSideRole,
    EventTransition, WORLD_EVENT_SCHEMA_VERSION, WorldEvent, WorldEventAction, WorldEventStatus,
    validate_reason,
};

/// A complete serializable calendar for persistence and recovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarSnapshot {
    /// Snapshot schema version.
    pub schema_version: SchemaVersion,
    /// Event records, ordered by ID.
    pub events: Vec<WorldEvent>,
    /// Append-only lifecycle transitions, oldest first.
    pub transitions: Vec<EventTransition>,
    /// Enrollment records, ordered by event then participant.
    pub enrollments: Vec<EventEnrollment>,
    /// Final conflict results, ordered by event.
    pub resolutions: Vec<ConflictResolution>,
}

impl Default for CalendarSnapshot {
    fn default() -> Self {
        Self {
            schema_version: CALENDAR_SCHEMA_VERSION,
            events: Vec::new(),
            transitions: Vec::new(),
            enrollments: Vec::new(),
            resolutions: Vec::new(),
        }
    }
}

impl CalendarSnapshot {
    /// Validate the snapshot by rebuilding a calendar from it.
    pub fn validate(&self) -> Result<(), EngineError> {
        WorldCalendar::from_snapshot(self.clone()).map(|_| ())
    }
}

/// Durable events, enrollment, transitions, and conflict results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldCalendar {
    events: BTreeMap<DefinitionId, WorldEvent>,
    transitions: Vec<EventTransition>,
    transition_keys: BTreeSet<String>,
    enrollments: BTreeMap<DefinitionId, BTreeMap<StableId, EventEnrollment>>,
    resolutions: BTreeMap<DefinitionId, ConflictResolution>,
}

impl WorldCalendar {
    /// An empty calendar.
    #[must_use]
    pub fn new() -> Self {
        Self {
            events: BTreeMap::new(),
            transitions: Vec::new(),
            transition_keys: BTreeSet::new(),
            enrollments: BTreeMap::new(),
            resolutions: BTreeMap::new(),
        }
    }

    /// Every event, ordered by stable ID.
    pub fn events(&self) -> impl Iterator<Item = &WorldEvent> {
        self.events.values()
    }

    /// Events visible on the player-facing calendar, ordered by start time.
    pub fn public_events(&self) -> Vec<&WorldEvent> {
        let mut events: Vec<&WorldEvent> = self
            .events
            .values()
            .filter(|event| event.status.is_public())
            .collect();
        events.sort_by_key(|event| (event.schedule.starts_at, event.id.clone()));
        events
    }

    /// One event.
    #[must_use]
    pub fn event(&self, event: &DefinitionId) -> Option<&WorldEvent> {
        self.events.get(event)
    }

    /// Append-only lifecycle history.
    #[must_use]
    pub fn transitions(&self) -> &[EventTransition] {
        &self.transitions
    }

    /// Every enrollment for one event, ordered by participant.
    pub fn enrollments(&self, event: &DefinitionId) -> impl Iterator<Item = &EventEnrollment> {
        self.enrollments
            .get(event)
            .into_iter()
            .flat_map(BTreeMap::values)
    }

    /// One participant's enrollment.
    #[must_use]
    pub fn enrollment(
        &self,
        event: &DefinitionId,
        participant: StableId,
    ) -> Option<&EventEnrollment> {
        self.enrollments
            .get(event)
            .and_then(|enrollments| enrollments.get(&participant))
    }

    /// Every final result, ordered by event.
    pub fn resolutions(&self) -> impl Iterator<Item = &ConflictResolution> {
        self.resolutions.values()
    }

    /// One event's final result.
    #[must_use]
    pub fn resolution(&self, event: &DefinitionId) -> Option<&ConflictResolution> {
        self.resolutions.get(event)
    }

    /// Number of enrolled participants on one side.
    #[must_use]
    pub fn participants(&self, event: &DefinitionId, side: &DefinitionId) -> u32 {
        self.enrollments.get(event).map_or(0, |enrollments| {
            enrollments
                .values()
                .filter(|enrollment| &enrollment.side == side)
                .count()
                .try_into()
                .unwrap_or(u32::MAX)
        })
    }

    /// Add a validated draft event.
    pub fn create(
        &mut self,
        mut event: WorldEvent,
        actor: StableId,
        at: Millis,
    ) -> Result<WorldEvent, EngineError> {
        if event.status != WorldEventStatus::Draft {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "new world events must start as drafts",
            )
            .with_detail("event", event.id.to_string())
            .with_detail("status", event.status.as_str()));
        }
        event.schema_version = WORLD_EVENT_SCHEMA_VERSION;
        event.validate()?;
        if self.events.contains_key(&event.id) {
            return Err(
                EngineError::new(ErrorCode::Conflict, "world event already exists")
                    .with_detail("event", event.id.to_string()),
            );
        }
        let created = EventTransition {
            key: self.next_transition_key(&event.id),
            event: event.id.clone(),
            action: WorldEventAction::Created,
            from: None,
            to: WorldEventStatus::Draft,
            actor,
            at,
            reason: None,
            schema_version: WORLD_EVENT_SCHEMA_VERSION,
        };
        created.validate()?;
        self.events.insert(event.id.clone(), event.clone());
        self.enrollments.insert(event.id.clone(), BTreeMap::new());
        self.push_transition(created);
        Ok(event)
    }

    /// Publish a draft with its fixed schedule.
    pub fn schedule(
        &mut self,
        factions: &FactionWorld,
        event: &DefinitionId,
        actor: StableId,
        at: Millis,
    ) -> Result<EventTransition, EngineError> {
        let current = self.event_ref(event)?.clone();
        if current.status != WorldEventStatus::Draft {
            return Err(transition_error(
                event,
                current.status,
                WorldEventAction::Scheduled,
            ));
        }
        if at >= current.schedule.enrollment_closes_at {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "the enrollment window has already closed",
            )
            .with_detail("event", event.to_string()));
        }
        current.validate()?;
        Self::validate_event_against_factions(&current, factions)?;
        let target = factions
            .territory(&current.target_zone)
            .ok_or_else(|| EngineError::not_found("territory", &current.target_zone))?;
        if target.contested {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "a contested territory cannot host a new event",
            )
            .with_detail("event", event.to_string())
            .with_detail("zone", current.target_zone.to_string()));
        }
        let defender = current.defender().expect("validated defender");
        if target.owner.as_ref() != Some(&defender.faction) {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "the defending side must currently own the target territory",
            )
            .with_detail("event", event.to_string())
            .with_detail("zone", current.target_zone.to_string()));
        }
        self.ensure_target_available(event, &current.target_zone)?;
        self.apply_transition(event, WorldEventAction::Scheduled, actor, at, None)
    }

    /// Open enrollment for a scheduled event.
    pub fn open(
        &mut self,
        event: &DefinitionId,
        actor: StableId,
        at: Millis,
    ) -> Result<EventTransition, EngineError> {
        let current = self.event_ref(event)?.clone();
        if current.status != WorldEventStatus::Scheduled {
            return Err(transition_error(
                event,
                current.status,
                WorldEventAction::Opened,
            ));
        }
        if !current.schedule.enrollment_is_open_at(at) {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "event enrollment is not open at this time",
            )
            .with_detail("event", event.to_string())
            .with_detail("now", at.value().to_string()));
        }
        self.apply_transition(event, WorldEventAction::Opened, actor, at, None)
    }

    /// Start the conflict and reserve its target territory.
    pub fn activate(
        &mut self,
        factions: &mut FactionWorld,
        event: &DefinitionId,
        actor: StableId,
        at: Millis,
    ) -> Result<EventTransition, EngineError> {
        let current = self.event_ref(event)?.clone();
        if current.status != WorldEventStatus::Open {
            return Err(transition_error(
                event,
                current.status,
                WorldEventAction::Activated,
            ));
        }
        if at < current.schedule.starts_at || at >= current.schedule.ends_at {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "the conflict cannot start outside its scheduled window",
            )
            .with_detail("event", event.to_string())
            .with_detail("now", at.value().to_string()));
        }
        let defender = current.defender().expect("validated defender");
        let territory = factions
            .territory(&current.target_zone)
            .ok_or_else(|| EngineError::not_found("territory", &current.target_zone))?;
        if territory.contested {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "the target territory is already contested",
            )
            .with_detail("event", event.to_string())
            .with_detail("zone", current.target_zone.to_string()));
        }
        if territory.owner.as_ref() != Some(&defender.faction) {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "the defending side must own the target territory at activation",
            )
            .with_detail("event", event.to_string())
            .with_detail("zone", current.target_zone.to_string()));
        }
        let mut next = factions.clone();
        next.set_territory_contested(&current.target_zone, true)?;
        let transition =
            self.apply_transition(event, WorldEventAction::Activated, actor, at, None)?;
        *factions = next;
        Ok(transition)
    }

    /// Pause an active conflict and release its target for ordinary rules.
    pub fn suspend(
        &mut self,
        factions: &mut FactionWorld,
        event: &DefinitionId,
        actor: StableId,
        at: Millis,
        reason: &str,
    ) -> Result<EventTransition, EngineError> {
        let current = self.event_ref(event)?.clone();
        if current.status != WorldEventStatus::Active {
            return Err(transition_error(
                event,
                current.status,
                WorldEventAction::Suspended,
            ));
        }
        validate_reason(reason)?;
        let mut next = factions.clone();
        next.set_territory_contested(&current.target_zone, false)?;
        let transition =
            self.apply_transition(event, WorldEventAction::Suspended, actor, at, Some(reason))?;
        *factions = next;
        Ok(transition)
    }

    /// Resume a suspended conflict if the target still belongs to the defender.
    pub fn resume(
        &mut self,
        factions: &mut FactionWorld,
        event: &DefinitionId,
        actor: StableId,
        at: Millis,
    ) -> Result<EventTransition, EngineError> {
        let current = self.event_ref(event)?.clone();
        if current.status != WorldEventStatus::Suspended {
            return Err(transition_error(
                event,
                current.status,
                WorldEventAction::Resumed,
            ));
        }
        if at >= current.schedule.ends_at {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "a suspended conflict past its end must be cancelled or rescheduled",
            )
            .with_detail("event", event.to_string())
            .with_detail("ends_at", current.schedule.ends_at.value().to_string()));
        }
        let defender = current.defender().expect("validated defender");
        let territory = factions
            .territory(&current.target_zone)
            .ok_or_else(|| EngineError::not_found("territory", &current.target_zone))?;
        if territory.contested || territory.owner.as_ref() != Some(&defender.faction) {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "the target territory changed hands and the conflict must be rescheduled",
            )
            .with_detail("event", event.to_string())
            .with_detail("zone", current.target_zone.to_string()));
        }
        let mut next = factions.clone();
        next.set_territory_contested(&current.target_zone, true)?;
        let transition =
            self.apply_transition(event, WorldEventAction::Resumed, actor, at, None)?;
        *factions = next;
        Ok(transition)
    }

    /// Cancel an event without applying any result.
    pub fn cancel(
        &mut self,
        factions: &mut FactionWorld,
        event: &DefinitionId,
        actor: StableId,
        at: Millis,
        reason: &str,
    ) -> Result<EventTransition, EngineError> {
        let current = self.event_ref(event)?.clone();
        if current.status.is_terminal() {
            return Err(transition_error(
                event,
                current.status,
                WorldEventAction::Cancelled,
            ));
        }
        if current.cancellation == super::CancellationPolicy::BeforeStartOnly
            && matches!(
                current.status,
                WorldEventStatus::Active | WorldEventStatus::Suspended
            )
        {
            return Err(EngineError::new(
                ErrorCode::PermissionDenied,
                "this conflict can no longer be cancelled",
            )
            .with_detail("event", event.to_string())
            .with_detail("status", current.status.as_str()));
        }
        validate_reason(reason)?;
        let mut next = factions.clone();
        if matches!(
            current.status,
            WorldEventStatus::Active | WorldEventStatus::Suspended
        ) {
            next.set_territory_contested(&current.target_zone, false)?;
        }
        let transition =
            self.apply_transition(event, WorldEventAction::Cancelled, actor, at, Some(reason))?;
        *factions = next;
        Ok(transition)
    }

    /// Resolve an active conflict after its scheduled end.
    ///
    /// Ties are retained by the defender. Replaying a resolved event returns
    /// the original result without mutating either aggregate.
    pub fn resolve(
        &mut self,
        factions: &mut FactionWorld,
        event: &DefinitionId,
        resolver: StableId,
        at: Millis,
    ) -> Result<ConflictResolution, EngineError> {
        if let Some(existing) = self.resolutions.get(event) {
            return Ok(existing.clone());
        }
        let current = self.event_ref(event)?.clone();
        if current.status != WorldEventStatus::Active {
            return Err(transition_error(
                event,
                current.status,
                WorldEventAction::Resolved,
            ));
        }
        if at < current.schedule.ends_at {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "the conflict cannot resolve before its scheduled end",
            )
            .with_detail("event", event.to_string())
            .with_detail("ends_at", current.schedule.ends_at.value().to_string()));
        }
        Self::validate_event_against_factions(&current, factions)?;
        Self::require_active_target(factions, &current)?;

        let side_results = self.score_sides(factions, &current)?;
        let attacker = side_result(&side_results, EventSideRole::Attacker, event)?;
        let defender_result = side_result(&side_results, EventSideRole::Defender, event)?;
        let (winning, outcome) = if attacker.score > defender_result.score {
            (attacker, ConflictOutcome::AttackerVictory)
        } else {
            (defender_result, ConflictOutcome::DefenderVictory)
        };

        let capture_key = format!("{}:result", current.id);
        let mut next = factions.clone();
        let territory_change = apply_outcome(
            &mut next,
            &current,
            &capture_key,
            resolver,
            &winning.faction,
            outcome,
            at,
        )?;

        let resolved_event = WorldEvent {
            status: WorldEventStatus::Resolved,
            ..current.clone()
        };
        let winning_side = winning.side.clone();
        let resolution = ConflictResolution::new(
            &resolved_event,
            resolver,
            at,
            winning_side,
            outcome,
            side_results,
            territory_change,
        )?;
        resolution.validate(&resolved_event)?;

        self.apply_transition(event, WorldEventAction::Resolved, resolver, at, None)?;
        self.resolutions.insert(event.clone(), resolution.clone());
        *factions = next;
        Ok(resolution)
    }

    fn score_sides(
        &self,
        factions: &FactionWorld,
        current: &WorldEvent,
    ) -> Result<Vec<ConflictSideResult>, EngineError> {
        let mut side_results = Vec::with_capacity(2);
        for side in &current.sides {
            let participants = self.participants(&current.id, &side.id);
            if participants < current.conflict.minimum_participants_per_side {
                return Err(EngineError::new(
                    ErrorCode::Conflict,
                    "a conflict side does not meet its minimum participation",
                )
                .with_detail("event", current.id.to_string())
                .with_detail("side", side.id.to_string())
                .with_detail("participants", participants.to_string())
                .with_detail(
                    "required",
                    current.conflict.minimum_participants_per_side.to_string(),
                ));
            }
            let faction_power = factions.power(&side.faction)?.power;
            let score = match current.conflict.metric {
                ConflictMetric::Participants => u64::from(participants),
                ConflictMetric::FactionPower => faction_power,
            };
            side_results.push(ConflictSideResult {
                side: side.id.clone(),
                role: side.role,
                faction: side.faction.clone(),
                participants,
                faction_power,
                score,
            });
        }
        side_results.sort_by(|left, right| left.side.cmp(&right.side));
        Ok(side_results)
    }

    /// Check that an active conflict still contests the defender's territory.
    fn require_active_target(
        factions: &FactionWorld,
        current: &WorldEvent,
    ) -> Result<(), EngineError> {
        let defender = current.defender().expect("validated defender");
        let territory = factions
            .territory(&current.target_zone)
            .ok_or_else(|| EngineError::not_found("territory", &current.target_zone))?;
        if territory.owner.as_ref() != Some(&defender.faction) || !territory.contested {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "an active conflict must still contest its defender's territory",
            )
            .with_detail("event", current.id.to_string())
            .with_detail("zone", current.target_zone.to_string()));
        }
        Ok(())
    }

    /// Enroll a participant in one side after verifying eligibility and capacity.
    ///
    /// The participant's faction is read from the authoritative faction world.
    /// Replaying the same side returns the original enrollment; choosing another
    /// side after enrolling is a conflict.
    pub fn enroll(
        &mut self,
        factions: &FactionWorld,
        event: &DefinitionId,
        participant: StableId,
        requested_side: &DefinitionId,
        at: Millis,
    ) -> Result<EventEnrollment, EngineError> {
        let current = self
            .event(event)
            .ok_or_else(|| EngineError::not_found("world event", event))?
            .clone();
        if current.status != WorldEventStatus::Open {
            return Err(
                EngineError::new(ErrorCode::Conflict, "event enrollment is not open")
                    .with_detail("event", event.to_string())
                    .with_detail("status", current.status.as_str()),
            );
        }
        if !current.schedule.enrollment_is_open_at(at) {
            return Err(
                EngineError::new(ErrorCode::Conflict, "the enrollment window is closed")
                    .with_detail("event", event.to_string())
                    .with_detail("now", at.value().to_string()),
            );
        }
        let side = current
            .side(requested_side)
            .ok_or_else(|| EngineError::not_found("event side", requested_side))?;
        let faction = factions.faction_of(participant).cloned();
        if let Some(existing) = self.enrollment(event, participant) {
            if existing.side == *requested_side {
                return Ok(existing.clone());
            }
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "a participant cannot change sides after enrolling",
            )
            .with_detail("event", event.to_string())
            .with_detail("participant", participant.to_string()));
        }
        if current.enrollment.requires_faction_membership {
            let membership = faction.as_ref().ok_or_else(|| {
                EngineError::new(
                    ErrorCode::PermissionDenied,
                    "event enrollment requires faction membership",
                )
                .with_detail("event", event.to_string())
                .with_detail("participant", participant.to_string())
            })?;
            if membership != &side.faction {
                return Err(EngineError::new(
                    ErrorCode::PermissionDenied,
                    "the participant does not belong to the side's faction",
                )
                .with_detail("event", event.to_string())
                .with_detail("side", requested_side.to_string()));
            }
        } else if let Some(membership) = &faction {
            if let Some(belonging) = current.side_for_faction(membership)
                && belonging.id != *requested_side
            {
                return Err(EngineError::new(
                    ErrorCode::PermissionDenied,
                    "the participant's faction fights on the other side",
                )
                .with_detail("event", event.to_string())
                .with_detail("side", requested_side.to_string()));
            }
        } else if !current.enrollment.allow_unaffiliated {
            return Err(EngineError::new(
                ErrorCode::PermissionDenied,
                "unaffiliated participants may not enroll in this event",
            )
            .with_detail("event", event.to_string())
            .with_detail("participant", participant.to_string()));
        }
        if let Some(capacity) = current.enrollment.max_participants_per_side
            && self.participants(event, requested_side) >= capacity
        {
            return Err(
                EngineError::new(ErrorCode::CapacityExceeded, "the event side is full")
                    .with_detail("event", event.to_string())
                    .with_detail("side", requested_side.to_string()),
            );
        }

        let enrollment = EventEnrollment {
            event: event.clone(),
            participant,
            side: requested_side.clone(),
            faction,
            at,
            schema_version: WORLD_EVENT_SCHEMA_VERSION,
        };
        enrollment.validate()?;
        self.enrollments
            .entry(event.clone())
            .or_default()
            .insert(participant, enrollment.clone());
        Ok(enrollment)
    }

    /// Capture the complete durable state.
    #[must_use]
    pub fn snapshot(&self) -> CalendarSnapshot {
        CalendarSnapshot {
            schema_version: CALENDAR_SCHEMA_VERSION,
            events: self.events.values().cloned().collect(),
            transitions: self.transitions.clone(),
            enrollments: self
                .enrollments
                .values()
                .flat_map(|enrollments| enrollments.values().cloned())
                .collect(),
            resolutions: self.resolutions.values().cloned().collect(),
        }
    }

    /// Rebuild and validate a calendar from persisted state.
    pub fn from_snapshot(snapshot: CalendarSnapshot) -> Result<Self, EngineError> {
        if snapshot.schema_version.major != CALENDAR_SCHEMA_VERSION.major {
            return Err(EngineError::new(
                ErrorCode::VersionMismatch,
                "unsupported calendar snapshot version",
            )
            .with_detail("version", snapshot.schema_version.to_string())
            .with_detail("supported_major", CALENDAR_SCHEMA_VERSION.major.to_string()));
        }
        let mut calendar = Self::new();
        let mut histories: BTreeMap<DefinitionId, Vec<EventTransition>> = BTreeMap::new();
        calendar.load_events(snapshot.events)?;
        calendar.load_transitions(snapshot.transitions, &mut histories)?;
        calendar.validate_histories(&histories)?;
        calendar.load_enrollments(snapshot.enrollments)?;
        calendar.validate_enrollment_capacity()?;
        calendar.load_resolutions(snapshot.resolutions)?;
        calendar.validate_internal()?;
        Ok(calendar)
    }

    fn load_events(&mut self, events: Vec<WorldEvent>) -> Result<(), EngineError> {
        for event in events {
            event.validate()?;
            if self.events.contains_key(&event.id) {
                return Err(EngineError::new(
                    ErrorCode::Conflict,
                    "calendar snapshot contains a duplicate event",
                )
                .with_detail("event", event.id.to_string()));
            }
            self.enrollments.insert(event.id.clone(), BTreeMap::new());
            self.events.insert(event.id.clone(), event);
        }
        Ok(())
    }

    fn load_transitions(
        &mut self,
        transitions: Vec<EventTransition>,
        histories: &mut BTreeMap<DefinitionId, Vec<EventTransition>>,
    ) -> Result<(), EngineError> {
        for transition in transitions {
            transition.validate()?;
            if !self.events.contains_key(&transition.event) {
                return Err(EngineError::not_found("world event", &transition.event));
            }
            if !self.transition_keys.insert(transition.key.clone()) {
                return Err(EngineError::new(
                    ErrorCode::Conflict,
                    "calendar snapshot contains a duplicate transition key",
                )
                .with_detail("key", &transition.key));
            }
            histories
                .entry(transition.event.clone())
                .or_default()
                .push(transition.clone());
            self.transitions.push(transition);
        }
        Ok(())
    }

    fn validate_histories(
        &self,
        histories: &BTreeMap<DefinitionId, Vec<EventTransition>>,
    ) -> Result<(), EngineError> {
        for (event_id, event) in &self.events {
            let history = histories.get(event_id).ok_or_else(|| {
                EngineError::new(
                    ErrorCode::InvalidState,
                    "world event has no lifecycle history",
                )
                .with_detail("event", event_id.to_string())
            })?;
            validate_history(event, history)?;
        }
        Ok(())
    }

    fn load_enrollments(&mut self, enrollments: Vec<EventEnrollment>) -> Result<(), EngineError> {
        for enrollment in enrollments {
            enrollment.validate()?;
            let event = self
                .events
                .get(&enrollment.event)
                .ok_or_else(|| EngineError::not_found("world event", &enrollment.event))?;
            let side = event
                .side(&enrollment.side)
                .ok_or_else(|| EngineError::not_found("event side", &enrollment.side))?;
            validate_enrollment(event, side, &enrollment)?;
            if self
                .enrollments
                .entry(enrollment.event.clone())
                .or_default()
                .insert(enrollment.participant, enrollment.clone())
                .is_some()
            {
                return Err(EngineError::new(
                    ErrorCode::Conflict,
                    "calendar snapshot contains a duplicate enrollment",
                )
                .with_detail("event", enrollment.event.to_string())
                .with_detail("participant", enrollment.participant.to_string()));
            }
        }
        Ok(())
    }

    fn validate_enrollment_capacity(&self) -> Result<(), EngineError> {
        for event in self.events.values() {
            for side in &event.sides {
                let participants = self.participants(&event.id, &side.id);
                if let Some(capacity) = event.enrollment.max_participants_per_side
                    && participants > capacity
                {
                    return Err(EngineError::new(
                        ErrorCode::InvalidState,
                        "event enrollment exceeds its per-side capacity",
                    )
                    .with_detail("event", event.id.to_string())
                    .with_detail("side", side.id.to_string()));
                }
            }
        }
        Ok(())
    }

    fn load_resolutions(
        &mut self,
        resolutions: Vec<ConflictResolution>,
    ) -> Result<(), EngineError> {
        for resolution in resolutions {
            let event = self
                .events
                .get(&resolution.event)
                .ok_or_else(|| EngineError::not_found("world event", &resolution.event))?
                .clone();
            resolution.validate(&event)?;
            if self
                .resolutions
                .insert(resolution.event.clone(), resolution)
                .is_some()
            {
                return Err(EngineError::new(
                    ErrorCode::Conflict,
                    "calendar snapshot contains a duplicate conflict resolution",
                ));
            }
        }
        Ok(())
    }

    /// Validate all cross-references against the authoritative faction world.
    pub fn validate_against(&self, factions: &FactionWorld) -> Result<(), EngineError> {
        self.validate_internal()?;
        let mut reserved: BTreeMap<DefinitionId, DefinitionId> = BTreeMap::new();
        for event in self.events.values() {
            Self::validate_event_against_factions(event, factions)?;
            let defender = event.defender().expect("validated defender");
            let territory = factions
                .territory(&event.target_zone)
                .ok_or_else(|| EngineError::not_found("territory", &event.target_zone))?;
            if event.reserves_target() {
                if let Some(other) = reserved.insert(event.target_zone.clone(), event.id.clone()) {
                    return Err(EngineError::new(
                        ErrorCode::Conflict,
                        "two non-terminal events reserve the same territory",
                    )
                    .with_detail("zone", event.target_zone.to_string())
                    .with_detail("first", other.to_string())
                    .with_detail("second", event.id.to_string()));
                }
                if event.status == WorldEventStatus::Active {
                    if !territory.contested {
                        return Err(EngineError::new(
                            ErrorCode::InvalidState,
                            "an active conflict must contest its target territory",
                        )
                        .with_detail("event", event.id.to_string())
                        .with_detail("zone", event.target_zone.to_string()));
                    }
                    if territory.owner.as_ref() != Some(&defender.faction) {
                        return Err(EngineError::new(
                            ErrorCode::InvalidState,
                            "an active conflict target must belong to the defender",
                        )
                        .with_detail("event", event.id.to_string())
                        .with_detail("zone", event.target_zone.to_string()));
                    }
                } else if territory.contested {
                    return Err(EngineError::new(
                        ErrorCode::InvalidState,
                        "a non-active conflict must not contest its target",
                    )
                    .with_detail("event", event.id.to_string())
                    .with_detail("zone", event.target_zone.to_string()));
                }
            }
            for side in &event.sides {
                if factions.faction(&side.faction).is_none() {
                    return Err(EngineError::not_found("faction", &side.faction));
                }
            }
            if let Some(resolution) = self.resolutions.get(&event.id) {
                resolution.validate(event)?;
                if let Some(change) = &resolution.territory_change
                    && !factions
                        .territory_history()
                        .iter()
                        .any(|recorded| recorded == change)
                {
                    return Err(EngineError::new(
                        ErrorCode::InvalidState,
                        "conflict result territory change is not in faction history",
                    )
                    .with_detail("event", event.id.to_string())
                    .with_detail("key", &change.key));
                }
            }
        }
        for (event_id, enrollments) in &self.enrollments {
            let event = self
                .events
                .get(event_id)
                .ok_or_else(|| EngineError::not_found("world event", event_id))?;
            for enrollment in enrollments.values() {
                let side = event
                    .side(&enrollment.side)
                    .ok_or_else(|| EngineError::not_found("event side", &enrollment.side))?;
                if let Some(faction) = &enrollment.faction
                    && faction != &side.faction
                {
                    return Err(EngineError::new(
                        ErrorCode::InvalidState,
                        "event enrollment faction does not match its side",
                    )
                    .with_detail("event", event_id.to_string())
                    .with_detail("participant", enrollment.participant.to_string()));
                }
            }
        }
        Ok(())
    }

    fn validate_internal(&self) -> Result<(), EngineError> {
        for event in self.events.values() {
            event.validate()?;
            if event.status == WorldEventStatus::Resolved
                && !self.resolutions.contains_key(&event.id)
            {
                return Err(EngineError::new(
                    ErrorCode::InvalidState,
                    "resolved event is missing its conflict result",
                )
                .with_detail("event", event.id.to_string()));
            }
            if event.status != WorldEventStatus::Resolved
                && self.resolutions.contains_key(&event.id)
            {
                return Err(EngineError::new(
                    ErrorCode::InvalidState,
                    "unresolved event has a conflict result",
                )
                .with_detail("event", event.id.to_string()));
            }
        }
        for (event_id, resolution) in &self.resolutions {
            for side in &resolution.sides {
                let enrolled = self.participants(event_id, &side.side);
                if side.participants != enrolled {
                    return Err(EngineError::new(
                        ErrorCode::InvalidState,
                        "conflict result participant count does not match enrollment",
                    )
                    .with_detail("event", event_id.to_string())
                    .with_detail("side", side.side.to_string())
                    .with_detail("recorded", side.participants.to_string())
                    .with_detail("enrolled", enrolled.to_string()));
                }
            }
        }
        Ok(())
    }

    fn validate_event_against_factions(
        event: &WorldEvent,
        factions: &FactionWorld,
    ) -> Result<(), EngineError> {
        let defender = event.defender().ok_or_else(|| {
            EngineError::new(ErrorCode::InvalidState, "event has no defender")
                .with_detail("event", event.id.to_string())
        })?;
        for side in &event.sides {
            if factions.faction(&side.faction).is_none() {
                return Err(EngineError::not_found("faction", &side.faction));
            }
        }
        let territory = factions
            .territory(&event.target_zone)
            .ok_or_else(|| EngineError::not_found("territory", &event.target_zone))?;
        if event.status != WorldEventStatus::Resolved
            && territory.owner.as_ref() != Some(&defender.faction)
        {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "the defending side must own the target territory",
            )
            .with_detail("event", event.id.to_string())
            .with_detail("zone", event.target_zone.to_string()));
        }
        Ok(())
    }

    fn ensure_target_available(
        &self,
        event: &DefinitionId,
        target: &DefinitionId,
    ) -> Result<(), EngineError> {
        for other in self.events.values() {
            if &other.id != event && other.target_zone == *target && other.reserves_target() {
                return Err(EngineError::new(
                    ErrorCode::Conflict,
                    "another non-terminal event already targets this territory",
                )
                .with_detail("zone", target.to_string())
                .with_detail("event", other.id.to_string()));
            }
        }
        Ok(())
    }

    fn event_ref(&self, event: &DefinitionId) -> Result<&WorldEvent, EngineError> {
        self.events
            .get(event)
            .ok_or_else(|| EngineError::not_found("world event", event))
    }

    fn apply_transition(
        &mut self,
        event: &DefinitionId,
        action: WorldEventAction,
        actor: StableId,
        at: Millis,
        reason: Option<&str>,
    ) -> Result<EventTransition, EngineError> {
        let current = self.event_ref(event)?.clone();
        let from = Some(current.status);
        let to = action.target();
        if !action.is_valid_transition(from, to) {
            return Err(transition_error(event, current.status, action));
        }
        let transition = EventTransition {
            key: self.next_transition_key(event),
            event: event.clone(),
            action,
            from,
            to,
            actor,
            at,
            reason: reason.map(str::to_owned),
            schema_version: WORLD_EVENT_SCHEMA_VERSION,
        };
        transition.validate()?;
        self.events
            .get_mut(event)
            .expect("validated event exists")
            .status = to;
        self.push_transition(transition.clone());
        Ok(transition)
    }

    fn next_transition_key(&self, event: &DefinitionId) -> String {
        let mut sequence = self.transitions.len() + 1;
        loop {
            let key = format!("{event}:{sequence}");
            if !self.transition_keys.contains(&key)
                && key.len() <= crate::faction::MAX_ACTION_KEY_LEN
            {
                return key;
            }
            sequence += 1;
        }
    }

    fn push_transition(&mut self, transition: EventTransition) {
        self.transition_keys.insert(transition.key.clone());
        self.transitions.push(transition);
    }
}

impl Default for WorldCalendar {
    fn default() -> Self {
        Self::new()
    }
}

fn transition_error(
    event: &DefinitionId,
    status: WorldEventStatus,
    action: WorldEventAction,
) -> EngineError {
    EngineError::new(
        ErrorCode::InvalidState,
        "event action is not valid in its current state",
    )
    .with_detail("event", event.to_string())
    .with_detail("status", status.as_str())
    .with_detail("action", action.as_str())
}

fn validate_history(event: &WorldEvent, history: &[EventTransition]) -> Result<(), EngineError> {
    let first = history.first().ok_or_else(|| {
        EngineError::new(
            ErrorCode::InvalidState,
            "world event has no creation transition",
        )
        .with_detail("event", event.id.to_string())
    })?;
    if first.action != WorldEventAction::Created
        || first.from.is_some()
        || first.to != WorldEventStatus::Draft
    {
        return Err(EngineError::new(
            ErrorCode::InvalidState,
            "world event history must begin with creation",
        )
        .with_detail("event", event.id.to_string()));
    }
    let mut previous = first.to;
    let mut previous_at = first.at;
    for transition in &history[1..] {
        if transition.from != Some(previous) {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "event history is not a contiguous transition chain",
            )
            .with_detail("event", event.id.to_string())
            .with_detail("key", &transition.key));
        }
        if transition.at < previous_at {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "event transitions must not move backwards in time",
            )
            .with_detail("event", event.id.to_string())
            .with_detail("key", &transition.key));
        }
        previous = transition.to;
        previous_at = transition.at;
    }
    if previous != event.status {
        return Err(EngineError::new(
            ErrorCode::InvalidState,
            "event status does not match its lifecycle history",
        )
        .with_detail("event", event.id.to_string())
        .with_detail("status", event.status.as_str())
        .with_detail("history", previous.as_str()));
    }
    Ok(())
}

fn side_result<'a>(
    sides: &'a [ConflictSideResult],
    role: EventSideRole,
    event: &DefinitionId,
) -> Result<&'a ConflictSideResult, EngineError> {
    sides.iter().find(|side| side.role == role).ok_or_else(|| {
        EngineError::new(ErrorCode::InvalidState, "conflict result is missing a side")
            .with_detail("event", event.to_string())
            .with_detail("role", role.as_str())
    })
}

/// Apply a conflict outcome to a cloned faction world, returning the capture
/// proof when the attacker wins.
fn apply_outcome(
    factions: &mut FactionWorld,
    current: &WorldEvent,
    capture_key: &str,
    resolver: StableId,
    winning_faction: &DefinitionId,
    outcome: ConflictOutcome,
    at: Millis,
) -> Result<Option<TerritoryChange>, EngineError> {
    match outcome {
        ConflictOutcome::AttackerVictory => {
            let defender = current.defender().expect("validated defender");
            factions
                .award_territory(
                    capture_key,
                    resolver,
                    winning_faction,
                    &current.target_zone,
                    &defender.faction,
                    at,
                )
                .map(Some)
        }
        ConflictOutcome::DefenderVictory => {
            factions.set_territory_contested(&current.target_zone, false)?;
            Ok(None)
        }
    }
}

/// Validate one persisted enrollment against its event and side rules.
fn validate_enrollment(
    event: &WorldEvent,
    side: &EventSide,
    enrollment: &EventEnrollment,
) -> Result<(), EngineError> {
    if let Some(faction) = &enrollment.faction
        && faction != &side.faction
    {
        return Err(EngineError::new(
            ErrorCode::InvalidState,
            "event enrollment faction does not match its side",
        )
        .with_detail("event", enrollment.event.to_string())
        .with_detail("participant", enrollment.participant.to_string()));
    }
    if event.enrollment.requires_faction_membership && enrollment.faction.is_none() {
        return Err(EngineError::new(
            ErrorCode::InvalidState,
            "event enrollment is missing a required faction",
        )
        .with_detail("event", enrollment.event.to_string())
        .with_detail("participant", enrollment.participant.to_string()));
    }
    if enrollment.faction.is_none() && !event.enrollment.allow_unaffiliated {
        return Err(EngineError::new(
            ErrorCode::InvalidState,
            "event does not allow unaffiliated participants",
        )
        .with_detail("event", enrollment.event.to_string())
        .with_detail("participant", enrollment.participant.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::faction::{
        Faction, FactionDefinition, GovernancePolicy, PowerRules, TerritoryRules,
    };
    use crate::id::IdKind;
    use crate::world_event::{
        CancellationPolicy, ConflictRule, EnrollmentRules, EventSchedule, EventSide,
    };

    fn faction_id(value: &str) -> DefinitionId {
        DefinitionId::new(value).unwrap()
    }

    fn zone(value: &str) -> DefinitionId {
        DefinitionId::new(value).unwrap()
    }

    fn side(value: &str) -> DefinitionId {
        DefinitionId::new(value).unwrap()
    }

    fn actor(seed: u128) -> StableId {
        StableId::from_parts(IdKind::Character, 0, seed)
    }

    fn world() -> (FactionWorld, DefinitionId, DefinitionId) {
        let mut world = FactionWorld::new(TerritoryRules {
            claim_protection: crate::time::DurationMs::new(0),
            capture_cooldown: crate::time::DurationMs::new(0),
            minimum_capture_power: 0,
            ..TerritoryRules::default()
        });
        let guards = faction_id("faction.guards");
        let bandits = faction_id("faction.bandits");
        for (id, founder, name) in [
            (guards.clone(), actor(1), "Guards"),
            (bandits.clone(), actor(2), "Bandits"),
        ] {
            world
                .add_faction(
                    Faction::new(
                        FactionDefinition::new(id, name),
                        GovernancePolicy::default(),
                        PowerRules::default(),
                        founder,
                        Millis::new(0),
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        world
            .deposit(
                "seed",
                actor(1),
                &guards,
                10_000,
                "war chest",
                Millis::new(0),
            )
            .unwrap();
        world.register_territory(zone("zone.town_square")).unwrap();
        world
            .claim_territory(
                "claim-1",
                actor(1),
                &guards,
                &zone("zone.town_square"),
                Millis::new(0),
            )
            .unwrap();
        (world, guards, bandits)
    }

    fn event() -> WorldEvent {
        WorldEvent {
            id: faction_id("event.siege"),
            title: "Siege of the square".into(),
            description: None,
            schedule: EventSchedule {
                enrollment_opens_at: Millis::new(10),
                enrollment_closes_at: Millis::new(20),
                starts_at: Millis::new(20),
                ends_at: Millis::new(30),
            },
            target_zone: zone("zone.town_square"),
            sides: vec![
                EventSide {
                    id: side("side.attackers"),
                    role: EventSideRole::Attacker,
                    display_name: "Bandits".into(),
                    faction: faction_id("faction.bandits"),
                },
                EventSide {
                    id: side("side.defenders"),
                    role: EventSideRole::Defender,
                    display_name: "Guards".into(),
                    faction: faction_id("faction.guards"),
                },
            ],
            enrollment: EnrollmentRules::default(),
            conflict: ConflictRule::default(),
            cancellation: CancellationPolicy::AnyTimeBeforeResolution,
            status: WorldEventStatus::Draft,
            schema_version: WORLD_EVENT_SCHEMA_VERSION,
        }
    }

    fn opened(world: &mut FactionWorld) -> (WorldCalendar, DefinitionId) {
        let mut calendar = WorldCalendar::new();
        let event = event();
        let id = event.id.clone();
        calendar.create(event, actor(9), Millis::new(1)).unwrap();
        calendar
            .schedule(world, &id, actor(9), Millis::new(2))
            .unwrap();
        calendar.open(&id, actor(9), Millis::new(10)).unwrap();
        (calendar, id)
    }

    fn started(world: &mut FactionWorld) -> (WorldCalendar, DefinitionId) {
        let (mut calendar, id) = opened(world);
        calendar
            .activate(world, &id, actor(9), Millis::new(20))
            .unwrap();
        (calendar, id)
    }

    #[test]
    fn lifecycle_requires_ordered_audited_transitions() {
        let (mut world, _, _) = world();
        let mut calendar = WorldCalendar::new();
        let event = event();
        let id = event.id.clone();
        calendar.create(event, actor(9), Millis::new(1)).unwrap();
        assert_eq!(calendar.event(&id).unwrap().status, WorldEventStatus::Draft);
        assert_eq!(
            calendar
                .open(&id, actor(9), Millis::new(10))
                .unwrap_err()
                .code(),
            ErrorCode::InvalidState
        );
        calendar
            .schedule(&world, &id, actor(9), Millis::new(2))
            .unwrap();
        assert_eq!(
            calendar
                .open(&id, actor(9), Millis::new(9))
                .unwrap_err()
                .code(),
            ErrorCode::Conflict
        );
        calendar.open(&id, actor(9), Millis::new(10)).unwrap();
        calendar
            .activate(&mut world, &id, actor(9), Millis::new(20))
            .unwrap();
        assert!(
            world
                .territory(&zone("zone.town_square"))
                .unwrap()
                .contested
        );
        calendar
            .suspend(&mut world, &id, actor(9), Millis::new(21), "storm")
            .unwrap();
        assert!(
            !world
                .territory(&zone("zone.town_square"))
                .unwrap()
                .contested
        );
        calendar
            .resume(&mut world, &id, actor(9), Millis::new(22))
            .unwrap();
        assert!(
            world
                .territory(&zone("zone.town_square"))
                .unwrap()
                .contested
        );
        assert_eq!(calendar.transitions().len(), 6);
        assert_eq!(
            calendar.event(&id).unwrap().status,
            WorldEventStatus::Active
        );
    }

    #[test]
    fn enrollment_checks_membership_side_and_capacity() {
        let (mut world, guards, _) = world();
        let (mut calendar, id) = opened(&mut world);
        let defender = side("side.defenders");
        let attacker = side("side.attackers");
        let enrollment = calendar
            .enroll(&world, &id, actor(1), &defender, Millis::new(11))
            .unwrap();
        assert_eq!(enrollment.faction, Some(guards));
        assert_eq!(calendar.participants(&id, &defender), 1);
        assert_eq!(
            calendar
                .enroll(&world, &id, actor(1), &attacker, Millis::new(12))
                .unwrap_err()
                .code(),
            ErrorCode::Conflict
        );
        assert_eq!(
            calendar
                .enroll(&world, &id, actor(7), &attacker, Millis::new(12))
                .unwrap_err()
                .code(),
            ErrorCode::PermissionDenied
        );
        assert_eq!(
            calendar
                .enroll(&world, &id, actor(1), &defender, Millis::new(10))
                .unwrap()
                .participant,
            actor(1)
        );
        assert_eq!(calendar.enrollments(&id).count(), 1);
    }

    #[test]
    fn sideless_members_cannot_enroll_when_membership_is_required() {
        let (mut world, _, bandits) = world();
        let (mut calendar, id) = opened(&mut world);
        world
            .invite(
                actor(2),
                &bandits,
                actor(3),
                crate::faction::FactionRole::Member,
                Millis::new(1),
            )
            .unwrap();
        let enrollment = calendar
            .enroll(
                &world,
                &id,
                actor(3),
                &side("side.attackers"),
                Millis::new(11),
            )
            .unwrap();
        assert_eq!(enrollment.faction, Some(bandits));
        assert_eq!(
            calendar
                .enroll(
                    &world,
                    &id,
                    actor(3),
                    &side("side.defenders"),
                    Millis::new(12)
                )
                .unwrap_err()
                .code(),
            ErrorCode::Conflict
        );
    }

    #[test]
    fn capacity_is_enforced_before_commit() {
        let (world, _, _) = world();
        let mut calendar = WorldCalendar::new();
        let mut definition = event();
        definition.enrollment.requires_faction_membership = false;
        definition.enrollment.allow_unaffiliated = true;
        definition.enrollment.max_participants_per_side = Some(1);
        let id = definition.id.clone();
        calendar
            .create(definition, actor(9), Millis::new(1))
            .unwrap();
        calendar
            .schedule(&world, &id, actor(9), Millis::new(2))
            .unwrap();
        calendar.open(&id, actor(9), Millis::new(10)).unwrap();
        calendar
            .enroll(
                &world,
                &id,
                actor(1),
                &side("side.defenders"),
                Millis::new(11),
            )
            .unwrap();
        assert_eq!(
            calendar
                .enroll(
                    &world,
                    &id,
                    actor(3),
                    &side("side.defenders"),
                    Millis::new(12)
                )
                .unwrap_err()
                .code(),
            ErrorCode::CapacityExceeded
        );
    }

    #[test]
    fn attacker_victory_captures_and_replays_once() {
        let (mut world, _, bandits) = world();
        let (mut calendar, id) = opened(&mut world);
        world
            .invite(
                actor(2),
                &bandits,
                actor(3),
                crate::faction::FactionRole::Member,
                Millis::new(1),
            )
            .unwrap();
        for participant in [actor(2), actor(3)] {
            calendar
                .enroll(
                    &world,
                    &id,
                    participant,
                    &side("side.attackers"),
                    Millis::new(11),
                )
                .unwrap();
        }
        calendar
            .enroll(
                &world,
                &id,
                actor(1),
                &side("side.defenders"),
                Millis::new(11),
            )
            .unwrap();
        calendar
            .activate(&mut world, &id, actor(9), Millis::new(20))
            .unwrap();
        let resolution = calendar
            .resolve(&mut world, &id, actor(9), Millis::new(30))
            .unwrap();
        assert_eq!(resolution.outcome, ConflictOutcome::AttackerVictory);
        assert_eq!(
            world.territory(&zone("zone.town_square")).unwrap().owner,
            Some(bandits)
        );
        assert!(
            !world
                .territory(&zone("zone.town_square"))
                .unwrap()
                .contested
        );
        assert!(resolution.territory_change.is_some());
        assert_eq!(
            calendar.event(&id).unwrap().status,
            WorldEventStatus::Resolved
        );
        let replay = calendar
            .resolve(&mut world, &id, actor(9), Millis::new(31))
            .unwrap();
        assert_eq!(resolution, replay);
        assert_eq!(calendar.transitions().len(), 5);
        assert_eq!(world.territory_history().len(), 2);
        calendar.validate_against(&world).unwrap();
        let restored = WorldCalendar::from_snapshot(calendar.snapshot()).unwrap();
        restored.validate_against(&world).unwrap();
        assert_eq!(
            restored.event(&id).unwrap().status,
            WorldEventStatus::Resolved
        );
    }

    #[test]
    fn defender_wins_ties_and_keeps_ownership() {
        let (mut world, guards, _) = world();
        let (mut calendar, id) = opened(&mut world);
        calendar
            .enroll(
                &world,
                &id,
                actor(1),
                &side("side.defenders"),
                Millis::new(11),
            )
            .unwrap();
        calendar
            .activate(&mut world, &id, actor(9), Millis::new(20))
            .unwrap();
        let resolution = calendar
            .resolve(&mut world, &id, actor(9), Millis::new(30))
            .unwrap();
        assert_eq!(resolution.outcome, ConflictOutcome::DefenderVictory);
        assert!(resolution.territory_change.is_none());
        assert_eq!(
            world.territory(&zone("zone.town_square")).unwrap().owner,
            Some(guards)
        );
        assert!(
            !world
                .territory(&zone("zone.town_square"))
                .unwrap()
                .contested
        );
    }

    #[test]
    fn cancellation_never_applies_a_result() {
        let (mut world, guards, _) = world();
        let (mut calendar, id) = started(&mut world);
        calendar
            .cancel(
                &mut world,
                &id,
                actor(9),
                Millis::new(21),
                "server maintenance",
            )
            .unwrap();
        assert_eq!(
            calendar.event(&id).unwrap().status,
            WorldEventStatus::Cancelled
        );
        assert!(calendar.resolution(&id).is_none());
        assert!(
            !world
                .territory(&zone("zone.town_square"))
                .unwrap()
                .contested
        );
        assert_eq!(
            world.territory(&zone("zone.town_square")).unwrap().owner,
            Some(guards)
        );
        assert_eq!(
            calendar
                .resolve(&mut world, &id, actor(9), Millis::new(30))
                .unwrap_err()
                .code(),
            ErrorCode::InvalidState
        );
    }

    #[test]
    fn snapshot_round_trip_preserves_history_and_rejects_tampering() {
        let (mut world, _, _) = world();
        let (mut calendar, id) = opened(&mut world);
        calendar
            .enroll(
                &world,
                &id,
                actor(1),
                &side("side.defenders"),
                Millis::new(11),
            )
            .unwrap();
        calendar
            .activate(&mut world, &id, actor(9), Millis::new(20))
            .unwrap();
        calendar
            .resolve(&mut world, &id, actor(9), Millis::new(30))
            .unwrap();
        let snapshot = calendar.snapshot();
        snapshot.validate().unwrap();
        calendar.validate_against(&world).unwrap();
        let reparsed: CalendarSnapshot =
            serde_json::from_str(&serde_json::to_string(&snapshot).unwrap()).unwrap();
        assert_eq!(reparsed, snapshot);
        assert_eq!(
            WorldCalendar::from_snapshot(reparsed).unwrap().snapshot(),
            snapshot
        );

        let mut broken = snapshot.clone();
        broken.events[0].status = WorldEventStatus::Active;
        assert_eq!(
            WorldCalendar::from_snapshot(broken).unwrap_err().code(),
            ErrorCode::InvalidState
        );
        let mut broken = snapshot.clone();
        broken.resolutions.clear();
        assert_eq!(
            WorldCalendar::from_snapshot(broken).unwrap_err().code(),
            ErrorCode::InvalidState
        );
        let mut broken = snapshot;
        broken.transitions[1].from = Some(WorldEventStatus::Active);
        assert_eq!(
            WorldCalendar::from_snapshot(broken).unwrap_err().code(),
            ErrorCode::InvalidState
        );
    }

    #[test]
    fn recovery_rejects_a_calendar_that_disagrees_with_factions() {
        let (mut world, _, _) = world();
        let (calendar, id) = started(&mut world);
        calendar.validate_against(&world).unwrap();
        let mut broken_world = world.clone();
        broken_world
            .set_territory_contested(&zone("zone.town_square"), false)
            .unwrap();
        assert_eq!(
            calendar.validate_against(&broken_world).unwrap_err().code(),
            ErrorCode::InvalidState
        );
        let mut broken = calendar.clone();
        broken.events.get_mut(&id).unwrap().sides[0].faction = faction_id("faction.ghost");
        assert_eq!(
            broken.validate_against(&world).unwrap_err().code(),
            ErrorCode::NotFound
        );
    }

    #[test]
    fn overlapping_conflicts_are_refused() {
        let (mut world, _, _) = world();
        let (mut calendar, _) = started(&mut world);
        let mut second = event();
        second.id = faction_id("event.second_siege");
        second.sides[0].faction = faction_id("faction.bandits");
        let second_id = second.id.clone();
        calendar.create(second, actor(9), Millis::new(2)).unwrap();
        assert_eq!(
            calendar
                .schedule(&world, &second_id, actor(9), Millis::new(3))
                .unwrap_err()
                .code(),
            ErrorCode::Conflict
        );
    }

    #[test]
    fn cancellation_policy_is_enforced() {
        let (mut world, _, _) = world();
        let mut calendar = WorldCalendar::new();
        let mut definition = event();
        definition.cancellation = CancellationPolicy::BeforeStartOnly;
        let id = definition.id.clone();
        calendar
            .create(definition, actor(9), Millis::new(1))
            .unwrap();
        calendar
            .schedule(&world, &id, actor(9), Millis::new(2))
            .unwrap();
        calendar.open(&id, actor(9), Millis::new(10)).unwrap();
        calendar
            .activate(&mut world, &id, actor(9), Millis::new(20))
            .unwrap();
        assert_eq!(
            calendar
                .cancel(&mut world, &id, actor(9), Millis::new(21), "late")
                .unwrap_err()
                .code(),
            ErrorCode::PermissionDenied
        );
    }
}
