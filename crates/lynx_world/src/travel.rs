//! Authored-route validation and in-process player handoff coordination.
//!
//! This module is the server-side seam between atlas topology and the
//! authoritative simulation. It rejects unplaced or un-routed pairs before
//! delegating the state move to `Zone::transfer_entity_to`. Receipts are bounded
//! and idempotent for the lifetime of this coordinator; durable cross-zone
//! receipts and atomic multi-zone checkpoints remain a persistence follow-up.

use std::collections::{BTreeMap, VecDeque};

use engine_content::{RouteKind, WorldMapDefinition};
use engine_core::error::{EngineError, ErrorCode};
use engine_core::id::{IdKind, StableId};
use engine_core::math::Vec3;
use engine_sim::zone::Zone;

use crate::authority::ZoneAuthoritySet;

/// Maximum in-process handoff receipts retained for retry deduplication.
pub const HANDOFF_RECEIPT_LIMIT: usize = 1_024;

/// Trusted server input for one player handoff.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerHandoffRequest {
    /// Stable player-character principal.
    pub character: StableId,
    /// Server-selected destination position.
    pub arrival: Vec3,
    /// Unique server request key for retries.
    pub request_key: String,
}

impl PlayerHandoffRequest {
    /// Build one handoff request from trusted server state.
    #[must_use]
    pub fn new(character: StableId, arrival: Vec3, request_key: impl Into<String>) -> Self {
        Self {
            character,
            arrival,
            request_key: request_key.into(),
        }
    }
}

/// Server-created evidence of one completed zone handoff.
#[derive(Debug, Clone, PartialEq)]
pub struct ZoneHandoffReceipt {
    /// Caller-supplied server request key.
    pub request_key: String,
    /// Stable player-character principal.
    pub character: StableId,
    /// Stable entity identity moved between zones.
    pub entity: StableId,
    /// Source zone instance.
    pub source_instance: StableId,
    /// Destination zone instance.
    pub destination_instance: StableId,
    /// Authored route style used for the handoff.
    pub route: RouteKind,
    /// Server-selected destination position.
    pub destination: Vec3,
}

/// Coordinates validated, duplicate-safe handoffs between two live zones.
///
/// A receipt key may be retried with identical inputs and returns the original
/// result without moving the entity again. Reusing a key with different
/// principal, zones, route, or arrival is a conflict. This implementation is
/// intentionally in-memory; it does not claim crash-safe exactly-once transfer.
#[derive(Debug, Default)]
pub struct ZoneHandoffCoordinator {
    receipts: BTreeMap<String, ZoneHandoffReceipt>,
    order: VecDeque<String>,
}

impl ZoneHandoffCoordinator {
    /// Validate an authored route and move one persistent living player entity.
    ///
    /// The destination must be an atlas-placed zone, and travel must be an
    /// authored directed route or an explicitly bidirectional reverse route.
    /// The caller owns the zones exclusively for the duration of this call and
    /// must present a lease set containing both zone instances. It is also
    /// responsible for disconnecting the old session and opening a fresh
    /// destination session from its private snapshot.
    ///
    /// # Errors
    /// Returns an error for invalid request keys/principals, missing route or
    /// atlas placement, ambiguous/missing player actors, an existing
    /// destination actor, or any simulation transfer validation failure.
    #[allow(clippy::too_many_lines)] // Keep route, lease, actor, and receipt validation in one transaction.
    pub fn handoff_player(
        &mut self,
        atlas: &WorldMapDefinition,
        authorities: &ZoneAuthoritySet,
        source: &mut Zone,
        destination: &mut Zone,
        request: &PlayerHandoffRequest,
    ) -> Result<ZoneHandoffReceipt, EngineError> {
        let PlayerHandoffRequest {
            character,
            arrival,
            request_key,
        } = request;
        if request_key.is_empty() || request_key.len() > 128 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "handoff request key must contain 1 to 128 bytes",
            ));
        }
        character.expect_kind(IdKind::Character)?;
        for instance in [source.config().instance, destination.config().instance] {
            let Some(authority) = authorities.get(&instance.to_string()) else {
                return Err(EngineError::new(
                    ErrorCode::InvalidState,
                    "handoff requires authority over both zone instances",
                ));
            };
            if authority.renew()?.is_none() {
                return Err(EngineError::new(
                    ErrorCode::Conflict,
                    "zone authority was lost before handoff",
                ));
            }
        }
        let source_definition = &source.config().definition;
        let destination_definition = &destination.config().definition;
        let route = atlas
            .route_kind(source_definition, destination_definition)
            .filter(|_| {
                atlas
                    .zones
                    .iter()
                    .any(|zone| zone.zone == *source_definition)
                    && atlas
                        .zones
                        .iter()
                        .any(|zone| zone.zone == *destination_definition)
            })
            .ok_or_else(|| {
                EngineError::new(
                    ErrorCode::InvalidRequest,
                    "no authored route connects the source and destination zones",
                )
            })?;

        if let Some(receipt) = self.receipts.get(request_key) {
            if receipt.character == *character
                && receipt.source_instance == source.config().instance
                && receipt.destination_instance == destination.config().instance
                && receipt.route == route
                && receipt.destination == *arrival
            {
                return Ok(receipt.clone());
            }
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "handoff request key was already used with different inputs",
            ));
        }

        let entity = {
            let mut source_actors = source.entities().filter(|entity| {
                entity
                    .mind
                    .as_ref()
                    .is_some_and(|mind| mind.player_character == Some(*character))
            });
            let actor = source_actors
                .next()
                .ok_or_else(|| EngineError::not_found("player actor", *character))?;
            if source_actors.next().is_some() {
                return Err(EngineError::new(
                    ErrorCode::Conflict,
                    "the character has multiple actors in the source zone",
                ));
            }
            actor.id
        };
        if destination.entities().any(|candidate| {
            candidate
                .mind
                .as_ref()
                .is_some_and(|mind| mind.player_character == Some(*character))
        }) {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "the character already has an actor in the destination zone",
            ));
        }

        source.transfer_entity_to(destination, entity, *arrival)?;
        let receipt = ZoneHandoffReceipt {
            request_key: request_key.clone(),
            character: *character,
            entity,
            source_instance: source.config().instance,
            destination_instance: destination.config().instance,
            route,
            destination: *arrival,
        };
        self.receipts.insert(request_key.clone(), receipt.clone());
        self.order.push_back(request_key.clone());
        while self.order.len() > HANDOFF_RECEIPT_LIMIT {
            if let Some(expired) = self.order.pop_front() {
                self.receipts.remove(&expired);
            }
        }
        Ok(receipt)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use engine_content::{DisplayText, MapPosition, MapZone, ZoneRoute};
    use engine_core::i18n::MessageKey;
    use engine_core::inventory::UniformItemRules;
    use engine_core::schema::{PersistencePolicy, SchemaVersion};
    use engine_core::time::Tick;
    use engine_sim::combat::DamageType;
    use engine_sim::entity::{
        EntityRecord, MovementProfile, TimedDamageOverTime, TimedShield, TimedSlow,
    };
    use engine_sim::mind::{Controller, Mind};
    use engine_sim::nav::NavGrid;
    use engine_sim::zone::ZoneConfig;

    use super::*;
    use crate::authority::tests::MemoryLeases;
    use crate::persistence::ZoneLeaseStore;

    fn id(kind: IdKind, value: u128) -> StableId {
        StableId::from_parts(kind, 1, value)
    }

    fn definition(value: &str) -> engine_core::id::DefinitionId {
        engine_core::id::DefinitionId::new(value).unwrap()
    }

    fn zone(definition_id: &str, instance: u128) -> Zone {
        let config = ZoneConfig::new(
            definition(definition_id),
            id(IdKind::Zone, instance),
            Vec3::ZERO,
            Vec3::new(10.0, 2.0, 10.0),
        )
        .unwrap();
        Zone::new(
            config,
            NavGrid::new(10, 10, 1.0, Vec3::ZERO).unwrap(),
            Arc::new(UniformItemRules::new(10)),
        )
        .unwrap()
    }

    fn atlas(bidirectional: bool) -> WorldMapDefinition {
        let source = definition("zone.source");
        let destination = definition("zone.destination");
        WorldMapDefinition {
            id: definition("map.test"),
            version: SchemaVersion::new(1, 0),
            display: DisplayText {
                name: MessageKey::parse("map.test.name").unwrap(),
                description: MessageKey::parse("map.test.description").unwrap(),
            },
            zones: vec![
                MapZone {
                    zone: source.clone(),
                    position: MapPosition { x: 0, y: 0 },
                },
                MapZone {
                    zone: destination.clone(),
                    position: MapPosition { x: 1, y: 0 },
                },
            ],
            routes: vec![ZoneRoute {
                from: source,
                to: destination,
                bidirectional,
                kind: RouteKind::Gateway,
            }],
        }
    }

    fn authorities(source: u128, destination: u128) -> ZoneAuthoritySet {
        let store: Arc<dyn ZoneLeaseStore> = Arc::new(MemoryLeases::default());
        ZoneAuthoritySet::acquire(
            &store,
            [
                id(IdKind::Zone, source).to_string(),
                id(IdKind::Zone, destination).to_string(),
            ],
            "travel-test-owner",
            Duration::from_secs(30),
        )
        .unwrap()
        .unwrap()
    }

    #[test]
    #[allow(clippy::too_many_lines)] // Exercises handoff, retry deduplication, and snapshot recovery together.
    fn authored_handoff_is_duplicate_safe_and_survives_zone_checkpoint_recovery() {
        let atlas = atlas(false);
        let character = id(IdKind::Character, 7);
        let actor = id(IdKind::Entity, 7);
        let mut source = zone("zone.source", 1);
        source
            .spawn(
                EntityRecord::new(
                    actor,
                    definition("character.player"),
                    Vec3::new(1.5, 0.0, 1.5),
                )
                .with_health(50)
                .with_mind(Mind::player(character, "source-session"))
                .with_movement(MovementProfile::default())
                .with_persistence(PersistencePolicy::PersistentEntity),
            )
            .unwrap();
        let mut destination = zone("zone.destination", 2);
        for _ in 0..3 {
            source.advance_tick();
        }
        for _ in 0..10 {
            destination.advance_tick();
        }
        let source_actor = source.entity_mut(actor).unwrap();
        source_actor.stun_until = Some(Tick(7));
        source_actor.slow = Some(TimedSlow {
            until: Tick(8),
            multiplier: 0.5,
        });
        source_actor.damage_over_time.push(TimedDamageOverTime {
            source: actor,
            damage_type: DamageType::Fire,
            damage_per_tick: 2,
            interval_ticks: 3,
            next_tick: Tick(4),
            until: Tick(8),
        });
        source_actor.shields.push(TimedShield {
            source: actor,
            remaining: 5,
            until: Tick(8),
        });
        let mut coordinator = ZoneHandoffCoordinator::default();
        let authorities = authorities(1, 2);
        let arrival = Vec3::new(5.5, 0.0, 5.5);

        let receipt = coordinator
            .handoff_player(
                &atlas,
                &authorities,
                &mut source,
                &mut destination,
                &PlayerHandoffRequest::new(character, arrival, "travel-1"),
            )
            .unwrap();
        assert_eq!(receipt.entity, actor);
        assert!(source.entity(actor).is_none());
        let moved = destination.entity(actor).unwrap();
        assert_eq!(moved.position(), arrival);
        assert_eq!(moved.mind.as_ref().unwrap().controller, Controller::None);
        assert_eq!(
            moved.mind.as_ref().unwrap().player_character,
            Some(character)
        );
        assert_eq!(moved.stun_until, Some(Tick(14)));
        assert_eq!(moved.slow.unwrap().until, Tick(15));
        assert_eq!(moved.damage_over_time[0].next_tick, Tick(11));
        assert_eq!(moved.damage_over_time[0].until, Tick(15));
        assert_eq!(moved.shields[0].until, Tick(15));

        let retried = coordinator
            .handoff_player(
                &atlas,
                &authorities,
                &mut source,
                &mut destination,
                &PlayerHandoffRequest::new(character, arrival, "travel-1"),
            )
            .unwrap();
        assert_eq!(retried, receipt);
        assert_eq!(destination.entities().count(), 1);
        assert!(
            coordinator
                .handoff_player(
                    &atlas,
                    &authorities,
                    &mut source,
                    &mut destination,
                    &PlayerHandoffRequest::new(character, Vec3::new(6.5, 0.0, 5.5), "travel-1",),
                )
                .is_err()
        );

        let source_snapshot = source.snapshot();
        let destination_snapshot = destination.snapshot();
        let restored_source = Zone::restore(
            &source_snapshot,
            NavGrid::new(10, 10, 1.0, Vec3::ZERO).unwrap(),
            Arc::new(UniformItemRules::new(10)),
        )
        .unwrap();
        let restored_destination = Zone::restore(
            &destination_snapshot,
            NavGrid::new(10, 10, 1.0, Vec3::ZERO).unwrap(),
            Arc::new(UniformItemRules::new(10)),
        )
        .unwrap();
        assert!(restored_source.entity(actor).is_none());
        assert!(restored_destination.entity(actor).is_some());
    }

    #[test]
    fn arbitrary_and_reverse_unpublished_routes_cannot_move_a_player() {
        let atlas = atlas(false);
        let character = id(IdKind::Character, 8);
        let actor = id(IdKind::Entity, 8);
        let mut source = zone("zone.destination", 2);
        source
            .spawn(
                EntityRecord::new(
                    actor,
                    definition("character.player"),
                    Vec3::new(1.5, 0.0, 1.5),
                )
                .with_health(50)
                .with_mind(Mind::player(character, "source-session"))
                .with_persistence(PersistencePolicy::PersistentEntity),
            )
            .unwrap();
        let mut destination = zone("zone.source", 1);
        let mut coordinator = ZoneHandoffCoordinator::default();
        let authorities = authorities(2, 1);

        assert!(
            coordinator
                .handoff_player(
                    &atlas,
                    &authorities,
                    &mut source,
                    &mut destination,
                    &PlayerHandoffRequest::new(
                        character,
                        Vec3::new(5.5, 0.0, 5.5),
                        "reverse-denied",
                    ),
                )
                .is_err()
        );
        assert!(source.entity(actor).is_some());
        assert!(destination.entity(actor).is_none());
    }

    #[test]
    fn blocked_arrival_leaves_the_source_entity_untouched() {
        let atlas = atlas(false);
        let character = id(IdKind::Character, 9);
        let actor = id(IdKind::Entity, 9);
        let mut source = zone("zone.source", 1);
        source
            .spawn(
                EntityRecord::new(
                    actor,
                    definition("character.player"),
                    Vec3::new(1.5, 0.0, 1.5),
                )
                .with_health(50)
                .with_mind(Mind::player(character, "source-session"))
                .with_persistence(PersistencePolicy::PersistentEntity),
            )
            .unwrap();
        let mut destination = zone("zone.destination", 2);
        destination.nav_mut().set_blocked(5, 5, true);
        let mut coordinator = ZoneHandoffCoordinator::default();
        let authorities = authorities(1, 2);

        assert!(
            coordinator
                .handoff_player(
                    &atlas,
                    &authorities,
                    &mut source,
                    &mut destination,
                    &PlayerHandoffRequest::new(
                        character,
                        Vec3::new(5.5, 0.0, 5.5),
                        "blocked-arrival",
                    ),
                )
                .is_err()
        );
        assert!(source.entity(actor).is_some());
        assert!(destination.entity(actor).is_none());
    }
}
