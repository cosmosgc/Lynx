//! Faction treasury ledger, idempotency, and inspectable power calculation.
//!
//! Treasury values are unsigned minor units. Every change is an append-only
//! signed ledger entry; the balance is the sum of those entries. Transfers
//! produce matching outbound and inbound entries so value is conserved.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::error::{EngineError, ErrorCode};
use crate::id::DefinitionId;
use crate::schema::SchemaVersion;
use crate::time::Millis;

use super::{validate_action_key, validate_reason};

/// Schema version for one treasury ledger entry.
pub const TREASURY_ENTRY_SCHEMA_VERSION: SchemaVersion = SchemaVersion::new(1, 0);

/// The kind of treasury ledger movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TreasuryEntryKind {
    /// Value entered from outside the faction system.
    Deposit,
    /// Value left the faction system.
    Spend,
    /// Value moved to another faction.
    TransferOut,
    /// Value arrived from another faction.
    TransferIn,
    /// Value was destroyed by an authored loss.
    Loss,
    /// Value was granted by the server.
    Reward,
}

impl TreasuryEntryKind {
    /// The wire and persistence string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Deposit => "deposit",
            Self::Spend => "spend",
            Self::TransferOut => "transfer_out",
            Self::TransferIn => "transfer_in",
            Self::Loss => "loss",
            Self::Reward => "reward",
        }
    }

    /// The signed balance delta for a positive `amount`.
    #[must_use]
    pub const fn sign(self) -> i64 {
        match self {
            Self::Deposit | Self::TransferIn | Self::Reward => 1,
            Self::Spend | Self::TransferOut | Self::Loss => -1,
        }
    }
}

/// One immutable treasury ledger row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreasuryEntry {
    /// Idempotency key, unique within one faction ledger.
    pub key: String,
    /// Movement category.
    pub kind: TreasuryEntryKind,
    /// Absolute value moved, always positive.
    pub amount: u64,
    /// The other faction for a transfer.
    pub counterparty: Option<DefinitionId>,
    /// Why the movement happened, for audit and inspection.
    pub reason: String,
    /// When the movement occurred.
    pub at: Millis,
    /// Record schema version.
    pub schema_version: SchemaVersion,
}

impl TreasuryEntry {
    /// Validate one persisted entry.
    pub fn validate(&self) -> Result<(), EngineError> {
        validate_action_key(&self.key)?;
        validate_reason(&self.reason)?;
        if self.amount == 0 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "treasury entry amount must be positive",
            )
            .with_detail("key", &self.key));
        }
        let is_transfer = matches!(
            self.kind,
            TreasuryEntryKind::TransferOut | TreasuryEntryKind::TransferIn
        );
        if is_transfer != self.counterparty.is_some() {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "transfer entries must name exactly one counterparty",
            )
            .with_detail("key", &self.key)
            .with_detail("kind", self.kind.as_str()));
        }
        if self.schema_version.major != TREASURY_ENTRY_SCHEMA_VERSION.major {
            return Err(EngineError::new(
                ErrorCode::VersionMismatch,
                "unsupported treasury entry schema version",
            )
            .with_detail("key", &self.key)
            .with_detail("version", self.schema_version.to_string()));
        }
        Ok(())
    }

    fn signed_delta(&self) -> i128 {
        i128::from(self.kind.sign()) * i128::from(self.amount)
    }
}

/// A faction treasury backed by an append-only ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Treasury {
    balance: u64,
    entries: Vec<TreasuryEntry>,
}

impl Treasury {
    /// An empty treasury.
    #[must_use]
    pub fn new() -> Self {
        Self {
            balance: 0,
            entries: Vec::new(),
        }
    }

    /// Rebuild a treasury from persisted entries, validating conservation.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicate keys, invalid entries, a negative
    /// intermediate balance, or an overflowing balance.
    pub fn from_entries(entries: Vec<TreasuryEntry>) -> Result<Self, EngineError> {
        let mut treasury = Self::new();
        let mut seen = BTreeSet::new();
        for entry in entries {
            if !seen.insert(entry.key.clone()) {
                return Err(EngineError::new(
                    ErrorCode::Conflict,
                    "treasury ledger contains a duplicate entry key",
                )
                .with_detail("key", &entry.key));
            }
            treasury.apply(entry)?;
        }
        Ok(treasury)
    }

    /// Current spendable balance.
    #[must_use]
    pub const fn balance(&self) -> u64 {
        self.balance
    }

    /// Ledger rows in application order.
    #[must_use]
    pub fn entries(&self) -> &[TreasuryEntry] {
        &self.entries
    }

    /// Find an entry by its idempotency key.
    #[must_use]
    pub fn entry(&self, key: &str) -> Option<&TreasuryEntry> {
        self.entries.iter().find(|entry| entry.key == key)
    }

    /// Whether this ledger already applied `key`.
    #[must_use]
    pub fn contains_key(&self, key: &str) -> bool {
        self.entry(key).is_some()
    }

    /// Deposit value from outside the faction system.
    pub fn deposit(
        &mut self,
        key: &str,
        amount: u64,
        reason: &str,
        at: Millis,
    ) -> Result<TreasuryEntry, EngineError> {
        self.apply(movement(
            key,
            TreasuryEntryKind::Deposit,
            amount,
            None,
            reason,
            at,
        ))
    }

    /// Grant value from the server, tracked separately from player deposits.
    pub fn reward(
        &mut self,
        key: &str,
        amount: u64,
        reason: &str,
        at: Millis,
    ) -> Result<TreasuryEntry, EngineError> {
        self.apply(movement(
            key,
            TreasuryEntryKind::Reward,
            amount,
            None,
            reason,
            at,
        ))
    }

    /// Spend value out of the faction system.
    pub fn spend(
        &mut self,
        key: &str,
        amount: u64,
        reason: &str,
        at: Millis,
    ) -> Result<TreasuryEntry, EngineError> {
        self.apply(movement(
            key,
            TreasuryEntryKind::Spend,
            amount,
            None,
            reason,
            at,
        ))
    }

    /// Destroy value through an authored loss.
    pub fn lose(
        &mut self,
        key: &str,
        amount: u64,
        reason: &str,
        at: Millis,
    ) -> Result<TreasuryEntry, EngineError> {
        self.apply(movement(
            key,
            TreasuryEntryKind::Loss,
            amount,
            None,
            reason,
            at,
        ))
    }

    /// Move value to another faction. The matching inbound entry is written by
    /// the receiver.
    pub fn transfer_out(
        &mut self,
        key: &str,
        amount: u64,
        counterparty: &DefinitionId,
        reason: &str,
        at: Millis,
    ) -> Result<TreasuryEntry, EngineError> {
        self.apply(movement(
            key,
            TreasuryEntryKind::TransferOut,
            amount,
            Some(counterparty.clone()),
            reason,
            at,
        ))
    }

    /// Receive value from another faction.
    pub fn transfer_in(
        &mut self,
        key: &str,
        amount: u64,
        counterparty: &DefinitionId,
        reason: &str,
        at: Millis,
    ) -> Result<TreasuryEntry, EngineError> {
        self.apply(movement(
            key,
            TreasuryEntryKind::TransferIn,
            amount,
            Some(counterparty.clone()),
            reason,
            at,
        ))
    }

    fn apply(&mut self, entry: TreasuryEntry) -> Result<TreasuryEntry, EngineError> {
        entry.validate()?;
        if let Some(existing) = self.entry(&entry.key) {
            if existing.kind != entry.kind
                || existing.amount != entry.amount
                || existing.counterparty != entry.counterparty
            {
                return Err(EngineError::new(
                    ErrorCode::Conflict,
                    "treasury key was already used by a different movement",
                )
                .with_detail("key", &entry.key));
            }
            return Ok(existing.clone());
        }
        let next = i128::from(self.balance) + entry.signed_delta();
        if next < 0 {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "treasury balance cannot become negative",
            )
            .with_detail("key", &entry.key)
            .with_detail("balance", self.balance.to_string())
            .with_detail("amount", entry.amount.to_string()));
        }
        let next = u64::try_from(next).map_err(|_| {
            EngineError::new(
                ErrorCode::CapacityExceeded,
                "treasury balance exceeds the supported range",
            )
            .with_detail("key", &entry.key)
        })?;
        self.balance = next;
        self.entries.push(entry.clone());
        Ok(entry)
    }

    /// Validate a persisted treasury without trusting its recorded balance.
    pub fn validate(&self) -> Result<(), EngineError> {
        let rebuilt = Self::from_entries(self.entries.clone())?;
        if rebuilt.balance != self.balance {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "treasury balance does not match its ledger",
            )
            .with_detail("recorded", self.balance.to_string())
            .with_detail("ledger", rebuilt.balance.to_string()));
        }
        Ok(())
    }
}

fn movement(
    key: &str,
    kind: TreasuryEntryKind,
    amount: u64,
    counterparty: Option<DefinitionId>,
    reason: &str,
    at: Millis,
) -> TreasuryEntry {
    TreasuryEntry {
        key: key.to_owned(),
        kind,
        amount,
        counterparty,
        reason: reason.to_owned(),
        at,
        schema_version: TREASURY_ENTRY_SCHEMA_VERSION,
    }
}

impl Default for Treasury {
    fn default() -> Self {
        Self::new()
    }
}

/// Versioned rules that turn eligible treasury into strategic power.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowerRules {
    /// Rule version recorded on every breakdown.
    pub version: SchemaVersion,
    /// Power granted regardless of wealth.
    pub base_power: u64,
    /// Basis points of eligible treasury counted as power.
    pub treasury_weight_bps: u16,
    /// Treasury reserved for upkeep and excluded from power.
    pub reserve: u64,
    /// Maximum power this rule can produce.
    pub cap: u64,
}

impl PowerRules {
    /// Validate the rule set.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] for an unsupported version,
    /// weighting above 100%, or a cap below the base power.
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.version.major == 0 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "power rule versions must have a major version of at least one",
            ));
        }
        if self.treasury_weight_bps > 10_000 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "treasury power weight must not exceed 10000 basis points",
            )
            .with_detail("treasury_weight_bps", self.treasury_weight_bps.to_string()));
        }
        if self.cap < self.base_power {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "power cap must be at least the base power",
            )
            .with_detail("cap", self.cap.to_string())
            .with_detail("base_power", self.base_power.to_string()));
        }
        Ok(())
    }

    /// Evaluate power and retain the documented inputs and rule version.
    #[must_use]
    pub fn evaluate(&self, treasury: &Treasury) -> PowerBreakdown {
        let eligible_treasury = treasury.balance().saturating_sub(self.reserve);
        let contribution = u64::try_from(
            (u128::from(eligible_treasury) * u128::from(self.treasury_weight_bps)) / 10_000,
        )
        .unwrap_or(u64::MAX);
        let raw_power = self.base_power.saturating_add(contribution);
        let power = raw_power.min(self.cap);
        PowerBreakdown {
            rule_version: self.version,
            treasury_balance: treasury.balance(),
            reserve: self.reserve,
            eligible_treasury,
            base_power: self.base_power,
            treasury_weight_bps: self.treasury_weight_bps,
            treasury_power: contribution,
            raw_power,
            power,
            applied_cap: raw_power > self.cap,
        }
    }
}

impl Default for PowerRules {
    fn default() -> Self {
        Self {
            version: SchemaVersion::new(1, 0),
            base_power: 0,
            treasury_weight_bps: 1_000,
            reserve: 0,
            cap: 1_000_000,
        }
    }
}

/// Inspectable result of one power calculation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowerBreakdown {
    /// Rule version that produced this result.
    pub rule_version: SchemaVersion,
    /// Full treasury balance at evaluation time.
    pub treasury_balance: u64,
    /// Funds excluded from the calculation.
    pub reserve: u64,
    /// Funds that participated in the calculation.
    pub eligible_treasury: u64,
    /// Base power for this rule set.
    pub base_power: u64,
    /// Weight applied to eligible treasury.
    pub treasury_weight_bps: u16,
    /// Power contributed by eligible treasury.
    pub treasury_power: u64,
    /// Sum before the cap.
    pub raw_power: u64,
    /// Final power after the cap.
    pub power: u64,
    /// Whether the cap changed the result.
    pub applied_cap: bool,
}

/// Receipt for a completed inter-faction treasury transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreasuryTransferReceipt {
    /// Idempotency key supplied by the caller.
    pub key: String,
    /// Faction that lost value.
    pub source: DefinitionId,
    /// Faction that gained value.
    pub target: DefinitionId,
    /// Value moved.
    pub amount: u64,
    /// When the transfer committed.
    pub at: Millis,
    /// Record schema version.
    pub schema_version: SchemaVersion,
}

impl TreasuryTransferReceipt {
    /// Schema version written for new receipts.
    pub const SCHEMA_VERSION: SchemaVersion = SchemaVersion::new(1, 0);

    /// Validate the receipt before persisting or replaying it.
    pub fn validate(&self) -> Result<(), EngineError> {
        validate_action_key(&self.key)?;
        if self.amount == 0 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "treasury transfer amount must be positive",
            )
            .with_detail("key", &self.key));
        }
        if self.source == self.target {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "a treasury transfer must target a different faction",
            )
            .with_detail("key", &self.key));
        }
        if self.schema_version.major != Self::SCHEMA_VERSION.major {
            return Err(EngineError::new(
                ErrorCode::VersionMismatch,
                "unsupported treasury transfer schema version",
            )
            .with_detail("key", &self.key)
            .with_detail("version", self.schema_version.to_string()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn faction(name: &str) -> DefinitionId {
        DefinitionId::new(name).unwrap()
    }

    #[test]
    fn deposits_and_spends_conserve_the_ledger() {
        let mut treasury = Treasury::new();
        treasury
            .deposit("seed", 1_000, "starter grant", Millis::new(1))
            .unwrap();
        treasury
            .spend("upkeep", 250, "weekly upkeep", Millis::new(2))
            .unwrap();
        assert_eq!(treasury.balance(), 750);
        assert_eq!(treasury.entries().len(), 2);
        treasury.validate().unwrap();

        let error = treasury
            .spend("overspend", 751, "too much", Millis::new(3))
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidState);
        assert_eq!(treasury.balance(), 750);
        assert_eq!(treasury.entries().len(), 2);
    }

    #[test]
    fn replayed_keys_do_not_move_value_twice() {
        let mut treasury = Treasury::new();
        let first = treasury
            .deposit("deposit-1", 100, "quest reward", Millis::new(1))
            .unwrap();
        let replay = treasury
            .deposit("deposit-1", 100, "quest reward", Millis::new(2))
            .unwrap();
        assert_eq!(first, replay);
        assert_eq!(treasury.balance(), 100);
        assert_eq!(treasury.entries().len(), 1);
    }

    #[test]
    fn replayed_keys_must_describe_the_same_movement() {
        let mut treasury = Treasury::new();
        treasury
            .deposit("deposit-1", 100, "quest reward", Millis::new(1))
            .unwrap();

        let error = treasury
            .deposit("deposit-1", 250, "quest reward", Millis::new(2))
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::Conflict);
        assert_eq!(treasury.balance(), 100);
        assert_eq!(treasury.entries().len(), 1);

        let error = treasury
            .spend("deposit-1", 100, "quest reward", Millis::new(3))
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::Conflict);
        assert_eq!(treasury.balance(), 100);
    }

    #[test]
    fn transfers_require_matching_counterparties_and_signs() {
        let mut treasury = Treasury::new();
        treasury
            .deposit("seed", 500, "starter grant", Millis::new(1))
            .unwrap();
        treasury
            .transfer_out(
                "move-1",
                200,
                &faction("faction.other"),
                "aid",
                Millis::new(2),
            )
            .unwrap();
        assert_eq!(treasury.balance(), 300);
        treasury
            .transfer_in(
                "move-2",
                50,
                &faction("faction.other"),
                "aid",
                Millis::new(3),
            )
            .unwrap();
        assert_eq!(treasury.balance(), 350);
        treasury.validate().unwrap();
    }

    #[test]
    fn invalid_entries_are_rejected_during_recovery() {
        let good = TreasuryEntry {
            key: "seed".into(),
            kind: TreasuryEntryKind::Deposit,
            amount: 10,
            counterparty: None,
            reason: "starter grant".into(),
            at: Millis::new(1),
            schema_version: TREASURY_ENTRY_SCHEMA_VERSION,
        };
        let mut duplicate = good.clone();
        duplicate.at = Millis::new(2);
        assert_eq!(
            Treasury::from_entries(vec![good.clone(), duplicate])
                .unwrap_err()
                .code(),
            ErrorCode::Conflict
        );

        let mut negative = good.clone();
        negative.kind = TreasuryEntryKind::Spend;
        assert_eq!(
            Treasury::from_entries(vec![negative]).unwrap_err().code(),
            ErrorCode::InvalidState
        );

        let mut inconsistent = Treasury::new();
        inconsistent
            .deposit("seed", 10, "starter grant", Millis::new(1))
            .unwrap();
        inconsistent.balance = 99;
        assert_eq!(
            inconsistent.validate().unwrap_err().code(),
            ErrorCode::InvalidState
        );
    }

    #[test]
    fn power_uses_documented_inputs_and_caps() {
        let rules = PowerRules {
            version: SchemaVersion::new(2, 0),
            base_power: 100,
            treasury_weight_bps: 5_000,
            reserve: 200,
            cap: 1_000,
        };
        let mut treasury = Treasury::new();
        treasury
            .deposit("seed", 1_000, "starter grant", Millis::new(1))
            .unwrap();
        let breakdown = rules.evaluate(&treasury);
        assert_eq!(breakdown.treasury_balance, 1_000);
        assert_eq!(breakdown.eligible_treasury, 800);
        assert_eq!(breakdown.treasury_power, 400);
        assert_eq!(breakdown.power, 500);
        assert!(!breakdown.applied_cap);
        assert_eq!(breakdown.rule_version, SchemaVersion::new(2, 0));

        treasury
            .deposit("more", 10_000, "war chest", Millis::new(2))
            .unwrap();
        let capped = rules.evaluate(&treasury);
        assert_eq!(capped.raw_power, 5_500);
        assert_eq!(capped.power, 1_000);
        assert!(capped.applied_cap);
    }

    #[test]
    fn power_rules_are_validated() {
        let mut rules = PowerRules::default();
        rules.validate().unwrap();
        rules.treasury_weight_bps = 10_001;
        assert_eq!(
            rules.validate().unwrap_err().code(),
            ErrorCode::InvalidRequest
        );
        rules.treasury_weight_bps = 0;
        rules.base_power = 10;
        rules.cap = 1;
        assert_eq!(
            rules.validate().unwrap_err().code(),
            ErrorCode::InvalidRequest
        );
    }
}
