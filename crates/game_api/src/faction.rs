//! Persistence-facing faction contracts.
//!
//! Faction mutations are authoritative simulation results. Clients never call
//! [`FactionStore`]; the world service validates an action first and persists
//! the resulting [`FactionSnapshot`]. [`FactionService`] is the read-only view
//! exposed to authenticated accounts.

use engine_core::faction::{
    Faction, FactionController, FactionRole, FactionSnapshot, PowerBreakdown, Territory,
};
use engine_core::time::Millis;
use engine_core::{DefinitionId, EngineError, ErrorCode, StableId};
use serde::{Deserialize, Serialize};

use crate::service::{ApiError, SessionToken};

/// One faction member as seen by a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactionMemberView {
    /// Character or AI entity identifier.
    pub member_id: StableId,
    /// Rank inside the faction.
    pub role: FactionRole,
    /// When the member joined.
    pub joined_at: Millis,
}

/// Public faction state: identity, governance, roster, treasury, and power.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactionView {
    /// Stable faction key.
    pub id: DefinitionId,
    /// Player-facing name.
    pub display_name: String,
    /// Optional authoring note.
    pub description: Option<String>,
    /// Who ultimately controls the faction.
    pub controller: FactionController,
    /// Every member, ordered by stable ID.
    pub members: Vec<FactionMemberView>,
    /// Current treasury balance.
    pub treasury_balance: u64,
    /// Inspectable power calculation for the current treasury.
    pub power: PowerBreakdown,
}

impl FactionView {
    /// Build a client view from an authoritative faction record.
    #[must_use]
    pub fn from_faction(faction: &Faction) -> Self {
        Self {
            id: faction.definition.id.clone(),
            display_name: faction.definition.display_name.clone(),
            description: faction.definition.description.clone(),
            controller: faction.governance.controller,
            members: faction
                .membership
                .iter()
                .map(|member| FactionMemberView {
                    member_id: member.id,
                    role: member.role,
                    joined_at: member.joined_at,
                })
                .collect(),
            treasury_balance: faction.treasury.balance(),
            power: faction.power(),
        }
    }
}

/// Current ownership of one authored zone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerritoryView {
    /// Stable zone definition ID.
    pub zone: DefinitionId,
    /// Owning faction, or `None` when the territory is unclaimed.
    pub owner: Option<DefinitionId>,
    /// Whether a conflict currently blocks ownership changes.
    pub contested: bool,
    /// When the current owner took control.
    pub claimed_at: Option<Millis>,
    /// Protection expiry for the current owner.
    pub protection_until: Option<Millis>,
    /// Earliest time the territory may change hands again.
    pub cooldown_until: Option<Millis>,
    /// Monotonic ownership revision.
    pub revision: u64,
}

impl TerritoryView {
    /// Build a client view from an authoritative territory record.
    #[must_use]
    pub fn from_territory(territory: &Territory) -> Self {
        Self {
            zone: territory.zone.clone(),
            owner: territory.owner.clone(),
            contested: territory.contested,
            claimed_at: territory.claimed_at,
            protection_until: territory.protection_until,
            cooldown_until: territory.cooldown_until,
            revision: territory.revision,
        }
    }
}

/// Validate a complete faction snapshot loaded from persistence or a client.
///
/// # Errors
///
/// Returns [`ApiError::Conflict`] for an inconsistent snapshot and
/// [`ApiError::InvalidRequest`] for malformed records.
pub fn validate_snapshot(snapshot: &FactionSnapshot) -> Result<(), ApiError> {
    snapshot
        .validate()
        .map_err(|error| map_engine_error(&error))
}

fn map_engine_error(error: &EngineError) -> ApiError {
    let message = error.message().to_owned();
    match error.code() {
        ErrorCode::NotFound => ApiError::NotFound(message),
        ErrorCode::Conflict | ErrorCode::InvalidState | ErrorCode::VersionMismatch => {
            ApiError::Conflict(message)
        }
        ErrorCode::PermissionDenied => ApiError::PermissionDenied,
        _ => ApiError::InvalidRequest(message),
    }
}

/// Read-only faction and territory access for authenticated accounts.
pub trait FactionService {
    /// Return every faction, ordered by stable ID.
    fn factions(&self, token: &SessionToken) -> Result<Vec<FactionView>, ApiError>;

    /// Return one faction, or [`ApiError::NotFound`] when it does not exist.
    fn faction(
        &self,
        token: &SessionToken,
        faction: &DefinitionId,
    ) -> Result<FactionView, ApiError>;

    /// Return every territory, ordered by zone.
    fn territories(&self, token: &SessionToken) -> Result<Vec<TerritoryView>, ApiError>;
}

/// Trusted persistence access used by the authoritative world service.
///
/// This trait is not a client contract. Callers must have validated the
/// mutation through the simulation and must not expose these methods over a
/// player-facing endpoint.
pub trait FactionStore {
    /// Load the last persisted faction-world snapshot.
    fn load_faction_world(&self) -> Result<FactionSnapshot, ApiError>;

    /// Replace the persisted faction-world snapshot atomically.
    fn replace_faction_world(&self, snapshot: &FactionSnapshot) -> Result<(), ApiError>;
}
