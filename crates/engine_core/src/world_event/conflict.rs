//! Deterministic conflict resolution records.

use serde::{Deserialize, Serialize};

use crate::error::{EngineError, ErrorCode};
use crate::faction::TerritoryChange;
use crate::id::{DefinitionId, StableId};
use crate::schema::SchemaVersion;
use crate::time::Millis;

use super::{
    ConflictRule, EventSideRole, WORLD_EVENT_SCHEMA_VERSION, WorldEvent, WorldEventStatus,
    territory_change_for_resolution,
};

/// Whether the attacker took the territory or the defender retained it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictOutcome {
    /// The attacker captured the target territory.
    AttackerVictory,
    /// The defender kept the target territory, including on a score tie.
    DefenderVictory,
}

impl ConflictOutcome {
    /// The stable wire and database value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AttackerVictory => "attacker_victory",
            Self::DefenderVictory => "defender_victory",
        }
    }
}

/// One side's recorded resolution inputs and score.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictSideResult {
    /// Side key.
    pub side: DefinitionId,
    /// Side role.
    pub role: EventSideRole,
    /// Faction fighting for the side.
    pub faction: DefinitionId,
    /// Enrolled participants at resolution.
    pub participants: u32,
    /// Faction power at resolution.
    pub faction_power: u64,
    /// The score selected by the conflict rule.
    pub score: u64,
}

impl ConflictSideResult {
    /// Validate one side result.
    pub fn validate(&self) -> Result<(), EngineError> {
        if !self.side.is_in("side") || !self.faction.is_in("faction") {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "conflict side results must use side and faction IDs",
            )
            .with_detail("side", self.side.to_string())
            .with_detail("faction", self.faction.to_string()));
        }
        Ok(())
    }
}

/// The durable, idempotent result of one conflict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictResolution {
    /// Event resolved.
    pub event: DefinitionId,
    /// Authenticated operator or system actor that resolved it.
    pub resolver: StableId,
    /// When resolution was applied.
    pub resolved_at: Millis,
    /// The exact rule snapshot used.
    pub rule: ConflictRule,
    /// Winning side.
    pub winning_side: DefinitionId,
    /// Faction that won.
    pub winning_faction: DefinitionId,
    /// Whether the attacker took the territory.
    pub outcome: ConflictOutcome,
    /// Both sides' inputs and scores, ordered by side ID.
    pub sides: Vec<ConflictSideResult>,
    /// The captured territory change, present only for an attacker victory.
    pub territory_change: Option<TerritoryChange>,
    /// Record schema version.
    pub schema_version: SchemaVersion,
}

impl ConflictResolution {
    /// Build and validate a resolution record.
    pub fn new(
        event: &WorldEvent,
        resolver: StableId,
        resolved_at: Millis,
        winning_side: DefinitionId,
        outcome: ConflictOutcome,
        sides: Vec<ConflictSideResult>,
        territory_change: Option<TerritoryChange>,
    ) -> Result<Self, EngineError> {
        let winning = event.side(&winning_side).ok_or_else(|| {
            EngineError::new(
                ErrorCode::InvalidState,
                "conflict resolution references an unknown side",
            )
            .with_detail("event", event.id.to_string())
            .with_detail("side", winning_side.to_string())
        })?;
        let expected_outcome = match winning.role {
            EventSideRole::Attacker => ConflictOutcome::AttackerVictory,
            EventSideRole::Defender => ConflictOutcome::DefenderVictory,
        };
        if outcome != expected_outcome {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "conflict outcome does not match the winning side's role",
            )
            .with_detail("event", event.id.to_string())
            .with_detail("side", winning_side.to_string()));
        }
        let resolution = Self {
            event: event.id.clone(),
            resolver,
            resolved_at,
            rule: event.conflict.clone(),
            winning_side,
            winning_faction: winning.faction.clone(),
            outcome,
            sides,
            territory_change,
            schema_version: WORLD_EVENT_SCHEMA_VERSION,
        };
        resolution.validate(event)?;
        Ok(resolution)
    }

    /// Validate a resolution against its event.
    pub fn validate(&self, event: &WorldEvent) -> Result<(), EngineError> {
        self.validate_header(event)?;
        self.validate_winning_side(event)?;
        self.validate_side_results(event)?;
        self.validate_territory(event)?;
        if self.schema_version.major != WORLD_EVENT_SCHEMA_VERSION.major {
            return Err(EngineError::new(
                ErrorCode::VersionMismatch,
                "unsupported conflict resolution schema version",
            )
            .with_detail("event", event.id.to_string()));
        }
        Ok(())
    }

    fn validate_header(&self, event: &WorldEvent) -> Result<(), EngineError> {
        if self.event != event.id {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "conflict resolution references a different event",
            )
            .with_detail("event", self.event.to_string())
            .with_detail("expected", event.id.to_string()));
        }
        if event.status != WorldEventStatus::Resolved {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "conflict resolution requires a resolved event",
            )
            .with_detail("event", event.id.to_string())
            .with_detail("status", event.status.as_str()));
        }
        if self.rule != event.conflict {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "conflict resolution rule does not match the event",
            )
            .with_detail("event", event.id.to_string()));
        }
        Ok(())
    }

    fn validate_winning_side(&self, event: &WorldEvent) -> Result<(), EngineError> {
        let winning = event.side(&self.winning_side).ok_or_else(|| {
            EngineError::new(
                ErrorCode::InvalidState,
                "conflict resolution references an unknown winning side",
            )
            .with_detail("event", event.id.to_string())
            .with_detail("side", self.winning_side.to_string())
        })?;
        if winning.faction != self.winning_faction {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "conflict resolution faction does not match its side",
            )
            .with_detail("event", event.id.to_string())
            .with_detail("side", self.winning_side.to_string()));
        }
        let expected_outcome = outcome_for(winning.role);
        if self.outcome != expected_outcome {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "conflict outcome does not match the winning side's role",
            )
            .with_detail("event", event.id.to_string())
            .with_detail("side", self.winning_side.to_string()));
        }
        Ok(())
    }

    fn validate_side_results(&self, event: &WorldEvent) -> Result<(), EngineError> {
        if self.sides.len() != 2 {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "conflict resolution must record both sides",
            )
            .with_detail("event", event.id.to_string()));
        }
        let mut seen = std::collections::BTreeSet::new();
        for side in &self.sides {
            side.validate()?;
            if !seen.insert(side.side.clone()) {
                return Err(EngineError::new(
                    ErrorCode::InvalidState,
                    "conflict resolution contains a duplicate side result",
                )
                .with_detail("event", event.id.to_string())
                .with_detail("side", side.side.to_string()));
            }
            let expected = event.side(&side.side).ok_or_else(|| {
                EngineError::new(
                    ErrorCode::InvalidState,
                    "conflict resolution references an unknown side",
                )
                .with_detail("event", event.id.to_string())
                .with_detail("side", side.side.to_string())
            })?;
            if expected.faction != side.faction || expected.role != side.role {
                return Err(EngineError::new(
                    ErrorCode::InvalidState,
                    "conflict side result does not match the event side",
                )
                .with_detail("event", event.id.to_string())
                .with_detail("side", side.side.to_string()));
            }
        }
        Ok(())
    }

    fn validate_territory(&self, event: &WorldEvent) -> Result<(), EngineError> {
        match self.outcome {
            ConflictOutcome::AttackerVictory => {
                let change = self.territory_change.as_ref().ok_or_else(|| {
                    EngineError::new(
                        ErrorCode::InvalidState,
                        "an attacker victory must record its territory capture",
                    )
                    .with_detail("event", event.id.to_string())
                })?;
                if change.action != crate::faction::TerritoryChangeKind::Capture
                    || !territory_change_for_resolution(event, &self.winning_faction, change)
                {
                    return Err(EngineError::new(
                        ErrorCode::InvalidState,
                        "attacker victory territory change does not match the event",
                    )
                    .with_detail("event", event.id.to_string())
                    .with_detail("key", &change.key));
                }
            }
            ConflictOutcome::DefenderVictory => {
                if self.territory_change.is_some() {
                    return Err(EngineError::new(
                        ErrorCode::InvalidState,
                        "a defender victory must not record a territory change",
                    )
                    .with_detail("event", event.id.to_string()));
                }
            }
        }
        Ok(())
    }
}

/// The outcome implied by the winning side's role.
fn outcome_for(role: EventSideRole) -> ConflictOutcome {
    match role {
        EventSideRole::Attacker => ConflictOutcome::AttackerVictory,
        EventSideRole::Defender => ConflictOutcome::DefenderVictory,
    }
}
