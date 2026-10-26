//! Durable territory ownership, capture rules, and append-only history.
//!
//! A territory is ownership state attached to an authored zone. Claiming or
//! capturing never rewrites the zone, its map geometry, or its content bundle.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{EngineError, ErrorCode};
use crate::id::{DefinitionId, IdKind, StableId};
use crate::schema::SchemaVersion;
use crate::time::{DurationMs, Millis};

use super::validate_action_key;

/// Schema version for one territory record or history row.
pub const TERRITORY_SCHEMA_VERSION: SchemaVersion = SchemaVersion::new(1, 0);

/// The longest temporary protection or cooldown the engine accepts (10 years).
pub const MAX_TERRITORY_WINDOW_MS: u64 = 315_576_000_000;

/// Configurable claim and capture rules.
///
/// Protection and cooldown windows are visible rules with a definite expiry;
/// the engine has no permanent-immunity flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerritoryRules {
    /// Whether unclaimed zones may be claimed.
    pub claim_enabled: bool,
    /// Whether an owned zone may be taken from another faction.
    pub capture_enabled: bool,
    /// Protection after a claim or capture.
    pub claim_protection: DurationMs,
    /// Minimum wait before the territory may change hands again.
    pub capture_cooldown: DurationMs,
    /// Minimum faction power required to capture.
    pub minimum_capture_power: u64,
}

impl TerritoryRules {
    /// Validate configured windows and limits.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] for a protection or cooldown
    /// window above [`MAX_TERRITORY_WINDOW_MS`].
    pub fn validate(&self) -> Result<(), EngineError> {
        for (name, window) in [
            ("claim_protection", self.claim_protection),
            ("capture_cooldown", self.capture_cooldown),
        ] {
            if window.0 > MAX_TERRITORY_WINDOW_MS {
                return Err(EngineError::new(
                    ErrorCode::InvalidRequest,
                    "territory windows must be at most ten years",
                )
                .with_detail("window", name)
                .with_detail("milliseconds", window.0.to_string()));
            }
        }
        Ok(())
    }
}

impl Default for TerritoryRules {
    fn default() -> Self {
        Self {
            claim_enabled: true,
            capture_enabled: true,
            claim_protection: DurationMs::new(24 * 60 * 60 * 1_000),
            capture_cooldown: DurationMs::new(60 * 60 * 1_000),
            minimum_capture_power: 0,
        }
    }
}

/// Current ownership state of one authored zone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Territory {
    /// Stable zone definition ID, for example `zone.town_square`.
    pub zone: DefinitionId,
    /// Owning faction, or `None` for an explicitly unclaimed territory.
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
    /// Record schema version.
    pub schema_version: SchemaVersion,
}

impl Territory {
    /// An unclaimed territory record.
    pub fn unclaimed(zone: DefinitionId) -> Result<Self, EngineError> {
        validate_zone(&zone)?;
        Ok(Self {
            zone,
            owner: None,
            contested: false,
            claimed_at: None,
            protection_until: None,
            cooldown_until: None,
            revision: 1,
            schema_version: TERRITORY_SCHEMA_VERSION,
        })
    }

    /// Validate persisted ownership state.
    pub fn validate(&self) -> Result<(), EngineError> {
        validate_zone(&self.zone)?;
        if self.schema_version.major != TERRITORY_SCHEMA_VERSION.major {
            return Err(EngineError::new(
                ErrorCode::VersionMismatch,
                "unsupported territory schema version",
            )
            .with_detail("zone", self.zone.to_string())
            .with_detail("version", self.schema_version.to_string()));
        }
        if self.revision == 0 {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "territory revision must be at least one",
            )
            .with_detail("zone", self.zone.to_string()));
        }
        if let Some(owner) = &self.owner {
            if !owner.is_in("faction") {
                return Err(EngineError::new(
                    ErrorCode::InvalidRequest,
                    "territory owners must use the faction namespace",
                )
                .with_detail("zone", self.zone.to_string())
                .with_detail("owner", owner.to_string()));
            }
            if self.claimed_at.is_none() {
                return Err(EngineError::new(
                    ErrorCode::InvalidState,
                    "an owned territory must record when it was claimed",
                )
                .with_detail("zone", self.zone.to_string()));
            }
        } else if self.claimed_at.is_some() || self.protection_until.is_some() {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "an unclaimed territory must not have a claim time or protection",
            )
            .with_detail("zone", self.zone.to_string()));
        }
        if let (Some(claimed_at), Some(protection_until)) = (self.claimed_at, self.protection_until)
            && protection_until < claimed_at
        {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "territory protection must not end before the claim",
            )
            .with_detail("zone", self.zone.to_string()));
        }
        if let (Some(claimed_at), Some(cooldown_until)) = (self.claimed_at, self.cooldown_until)
            && cooldown_until < claimed_at
        {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "territory cooldown must not end before the claim",
            )
            .with_detail("zone", self.zone.to_string()));
        }
        Ok(())
    }
}

/// Why a territory changed hands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerritoryChangeKind {
    /// An unclaimed territory gained its first owner.
    Claim,
    /// An owned territory moved to a different faction.
    Capture,
    /// An owner gave the territory up.
    Release,
}

impl TerritoryChangeKind {
    /// The wire and persistence string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claim => "claim",
            Self::Capture => "capture",
            Self::Release => "release",
        }
    }
}

/// One append-only territory ownership transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerritoryChange {
    /// Idempotency key, unique across all territory changes.
    pub key: String,
    /// The affected zone.
    pub zone: DefinitionId,
    /// The transition kind.
    pub action: TerritoryChangeKind,
    /// Previous owner, when any.
    pub from: Option<DefinitionId>,
    /// New owner, absent for a release.
    pub to: Option<DefinitionId>,
    /// Character or AI entity that performed the change.
    pub actor: StableId,
    /// When the change committed.
    pub at: Millis,
    /// Territory revision after the change.
    pub revision: u64,
    /// Record schema version.
    pub schema_version: SchemaVersion,
}

impl TerritoryChange {
    /// Validate one ownership transition.
    pub fn validate(&self) -> Result<(), EngineError> {
        validate_action_key(&self.key)?;
        validate_zone(&self.zone)?;
        if !matches!(self.actor.kind(), IdKind::Character | IdKind::Entity) {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "territory actors must be characters or AI entities",
            )
            .with_detail("actor", self.actor.to_string()));
        }
        if self.revision == 0 {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "territory history revisions must be at least one",
            )
            .with_detail("key", &self.key));
        }
        if self.schema_version.major != TERRITORY_SCHEMA_VERSION.major {
            return Err(EngineError::new(
                ErrorCode::VersionMismatch,
                "unsupported territory history schema version",
            )
            .with_detail("key", &self.key)
            .with_detail("version", self.schema_version.to_string()));
        }
        for owner in [&self.from, &self.to].into_iter().flatten() {
            if !owner.is_in("faction") {
                return Err(EngineError::new(
                    ErrorCode::InvalidRequest,
                    "territory owners must use the faction namespace",
                )
                .with_detail("key", &self.key)
                .with_detail("owner", owner.to_string()));
            }
        }
        let consistent = match self.action {
            TerritoryChangeKind::Claim => self.from.is_none() && self.to.is_some(),
            TerritoryChangeKind::Capture => {
                self.from.is_some() && self.to.is_some() && self.from != self.to
            }
            TerritoryChangeKind::Release => self.from.is_some() && self.to.is_none(),
        };
        if !consistent {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "territory history transition does not match its action",
            )
            .with_detail("key", &self.key)
            .with_detail("action", self.action.as_str()));
        }
        Ok(())
    }
}

/// All territories, rules, and ownership history in one world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerritoryBook {
    rules: TerritoryRules,
    territories: BTreeMap<DefinitionId, Territory>,
    history: Vec<TerritoryChange>,
    receipts: BTreeMap<String, TerritoryChange>,
}

impl TerritoryBook {
    /// Create an empty book with `rules`.
    #[must_use]
    pub fn new(rules: TerritoryRules) -> Self {
        Self {
            rules,
            territories: BTreeMap::new(),
            history: Vec::new(),
            receipts: BTreeMap::new(),
        }
    }

    /// Rebuild a book from persisted state and validate its history.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicate zones or keys, a history row for an
    /// unknown zone, non-increasing revisions, or a final owner that does not
    /// match the latest ownership transition.
    pub fn from_parts(
        rules: TerritoryRules,
        territories: Vec<Territory>,
        history: Vec<TerritoryChange>,
    ) -> Result<Self, EngineError> {
        rules.validate()?;
        let mut book = Self::new(rules);
        for territory in territories {
            territory.validate()?;
            if book
                .territories
                .insert(territory.zone.clone(), territory)
                .is_some()
            {
                return Err(EngineError::new(
                    ErrorCode::Conflict,
                    "territory snapshot contains a duplicate zone",
                ));
            }
        }

        let mut revisions: BTreeMap<DefinitionId, u64> = BTreeMap::new();
        let mut owners: BTreeMap<DefinitionId, Option<DefinitionId>> = BTreeMap::new();
        for change in history {
            change.validate()?;
            if book.receipts.contains_key(&change.key) {
                return Err(EngineError::new(
                    ErrorCode::Conflict,
                    "territory history contains a duplicate key",
                )
                .with_detail("key", &change.key));
            }
            if !book.territories.contains_key(&change.zone) {
                return Err(EngineError::new(
                    ErrorCode::NotFound,
                    "territory history references an unknown zone",
                )
                .with_detail("zone", change.zone.to_string())
                .with_detail("key", &change.key));
            }
            let previous = revisions.get(&change.zone).copied().unwrap_or(0);
            if change.revision <= previous {
                return Err(EngineError::new(
                    ErrorCode::InvalidState,
                    "territory history revisions must increase",
                )
                .with_detail("zone", change.zone.to_string())
                .with_detail("revision", change.revision.to_string()));
            }
            let expected_from = owners.get(&change.zone).cloned().flatten();
            if change.from != expected_from {
                return Err(EngineError::new(
                    ErrorCode::InvalidState,
                    "territory history transition does not follow the previous owner",
                )
                .with_detail("zone", change.zone.to_string())
                .with_detail("key", &change.key));
            }
            revisions.insert(change.zone.clone(), change.revision);
            owners.insert(change.zone.clone(), change.to.clone());
            book.receipts.insert(change.key.clone(), change.clone());
            book.history.push(change);
        }

        for territory in book.territories.values() {
            let latest = book
                .history
                .iter()
                .rev()
                .find(|change| change.zone == territory.zone);
            if let Some(change) = latest {
                if change.to != territory.owner {
                    return Err(EngineError::new(
                        ErrorCode::InvalidState,
                        "territory owner does not match the latest history row",
                    )
                    .with_detail("zone", territory.zone.to_string()));
                }
                if change.revision > territory.revision {
                    return Err(EngineError::new(
                        ErrorCode::InvalidState,
                        "territory revision is older than its history",
                    )
                    .with_detail("zone", territory.zone.to_string()));
                }
            } else if territory.owner.is_some() {
                return Err(EngineError::new(
                    ErrorCode::InvalidState,
                    "an owned territory must have an ownership history",
                )
                .with_detail("zone", territory.zone.to_string()));
            }
        }

        Ok(book)
    }

    /// Configured rules.
    #[must_use]
    pub const fn rules(&self) -> &TerritoryRules {
        &self.rules
    }

    /// Every territory, ordered by zone.
    pub fn iter(&self) -> impl Iterator<Item = &Territory> {
        self.territories.values()
    }

    /// Look up one zone's ownership state.
    #[must_use]
    pub fn territory(&self, zone: &DefinitionId) -> Option<&Territory> {
        self.territories.get(zone)
    }

    /// Append-only history, oldest first.
    #[must_use]
    pub fn history(&self) -> &[TerritoryChange] {
        &self.history
    }

    /// History for one zone, oldest first.
    pub fn history_for(&self, zone: &DefinitionId) -> impl Iterator<Item = &TerritoryChange> {
        self.history
            .iter()
            .filter(move |change| &change.zone == zone)
    }

    /// Look up a previously applied change by key.
    #[must_use]
    pub fn receipt(&self, key: &str) -> Option<&TerritoryChange> {
        self.receipts.get(key)
    }

    /// Register a content zone as unclaimed. Registering twice is a no-op.
    pub fn register(&mut self, zone: DefinitionId) -> Result<bool, EngineError> {
        validate_zone(&zone)?;
        if self.territories.contains_key(&zone) {
            return Ok(false);
        }
        self.territories
            .insert(zone.clone(), Territory::unclaimed(zone)?);
        Ok(true)
    }

    /// Claim an unclaimed zone for `faction`.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::NotFound`] for an unregistered zone,
    /// [`ErrorCode::Unsupported`] when claiming is disabled,
    /// [`ErrorCode::Conflict`] when the zone is already owned or contested, and
    /// [`ErrorCode::Cooldown`] while its release cooldown is active.
    pub fn claim(
        &mut self,
        key: &str,
        actor: StableId,
        faction: &DefinitionId,
        zone: &DefinitionId,
        now: Millis,
    ) -> Result<TerritoryChange, EngineError> {
        if let Some(existing) = self.receipts.get(key) {
            return Ok(existing.clone());
        }
        validate_action_key(key)?;
        if !self.rules.claim_enabled {
            return Err(EngineError::new(
                ErrorCode::Unsupported,
                "claiming is disabled by the territory rules",
            ));
        }
        let territory = self
            .territories
            .get(zone)
            .ok_or_else(|| EngineError::not_found("territory", zone))?
            .clone();
        if territory.owner.is_some() {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "territory is already owned; capture it instead",
            )
            .with_detail("zone", zone.to_string()));
        }
        if territory.contested {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "a contested territory cannot be claimed",
            )
            .with_detail("zone", zone.to_string()));
        }
        if let Some(cooldown_until) = territory.cooldown_until
            && now < cooldown_until
        {
            return Err(EngineError::new(
                ErrorCode::Cooldown,
                "territory cooldown is still active",
            )
            .with_detail("zone", zone.to_string())
            .with_detail(
                "remaining_ms",
                now.remaining_until(cooldown_until).to_string(),
            ));
        }

        let revision = territory.revision.saturating_add(1);
        let protection_until = deadline(now, self.rules.claim_protection);
        let updated = Territory {
            owner: Some(faction.clone()),
            contested: false,
            claimed_at: Some(now),
            protection_until: Some(protection_until),
            cooldown_until: Some(deadline(now, self.rules.capture_cooldown)),
            revision,
            ..territory
        };
        self.territories.insert(zone.clone(), updated);
        let change = TerritoryChange {
            key: key.to_owned(),
            zone: zone.clone(),
            action: TerritoryChangeKind::Claim,
            from: None,
            to: Some(faction.clone()),
            actor,
            at: now,
            revision,
            schema_version: TERRITORY_SCHEMA_VERSION,
        };
        self.record(change.clone());
        Ok(change)
    }

    /// Capture an owned zone for `faction`.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::NotFound`] for an unregistered zone,
    /// [`ErrorCode::Unsupported`] when capture is disabled,
    /// [`ErrorCode::Conflict`] when the zone is unowned, already held by the
    /// caller, or contested, [`ErrorCode::PermissionDenied`] when faction power
    /// is below the configured prerequisite, and [`ErrorCode::Cooldown`] while
    /// protection or a cooldown is active.
    pub fn capture(
        &mut self,
        key: &str,
        actor: StableId,
        faction: &DefinitionId,
        zone: &DefinitionId,
        faction_power: u64,
        now: Millis,
    ) -> Result<TerritoryChange, EngineError> {
        if let Some(existing) = self.receipts.get(key) {
            return Ok(existing.clone());
        }
        validate_action_key(key)?;
        if !self.rules.capture_enabled {
            return Err(EngineError::new(
                ErrorCode::Unsupported,
                "capture is disabled by the territory rules",
            ));
        }
        if faction_power < self.rules.minimum_capture_power {
            return Err(EngineError::new(
                ErrorCode::PermissionDenied,
                "faction power is below the territory's capture prerequisite",
            )
            .with_detail("zone", zone.to_string())
            .with_detail("power", faction_power.to_string())
            .with_detail(
                "required_power",
                self.rules.minimum_capture_power.to_string(),
            ));
        }
        let territory = self
            .territories
            .get(zone)
            .ok_or_else(|| EngineError::not_found("territory", zone))?
            .clone();
        let previous = territory.owner.clone().ok_or_else(|| {
            EngineError::new(
                ErrorCode::Conflict,
                "an unclaimed territory must be claimed, not captured",
            )
            .with_detail("zone", zone.to_string())
        })?;
        if previous == *faction {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "the faction already owns this territory",
            )
            .with_detail("zone", zone.to_string()));
        }
        if territory.contested {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "a contested territory cannot change owners",
            )
            .with_detail("zone", zone.to_string()));
        }
        if let Some(protection_until) = territory.protection_until
            && now < protection_until
        {
            return Err(EngineError::new(
                ErrorCode::Cooldown,
                "territory protection is still active",
            )
            .with_detail("zone", zone.to_string())
            .with_detail(
                "remaining_ms",
                now.remaining_until(protection_until).to_string(),
            ));
        }
        if let Some(cooldown_until) = territory.cooldown_until
            && now < cooldown_until
        {
            return Err(EngineError::new(
                ErrorCode::Cooldown,
                "territory cooldown is still active",
            )
            .with_detail("zone", zone.to_string())
            .with_detail(
                "remaining_ms",
                now.remaining_until(cooldown_until).to_string(),
            ));
        }

        let revision = territory.revision.saturating_add(1);
        let updated = Territory {
            owner: Some(faction.clone()),
            contested: false,
            claimed_at: Some(now),
            protection_until: Some(deadline(now, self.rules.claim_protection)),
            cooldown_until: Some(deadline(now, self.rules.capture_cooldown)),
            revision,
            ..territory
        };
        self.territories.insert(zone.clone(), updated);
        let change = TerritoryChange {
            key: key.to_owned(),
            zone: zone.clone(),
            action: TerritoryChangeKind::Capture,
            from: Some(previous),
            to: Some(faction.clone()),
            actor,
            at: now,
            revision,
            schema_version: TERRITORY_SCHEMA_VERSION,
        };
        self.record(change.clone());
        Ok(change)
    }

    /// Award a contested territory to a conflict winner.
    ///
    /// This is the trusted conflict-resolution path, not a player action. It
    /// requires the recorded defender to still own the territory, respects the
    /// configured protection and cooldown windows, and clears the contest.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::Unsupported`] when capture is disabled,
    /// [`ErrorCode::NotFound`] for an unregistered zone,
    /// [`ErrorCode::Conflict`] when the defender no longer owns the zone,
    /// and [`ErrorCode::Cooldown`] while protection or a cooldown is active.
    pub fn award_capture(
        &mut self,
        key: &str,
        actor: StableId,
        faction: &DefinitionId,
        zone: &DefinitionId,
        expected_owner: &DefinitionId,
        now: Millis,
    ) -> Result<TerritoryChange, EngineError> {
        if let Some(existing) = self.receipts.get(key) {
            return Ok(existing.clone());
        }
        validate_action_key(key)?;
        if !self.rules.capture_enabled {
            return Err(EngineError::new(
                ErrorCode::Unsupported,
                "capture is disabled by the territory rules",
            ));
        }
        let territory = self
            .territories
            .get(zone)
            .ok_or_else(|| EngineError::not_found("territory", zone))?
            .clone();
        let previous = territory.owner.clone().ok_or_else(|| {
            EngineError::new(
                ErrorCode::Conflict,
                "an unclaimed territory cannot be awarded by a conflict",
            )
            .with_detail("zone", zone.to_string())
        })?;
        if previous != *expected_owner {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "the territory is no longer owned by the recorded defender",
            )
            .with_detail("zone", zone.to_string())
            .with_detail("expected_owner", expected_owner.to_string())
            .with_detail("actual_owner", previous.to_string()));
        }
        if previous == *faction {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "the winning faction already owns this territory",
            )
            .with_detail("zone", zone.to_string()));
        }
        if let Some(protection_until) = territory.protection_until
            && now < protection_until
        {
            return Err(EngineError::new(
                ErrorCode::Cooldown,
                "territory protection is still active",
            )
            .with_detail("zone", zone.to_string())
            .with_detail(
                "remaining_ms",
                now.remaining_until(protection_until).to_string(),
            ));
        }
        if let Some(cooldown_until) = territory.cooldown_until
            && now < cooldown_until
        {
            return Err(EngineError::new(
                ErrorCode::Cooldown,
                "territory cooldown is still active",
            )
            .with_detail("zone", zone.to_string())
            .with_detail(
                "remaining_ms",
                now.remaining_until(cooldown_until).to_string(),
            ));
        }

        let revision = territory.revision.saturating_add(1);
        let updated = Territory {
            owner: Some(faction.clone()),
            contested: false,
            claimed_at: Some(now),
            protection_until: Some(deadline(now, self.rules.claim_protection)),
            cooldown_until: Some(deadline(now, self.rules.capture_cooldown)),
            revision,
            ..territory
        };
        self.territories.insert(zone.clone(), updated);
        let change = TerritoryChange {
            key: key.to_owned(),
            zone: zone.clone(),
            action: TerritoryChangeKind::Capture,
            from: Some(previous),
            to: Some(faction.clone()),
            actor,
            at: now,
            revision,
            schema_version: TERRITORY_SCHEMA_VERSION,
        };
        self.record(change.clone());
        Ok(change)
    }

    /// Release a territory owned by `faction`.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::NotFound`] for an unregistered zone,
    /// [`ErrorCode::PermissionDenied`] when the caller does not own it, and
    /// [`ErrorCode::Conflict`] while it is contested.
    pub fn release(
        &mut self,
        key: &str,
        actor: StableId,
        faction: &DefinitionId,
        zone: &DefinitionId,
        now: Millis,
    ) -> Result<TerritoryChange, EngineError> {
        if let Some(existing) = self.receipts.get(key) {
            return Ok(existing.clone());
        }
        validate_action_key(key)?;
        let territory = self
            .territories
            .get(zone)
            .ok_or_else(|| EngineError::not_found("territory", zone))?
            .clone();
        let previous = territory.owner.clone().ok_or_else(|| {
            EngineError::new(ErrorCode::Conflict, "territory is already unclaimed")
                .with_detail("zone", zone.to_string())
        })?;
        if previous != *faction {
            return Err(EngineError::new(
                ErrorCode::PermissionDenied,
                "only the owning faction may release a territory",
            )
            .with_detail("zone", zone.to_string()));
        }
        if territory.contested {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "a contested territory cannot be released",
            )
            .with_detail("zone", zone.to_string()));
        }

        let revision = territory.revision.saturating_add(1);
        let updated = Territory {
            owner: None,
            contested: false,
            claimed_at: None,
            protection_until: None,
            cooldown_until: Some(deadline(now, self.rules.capture_cooldown)),
            revision,
            ..territory
        };
        self.territories.insert(zone.clone(), updated);
        let change = TerritoryChange {
            key: key.to_owned(),
            zone: zone.clone(),
            action: TerritoryChangeKind::Release,
            from: Some(previous),
            to: None,
            actor,
            at: now,
            revision,
            schema_version: TERRITORY_SCHEMA_VERSION,
        };
        self.record(change.clone());
        Ok(change)
    }

    /// Mark a territory as contested or clear the contest.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::NotFound`] for an unregistered zone.
    pub fn set_contested(
        &mut self,
        zone: &DefinitionId,
        contested: bool,
    ) -> Result<(), EngineError> {
        let territory = self
            .territories
            .get_mut(zone)
            .ok_or_else(|| EngineError::not_found("territory", zone))?;
        territory.contested = contested;
        Ok(())
    }

    /// Validate the complete book, including history and current owners.
    pub fn validate(&self) -> Result<(), EngineError> {
        self.rules.validate()?;
        let rebuilt = Self::from_parts(
            self.rules.clone(),
            self.territories.values().cloned().collect(),
            self.history.clone(),
        )?;
        if rebuilt.receipts.len() != self.receipts.len() {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "territory receipt index does not match its history",
            ));
        }
        Ok(())
    }

    fn record(&mut self, change: TerritoryChange) {
        self.receipts.insert(change.key.clone(), change.clone());
        self.history.push(change);
    }
}

fn validate_zone(zone: &DefinitionId) -> Result<(), EngineError> {
    if !zone.is_in("zone") {
        return Err(EngineError::new(
            ErrorCode::InvalidRequest,
            "territories must reference a zone definition",
        )
        .with_detail("zone", zone.to_string()));
    }
    Ok(())
}

fn deadline(now: Millis, window: DurationMs) -> Millis {
    Millis::new(now.value().saturating_add(window.as_i64()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn faction(name: &str) -> DefinitionId {
        DefinitionId::new(name).unwrap()
    }

    fn zone(name: &str) -> DefinitionId {
        DefinitionId::new(name).unwrap()
    }

    fn actor(seed: u128) -> StableId {
        StableId::from_parts(IdKind::Character, 0, seed)
    }

    fn book(rules: TerritoryRules) -> (TerritoryBook, DefinitionId) {
        let mut book = TerritoryBook::new(rules);
        let zone = zone("zone.town_square");
        book.register(zone.clone()).unwrap();
        (book, zone)
    }

    #[test]
    fn claims_are_bounded_by_protection_and_cooldown() {
        let rules = TerritoryRules {
            claim_protection: DurationMs::new(1_000),
            capture_cooldown: DurationMs::new(500),
            ..TerritoryRules::default()
        };
        let (mut book, zone) = book(rules);
        let guards = faction("faction.guards");
        let bandits = faction("faction.bandits");
        let change = book
            .claim("claim-1", actor(1), &guards, &zone, Millis::new(10_000))
            .unwrap();
        assert_eq!(change.action, TerritoryChangeKind::Claim);
        assert_eq!(change.to, Some(guards.clone()));
        assert_eq!(book.territory(&zone).unwrap().owner, Some(guards.clone()));

        let early = book
            .capture(
                "capture-early",
                actor(2),
                &bandits,
                &zone,
                0,
                Millis::new(10_500),
            )
            .unwrap_err();
        assert_eq!(early.code(), ErrorCode::Cooldown);

        let capture = book
            .capture(
                "capture-1",
                actor(2),
                &bandits,
                &zone,
                0,
                Millis::new(11_000),
            )
            .unwrap();
        assert_eq!(capture.from, Some(faction("faction.guards")));
        assert_eq!(capture.to, Some(bandits.clone()));
        assert_eq!(book.history().len(), 2);

        let release = book
            .release("release-1", actor(2), &bandits, &zone, Millis::new(11_000))
            .unwrap();
        assert_eq!(release.to, None);
        assert_eq!(book.territory(&zone).unwrap().owner, None);
        assert!(
            book.claim(
                "claim-too-early",
                actor(1),
                &guards,
                &zone,
                Millis::new(11_100)
            )
            .is_err()
        );
        book.validate().unwrap();
    }

    #[test]
    fn capture_enforces_configurable_power() {
        let rules = TerritoryRules {
            claim_protection: DurationMs::new(0),
            capture_cooldown: DurationMs::new(0),
            minimum_capture_power: 500,
            ..TerritoryRules::default()
        };
        let (mut book, zone) = book(rules);
        let guards = faction("faction.guards");
        let bandits = faction("faction.bandits");
        book.claim("claim-1", actor(1), &guards, &zone, Millis::new(1))
            .unwrap();
        let weak = book
            .capture(
                "capture-weak",
                actor(2),
                &bandits,
                &zone,
                499,
                Millis::new(2),
            )
            .unwrap_err();
        assert_eq!(weak.code(), ErrorCode::PermissionDenied);
        book.capture(
            "capture-strong",
            actor(2),
            &bandits,
            &zone,
            500,
            Millis::new(2),
        )
        .unwrap();
    }

    #[test]
    fn replays_are_idempotent_and_do_not_grow_history() {
        let (mut book, zone) = book(TerritoryRules {
            claim_protection: DurationMs::new(0),
            capture_cooldown: DurationMs::new(0),
            ..TerritoryRules::default()
        });
        let guards = faction("faction.guards");
        let first = book
            .claim("claim-1", actor(1), &guards, &zone, Millis::new(1))
            .unwrap();
        let replay = book
            .claim("claim-1", actor(1), &guards, &zone, Millis::new(99))
            .unwrap();
        assert_eq!(first, replay);
        assert_eq!(book.history().len(), 1);
        assert_eq!(book.territory(&zone).unwrap().revision, first.revision);
    }

    #[test]
    fn contested_territories_reject_changes() {
        let (mut book, zone) = book(TerritoryRules::default());
        let guards = faction("faction.guards");
        book.claim("claim-1", actor(1), &guards, &zone, Millis::new(1))
            .unwrap();
        book.set_contested(&zone, true).unwrap();
        assert_eq!(
            book.release("release-1", actor(1), &guards, &zone, Millis::new(2))
                .unwrap_err()
                .code(),
            ErrorCode::Conflict
        );
    }

    #[test]
    fn recovery_rejects_owner_history_mismatches() {
        let rules = TerritoryRules::default();
        let mut territory = Territory::unclaimed(zone("zone.town_square")).unwrap();
        territory.owner = Some(faction("faction.guards"));
        territory.claimed_at = Some(Millis::new(1));
        assert_eq!(
            TerritoryBook::from_parts(rules, vec![territory], Vec::new())
                .unwrap_err()
                .code(),
            ErrorCode::InvalidState
        );
    }

    #[test]
    fn recovery_rejects_a_broken_owner_chain() {
        let rules = TerritoryRules {
            claim_protection: DurationMs::new(0),
            capture_cooldown: DurationMs::new(0),
            ..TerritoryRules::default()
        };
        let (mut book, zone) = book(rules.clone());
        let guards = faction("faction.guards");
        let bandits = faction("faction.bandits");
        book.claim("claim-1", actor(1), &guards, &zone, Millis::new(1))
            .unwrap();
        book.capture("capture-1", actor(2), &bandits, &zone, 0, Millis::new(2))
            .unwrap();

        let territories: Vec<Territory> = book.iter().cloned().collect();
        let mut history = book.history().to_vec();
        history[1].from = Some(faction("faction.raiders"));
        assert_eq!(
            TerritoryBook::from_parts(rules, territories, history)
                .unwrap_err()
                .code(),
            ErrorCode::InvalidState
        );
    }
}
