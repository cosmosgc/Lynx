//! Faction-aware perception and bounded ally assistance.

use engine_core::faction::{
    Alignment, AlignmentClass, FactionCatalog, Thresholds, resolve_alignment,
};
use engine_core::id::{DefinitionId, StableId};
use engine_core::math::Vec3;
use engine_core::time::Millis;
use serde::{Deserialize, Serialize};

use crate::entity::EntityRecord;
use crate::mind::Controller;
use crate::zone::Zone;

/// A bounded request for nearby allies to assist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelpRequest {
    /// Entity asking for help.
    pub requester: StableId,
    /// Requester position, used for range checks.
    pub position: Vec3,
    /// Requester faction.
    pub faction: DefinitionId,
    /// Maximum horizontal range in meters.
    pub range_m: f32,
    /// Maximum number of responders.
    pub max_responders: usize,
    /// Current time for reputation expiry.
    pub now: Millis,
}

impl HelpRequest {
    /// Create a request with a default limit of three responders.
    #[must_use]
    pub fn new(
        requester: StableId,
        position: Vec3,
        faction: DefinitionId,
        range_m: f32,
        now: Millis,
    ) -> Self {
        Self {
            requester,
            position,
            faction,
            range_m,
            max_responders: 3,
            now,
        }
    }
}

/// Why a candidate did not respond.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelpSkipped {
    /// Candidate entity.
    pub entity: StableId,
    /// Stable reason code.
    pub reason: String,
}

/// The bounded result of an assistance query.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelpSelection {
    /// Selected responders, nearest first.
    pub responders: Vec<StableId>,
    /// Number of candidates inspected.
    pub considered: usize,
    /// Skipped candidates and reasons, for AI debugging.
    pub skipped: Vec<HelpSkipped>,
}

/// Select nearby AI entities whose effective alignment treats the requester as
/// a friend.
///
/// Eligibility requires: alive, AI/scripted control, a faction, range, line of
/// sight, and a friendly alignment resolved from the candidate's perspective.
#[must_use]
pub fn select_helpers<'a>(
    request: &HelpRequest,
    entities: impl IntoIterator<Item = &'a EntityRecord>,
    line_of_sight: impl Fn(Vec3, Vec3) -> bool,
    catalog: &FactionCatalog,
    thresholds: Thresholds,
) -> HelpSelection {
    let mut selection = HelpSelection::default();
    let mut candidates = Vec::new();

    for entity in entities {
        selection.considered += 1;
        if entity.id == request.requester {
            continue;
        }
        if !entity.is_alive() {
            selection.skipped.push(skip(entity.id, "dead"));
            continue;
        }
        let Some(mind) = &entity.mind else {
            selection.skipped.push(skip(entity.id, "no_mind"));
            continue;
        };
        if !matches!(
            mind.controller,
            Controller::Ai { .. } | Controller::Scripted { .. }
        ) {
            selection.skipped.push(skip(entity.id, "not_ai"));
            continue;
        }
        let Some(membership) = &entity.faction else {
            selection.skipped.push(skip(entity.id, "no_faction"));
            continue;
        };
        let distance = request.position.distance_xz(entity.position());
        if distance > request.range_m {
            selection.skipped.push(skip(entity.id, "out_of_range"));
            continue;
        }
        if !line_of_sight(request.position, entity.position()) {
            selection.skipped.push(skip(entity.id, "no_line_of_sight"));
            continue;
        }
        let alignment = resolve_alignment(
            catalog,
            &membership.faction,
            &request.faction,
            Some(&membership.reputation),
            request.now,
            &[],
        );
        if alignment.class(thresholds) != AlignmentClass::Friend {
            selection.skipped.push(skip(entity.id, "not_friendly"));
            continue;
        }
        candidates.push((distance, entity.id));
    }

    candidates.sort_by(|(distance_a, id_a), (distance_b, id_b)| {
        distance_a
            .total_cmp(distance_b)
            .then_with(|| id_a.cmp(id_b))
    });
    selection.responders = candidates
        .into_iter()
        .take(request.max_responders)
        .map(|(_, id)| id)
        .collect();
    selection
}

/// A hostile entity perceived by a controller.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PerceivedTarget {
    /// Perceived entity.
    pub entity: StableId,
    /// Horizontal distance in meters.
    pub distance_m: f32,
    /// Effective alignment from the observer's perspective.
    pub alignment: Alignment,
}

/// Perceive hostile entities within range and line of sight, nearest first.
#[must_use]
pub fn perceive_hostiles<'a>(
    observer: &EntityRecord,
    entities: impl IntoIterator<Item = &'a EntityRecord>,
    line_of_sight: impl Fn(Vec3, Vec3) -> bool,
    catalog: &FactionCatalog,
    thresholds: Thresholds,
    range_m: f32,
    now: Millis,
) -> Vec<PerceivedTarget> {
    let Some(membership) = &observer.faction else {
        return Vec::new();
    };

    let mut targets: Vec<PerceivedTarget> = entities
        .into_iter()
        .filter(|entity| entity.id != observer.id && entity.is_alive())
        .filter_map(|entity| {
            let other = entity.faction.as_ref()?;
            let distance = observer.position().distance_xz(entity.position());
            if distance > range_m || !line_of_sight(observer.position(), entity.position()) {
                return None;
            }
            let alignment = resolve_alignment(
                catalog,
                &membership.faction,
                &other.faction,
                Some(&membership.reputation),
                now,
                &[],
            );
            (alignment.class(thresholds) == AlignmentClass::Enemy).then_some(PerceivedTarget {
                entity: entity.id,
                distance_m: distance,
                alignment,
            })
        })
        .collect();

    targets.sort_by(|a, b| {
        a.distance_m
            .total_cmp(&b.distance_m)
            .then_with(|| a.entity.cmp(&b.entity))
    });
    targets
}

impl Zone {
    /// Select nearby allies willing to help, using this zone's nav grid for
    /// line of sight.
    #[must_use]
    pub fn select_helpers(
        &self,
        catalog: &FactionCatalog,
        thresholds: Thresholds,
        request: &HelpRequest,
    ) -> HelpSelection {
        select_helpers(
            request,
            self.entities.values(),
            |from, to| self.nav.line_of_sight(from, to),
            catalog,
            thresholds,
        )
    }

    /// Perceive hostile entities using this zone's nav grid.
    #[must_use]
    pub fn perceive_hostiles(
        &self,
        observer_id: StableId,
        catalog: &FactionCatalog,
        thresholds: Thresholds,
        range_m: f32,
        now: Millis,
    ) -> Vec<PerceivedTarget> {
        self.entities
            .get(&observer_id)
            .map_or_else(Vec::new, |observer| {
                perceive_hostiles(
                    observer,
                    self.entities.values(),
                    |from, to| self.nav.line_of_sight(from, to),
                    catalog,
                    thresholds,
                    range_m,
                    now,
                )
            })
    }
}

fn skip(entity: StableId, reason: &str) -> HelpSkipped {
    HelpSkipped {
        entity,
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::id::IdKind;
    use engine_core::inventory::Inventory;
    use engine_core::schema::PersistencePolicy;

    use crate::entity::{FactionMembership, Health, Interactable};
    use crate::mind::Mind;

    fn id(seed: u128) -> StableId {
        StableId::from_parts(IdKind::Entity, 1, seed)
    }

    fn faction(name: &str) -> DefinitionId {
        DefinitionId::new(name).unwrap()
    }

    fn ai_at(seed: u128, faction_name: &str, x: f32, z: f32) -> EntityRecord {
        EntityRecord::new(
            id(seed),
            DefinitionId::new("npc.guard").unwrap(),
            Vec3::new(x, 0.0, z),
        )
        .with_health(100)
        .with_mind(Mind::ai(DefinitionId::new("behavior.guard").unwrap()))
        .with_faction(FactionMembership::new(faction(faction_name)))
        .with_interactable(Interactable::new(2.0, std::iter::empty()))
        .with_persistence(PersistencePolicy::PersistentEntity)
        .with_inventory(Inventory::with_capacity(4).unwrap())
    }

    fn catalog() -> FactionCatalog {
        let mut catalog = FactionCatalog::new();
        let guards = faction("faction.guards");
        let bandits = faction("faction.bandits");
        catalog.set_bidirectional(guards.clone(), guards.clone(), Alignment::new(80));
        catalog.set_bidirectional(bandits.clone(), bandits.clone(), Alignment::new(60));
        catalog.set(guards.clone(), bandits.clone(), Alignment::new(-90));
        catalog.set(bandits.clone(), guards.clone(), Alignment::new(-90));
        catalog
    }

    #[test]
    fn only_friendly_allies_in_range_respond() {
        let guards = faction("faction.guards");
        let requester = id(1);
        let near = ai_at(2, "faction.guards", 2.0, 0.0);
        let far = ai_at(3, "faction.guards", 50.0, 0.0);
        let bandit = ai_at(4, "faction.bandits", 1.0, 0.0);
        let entities = vec![near, far, bandit];

        let request = HelpRequest::new(requester, Vec3::ZERO, guards, 10.0, Millis::new(10));
        let selection = select_helpers(
            &request,
            &entities,
            |_, _| true,
            &catalog(),
            Thresholds::default(),
        );

        assert_eq!(selection.responders, vec![id(2)]);
        assert!(selection.considered >= 3);
        assert!(
            selection
                .skipped
                .iter()
                .any(|s| s.entity == id(3) && s.reason == "out_of_range")
        );
        assert!(
            selection
                .skipped
                .iter()
                .any(|s| s.entity == id(4) && s.reason == "not_friendly")
        );
    }

    #[test]
    fn responders_are_capped_and_sorted_by_distance() {
        let guards = faction("faction.guards");
        let entities = vec![
            ai_at(5, "faction.guards", 5.0, 0.0),
            ai_at(2, "faction.guards", 2.0, 0.0),
            ai_at(3, "faction.guards", 3.0, 0.0),
        ];
        let mut request = HelpRequest::new(id(1), Vec3::ZERO, guards, 10.0, Millis::new(10));
        request.max_responders = 2;
        let selection = select_helpers(
            &request,
            &entities,
            |_, _| true,
            &catalog(),
            Thresholds::default(),
        );
        assert_eq!(selection.responders, vec![id(2), id(3)]);
    }

    #[test]
    fn line_of_sight_filters_responders() {
        let guards = faction("faction.guards");
        let entities = vec![ai_at(2, "faction.guards", 2.0, 0.0)];
        let request = HelpRequest::new(id(1), Vec3::ZERO, guards, 10.0, Millis::new(10));
        let selection = select_helpers(
            &request,
            &entities,
            |_, _| false,
            &catalog(),
            Thresholds::default(),
        );
        assert_eq!(selection.responders, Vec::<StableId>::new());
        assert_eq!(selection.skipped[0].reason, "no_line_of_sight");
    }

    #[test]
    fn dead_and_non_ai_entities_do_not_respond() {
        let guards = faction("faction.guards");
        let mut dead = ai_at(2, "faction.guards", 1.0, 0.0);
        dead.health = Some(Health {
            current: 0,
            max: 10,
        });
        let mut player = ai_at(3, "faction.guards", 1.0, 0.0);
        player.mind = Some(Mind::player(id(99), "session"));

        let request = HelpRequest::new(id(1), Vec3::ZERO, guards, 10.0, Millis::new(10));
        let selection = select_helpers(
            &request,
            &[dead, player],
            |_, _| true,
            &catalog(),
            Thresholds::default(),
        );
        assert_eq!(selection.responders, Vec::<StableId>::new());
        assert!(selection.skipped.iter().any(|s| s.reason == "dead"));
        assert!(selection.skipped.iter().any(|s| s.reason == "not_ai"));
    }

    #[test]
    fn perception_returns_hostiles_nearest_first() {
        let guard = ai_at(1, "faction.guards", 0.0, 0.0);
        let near = ai_at(2, "faction.bandits", 2.0, 0.0);
        let far = ai_at(3, "faction.bandits", 6.0, 0.0);
        let friend = ai_at(4, "faction.guards", 1.0, 0.0);
        let entities = vec![guard.clone(), near, far, friend];

        let targets = perceive_hostiles(
            &guard,
            &entities,
            |_, _| true,
            &catalog(),
            Thresholds::default(),
            10.0,
            Millis::new(10),
        );
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].entity, id(2));
        assert_eq!(targets[1].entity, id(3));
    }
}
