//! Versioned, server-owned combat configuration and deterministic damage math.
//!
//! Content supplies [`CombatRules`] for a zone and [`CombatStats`] for its
//! entities. Clients send only the `attack` intent; they cannot select damage,
//! defense, a formula version, or faction disposition. Snapshots from before
//! this additive component use [`CombatStats::default`] server-side.

use engine_core::error::{EngineError, ErrorCode};
use engine_core::id::DefinitionId;
use engine_core::schema::SchemaVersion;
use serde::{Deserialize, Serialize};

use crate::entity::FactionMembership;

/// The initial version of the built-in physical combat formula.
pub const COMBAT_RULES_VERSION: SchemaVersion = SchemaVersion::new(1, 0);

/// Server-owned, resolved combat inputs for one entity.
///
/// A content loader or authoritative equipment system resolves base attributes,
/// equipment, and temporary effects into this component before the action
/// pipeline runs. It is never populated from an action request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CombatStats {
    /// Non-negative outgoing physical power after server-owned loadout effects.
    pub attack: u32,
    /// Non-negative incoming physical mitigation after server-owned loadout effects.
    pub defense: u32,
}

impl CombatStats {
    /// Build resolved combat inputs.
    #[must_use]
    pub const fn new(attack: u32, defense: u32) -> Self {
        Self { attack, defense }
    }
}

impl Default for CombatStats {
    fn default() -> Self {
        Self::new(1, 0)
    }
}

/// One directional faction attack policy supplied with a zone's combat rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactionAttackRule {
    /// Attacking faction.
    pub attacker: DefinitionId,
    /// Target faction.
    pub target: DefinitionId,
    /// Whether this directional pairing may be attacked.
    pub allowed: bool,
}

/// Versioned combat formula and faction policy for one zone.
///
/// Formula v1 applies `max(minimum_damage, attacker.attack - target.defense)`.
/// Equal-faction attacks are forbidden unless `allow_friendly_fire` is enabled.
/// `faction_rules` then override the default for exact, directional faction
/// pairs. Missing factions and unlisted pairs are neutral and attackable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CombatRules {
    /// Content/schema version of these rules.
    pub version: SchemaVersion,
    /// The lowest non-zero damage of a successful v1 attack.
    pub minimum_damage: u32,
    /// Whether members of the same faction may attack each other.
    pub allow_friendly_fire: bool,
    /// Directional authored exceptions to the default faction policy.
    pub faction_rules: Vec<FactionAttackRule>,
}

impl Default for CombatRules {
    fn default() -> Self {
        Self {
            version: COMBAT_RULES_VERSION,
            minimum_damage: 1,
            allow_friendly_fire: false,
            faction_rules: Vec::new(),
        }
    }
}

impl CombatRules {
    /// Validate an authored combat ruleset before a zone starts.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] when the version or minimum damage
    /// is invalid, or when the same directional faction pair occurs twice.
    pub fn validate(&self) -> Result<(), EngineError> {
        if !self.version.is_compatible_with(COMBAT_RULES_VERSION) {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "unsupported combat rules major version",
            ));
        }
        if self.minimum_damage == 0 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "combat rules require positive minimum damage",
            ));
        }
        for (index, rule) in self.faction_rules.iter().enumerate() {
            if !rule.attacker.is_in("faction") || !rule.target.is_in("faction") {
                return Err(EngineError::new(
                    ErrorCode::InvalidRequest,
                    "combat faction rules require faction identifiers",
                ));
            }
            if self.faction_rules[..index]
                .iter()
                .any(|prior| prior.attacker == rule.attacker && prior.target == rule.target)
            {
                return Err(EngineError::new(
                    ErrorCode::InvalidRequest,
                    "combat faction rules must not repeat a directional pair",
                ));
            }
        }
        Ok(())
    }

    /// Whether the server-owned faction state permits this attack.
    #[must_use]
    pub fn permits_attack(
        &self,
        attacker: Option<&FactionMembership>,
        target: Option<&FactionMembership>,
    ) -> bool {
        let (Some(attacker), Some(target)) = (attacker, target) else {
            return true;
        };
        if let Some(rule) = self
            .faction_rules
            .iter()
            .find(|rule| rule.attacker == attacker.faction && rule.target == target.faction)
        {
            return rule.allowed;
        }
        self.allow_friendly_fire || attacker.faction != target.faction
    }

    /// Resolve v1 physical damage from server-owned stats only.
    #[must_use]
    pub fn resolve_damage(&self, attacker: CombatStats, target: CombatStats) -> i32 {
        let raw = attacker.attack.saturating_sub(target.defense);
        let damage = raw.max(self.minimum_damage).min(i32::MAX as u32);
        i32::try_from(damage).unwrap_or(i32::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::FactionMembership;

    fn faction(value: &str) -> DefinitionId {
        DefinitionId::new(value).unwrap()
    }

    #[test]
    fn rules_derive_damage_and_apply_directional_faction_policy() {
        let rules = CombatRules {
            minimum_damage: 2,
            faction_rules: vec![FactionAttackRule {
                attacker: faction("faction.guard"),
                target: faction("faction.bandit"),
                allowed: true,
            }],
            ..CombatRules::default()
        };
        rules.validate().unwrap();
        assert_eq!(
            rules.resolve_damage(CombatStats::new(9, 0), CombatStats::new(3, 4)),
            5
        );
        assert_eq!(
            rules.resolve_damage(CombatStats::new(1, 0), CombatStats::new(9, 9)),
            2
        );

        let guard = FactionMembership::new(faction("faction.guard"));
        let bandit = FactionMembership::new(faction("faction.bandit"));
        assert!(rules.permits_attack(Some(&guard), Some(&bandit)));
        assert!(!rules.permits_attack(Some(&guard), Some(&guard)));

        let unsupported = CombatRules {
            version: SchemaVersion::new(2, 0),
            ..CombatRules::default()
        };
        assert!(unsupported.validate().is_err());
    }
}
