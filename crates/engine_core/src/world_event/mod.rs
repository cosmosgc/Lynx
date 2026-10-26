//! Durable world calendar, event lifecycle, enrollment, and faction conflicts.
//!
//! Events are server state, not authored zone content. A conflict resolves once
//! per event, applies at most one territory change, and leaves the calendar and
//! faction world either both updated or both unchanged.

mod calendar;
mod conflict;

pub use calendar::{CalendarSnapshot, WorldCalendar};
pub use conflict::{ConflictOutcome, ConflictResolution, ConflictSideResult};

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{EngineError, ErrorCode};
use crate::faction::{MAX_ACTION_KEY_LEN, TerritoryChange};
use crate::id::{DefinitionId, StableId};
use crate::schema::SchemaVersion;
use crate::time::Millis;

/// Schema version for one durable world event.
pub const WORLD_EVENT_SCHEMA_VERSION: SchemaVersion = SchemaVersion::new(1, 0);

/// Schema version for the complete calendar snapshot.
pub const CALENDAR_SCHEMA_VERSION: SchemaVersion = SchemaVersion::new(1, 0);

/// The largest accepted event title.
pub const MAX_EVENT_TITLE_LEN: usize = 128;

/// The largest accepted event description.
pub const MAX_EVENT_DESCRIPTION_LEN: usize = 1_024;

/// The longest accepted scheduled event window (10 years).
pub const MAX_EVENT_WINDOW_MS: u64 = 315_576_000_000;

/// The lifecycle state of a durable world event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorldEventStatus {
    /// Being authored; never shown on the public calendar.
    Draft,
    /// Published with a fixed schedule and sides.
    Scheduled,
    /// Enrollment is open.
    Open,
    /// The conflict is running.
    Active,
    /// Temporarily stopped without resolving.
    Suspended,
    /// Stopped permanently without applying a result.
    Cancelled,
    /// Finished with a recorded and applied result.
    Resolved,
}

impl WorldEventStatus {
    /// The stable wire and database value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Scheduled => "scheduled",
            Self::Open => "open",
            Self::Active => "active",
            Self::Suspended => "suspended",
            Self::Cancelled => "cancelled",
            Self::Resolved => "resolved",
        }
    }

    /// Whether no further transition is accepted.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Cancelled | Self::Resolved)
    }

    /// Whether the event appears on the player-facing calendar.
    #[must_use]
    pub const fn is_public(self) -> bool {
        !matches!(self, Self::Draft)
    }
}

impl fmt::Display for WorldEventStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One audited lifecycle transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorldEventAction {
    /// The draft record was created.
    Created,
    /// The draft was published.
    Scheduled,
    /// Enrollment opened.
    Opened,
    /// The conflict started.
    Activated,
    /// The conflict paused.
    Suspended,
    /// The conflict resumed.
    Resumed,
    /// The event was cancelled without a result.
    Cancelled,
    /// The conflict finished and applied a result.
    Resolved,
}

impl WorldEventAction {
    /// The stable wire and database value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Scheduled => "scheduled",
            Self::Opened => "opened",
            Self::Activated => "activated",
            Self::Suspended => "suspended",
            Self::Resumed => "resumed",
            Self::Cancelled => "cancelled",
            Self::Resolved => "resolved",
        }
    }

    /// Whether this action requires an operator-visible reason.
    #[must_use]
    pub const fn requires_reason(self) -> bool {
        matches!(self, Self::Suspended | Self::Cancelled)
    }

    /// The only source state accepted for this action.
    #[must_use]
    pub const fn expected_from(self) -> Option<WorldEventStatus> {
        match self {
            Self::Created | Self::Cancelled => None,
            Self::Scheduled => Some(WorldEventStatus::Draft),
            Self::Opened => Some(WorldEventStatus::Scheduled),
            Self::Activated => Some(WorldEventStatus::Open),
            Self::Resumed => Some(WorldEventStatus::Suspended),
            Self::Suspended | Self::Resolved => Some(WorldEventStatus::Active),
        }
    }

    /// The only destination state produced by this action.
    #[must_use]
    pub const fn target(self) -> WorldEventStatus {
        match self {
            Self::Created => WorldEventStatus::Draft,
            Self::Scheduled => WorldEventStatus::Scheduled,
            Self::Opened => WorldEventStatus::Open,
            Self::Activated | Self::Resumed => WorldEventStatus::Active,
            Self::Suspended => WorldEventStatus::Suspended,
            Self::Cancelled => WorldEventStatus::Cancelled,
            Self::Resolved => WorldEventStatus::Resolved,
        }
    }

    /// Whether `self` may follow `from` to produce `to`.
    #[must_use]
    pub fn is_valid_transition(self, from: Option<WorldEventStatus>, to: WorldEventStatus) -> bool {
        if self == Self::Cancelled {
            return matches!(
                from,
                Some(
                    WorldEventStatus::Draft
                        | WorldEventStatus::Scheduled
                        | WorldEventStatus::Open
                        | WorldEventStatus::Active
                        | WorldEventStatus::Suspended
                )
            ) && to == WorldEventStatus::Cancelled;
        }
        self.expected_from() == from && self.target() == to
    }
}

impl fmt::Display for WorldEventAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The role a side plays in a bounded faction conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventSideRole {
    /// The side trying to take the target territory.
    Attacker,
    /// The side that currently owns the target territory.
    Defender,
}

impl EventSideRole {
    /// The stable wire and database value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Attacker => "attacker",
            Self::Defender => "defender",
        }
    }
}

impl fmt::Display for EventSideRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Absolute server timestamps for enrollment and the conflict itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventSchedule {
    /// When enrollment opens.
    pub enrollment_opens_at: Millis,
    /// When enrollment closes.
    pub enrollment_closes_at: Millis,
    /// When the conflict starts.
    pub starts_at: Millis,
    /// When the conflict ends and may be resolved.
    pub ends_at: Millis,
}

impl EventSchedule {
    /// Validate the ordering and length of the event window.
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.enrollment_opens_at >= self.enrollment_closes_at {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "event enrollment must open before it closes",
            )
            .with_detail("opens_at", self.enrollment_opens_at.value().to_string())
            .with_detail("closes_at", self.enrollment_closes_at.value().to_string()));
        }
        if self.enrollment_closes_at > self.starts_at {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "event enrollment must close no later than the conflict starts",
            )
            .with_detail("closes_at", self.enrollment_closes_at.value().to_string())
            .with_detail("starts_at", self.starts_at.value().to_string()));
        }
        if self.starts_at >= self.ends_at {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "event must start before it ends",
            )
            .with_detail("starts_at", self.starts_at.value().to_string())
            .with_detail("ends_at", self.ends_at.value().to_string()));
        }
        let window = self.ends_at.value().saturating_sub(self.starts_at.value());
        if window > i64::try_from(MAX_EVENT_WINDOW_MS).unwrap_or(i64::MAX) {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "event windows must be at most ten years",
            ));
        }
        Ok(())
    }

    /// Whether `now` is inside the enrollment window.
    #[must_use]
    pub fn enrollment_is_open_at(&self, now: Millis) -> bool {
        now >= self.enrollment_opens_at && now < self.enrollment_closes_at
    }
}

/// Configurable enrollment eligibility and capacity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollmentRules {
    /// Whether a participant must belong to a faction on the chosen side.
    pub requires_faction_membership: bool,
    /// Whether a participant with no faction may join any side.
    pub allow_unaffiliated: bool,
    /// Maximum participants per side, or `None` for no limit.
    pub max_participants_per_side: Option<u32>,
}

impl EnrollmentRules {
    /// Validate the rule combination.
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.requires_faction_membership && self.allow_unaffiliated {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "faction membership cannot be required while unaffiliated players are allowed",
            ));
        }
        if self.max_participants_per_side == Some(0) {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "per-side enrollment capacity must be positive",
            ));
        }
        Ok(())
    }
}

impl Default for EnrollmentRules {
    fn default() -> Self {
        Self {
            requires_faction_membership: true,
            allow_unaffiliated: false,
            max_participants_per_side: None,
        }
    }
}

/// The deterministic input used to score a conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictMetric {
    /// Each enrolled participant contributes one point.
    Participants,
    /// Each side's faction power contributes its documented power value.
    FactionPower,
}

impl ConflictMetric {
    /// The stable wire and database value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Participants => "participants",
            Self::FactionPower => "faction_power",
        }
    }
}

impl fmt::Display for ConflictMetric {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Versioned resolution inputs for a bounded conflict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictRule {
    /// Rule schema version recorded in every result.
    pub version: SchemaVersion,
    /// The scored input.
    pub metric: ConflictMetric,
    /// Minimum enrolled participants required on each side.
    pub minimum_participants_per_side: u32,
}

impl ConflictRule {
    /// Validate the rule version.
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.version.major == 0 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "conflict rule versions must have a non-zero major version",
            ));
        }
        Ok(())
    }
}

impl Default for ConflictRule {
    fn default() -> Self {
        Self {
            version: SchemaVersion::new(1, 0),
            metric: ConflictMetric::Participants,
            minimum_participants_per_side: 0,
        }
    }
}

/// When an operator may cancel an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancellationPolicy {
    /// Cancellation is accepted only before the conflict starts.
    BeforeStartOnly,
    /// Cancellation is accepted until the event resolves.
    AnyTimeBeforeResolution,
}

impl CancellationPolicy {
    /// The stable wire and database value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BeforeStartOnly => "before_start_only",
            Self::AnyTimeBeforeResolution => "any_time_before_resolution",
        }
    }
}

impl fmt::Display for CancellationPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One participating side of a bounded conflict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventSide {
    /// Stable side key such as `side.attackers`.
    pub id: DefinitionId,
    /// Whether this side attacks or defends.
    pub role: EventSideRole,
    /// Player-facing side name.
    pub display_name: String,
    /// The one faction fighting for this side.
    pub faction: DefinitionId,
}

impl EventSide {
    /// Validate one side.
    pub fn validate(&self) -> Result<(), EngineError> {
        if !self.id.is_in("side") {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "event side IDs must use the side namespace",
            )
            .with_detail("side", self.id.to_string()));
        }
        validate_label(&self.display_name, "event side display name")?;
        if !self.faction.is_in("faction") {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "event side factions must use the faction namespace",
            )
            .with_detail("side", self.id.to_string())
            .with_detail("faction", self.faction.to_string()));
        }
        Ok(())
    }
}

/// A durable, authored calendar event and its bounded faction conflict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldEvent {
    /// Stable event key such as `event.siege_town_square`.
    pub id: DefinitionId,
    /// Player-facing title.
    pub title: String,
    /// Optional authoring note.
    pub description: Option<String>,
    /// Absolute enrollment and conflict window.
    pub schedule: EventSchedule,
    /// The territory this conflict is about.
    pub target_zone: DefinitionId,
    /// Exactly two sides: one attacker and one defender.
    pub sides: Vec<EventSide>,
    /// Enrollment eligibility and capacity.
    pub enrollment: EnrollmentRules,
    /// Versioned resolution rule.
    pub conflict: ConflictRule,
    /// When cancellation is accepted.
    pub cancellation: CancellationPolicy,
    /// Current lifecycle state.
    pub status: WorldEventStatus,
    /// Record schema version.
    pub schema_version: SchemaVersion,
}

impl WorldEvent {
    /// Validate the event record without cross-checking factions or territories.
    pub fn validate(&self) -> Result<(), EngineError> {
        if !self.id.is_in("event") {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "world event IDs must use the event namespace",
            )
            .with_detail("event", self.id.to_string()));
        }
        validate_label(&self.title, "event title")?;
        if let Some(description) = &self.description {
            validate_optional_text(description, "event description", MAX_EVENT_DESCRIPTION_LEN)?;
        }
        self.schedule.validate()?;
        if !self.target_zone.is_in("zone") {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "event target zones must use the zone namespace",
            )
            .with_detail("event", self.id.to_string())
            .with_detail("zone", self.target_zone.to_string()));
        }
        if self.sides.len() != 2 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "a bounded conflict must have exactly two sides",
            )
            .with_detail("event", self.id.to_string())
            .with_detail("sides", self.sides.len().to_string()));
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut factions = std::collections::BTreeSet::new();
        let mut attackers = 0_u8;
        let mut defenders = 0_u8;
        for side in &self.sides {
            side.validate()?;
            if !ids.insert(side.id.clone()) {
                return Err(EngineError::new(
                    ErrorCode::Conflict,
                    "event contains a duplicate side ID",
                )
                .with_detail("event", self.id.to_string())
                .with_detail("side", side.id.to_string()));
            }
            if !factions.insert(side.faction.clone()) {
                return Err(EngineError::new(
                    ErrorCode::Conflict,
                    "an event faction cannot fight on both sides",
                )
                .with_detail("event", self.id.to_string())
                .with_detail("faction", side.faction.to_string()));
            }
            match side.role {
                EventSideRole::Attacker => attackers += 1,
                EventSideRole::Defender => defenders += 1,
            }
        }
        if attackers != 1 || defenders != 1 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "a bounded conflict needs exactly one attacker and one defender",
            )
            .with_detail("event", self.id.to_string()));
        }
        self.enrollment.validate()?;
        self.conflict.validate()?;
        if self.schema_version.major != WORLD_EVENT_SCHEMA_VERSION.major {
            return Err(EngineError::new(
                ErrorCode::VersionMismatch,
                "unsupported world event schema version",
            )
            .with_detail("event", self.id.to_string())
            .with_detail("version", self.schema_version.to_string()));
        }
        Ok(())
    }

    /// The attacker side.
    #[must_use]
    pub fn attacker(&self) -> Option<&EventSide> {
        self.sides
            .iter()
            .find(|side| side.role == EventSideRole::Attacker)
    }

    /// The defender side.
    #[must_use]
    pub fn defender(&self) -> Option<&EventSide> {
        self.sides
            .iter()
            .find(|side| side.role == EventSideRole::Defender)
    }

    /// Look up one side.
    #[must_use]
    pub fn side(&self, id: &DefinitionId) -> Option<&EventSide> {
        self.sides.iter().find(|side| &side.id == id)
    }

    /// Whether `faction` fights on the side identified by `side`.
    #[must_use]
    pub fn side_for_faction(&self, faction: &DefinitionId) -> Option<&EventSide> {
        self.sides.iter().find(|side| &side.faction == faction)
    }

    /// Whether this event is still non-terminal and therefore reserves its target.
    #[must_use]
    pub const fn reserves_target(&self) -> bool {
        matches!(
            self.status,
            WorldEventStatus::Scheduled
                | WorldEventStatus::Open
                | WorldEventStatus::Active
                | WorldEventStatus::Suspended
        )
    }
}

/// One audited lifecycle transition of an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventTransition {
    /// Unique audit key.
    pub key: String,
    /// The event that changed.
    pub event: DefinitionId,
    /// The transition applied.
    pub action: WorldEventAction,
    /// Previous state; `None` for creation.
    pub from: Option<WorldEventStatus>,
    /// New state.
    pub to: WorldEventStatus,
    /// Authenticated operator or system actor.
    pub actor: StableId,
    /// When the transition was applied.
    pub at: Millis,
    /// Required for suspension and cancellation.
    pub reason: Option<String>,
    /// Record schema version.
    pub schema_version: SchemaVersion,
}

impl EventTransition {
    /// Validate the transition's own fields.
    pub fn validate(&self) -> Result<(), EngineError> {
        validate_action_key(&self.key)?;
        if !self.event.is_in("event") {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "event transitions must reference event IDs",
            )
            .with_detail("key", &self.key));
        }
        if !self.action.is_valid_transition(self.from, self.to) {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "event transition does not match its previous and next states",
            )
            .with_detail("key", &self.key)
            .with_detail("action", self.action.as_str())
            .with_detail("from", self.from.map_or("none", WorldEventStatus::as_str))
            .with_detail("to", self.to.as_str()));
        }
        if self.action.requires_reason() {
            let reason = self.reason.as_deref().ok_or_else(|| {
                EngineError::new(
                    ErrorCode::InvalidRequest,
                    "this event transition requires a reason",
                )
                .with_detail("key", &self.key)
            })?;
            validate_reason(reason)?;
        } else if let Some(reason) = &self.reason {
            validate_reason(reason)?;
        }
        if self.schema_version.major != WORLD_EVENT_SCHEMA_VERSION.major {
            return Err(EngineError::new(
                ErrorCode::VersionMismatch,
                "unsupported event transition schema version",
            )
            .with_detail("key", &self.key));
        }
        Ok(())
    }
}

/// A participant bound to one event and one side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventEnrollment {
    /// Event joined.
    pub event: DefinitionId,
    /// Character or AI entity.
    pub participant: StableId,
    /// Chosen side.
    pub side: DefinitionId,
    /// The participant's faction at enrollment, when known.
    pub faction: Option<DefinitionId>,
    /// When the participant enrolled.
    pub at: Millis,
    /// Record schema version.
    pub schema_version: SchemaVersion,
}

impl EventEnrollment {
    /// Validate the enrollment's own fields.
    pub fn validate(&self) -> Result<(), EngineError> {
        if !self.event.is_in("event") {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "event enrollments must reference event IDs",
            )
            .with_detail("participant", self.participant.to_string()));
        }
        if !self.side.is_in("side") {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "event enrollments must reference side IDs",
            )
            .with_detail("event", self.event.to_string())
            .with_detail("participant", self.participant.to_string()));
        }
        if let Some(faction) = &self.faction
            && !faction.is_in("faction")
        {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "event enrollment factions must use the faction namespace",
            )
            .with_detail("event", self.event.to_string())
            .with_detail("faction", faction.to_string()));
        }
        if self.schema_version.major != WORLD_EVENT_SCHEMA_VERSION.major {
            return Err(EngineError::new(
                ErrorCode::VersionMismatch,
                "unsupported event enrollment schema version",
            )
            .with_detail("event", self.event.to_string())
            .with_detail("participant", self.participant.to_string()));
        }
        Ok(())
    }
}

/// Validate an idempotency or audit key.
pub(crate) fn validate_action_key(key: &str) -> Result<(), EngineError> {
    if key.is_empty() || key.len() > MAX_ACTION_KEY_LEN {
        return Err(EngineError::new(
            ErrorCode::InvalidRequest,
            "event action keys must be 1..=128 characters",
        ));
    }
    if key.chars().any(char::is_control) {
        return Err(EngineError::new(
            ErrorCode::InvalidRequest,
            "event action keys must not contain control characters",
        ));
    }
    Ok(())
}

/// Validate an operator-visible reason.
pub(crate) fn validate_reason(reason: &str) -> Result<(), EngineError> {
    if reason.trim().is_empty() || reason.len() > crate::faction::MAX_REASON_LEN {
        return Err(EngineError::new(
            ErrorCode::InvalidRequest,
            "event reasons must be 1..=512 non-blank characters",
        ));
    }
    if reason.chars().any(char::is_control) {
        return Err(EngineError::new(
            ErrorCode::InvalidRequest,
            "event reasons must not contain control characters",
        ));
    }
    Ok(())
}

fn validate_label(value: &str, subject: &str) -> Result<(), EngineError> {
    if value.trim().is_empty() || value.len() > MAX_EVENT_TITLE_LEN {
        return Err(EngineError::new(
            ErrorCode::InvalidRequest,
            format!("{subject} must be 1..={MAX_EVENT_TITLE_LEN} non-blank characters"),
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(EngineError::new(
            ErrorCode::InvalidRequest,
            format!("{subject} must not contain control characters"),
        ));
    }
    Ok(())
}

fn validate_optional_text(value: &str, subject: &str, limit: usize) -> Result<(), EngineError> {
    if value.len() > limit {
        return Err(EngineError::new(
            ErrorCode::InvalidRequest,
            format!("{subject} must be at most {limit} characters"),
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(EngineError::new(
            ErrorCode::InvalidRequest,
            format!("{subject} must not contain control characters"),
        ));
    }
    Ok(())
}

/// A territory change is the durable proof of an attacker victory.
#[must_use]
pub fn territory_change_for_resolution(
    event: &WorldEvent,
    winning_faction: &DefinitionId,
    change: &TerritoryChange,
) -> bool {
    let defender = event.defender().map(|side| &side.faction);
    change.zone == event.target_zone
        && change.from.as_ref() == defender
        && change.to.as_ref() == Some(winning_faction)
}
