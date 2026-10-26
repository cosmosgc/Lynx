//! Persistence-facing world-calendar contracts.
//!
//! Event lifecycle transitions and conflict resolution are authoritative
//! simulation results. Clients never call [`CalendarStore`]; the world service
//! validates an action first and persists the resulting [`CalendarSnapshot`].
//! [`CalendarService`] is the read-only calendar view exposed to authenticated
//! accounts.

use engine_core::faction::{FactionSnapshot, FactionWorld, TerritoryChange, TerritoryChangeKind};
use engine_core::time::Millis;
use engine_core::world_event::{
    CalendarSnapshot, CancellationPolicy, ConflictOutcome, ConflictResolution, ConflictRule,
    ConflictSideResult, EnrollmentRules, EventEnrollment, EventSchedule, EventSide, EventSideRole,
    WorldCalendar, WorldEvent, WorldEventStatus,
};
use engine_core::{DefinitionId, StableId};
use serde::{Deserialize, Serialize};

use crate::faction::map_engine_error;
use crate::service::{ApiError, SessionToken};

/// One conflict side as seen by a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventSideView {
    /// Stable side key.
    pub id: DefinitionId,
    /// Whether this side attacks or defends.
    pub role: EventSideRole,
    /// Player-facing side name.
    pub display_name: String,
    /// Faction fighting for this side.
    pub faction: DefinitionId,
    /// Participants enrolled on this side.
    pub participants: u32,
}

impl EventSideView {
    /// Build a client view from one authoritative side and its enrollment count.
    #[must_use]
    pub fn from_side(side: &EventSide, participants: u32) -> Self {
        Self {
            id: side.id.clone(),
            role: side.role,
            display_name: side.display_name.clone(),
            faction: side.faction.clone(),
            participants,
        }
    }
}

/// A public calendar event and its bounded conflict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldEventView {
    /// Stable event key.
    pub id: DefinitionId,
    /// Player-facing title.
    pub title: String,
    /// Optional authoring note.
    pub description: Option<String>,
    /// Absolute enrollment and conflict window.
    pub schedule: EventSchedule,
    /// The territory this conflict is about.
    pub target_zone: DefinitionId,
    /// Exactly one attacker and one defender, ordered by side ID.
    pub sides: Vec<EventSideView>,
    /// Current lifecycle state.
    pub status: WorldEventStatus,
    /// Enrollment eligibility and capacity.
    pub enrollment: EnrollmentRules,
    /// Versioned resolution rule.
    pub conflict: ConflictRule,
    /// When cancellation is accepted.
    pub cancellation: CancellationPolicy,
}

impl WorldEventView {
    /// Build a client view from an authoritative event and its calendar.
    #[must_use]
    pub fn from_event(event: &WorldEvent, calendar: &WorldCalendar) -> Self {
        Self {
            id: event.id.clone(),
            title: event.title.clone(),
            description: event.description.clone(),
            schedule: event.schedule,
            target_zone: event.target_zone.clone(),
            sides: event
                .sides
                .iter()
                .map(|side| {
                    EventSideView::from_side(side, calendar.participants(&event.id, &side.id))
                })
                .collect(),
            status: event.status,
            enrollment: event.enrollment.clone(),
            conflict: event.conflict.clone(),
            cancellation: event.cancellation,
        }
    }
}

/// One participant enrolled in an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventEnrollmentView {
    /// Character or AI entity.
    pub participant: StableId,
    /// Chosen side.
    pub side: DefinitionId,
    /// The participant's faction at enrollment, when known.
    pub faction: Option<DefinitionId>,
    /// When the participant enrolled.
    pub at: Millis,
}

impl EventEnrollmentView {
    /// Build a client view from an authoritative enrollment.
    #[must_use]
    pub fn from_enrollment(enrollment: &EventEnrollment) -> Self {
        Self {
            participant: enrollment.participant,
            side: enrollment.side.clone(),
            faction: enrollment.faction.clone(),
            at: enrollment.at,
        }
    }
}

/// A territory transfer produced by a resolved conflict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerritoryChangeView {
    /// Stable zone definition ID.
    pub zone: DefinitionId,
    /// What happened to the territory.
    pub action: TerritoryChangeKind,
    /// Previous owner.
    pub from: Option<DefinitionId>,
    /// New owner.
    pub to: Option<DefinitionId>,
    /// When the change was applied.
    pub at: Millis,
    /// Monotonic ownership revision.
    pub revision: u64,
}

impl TerritoryChangeView {
    /// Build a client view from an authoritative territory change.
    #[must_use]
    pub fn from_change(change: &TerritoryChange) -> Self {
        Self {
            zone: change.zone.clone(),
            action: change.action,
            from: change.from.clone(),
            to: change.to.clone(),
            at: change.at,
            revision: change.revision,
        }
    }
}

/// The durable, player-visible result of one conflict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictResolutionView {
    /// Event resolved.
    pub event: DefinitionId,
    /// When resolution was applied.
    pub resolved_at: Millis,
    /// The exact rule snapshot used.
    pub rule: ConflictRule,
    /// Winning side.
    pub winning_side: DefinitionId,
    /// Faction that won.
    pub winning_faction: DefinitionId,
    /// Whether the attacker took the territory or the defender retained it.
    pub outcome: ConflictOutcome,
    /// Both sides' inputs and scores, ordered by side ID.
    pub sides: Vec<ConflictSideResult>,
    /// The captured territory, present only for an attacker victory.
    pub territory_change: Option<TerritoryChangeView>,
}

impl ConflictResolutionView {
    /// Build a client view from an authoritative conflict result.
    #[must_use]
    pub fn from_resolution(resolution: &ConflictResolution) -> Self {
        Self {
            event: resolution.event.clone(),
            resolved_at: resolution.resolved_at,
            rule: resolution.rule.clone(),
            winning_side: resolution.winning_side.clone(),
            winning_faction: resolution.winning_faction.clone(),
            outcome: resolution.outcome,
            sides: resolution.sides.clone(),
            territory_change: resolution
                .territory_change
                .as_ref()
                .map(TerritoryChangeView::from_change),
        }
    }
}

/// Validate a calendar snapshot and its cross-references to a faction snapshot.
///
/// The adapter must run this before persisting either aggregate so a recovered
/// world cannot disagree about territory ownership or faction membership.
///
/// # Errors
///
/// Returns [`ApiError::Conflict`] for an inconsistent snapshot and
/// [`ApiError::InvalidRequest`] for malformed records.
pub fn validate_calendar_snapshot(
    snapshot: &CalendarSnapshot,
    factions: &FactionSnapshot,
) -> Result<(), ApiError> {
    let calendar =
        WorldCalendar::from_snapshot(snapshot.clone()).map_err(|error| map_engine_error(&error))?;
    let world =
        FactionWorld::from_snapshot(factions.clone()).map_err(|error| map_engine_error(&error))?;
    calendar
        .validate_against(&world)
        .map_err(|error| map_engine_error(&error))
}

/// Read-only calendar access for authenticated accounts.
///
/// Draft events are not part of the player-facing calendar and are treated as
/// absent by every method.
pub trait CalendarService {
    /// Return every published event, ordered by start time.
    fn events(&self, token: &SessionToken) -> Result<Vec<WorldEventView>, ApiError>;

    /// Return one published event, or [`ApiError::NotFound`].
    fn event(&self, token: &SessionToken, event: &DefinitionId)
    -> Result<WorldEventView, ApiError>;

    /// Return every enrollment for one published event.
    fn enrollments(
        &self,
        token: &SessionToken,
        event: &DefinitionId,
    ) -> Result<Vec<EventEnrollmentView>, ApiError>;

    /// Return the final result for one published event, when it has resolved.
    fn resolution(
        &self,
        token: &SessionToken,
        event: &DefinitionId,
    ) -> Result<Option<ConflictResolutionView>, ApiError>;
}

/// Trusted persistence access used by the authoritative world service.
///
/// This trait is not a client contract. Callers must have validated the
/// mutation through the simulation and must not expose these methods over a
/// player-facing endpoint.
pub trait CalendarStore {
    /// Load the last persisted calendar snapshot.
    fn load_calendar(&self) -> Result<CalendarSnapshot, ApiError>;

    /// Replace the persisted calendar snapshot atomically.
    ///
    /// Implementations must cross-validate the snapshot against the persisted
    /// faction world before replacing state.
    fn replace_calendar(&self, snapshot: &CalendarSnapshot) -> Result<(), ApiError>;
}
