//! Faction alignment, relations, and character reputation.
//!
//! Alignment is always a signed integer in `-100..=100`. The server resolves
//! effective alignment; clients receive only the result they are allowed to
//! know. See `docs/adr/0009-faction-alignment-and-reputation.md`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{EngineError, ErrorCode};
use crate::id::DefinitionId;
use crate::schema::SchemaVersion;
use crate::time::Millis;

/// The lowest possible alignment.
pub const ALIGNMENT_MIN: i16 = -100;
/// The highest possible alignment.
pub const ALIGNMENT_MAX: i16 = 100;
/// The default maximum alignment that still counts as an enemy.
pub const DEFAULT_ENEMY_MAX: i16 = -34;
/// The default minimum alignment that counts as a friend.
pub const DEFAULT_FRIEND_MIN: i16 = 34;

/// A clamped alignment value.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Alignment(i16);

impl Alignment {
    /// Neutral alignment.
    pub const NEUTRAL: Self = Self(0);
    /// The lowest alignment.
    pub const MIN: Self = Self(ALIGNMENT_MIN);
    /// The highest alignment.
    pub const MAX: Self = Self(ALIGNMENT_MAX);

    /// Clamp `value` into `-100..=100`.
    #[must_use]
    pub const fn new(value: i16) -> Self {
        Self(if value < ALIGNMENT_MIN {
            ALIGNMENT_MIN
        } else if value > ALIGNMENT_MAX {
            ALIGNMENT_MAX
        } else {
            value
        })
    }

    /// The raw signed value.
    #[must_use]
    pub const fn value(self) -> i16 {
        self.0
    }

    /// Classify this alignment with `thresholds`.
    #[must_use]
    pub const fn class(self, thresholds: Thresholds) -> AlignmentClass {
        if self.0 <= thresholds.enemy_max {
            AlignmentClass::Enemy
        } else if self.0 >= thresholds.friend_min {
            AlignmentClass::Friend
        } else {
            AlignmentClass::Neutral
        }
    }

    /// Add a signed delta, clamping the result.
    #[must_use]
    pub const fn offset(self, delta: i16) -> Self {
        Self::new(self.0.saturating_add(delta))
    }
}

impl std::fmt::Display for Alignment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Boundaries between enemy, neutral, and friend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thresholds {
    /// Alignment at or below this value is hostile.
    pub enemy_max: i16,
    /// Alignment at or above this value is friendly.
    pub friend_min: i16,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            enemy_max: DEFAULT_ENEMY_MAX,
            friend_min: DEFAULT_FRIEND_MIN,
        }
    }
}

impl Thresholds {
    /// Validate ordering and bounds.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] unless
    /// `-100 <= enemy_max < friend_min <= 100`.
    pub fn validate(self) -> Result<Self, EngineError> {
        if self.enemy_max < ALIGNMENT_MIN
            || self.friend_min > ALIGNMENT_MAX
            || self.enemy_max >= self.friend_min
        {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "alignment thresholds must satisfy -100 <= enemy_max < friend_min <= 100",
            )
            .with_detail("enemy_max", self.enemy_max.to_string())
            .with_detail("friend_min", self.friend_min.to_string()));
        }
        Ok(self)
    }
}

/// The behavior class implied by an alignment value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlignmentClass {
    /// Hostile.
    Enemy,
    /// Neither hostile nor friendly.
    Neutral,
    /// Friendly.
    Friend,
}

impl AlignmentClass {
    /// The wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Enemy => "enemy",
            Self::Neutral => "neutral",
            Self::Friend => "friend",
        }
    }
}

impl std::fmt::Display for AlignmentClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An authoritative faction record. Store these in the database, not in a
/// client content bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactionDefinition {
    /// Stable faction key, for example `faction.town_guard`.
    pub id: DefinitionId,
    /// Player-facing name.
    pub display_name: String,
    /// Record schema version.
    pub version: SchemaVersion,
    /// Optional authoring note.
    pub description: Option<String>,
}

impl FactionDefinition {
    /// Create a faction definition.
    #[must_use]
    pub fn new(id: DefinitionId, display_name: impl Into<String>) -> Self {
        Self {
            id,
            display_name: display_name.into(),
            version: SchemaVersion::new(1, 0),
            description: None,
        }
    }
}

/// A directional alignment from `source` toward `target`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactionRelation {
    /// The faction doing the judging.
    pub source: DefinitionId,
    /// The faction being judged.
    pub target: DefinitionId,
    /// Alignment points, clamped to `-100..=100`.
    pub points: Alignment,
}

impl FactionRelation {
    /// Create a relation.
    #[must_use]
    pub fn new(source: DefinitionId, target: DefinitionId, points: Alignment) -> Self {
        Self {
            source,
            target,
            points,
        }
    }

    /// Classify this relation.
    #[must_use]
    pub fn class(&self, thresholds: Thresholds) -> AlignmentClass {
        self.points.class(thresholds)
    }
}

/// A set of directional faction relations.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FactionCatalog {
    relations: BTreeMap<(DefinitionId, DefinitionId), Alignment>,
}

impl FactionCatalog {
    /// An empty catalog. Unlisted pairs resolve to neutral.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a directional relation.
    pub fn set(&mut self, source: DefinitionId, target: DefinitionId, points: Alignment) {
        self.relations.insert((source, target), points);
    }

    /// Set both directions to the same value.
    pub fn set_bidirectional(&mut self, a: DefinitionId, b: DefinitionId, points: Alignment) {
        self.set(a.clone(), b.clone(), points);
        self.set(b, a, points);
    }

    /// Resolve the base alignment `source` holds toward `target`.
    ///
    /// Missing relations are neutral (`0`).
    #[must_use]
    pub fn base_alignment(&self, source: &DefinitionId, target: &DefinitionId) -> Alignment {
        self.relations
            .get(&(source.clone(), target.clone()))
            .copied()
            .unwrap_or(Alignment::NEUTRAL)
    }

    /// Classify the base alignment from `source` toward `target`.
    #[must_use]
    pub fn class_of(
        &self,
        source: &DefinitionId,
        target: &DefinitionId,
        thresholds: Thresholds,
    ) -> AlignmentClass {
        self.base_alignment(source, target).class(thresholds)
    }

    /// The number of authored relations.
    #[must_use]
    pub fn len(&self) -> usize {
        self.relations.len()
    }

    /// Whether no relations are authored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.relations.is_empty()
    }

    /// All relations, ordered for deterministic output.
    pub fn relations(&self) -> impl Iterator<Item = FactionRelation> + '_ {
        self.relations.iter().map(|((source, target), points)| {
            FactionRelation::new(source.clone(), target.clone(), *points)
        })
    }
}

/// A single additive reputation change with an optional expiry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReputationModifier {
    /// Signed points added to the base alignment.
    pub amount: i16,
    /// Why the modifier exists, for audit and UI.
    pub reason: String,
    /// When the modifier stops applying. `None` means it never expires.
    pub expires_at: Option<Millis>,
}

impl ReputationModifier {
    /// A permanent modifier.
    #[must_use]
    pub fn permanent(amount: i16, reason: impl Into<String>) -> Self {
        Self {
            amount,
            reason: reason.into(),
            expires_at: None,
        }
    }

    /// A modifier that expires at `expires_at`.
    #[must_use]
    pub fn expiring(amount: i16, reason: impl Into<String>, expires_at: Millis) -> Self {
        Self {
            amount,
            reason: reason.into(),
            expires_at: Some(expires_at),
        }
    }

    /// Whether the modifier applies at `now`.
    #[must_use]
    pub fn is_active(&self, now: Millis) -> bool {
        self.expires_at.is_none_or(|expiry| now < expiry)
    }
}

/// The result of a reputation mutation, suitable for an audit record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReputationChange {
    /// The faction whose standing changed.
    pub faction: DefinitionId,
    /// Effective alignment before the change.
    pub before: Alignment,
    /// Effective alignment after the change.
    pub after: Alignment,
    /// The requested modifier amount, before clamping.
    pub amount: i16,
    /// The reason attached to the modifier.
    pub reason: String,
}

/// Per-character additive reputation modifiers, keyed by target faction.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReputationBook {
    entries: BTreeMap<DefinitionId, Vec<ReputationModifier>>,
}

impl ReputationBook {
    /// An empty reputation book.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply a modifier to the character's standing toward `faction` and
    /// return the before/after values for auditing.
    pub fn apply(
        &mut self,
        faction: DefinitionId,
        modifier: ReputationModifier,
        base: Alignment,
        now: Millis,
    ) -> ReputationChange {
        let before = self.effective(&faction, base, now, &[]);
        let reason = modifier.reason.clone();
        let amount = modifier.amount;
        self.entries
            .entry(faction.clone())
            .or_default()
            .push(modifier);
        let after = self.effective(&faction, base, now, &[]);
        ReputationChange {
            faction,
            before,
            after,
            amount,
            reason,
        }
    }

    /// Resolve effective alignment for `faction` by adding every active
    /// character modifier and any caller-supplied temporary modifiers to
    /// `base`.
    #[must_use]
    pub fn effective(
        &self,
        faction: &DefinitionId,
        base: Alignment,
        now: Millis,
        temporary: &[ReputationModifier],
    ) -> Alignment {
        let mut total = base.value();
        if let Some(modifiers) = self.entries.get(faction) {
            total = total.saturating_add(active_sum(modifiers, now));
        }
        total = total.saturating_add(active_sum(temporary, now));
        Alignment::new(total)
    }

    /// Remove expired modifiers and return how many were removed.
    pub fn prune_expired(&mut self, now: Millis) -> usize {
        let mut removed = 0;
        for modifiers in self.entries.values_mut() {
            let before = modifiers.len();
            modifiers.retain(|modifier| modifier.is_active(now));
            removed += before - modifiers.len();
        }
        self.entries.retain(|_, modifiers| !modifiers.is_empty());
        removed
    }

    /// Every active modifier toward `faction`, ordered deterministically.
    #[must_use]
    pub fn modifiers(&self, faction: &DefinitionId, now: Millis) -> Vec<&ReputationModifier> {
        self.entries
            .get(faction)
            .map(|modifiers| {
                modifiers
                    .iter()
                    .filter(|modifier| modifier.is_active(now))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether the character has any record toward `faction`.
    #[must_use]
    pub fn is_tracked(&self, faction: &DefinitionId) -> bool {
        self.entries.contains_key(faction)
    }
}

/// Resolve alignment between an observer and a target, applying the observer's
/// character-specific reputation and any temporary modifiers.
#[must_use]
pub fn resolve_alignment(
    catalog: &FactionCatalog,
    observer_faction: &DefinitionId,
    target_faction: &DefinitionId,
    reputation: Option<&ReputationBook>,
    now: Millis,
    temporary: &[ReputationModifier],
) -> Alignment {
    let base = catalog.base_alignment(observer_faction, target_faction);
    reputation.map_or_else(
        || Alignment::new(base.value().saturating_add(active_sum(temporary, now))),
        |book| book.effective(target_faction, base, now, temporary),
    )
}

fn active_sum(modifiers: &[ReputationModifier], now: Millis) -> i16 {
    modifiers
        .iter()
        .filter(|modifier| modifier.is_active(now))
        .fold(0_i16, |total, modifier| {
            total.saturating_add(modifier.amount)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn faction(name: &str) -> DefinitionId {
        DefinitionId::new(name).unwrap()
    }

    #[test]
    fn alignment_clamps_and_classifies_with_defaults() {
        assert_eq!(Alignment::new(-500).value(), -100);
        assert_eq!(Alignment::new(500).value(), 100);
        assert_eq!(
            Alignment::new(0).class(Thresholds::default()),
            AlignmentClass::Neutral
        );
        assert_eq!(
            Alignment::new(-34).class(Thresholds::default()),
            AlignmentClass::Enemy
        );
        assert_eq!(
            Alignment::new(-33).class(Thresholds::default()),
            AlignmentClass::Neutral
        );
        assert_eq!(
            Alignment::new(33).class(Thresholds::default()),
            AlignmentClass::Neutral
        );
        assert_eq!(
            Alignment::new(34).class(Thresholds::default()),
            AlignmentClass::Friend
        );
    }

    #[test]
    fn thresholds_are_validated() {
        assert!(Thresholds::default().validate().is_ok());
        assert!(
            Thresholds {
                enemy_max: 40,
                friend_min: 10
            }
            .validate()
            .is_err()
        );
        assert!(
            Thresholds {
                enemy_max: -101,
                friend_min: 0
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn relations_are_directional() {
        let guards = faction("faction.guards");
        let bandits = faction("faction.bandits");
        let mut catalog = FactionCatalog::new();
        catalog.set(guards.clone(), bandits.clone(), Alignment::new(-80));

        assert_eq!(
            catalog.base_alignment(&guards, &bandits),
            Alignment::new(-80)
        );
        assert_eq!(
            catalog.class_of(&guards, &bandits, Thresholds::default()),
            AlignmentClass::Enemy
        );
        // The reverse direction is unlisted and therefore neutral.
        assert_eq!(
            catalog.base_alignment(&bandits, &guards),
            Alignment::NEUTRAL
        );

        catalog.set_bidirectional(bandits.clone(), guards.clone(), Alignment::new(-60));
        assert_eq!(
            catalog.base_alignment(&guards, &bandits),
            Alignment::new(-60)
        );
        assert_eq!(catalog.len(), 2);
    }

    #[test]
    fn character_reputation_is_additive_and_clamped() {
        let guards = faction("faction.guards");
        let mut catalog = FactionCatalog::new();
        catalog.set(guards.clone(), guards.clone(), Alignment::new(20));
        let mut book = ReputationBook::new();
        let now = Millis::new(1_000);

        let change = book.apply(
            guards.clone(),
            ReputationModifier::permanent(50, "returned the lost badges"),
            catalog.base_alignment(&guards, &guards),
            now,
        );
        assert_eq!(change.before, Alignment::new(20));
        assert_eq!(change.after, Alignment::new(70));
        assert_eq!(
            book.effective(&guards, Alignment::new(20), now, &[])
                .class(Thresholds::default()),
            AlignmentClass::Friend
        );

        // Additive and clamped at the boundary.
        book.apply(
            guards.clone(),
            ReputationModifier::permanent(500, "absurd heroism"),
            Alignment::new(20),
            now,
        );
        assert_eq!(
            book.effective(&guards, Alignment::new(20), now, &[]),
            Alignment::MAX
        );
    }

    #[test]
    fn temporary_modifiers_expire_and_prune() {
        let faction = faction("faction.guards");
        let now = Millis::new(1_000);
        let mut book = ReputationBook::new();
        book.apply(
            faction.clone(),
            ReputationModifier::expiring(-60, "caught stealing", Millis::new(2_000)),
            Alignment::NEUTRAL,
            now,
        );
        assert_eq!(
            book.effective(&faction, Alignment::NEUTRAL, now, &[])
                .class(Thresholds::default()),
            AlignmentClass::Enemy
        );
        assert_eq!(
            book.effective(&faction, Alignment::NEUTRAL, Millis::new(2_000), &[])
                .class(Thresholds::default()),
            AlignmentClass::Neutral
        );

        assert_eq!(book.prune_expired(Millis::new(2_000)), 1);
        assert!(!book.is_tracked(&faction));
    }

    #[test]
    fn temporary_modifiers_stack_with_character_reputation() {
        let faction = faction("faction.guards");
        let now = Millis::new(10);
        let mut book = ReputationBook::new();
        book.apply(
            faction.clone(),
            ReputationModifier::permanent(20, "quest"),
            Alignment::NEUTRAL,
            now,
        );
        let temporary = vec![ReputationModifier::expiring(-50, "brawl", Millis::new(20))];
        assert_eq!(
            resolve_alignment(
                &FactionCatalog::new(),
                &faction,
                &faction,
                Some(&book),
                now,
                &temporary
            ),
            Alignment::new(-30)
        );
        assert_eq!(
            resolve_alignment(
                &FactionCatalog::new(),
                &faction,
                &faction,
                Some(&book),
                Millis::new(20),
                &temporary
            ),
            Alignment::new(20)
        );
    }

    #[test]
    fn faction_definitions_round_trip() {
        let definition = FactionDefinition::new(faction("faction.town_guard"), "Town Guard");
        let json = serde_json::to_string(&definition).unwrap();
        let parsed: FactionDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(definition, parsed);
    }
}
