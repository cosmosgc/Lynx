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

/// Server-owned damage school selected by an authoritative combat source.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum DamageType {
    /// Physical weapon and collision damage.
    #[default]
    Physical,
    /// Elemental fire damage.
    Fire,
    /// Elemental frost damage.
    Frost,
    /// Poison or disease damage.
    Poison,
}

/// Redacted server-only record of one resolved combat damage application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CombatAuditEvent {
    /// Simulation tick when the damage was resolved.
    pub tick: engine_core::time::Tick,
    /// Server-resolved source identity; no account credentials or chat data.
    pub source: engine_core::id::StableId,
    /// Target entity identity.
    pub target: engine_core::id::StableId,
    /// Resolved damage school.
    pub damage_type: DamageType,
    /// Input damage before mitigation.
    pub raw_damage: i32,
    /// Damage after defense/resistance and before shields.
    pub mitigated_damage: i32,
    /// Amount absorbed by shields.
    pub shield_absorbed: i32,
    /// Hit points removed.
    pub health_damage: i32,
    /// Hit points remaining, if the entity has health.
    pub remaining_health: Option<i32>,
}

/// Server-owned resistance values by damage type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Resistances {
    /// Flat physical mitigation.
    pub physical: u32,
    /// Flat fire mitigation.
    pub fire: u32,
    /// Flat frost mitigation.
    pub frost: u32,
    /// Flat poison mitigation.
    pub poison: u32,
}

impl Resistances {
    /// Flat mitigation for one damage type.
    #[must_use]
    pub const fn of(self, damage_type: DamageType) -> u32 {
        match damage_type {
            DamageType::Physical => self.physical,
            DamageType::Fire => self.fire,
            DamageType::Frost => self.frost,
            DamageType::Poison => self.poison,
        }
    }
}

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
    /// Damage school selected by the trusted server-side attack source.
    #[serde(default)]
    pub damage_type: DamageType,
    /// Server-owned typed resistances applied after `defense`.
    #[serde(default)]
    pub resistances: Resistances,
}

impl CombatStats {
    /// Build resolved combat inputs.
    #[must_use]
    pub const fn new(attack: u32, defense: u32) -> Self {
        Self {
            attack,
            defense,
            damage_type: DamageType::Physical,
            resistances: Resistances {
                physical: 0,
                fire: 0,
                frost: 0,
                poison: 0,
            },
        }
    }

    /// Continue building with typed resistances.
    #[must_use]
    pub const fn with_resistances(mut self, resistances: Resistances) -> Self {
        self.resistances = resistances;
        self
    }

    /// Select the attack's server-owned damage school.
    #[must_use]
    pub const fn with_damage_type(mut self, damage_type: DamageType) -> Self {
        self.damage_type = damage_type;
        self
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

    /// Resolve v1 damage from server-owned stats only.
    #[must_use]
    pub fn resolve_damage(&self, attacker: CombatStats, target: CombatStats) -> i32 {
        self.resolve_typed_damage(attacker, target, DamageType::Physical)
    }

    /// Resolve v1 damage for an authoritative damage type. Physical targets use
    /// `defense` plus physical resistance; other schools use only their typed
    /// resistance, so armor never silently mitigates magic.
    #[must_use]
    pub fn resolve_typed_damage(
        &self,
        attacker: CombatStats,
        target: CombatStats,
        damage_type: DamageType,
    ) -> i32 {
        let armor = if damage_type == DamageType::Physical {
            target.defense
        } else {
            0
        };
        let raw = attacker
            .attack
            .saturating_sub(armor)
            .saturating_sub(target.resistances.of(damage_type));
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

    #[test]
    fn typed_resistances_are_flat_and_armor_only_mitigates_physical() {
        let rules = CombatRules::default();
        let attacker = CombatStats::new(10, 0);
        let armored = CombatStats::new(0, 4).with_resistances(Resistances {
            physical: 1,
            fire: 2,
            frost: 3,
            poison: 4,
        });
        for (damage_type, expected) in [
            (DamageType::Physical, 5),
            (DamageType::Fire, 8),
            (DamageType::Frost, 7),
            (DamageType::Poison, 6),
        ] {
            assert_eq!(
                rules.resolve_typed_damage(attacker, armored, damage_type),
                expected,
                "{damage_type:?} mitigation",
            );
        }
        assert_eq!(
            rules.resolve_typed_damage(CombatStats::new(1, 0), armored, DamageType::Poison),
            i32::try_from(rules.minimum_damage).unwrap()
        );
        // The default physical path is unchanged for stats without resistances.
        assert_eq!(
            rules.resolve_damage(CombatStats::new(9, 0), CombatStats::new(3, 4)),
            5
        );
    }
}
