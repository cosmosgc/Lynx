//! The world calendar stage over the gateway wire.
//!
//! Calendar lifecycle and enrollment are world-scoped operations, not
//! zone-entity verbs, so they get their own envelope kinds beside the action
//! pipeline:
//!
//! - `calendar.command` carries a [`CalendarAction`] and a caller request ID.
//!   The gateway resolves the actor, principal, and staff flag from the
//!   authenticated session; a client body cannot name them.
//! - `calendar.result` correlates the outcome, including replayed results for
//!   repeated request IDs.
//! - `calendar.query` asks for the player-visible calendar; `calendar.state`
//!   replies with published events, side capacity, and conflict results. Draft
//!   events are never exposed to clients.
//!
//! Nothing is persisted inside this crate. Applied commands hand both
//! snapshots to [`CalendarPersistence`], which the owner must implement over
//! the trusted `game_api::CalendarStore` and `game_api::FactionStore` seams.

use engine_core::error::EngineError;
use engine_core::faction::{FactionSnapshot, FactionWorld, TerritoryChange};
use engine_core::id::DefinitionId;
use engine_core::time::Millis;
use engine_core::world_event::{
    ConflictOutcome, EventSchedule, EventSideRole, WorldCalendar, WorldEventStatus,
};
use engine_sim::calendar::{
    CalendarAction, CalendarActionRequest, CalendarActionResult, CalendarCommands,
};

/// A client request to execute one world calendar command.
pub const CALENDAR_COMMAND_KIND: &str = "calendar.command";

/// The server outcome of one calendar command, including replays.
pub const CALENDAR_RESULT_KIND: &str = "calendar.result";

/// A client request for the player-visible calendar.
pub const CALENDAR_QUERY_KIND: &str = "calendar.query";

/// The player-visible snapshot of the calendar.
pub const CALENDAR_STATE_KIND: &str = "calendar.state";

/// The body of `calendar.command`.
///
/// Only intent is accepted: the actor, principal, and staff flag are set by
/// the gateway from the authenticated session.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalendarCommandIntent {
    /// Caller-scoped ID used for deduplication and replies.
    pub request_id: String,
    /// Requested calendar intent.
    pub action: CalendarAction,
}

/// The body of `calendar.query`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalendarQuery {}

/// Trusted persistence for applied calendar commands.
///
/// Implementations run in the authority loop and must commit both snapshots
/// atomically: a failed commit is reported, the in-memory aggregates remain
/// authoritative, and the operator must reconcile before trusting a restart.
pub trait CalendarPersistence {
    /// Commit calendar and faction snapshots after one applied command.
    ///
    /// # Errors
    ///
    /// Returns an engine error describing the persistence failure.
    fn commit(
        &self,
        calendar: &engine_core::world_event::CalendarSnapshot,
        factions: &FactionSnapshot,
    ) -> Result<(), EngineError>;
}

/// One published event as clients may see it in `calendar.state`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CalendarEventView {
    /// Stable event ID.
    pub event: DefinitionId,
    /// Player-facing title.
    pub title: String,
    /// Current lifecycle state.
    pub status: WorldEventStatus,
    /// Absolute enrollment and conflict window.
    pub schedule: EventSchedule,
    /// The conflict's target territory.
    pub target_zone: DefinitionId,
    /// Both sides with their current participant counts.
    pub sides: Vec<CalendarSideView>,
    /// The deterministic result, present only after resolution.
    pub result: Option<CalendarResultView>,
}

/// One conflict side as clients may see it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CalendarSideView {
    /// Stable side ID clients use in enrollment requests.
    pub side: DefinitionId,
    /// Whether this side attacks or defends.
    pub role: EventSideRole,
    /// Player-facing side name.
    pub display_name: String,
    /// The faction fielding this side.
    pub faction: DefinitionId,
    /// Current participant count on this side.
    pub participants: u32,
}

/// A resolved conflict as clients may see it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CalendarResultView {
    /// Whether the attacker captured or the defender retained.
    pub outcome: ConflictOutcome,
    /// The winning faction.
    pub winning_faction: DefinitionId,
    /// The captured territory, present only for an attacker victory.
    pub territory_change: Option<TerritoryChange>,
}

/// The body of `calendar.state`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CalendarStateBody {
    /// Published events, ordered by event ID.
    pub events: Vec<CalendarEventView>,
}

/// The calendar aggregates and the replay-protected command stage.
///
/// The gateway owns this when the world runs a calendar; the stage keeps both
/// versioned snapshots that an applied command changed so the owner can commit
/// them together.
#[derive(Debug)]
pub struct CalendarStage {
    calendar: WorldCalendar,
    factions: FactionWorld,
    commands: CalendarCommands,
}

impl CalendarStage {
    /// Cross-validate and adopt calendar plus faction state.
    ///
    /// # Errors
    ///
    /// Returns an [`EngineError`] when the calendar disagrees with the
    /// faction world about enrollment, sides, or territories.
    pub fn new(calendar: WorldCalendar, factions: FactionWorld) -> Result<Self, EngineError> {
        calendar.validate_against(&factions)?;
        Ok(Self {
            calendar,
            factions,
            commands: CalendarCommands::new(),
        })
    }

    /// The authoritative calendar. Authoritative systems may read it.
    #[must_use]
    pub const fn calendar(&self) -> &WorldCalendar {
        &self.calendar
    }

    /// The authoritative faction world. Authoritative systems may read it.
    #[must_use]
    pub const fn factions(&self) -> &FactionWorld {
        &self.factions
    }

    /// Number of retained command results.
    #[must_use]
    pub fn command_cache_len(&self) -> usize {
        self.commands.len()
    }

    /// Execute one server-resolved calendar command.
    #[must_use]
    pub fn execute(
        &mut self,
        request: &CalendarActionRequest,
        now: Millis,
    ) -> CalendarActionResult {
        self.commands
            .execute(&mut self.calendar, &mut self.factions, request, now)
    }

    /// Both versioned snapshots of the current aggregate state.
    #[must_use]
    pub fn snapshots(&self) -> (engine_core::world_event::CalendarSnapshot, FactionSnapshot) {
        (self.calendar.snapshot(), self.factions.snapshot())
    }

    /// The player-visible calendar state.
    ///
    /// Draft events stay hidden, mirroring `game_api::CalendarService`.
    #[must_use]
    pub fn state(&self) -> CalendarStateBody {
        let events =
            self.calendar
                .public_events()
                .iter()
                .map(|event| CalendarEventView {
                    event: event.id.clone(),
                    title: event.title.clone(),
                    status: event.status,
                    schedule: event.schedule,
                    target_zone: event.target_zone.clone(),
                    sides: event
                        .sides
                        .iter()
                        .map(|side| CalendarSideView {
                            side: side.id.clone(),
                            role: side.role,
                            display_name: side.display_name.clone(),
                            faction: side.faction.clone(),
                            participants: self.calendar.participants(&event.id, &side.id),
                        })
                        .collect(),
                    result: self.calendar.resolution(&event.id).map(|resolution| {
                        CalendarResultView {
                            outcome: resolution.outcome,
                            winning_faction: resolution.winning_faction.clone(),
                            territory_change: resolution.territory_change.clone(),
                        }
                    }),
                })
                .collect();
        CalendarStateBody { events }
    }
}
