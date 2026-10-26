//! The durable faction world: governance, membership, treasury, and territory.
//!
//! One aggregate applies the same authorization and transaction rules to
//! player-governed guilds, NPC polities, and system factions. Operations either
//! commit fully or leave the world unchanged.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::error::{EngineError, ErrorCode};
use crate::faction::governance::{
    FactionMember, FactionPermission, FactionRole, GovernancePolicy, Membership,
};
use crate::faction::territory::{Territory, TerritoryBook, TerritoryChange, TerritoryRules};
use crate::faction::treasury::{
    PowerBreakdown, PowerRules, Treasury, TreasuryEntry, TreasuryEntryKind, TreasuryTransferReceipt,
};
use crate::faction::{FactionDefinition, validate_action_key};
use crate::id::{DefinitionId, StableId};
use crate::schema::SchemaVersion;
use crate::time::Millis;

/// Schema version for one durable faction record.
pub const FACTION_SCHEMA_VERSION: SchemaVersion = SchemaVersion::new(1, 0);

/// Schema version for a complete faction-world snapshot.
pub const FACTION_WORLD_SCHEMA_VERSION: SchemaVersion = SchemaVersion::new(1, 0);

const TRANSFER_REASON: &str = "faction treasury transfer";

/// One durable faction and its governance, members, treasury, and power rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Faction {
    /// Authored identity and display metadata.
    pub definition: FactionDefinition,
    /// Controller and data-driven role permissions.
    pub governance: GovernancePolicy,
    /// Rule set that converts eligible treasury into power.
    pub power_rules: PowerRules,
    /// Append-only treasury ledger.
    pub treasury: Treasury,
    /// Members and the current leader.
    pub membership: Membership,
    /// Record schema version.
    pub schema_version: SchemaVersion,
}

impl Faction {
    /// Create a faction with `founder` as its leader.
    pub fn new(
        definition: FactionDefinition,
        governance: GovernancePolicy,
        power_rules: PowerRules,
        founder: StableId,
        at: Millis,
    ) -> Result<Self, EngineError> {
        let membership = Membership::new(founder, at)?;
        let faction = Self {
            definition,
            governance,
            power_rules,
            treasury: Treasury::new(),
            membership,
            schema_version: FACTION_SCHEMA_VERSION,
        };
        faction.validate()?;
        Ok(faction)
    }

    /// Validate the durable faction record.
    pub fn validate(&self) -> Result<(), EngineError> {
        self.definition.validate()?;
        if self.schema_version.major != FACTION_SCHEMA_VERSION.major {
            return Err(EngineError::new(
                ErrorCode::VersionMismatch,
                "unsupported faction schema version",
            )
            .with_detail("faction", self.definition.id.to_string())
            .with_detail("version", self.schema_version.to_string()));
        }
        self.governance.validate()?;
        self.power_rules.validate()?;
        self.treasury.validate()?;
        self.membership.validate()?;
        Ok(())
    }

    /// Inspectable power result for the current treasury.
    #[must_use]
    pub fn power(&self) -> PowerBreakdown {
        self.power_rules.evaluate(&self.treasury)
    }

    /// Whether `member` belongs to this faction.
    #[must_use]
    pub fn is_member(&self, member: StableId) -> bool {
        self.membership.get(member).is_some()
    }

    /// A member's role, if any.
    #[must_use]
    pub fn role_of(&self, member: StableId) -> Option<FactionRole> {
        self.membership.role_of(member)
    }
}

/// A complete serializable faction-world state for persistence and recovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactionSnapshot {
    /// Snapshot schema version.
    pub schema_version: SchemaVersion,
    /// Territory claim and capture rules.
    pub territory_rules: TerritoryRules,
    /// Faction records, ordered by ID.
    pub factions: Vec<Faction>,
    /// Territory records, ordered by zone.
    pub territories: Vec<Territory>,
    /// Append-only ownership history, oldest first.
    pub territory_history: Vec<TerritoryChange>,
    /// Completed treasury transfers, ordered by key.
    pub transfers: Vec<TreasuryTransferReceipt>,
}

impl Default for FactionSnapshot {
    fn default() -> Self {
        Self {
            schema_version: FACTION_WORLD_SCHEMA_VERSION,
            territory_rules: TerritoryRules::default(),
            factions: Vec::new(),
            territories: Vec::new(),
            territory_history: Vec::new(),
            transfers: Vec::new(),
        }
    }
}

impl FactionSnapshot {
    /// Validate the snapshot by rebuilding a world from it.
    pub fn validate(&self) -> Result<(), EngineError> {
        FactionWorld::from_snapshot(self.clone()).map(|_| ())
    }
}

/// Faction state and the territory book for one world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactionWorld {
    factions: BTreeMap<DefinitionId, Faction>,
    territories: TerritoryBook,
    transfers: BTreeMap<String, TreasuryTransferReceipt>,
}

impl FactionWorld {
    /// An empty world with `rules`.
    #[must_use]
    pub fn new(rules: TerritoryRules) -> Self {
        Self {
            factions: BTreeMap::new(),
            territories: TerritoryBook::new(rules),
            transfers: BTreeMap::new(),
        }
    }

    /// Add a validated faction.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::Conflict`] for a duplicate faction ID.
    pub fn add_faction(&mut self, faction: Faction) -> Result<(), EngineError> {
        faction.validate()?;
        let id = faction.definition.id.clone();
        if self.factions.contains_key(&id) {
            return Err(
                EngineError::new(ErrorCode::Conflict, "faction already exists")
                    .with_detail("faction", id.to_string()),
            );
        }
        self.factions.insert(id, faction);
        Ok(())
    }

    /// Look up a faction.
    #[must_use]
    pub fn faction(&self, faction: &DefinitionId) -> Option<&Faction> {
        self.factions.get(faction)
    }

    /// Every faction, ordered by ID.
    pub fn factions(&self) -> impl Iterator<Item = &Faction> {
        self.factions.values()
    }

    /// The faction that contains `member`, if any.
    #[must_use]
    pub fn faction_of(&self, member: StableId) -> Option<&DefinitionId> {
        self.factions
            .iter()
            .find(|(_, faction)| faction.is_member(member))
            .map(|(id, _)| id)
    }

    /// Every territory, ordered by zone.
    pub fn territories(&self) -> impl Iterator<Item = &Territory> {
        self.territories.iter()
    }

    /// One territory record.
    #[must_use]
    pub fn territory(&self, zone: &DefinitionId) -> Option<&Territory> {
        self.territories.territory(zone)
    }

    /// Append-only territory history.
    #[must_use]
    pub fn territory_history(&self) -> &[TerritoryChange] {
        self.territories.history()
    }

    /// Territory rules.
    #[must_use]
    pub const fn territory_rules(&self) -> &TerritoryRules {
        self.territories.rules()
    }

    /// Register a content zone as unclaimed.
    pub fn register_territory(&mut self, zone: DefinitionId) -> Result<bool, EngineError> {
        self.territories.register(zone)
    }

    /// Invite a character or NPC agent under the caller's permission.
    pub fn invite(
        &mut self,
        actor: StableId,
        faction: &DefinitionId,
        subject: StableId,
        role: FactionRole,
        at: Millis,
    ) -> Result<FactionMember, EngineError> {
        let faction = self.faction_mut(faction)?;
        let policy = faction.governance.clone();
        faction.membership.add(actor, &policy, subject, role, at)
    }

    /// Remove a non-leader member under the caller's permission.
    pub fn remove_member(
        &mut self,
        actor: StableId,
        faction: &DefinitionId,
        subject: StableId,
    ) -> Result<FactionMember, EngineError> {
        let faction = self.faction_mut(faction)?;
        let policy = faction.governance.clone();
        faction.membership.remove(actor, &policy, subject)
    }

    /// Change a non-leader member's role under the caller's permission.
    pub fn set_member_role(
        &mut self,
        actor: StableId,
        faction: &DefinitionId,
        subject: StableId,
        role: FactionRole,
    ) -> Result<FactionMember, EngineError> {
        let faction = self.faction_mut(faction)?;
        let policy = faction.governance.clone();
        faction.membership.set_role(actor, &policy, subject, role)
    }

    /// Transfer leadership to another member.
    pub fn transfer_leadership(
        &mut self,
        actor: StableId,
        faction: &DefinitionId,
        subject: StableId,
    ) -> Result<(), EngineError> {
        let faction = self.faction_mut(faction)?;
        let policy = faction.governance.clone();
        faction
            .membership
            .transfer_leadership(actor, &policy, subject)
    }

    /// Leave the faction containing `actor`.
    pub fn leave_faction(&mut self, actor: StableId) -> Result<FactionMember, EngineError> {
        let faction_id = self
            .faction_of(actor)
            .cloned()
            .ok_or_else(|| EngineError::not_found("faction membership", actor))?;
        self.faction_mut(&faction_id)?.membership.leave(actor)
    }

    /// Deposit value into a faction treasury.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::PermissionDenied`] for a non-member without
    /// `DepositTreasury`, and validation errors from the ledger.
    pub fn deposit(
        &mut self,
        key: &str,
        actor: StableId,
        faction: &DefinitionId,
        amount: u64,
        reason: &str,
        at: Millis,
    ) -> Result<TreasuryEntry, EngineError> {
        let faction = self.faction_mut(faction)?;
        faction.membership.authorize(
            actor,
            &faction.governance,
            FactionPermission::DepositTreasury,
        )?;
        faction.treasury.deposit(key, amount, reason, at)
    }

    /// Spend value from a faction treasury within the caller's role limit.
    pub fn spend(
        &mut self,
        key: &str,
        actor: StableId,
        faction: &DefinitionId,
        amount: u64,
        reason: &str,
        at: Millis,
    ) -> Result<TreasuryEntry, EngineError> {
        let faction = self.faction_mut(faction)?;
        faction
            .membership
            .authorize_spend(actor, &faction.governance, amount)?;
        faction.treasury.spend(key, amount, reason, at)
    }

    /// Transfer value between two factions atomically and idempotently.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::PermissionDenied`] when the actor may not spend the
    /// requested amount, [`ErrorCode::NotFound`] for an unknown faction, and
    /// [`ErrorCode::InvalidState`] when the source treasury cannot cover the
    /// transfer.
    pub fn transfer_treasury(
        &mut self,
        key: &str,
        actor: StableId,
        source: &DefinitionId,
        target: &DefinitionId,
        amount: u64,
        at: Millis,
    ) -> Result<TreasuryTransferReceipt, EngineError> {
        if let Some(existing) = self.transfers.get(key) {
            if existing.source != *source || existing.target != *target || existing.amount != amount
            {
                return Err(EngineError::new(
                    ErrorCode::Conflict,
                    "treasury transfer key was already used by a different transfer",
                )
                .with_detail("key", key));
            }
            return Ok(existing.clone());
        }
        validate_action_key(key)?;
        if amount == 0 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "treasury transfer amount must be positive",
            ));
        }
        if source == target {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "a treasury transfer must target a different faction",
            ));
        }

        let source_faction = self
            .factions
            .get(source)
            .ok_or_else(|| EngineError::not_found("faction", source))?;
        let target_faction = self
            .factions
            .get(target)
            .ok_or_else(|| EngineError::not_found("faction", target))?;
        source_faction
            .membership
            .authorize_spend(actor, &source_faction.governance, amount)?;
        if source_faction.treasury.contains_key(key) || target_faction.treasury.contains_key(key) {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "treasury transfer key was already used by an earlier movement",
            )
            .with_detail("key", key)
            .with_detail("source", source.to_string())
            .with_detail("target", target.to_string()));
        }

        let mut source_treasury = source_faction.treasury.clone();
        let mut target_treasury = target_faction.treasury.clone();
        source_treasury.transfer_out(key, amount, target, TRANSFER_REASON, at)?;
        target_treasury.transfer_in(key, amount, source, TRANSFER_REASON, at)?;

        if let Some(faction) = self.factions.get_mut(source) {
            faction.treasury = source_treasury;
        }
        if let Some(faction) = self.factions.get_mut(target) {
            faction.treasury = target_treasury;
        }

        let receipt = TreasuryTransferReceipt {
            key: key.to_owned(),
            source: source.clone(),
            target: target.clone(),
            amount,
            at,
            schema_version: TreasuryTransferReceipt::SCHEMA_VERSION,
        };
        self.transfers.insert(key.to_owned(), receipt.clone());
        Ok(receipt)
    }

    /// Claim an unclaimed zone for a faction after authorization.
    pub fn claim_territory(
        &mut self,
        key: &str,
        actor: StableId,
        faction: &DefinitionId,
        zone: &DefinitionId,
        at: Millis,
    ) -> Result<TerritoryChange, EngineError> {
        {
            let faction = self
                .factions
                .get(faction)
                .ok_or_else(|| EngineError::not_found("faction", faction))?;
            faction.membership.authorize(
                actor,
                &faction.governance,
                FactionPermission::ClaimTerritory,
            )?;
        }
        self.territories.claim(key, actor, faction, zone, at)
    }

    /// Capture an owned zone for a faction after authorization and power checks.
    pub fn capture_territory(
        &mut self,
        key: &str,
        actor: StableId,
        faction: &DefinitionId,
        zone: &DefinitionId,
        at: Millis,
    ) -> Result<TerritoryChange, EngineError> {
        let power = {
            let faction = self
                .factions
                .get(faction)
                .ok_or_else(|| EngineError::not_found("faction", faction))?;
            faction.membership.authorize(
                actor,
                &faction.governance,
                FactionPermission::CaptureTerritory,
            )?;
            faction.power().power
        };
        self.territories
            .capture(key, actor, faction, zone, power, at)
    }

    /// Release a territory owned by the caller's faction.
    pub fn release_territory(
        &mut self,
        key: &str,
        actor: StableId,
        faction: &DefinitionId,
        zone: &DefinitionId,
        at: Millis,
    ) -> Result<TerritoryChange, EngineError> {
        {
            let faction = self
                .factions
                .get(faction)
                .ok_or_else(|| EngineError::not_found("faction", faction))?;
            faction.membership.authorize(
                actor,
                &faction.governance,
                FactionPermission::ReleaseTerritory,
            )?;
        }
        self.territories.release(key, actor, faction, zone, at)
    }

    /// Award a contested territory to the winner of a resolution.
    ///
    /// This trusted method is for the authoritative conflict resolver. It is not
    /// a player action and does not check role permissions.
    pub fn award_territory(
        &mut self,
        key: &str,
        actor: StableId,
        faction: &DefinitionId,
        zone: &DefinitionId,
        expected_owner: &DefinitionId,
        at: Millis,
    ) -> Result<TerritoryChange, EngineError> {
        if !self.factions.contains_key(faction) {
            return Err(EngineError::not_found("faction", faction));
        }
        if !self.factions.contains_key(expected_owner) {
            return Err(EngineError::not_found("faction", expected_owner));
        }
        self.territories
            .award_capture(key, actor, faction, zone, expected_owner, at)
    }

    /// Mark a territory as contested or clear the contest.
    pub fn set_territory_contested(
        &mut self,
        zone: &DefinitionId,
        contested: bool,
    ) -> Result<(), EngineError> {
        self.territories.set_contested(zone, contested)
    }

    /// Inspectable power result for one faction.
    pub fn power(&self, faction: &DefinitionId) -> Result<PowerBreakdown, EngineError> {
        self.factions
            .get(faction)
            .map(Faction::power)
            .ok_or_else(|| EngineError::not_found("faction", faction))
    }

    /// Capture the complete durable state.
    #[must_use]
    pub fn snapshot(&self) -> FactionSnapshot {
        FactionSnapshot {
            schema_version: FACTION_WORLD_SCHEMA_VERSION,
            territory_rules: self.territories.rules().clone(),
            factions: self.factions.values().cloned().collect(),
            territories: self.territories.iter().cloned().collect(),
            territory_history: self.territories.history().to_vec(),
            transfers: self.transfers.values().cloned().collect(),
        }
    }

    /// Rebuild and validate a world from persisted state.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::VersionMismatch`] for a newer major schema version,
    /// and validation errors for duplicate factions, cross-faction membership,
    /// unknown territory owners, or treasury transfers that are not backed by
    /// matching ledger entries.
    pub fn from_snapshot(snapshot: FactionSnapshot) -> Result<Self, EngineError> {
        if snapshot.schema_version.major != FACTION_WORLD_SCHEMA_VERSION.major {
            return Err(EngineError::new(
                ErrorCode::VersionMismatch,
                "unsupported faction world snapshot version",
            )
            .with_detail("version", snapshot.schema_version.to_string())
            .with_detail(
                "supported_major",
                FACTION_WORLD_SCHEMA_VERSION.major.to_string(),
            ));
        }

        let mut world = Self::new(snapshot.territory_rules);
        for faction in snapshot.factions {
            world.add_faction(faction)?;
        }
        world.validate_membership_uniqueness()?;

        world.territories = TerritoryBook::from_parts(
            world.territories.rules().clone(),
            snapshot.territories,
            snapshot.territory_history,
        )?;
        for territory in world.territories.iter() {
            if let Some(owner) = &territory.owner
                && !world.factions.contains_key(owner)
            {
                return Err(EngineError::new(
                    ErrorCode::NotFound,
                    "territory references an unknown owner faction",
                )
                .with_detail("zone", territory.zone.to_string())
                .with_detail("owner", owner.to_string()));
            }
        }

        for receipt in snapshot.transfers {
            receipt.validate()?;
            if world.transfers.contains_key(&receipt.key) {
                return Err(EngineError::new(
                    ErrorCode::Conflict,
                    "faction snapshot contains a duplicate transfer key",
                )
                .with_detail("key", &receipt.key));
            }
            world.transfers.insert(receipt.key.clone(), receipt);
        }
        world.validate_transfers()?;
        world.validate()?;
        Ok(world)
    }

    /// Validate the complete world, including cross-record references.
    pub fn validate(&self) -> Result<(), EngineError> {
        for faction in self.factions.values() {
            faction.validate()?;
        }
        self.validate_membership_uniqueness()?;
        self.territories.validate()?;
        for change in self.territories.history() {
            for faction in [&change.from, &change.to].into_iter().flatten() {
                if !self.factions.contains_key(faction) {
                    return Err(EngineError::new(
                        ErrorCode::NotFound,
                        "territory history references an unknown faction",
                    )
                    .with_detail("zone", change.zone.to_string())
                    .with_detail("faction", faction.to_string())
                    .with_detail("key", &change.key));
                }
            }
        }
        for territory in self.territories.iter() {
            if let Some(owner) = &territory.owner
                && !self.factions.contains_key(owner)
            {
                return Err(EngineError::new(
                    ErrorCode::NotFound,
                    "territory references an unknown owner faction",
                )
                .with_detail("zone", territory.zone.to_string())
                .with_detail("owner", owner.to_string()));
            }
        }
        self.validate_transfers()?;
        Ok(())
    }

    fn faction_mut(&mut self, faction: &DefinitionId) -> Result<&mut Faction, EngineError> {
        self.factions
            .get_mut(faction)
            .ok_or_else(|| EngineError::not_found("faction", faction))
    }

    fn validate_membership_uniqueness(&self) -> Result<(), EngineError> {
        let mut seen = BTreeMap::new();
        for (faction_id, faction) in &self.factions {
            for member in faction.membership.iter() {
                if let Some(other) = seen.insert(member.id, faction_id.clone()) {
                    return Err(EngineError::new(
                        ErrorCode::Conflict,
                        "a member belongs to more than one faction",
                    )
                    .with_detail("member", member.id.to_string())
                    .with_detail("first_faction", other.to_string())
                    .with_detail("second_faction", faction_id.to_string()));
                }
            }
        }
        Ok(())
    }

    fn validate_transfers(&self) -> Result<(), EngineError> {
        let mut represented = BTreeSet::new();
        for receipt in self.transfers.values() {
            receipt.validate()?;
            let source = self
                .factions
                .get(&receipt.source)
                .ok_or_else(|| EngineError::not_found("faction", &receipt.source))?;
            let target = self
                .factions
                .get(&receipt.target)
                .ok_or_else(|| EngineError::not_found("faction", &receipt.target))?;
            let outbound = source.treasury.entry(&receipt.key).ok_or_else(|| {
                EngineError::new(
                    ErrorCode::InvalidState,
                    "treasury transfer is missing its outbound ledger entry",
                )
                .with_detail("key", &receipt.key)
            })?;
            if outbound.kind != TreasuryEntryKind::TransferOut
                || outbound.amount != receipt.amount
                || outbound.counterparty.as_ref() != Some(&receipt.target)
            {
                return Err(EngineError::new(
                    ErrorCode::InvalidState,
                    "treasury transfer does not match its outbound ledger entry",
                )
                .with_detail("key", &receipt.key));
            }
            let inbound = target.treasury.entry(&receipt.key).ok_or_else(|| {
                EngineError::new(
                    ErrorCode::InvalidState,
                    "treasury transfer is missing its inbound ledger entry",
                )
                .with_detail("key", &receipt.key)
            })?;
            if inbound.kind != TreasuryEntryKind::TransferIn
                || inbound.amount != receipt.amount
                || inbound.counterparty.as_ref() != Some(&receipt.source)
            {
                return Err(EngineError::new(
                    ErrorCode::InvalidState,
                    "treasury transfer does not match its inbound ledger entry",
                )
                .with_detail("key", &receipt.key));
            }
            represented.insert(receipt.key.clone());
        }

        for faction in self.factions.values() {
            for entry in faction.treasury.entries() {
                if matches!(
                    entry.kind,
                    TreasuryEntryKind::TransferOut | TreasuryEntryKind::TransferIn
                ) && !represented.contains(&entry.key)
                {
                    return Err(EngineError::new(
                        ErrorCode::InvalidState,
                        "transfer ledger entry has no matching transfer receipt",
                    )
                    .with_detail("faction", faction.definition.id.to_string())
                    .with_detail("key", &entry.key));
                }
            }
        }
        Ok(())
    }
}

impl Default for FactionWorld {
    fn default() -> Self {
        Self::new(TerritoryRules::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{IdKind, StableId};
    use crate::time::DurationMs;

    fn faction_id(name: &str) -> DefinitionId {
        DefinitionId::new(name).unwrap()
    }

    fn zone(name: &str) -> DefinitionId {
        DefinitionId::new(name).unwrap()
    }

    fn actor(seed: u128) -> StableId {
        StableId::from_parts(IdKind::Character, 0, seed)
    }

    fn faction(name: &str, founder: StableId) -> Faction {
        Faction::new(
            FactionDefinition::new(faction_id(name), name),
            GovernancePolicy::new(crate::faction::FactionController::Player),
            PowerRules::default(),
            founder,
            Millis::new(0),
        )
        .unwrap()
    }

    fn world() -> (FactionWorld, DefinitionId, DefinitionId) {
        let mut world = FactionWorld::new(TerritoryRules {
            claim_protection: DurationMs::new(0),
            capture_cooldown: DurationMs::new(0),
            minimum_capture_power: 0,
            ..TerritoryRules::default()
        });
        world
            .add_faction(faction("faction.guards", actor(1)))
            .unwrap();
        world
            .add_faction(faction("faction.bandits", actor(2)))
            .unwrap();
        (
            world,
            faction_id("faction.guards"),
            faction_id("faction.bandits"),
        )
    }

    #[test]
    fn duplicate_faction_additions_do_not_replace_the_existing_record() {
        let (mut world, guards, _) = world();
        let existing = world.faction(&guards).unwrap().clone();
        let mut replacement = existing.clone();
        replacement.definition.display_name = "Replaced Guard".into();
        assert_eq!(
            world.add_faction(replacement).unwrap_err().code(),
            ErrorCode::Conflict
        );
        assert_eq!(world.faction(&guards), Some(&existing));
    }

    #[test]
    fn npc_and_player_factions_share_the_same_rules() {
        let agent = StableId::from_parts(IdKind::Entity, 0, 50);
        let mut world = FactionWorld::default();
        let mut npc = faction("faction.npc_polity", agent);
        npc.governance.controller = crate::faction::FactionController::Npc;
        world.add_faction(npc).unwrap();

        world
            .deposit(
                "npc-grant",
                agent,
                &faction_id("faction.npc_polity"),
                5_000,
                "treasury grant",
                Millis::new(1),
            )
            .unwrap();
        assert_eq!(
            world
                .faction(&faction_id("faction.npc_polity"))
                .unwrap()
                .treasury
                .balance(),
            5_000
        );

        assert_eq!(
            world
                .spend(
                    "npc-spend",
                    actor(99),
                    &faction_id("faction.npc_polity"),
                    1,
                    "not a member",
                    Millis::new(2),
                )
                .unwrap_err()
                .code(),
            ErrorCode::PermissionDenied
        );
    }

    #[test]
    fn officers_are_limited_but_leaders_can_spend() {
        let (mut world, guards, _) = world();
        world
            .deposit(
                "seed",
                actor(1),
                &guards,
                50_000,
                "war chest",
                Millis::new(1),
            )
            .unwrap();
        world
            .invite(
                actor(1),
                &guards,
                actor(3),
                FactionRole::Officer,
                Millis::new(2),
            )
            .unwrap();
        world
            .spend(
                "officer-spend",
                actor(3),
                &guards,
                crate::faction::governance::DEFAULT_OFFICER_SPEND_LIMIT,
                "supplies",
                Millis::new(3),
            )
            .unwrap();
        assert_eq!(
            world
                .spend(
                    "officer-over",
                    actor(3),
                    &guards,
                    crate::faction::governance::DEFAULT_OFFICER_SPEND_LIMIT + 1,
                    "too much",
                    Millis::new(4),
                )
                .unwrap_err()
                .code(),
            ErrorCode::PermissionDenied
        );
        world
            .spend(
                "leader-spend",
                actor(1),
                &guards,
                1,
                "leader override",
                Millis::new(5),
            )
            .unwrap();
    }

    #[test]
    fn treasury_transfers_conserve_value_and_replay_once() {
        let (mut world, guards, bandits) = world();
        world
            .deposit(
                "seed",
                actor(1),
                &guards,
                10_000,
                "war chest",
                Millis::new(1),
            )
            .unwrap();
        world
            .deposit("seed", actor(2), &bandits, 2_000, "loot", Millis::new(1))
            .unwrap();

        let receipt = world
            .transfer_treasury("aid-1", actor(1), &guards, &bandits, 3_000, Millis::new(2))
            .unwrap();
        let replay = world
            .transfer_treasury("aid-1", actor(1), &guards, &bandits, 3_000, Millis::new(9))
            .unwrap();
        assert_eq!(receipt, replay);
        assert_eq!(world.faction(&guards).unwrap().treasury.balance(), 7_000);
        assert_eq!(world.faction(&bandits).unwrap().treasury.balance(), 5_000);
        assert_eq!(world.snapshot().transfers.len(), 1);
        world.validate().unwrap();

        let error = world
            .transfer_treasury("aid-1", actor(1), &guards, &bandits, 1, Millis::new(10))
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::Conflict);
        assert_eq!(world.faction(&guards).unwrap().treasury.balance(), 7_000);
        assert_eq!(world.faction(&bandits).unwrap().treasury.balance(), 5_000);
        assert_eq!(world.snapshot().transfers.len(), 1);
    }

    #[test]
    fn failed_transfers_do_not_move_value() {
        let (mut world, guards, bandits) = world();
        world
            .deposit("seed", actor(1), &guards, 100, "war chest", Millis::new(1))
            .unwrap();
        let error = world
            .transfer_treasury("too-much", actor(1), &guards, &bandits, 101, Millis::new(2))
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidState);
        assert_eq!(world.faction(&guards).unwrap().treasury.balance(), 100);
        assert_eq!(world.faction(&bandits).unwrap().treasury.balance(), 0);
        assert_eq!(world.snapshot().transfers.len(), 0);
    }

    #[test]
    fn territory_claim_and_capture_use_faction_power() {
        let mut world = FactionWorld::new(TerritoryRules {
            claim_protection: DurationMs::new(0),
            capture_cooldown: DurationMs::new(0),
            minimum_capture_power: 500,
            ..TerritoryRules::default()
        });
        let guards = faction("faction.guards", actor(1));
        let bandits = faction("faction.bandits", actor(2));
        world.add_faction(guards).unwrap();
        world.add_faction(bandits).unwrap();
        let guards_id = faction_id("faction.guards");
        let bandits_id = faction_id("faction.bandits");
        let town = zone("zone.town_square");
        world.register_territory(town.clone()).unwrap();
        // 5000 * 10% = 500 power.
        world
            .deposit(
                "seed",
                actor(1),
                &guards_id,
                5_000,
                "war chest",
                Millis::new(1),
            )
            .unwrap();

        world
            .claim_territory("claim-1", actor(1), &guards_id, &town, Millis::new(2))
            .unwrap();
        assert_eq!(
            world.territory(&town).unwrap().owner,
            Some(guards_id.clone())
        );

        let weak = world
            .capture_territory("capture-weak", actor(2), &bandits_id, &town, Millis::new(3))
            .unwrap_err();
        assert_eq!(weak.code(), ErrorCode::PermissionDenied);

        world
            .deposit(
                "seed",
                actor(2),
                &bandits_id,
                5_000,
                "war chest",
                Millis::new(4),
            )
            .unwrap();
        world
            .capture_territory("capture-1", actor(2), &bandits_id, &town, Millis::new(5))
            .unwrap();
        assert_eq!(
            world.territory(&town).unwrap().owner,
            Some(bandits_id.clone())
        );
        assert_eq!(world.territory_history().len(), 2);
    }

    #[test]
    fn snapshot_round_trip_preserves_state_and_rejects_tampering() {
        let (mut world, guards, bandits) = world();
        let town = zone("zone.town_square");
        world.register_territory(town.clone()).unwrap();
        world
            .deposit(
                "seed",
                actor(1),
                &guards,
                10_000,
                "war chest",
                Millis::new(1),
            )
            .unwrap();
        world
            .transfer_treasury("aid-1", actor(1), &guards, &bandits, 2_500, Millis::new(2))
            .unwrap();
        world
            .claim_territory("claim-1", actor(1), &guards, &town, Millis::new(3))
            .unwrap();

        let snapshot = world.snapshot();
        snapshot.validate().unwrap();
        let restored = FactionWorld::from_snapshot(snapshot.clone()).unwrap();
        assert_eq!(restored.snapshot(), snapshot);
        assert_eq!(restored.faction(&guards).unwrap().treasury.balance(), 7_500);
        assert_eq!(restored.territory_history().len(), 1);

        let mut tampered = snapshot.clone();
        tampered.transfers.clear();
        assert_eq!(
            FactionWorld::from_snapshot(tampered).unwrap_err().code(),
            ErrorCode::InvalidState
        );

        let mut future = snapshot;
        future.schema_version = SchemaVersion::new(2, 0);
        assert_eq!(
            FactionWorld::from_snapshot(future).unwrap_err().code(),
            ErrorCode::VersionMismatch
        );
    }

    #[test]
    fn a_member_cannot_join_two_factions() {
        let (world, _, _) = world();
        let mut snapshot = world.snapshot();
        let mut second = snapshot.factions[1].clone();
        let first_member = snapshot.factions[0].membership.iter().next().unwrap().id;
        second.membership = Membership::new(first_member, Millis::new(0)).unwrap();
        snapshot.factions[1] = second;
        assert_eq!(
            FactionWorld::from_snapshot(snapshot).unwrap_err().code(),
            ErrorCode::Conflict
        );
    }

    #[test]
    fn territories_require_a_known_owner() {
        let (mut world, _, _) = world();
        let town = zone("zone.town_square");
        world.register_territory(town.clone()).unwrap();
        world
            .claim_territory(
                "claim-1",
                actor(1),
                &faction_id("faction.guards"),
                &town,
                Millis::new(1),
            )
            .unwrap();
        let mut snapshot = world.snapshot();
        snapshot
            .factions
            .retain(|faction| faction.definition.id != faction_id("faction.guards"));
        assert!(FactionWorld::from_snapshot(snapshot).is_err());
    }
}
