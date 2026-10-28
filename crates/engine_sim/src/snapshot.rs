//! Zone-state snapshots for durable persistence.
//!
//! A [`ZoneSnapshot`] captures every part of one zone that the authoritative
//! simulation owns: entity records, links, the tick counter, idempotency
//! caches, and active cooldowns. Persistence follows the explicit persistence
//! policy of each entity: entities marked
//! [`PersistencePolicy::Transient`] or [`PersistencePolicy::Session`] are never
//! saved, and a link is only saved when both of its endpoints were saved.
//!
//! [`Zone::restore`] is strict: every entity, link, cooldown, and transfer
//! receipt in the snapshot is re-validated against a fresh zone, so a stale or
//! tampered file fails startup instead of silently yielding a partial world.
//! Client input and wire payloads never reach this path; only an authoritative
//! caller may adopt a snapshot.

use std::collections::BTreeSet;
use std::sync::Arc;

use engine_core::error::{EngineError, ErrorCode};
use engine_core::id::{IdKind, StableId};
use engine_core::inventory::{ItemRules, TransferReceipt};
use engine_core::schema::{PersistencePolicy, SchemaVersion};
use engine_core::time::Tick;

use crate::action::{ActionEffect, ActionKey, ActionResult};
use crate::entity::EntityRecord;
use crate::link::{EntityLink, LinkEvent};
use crate::nav::NavGrid;
use crate::zone::{ACTION_CACHE_LIMIT, LINK_CACHE_LIMIT, Zone, ZoneConfig};

/// The schema version written by this engine release.
pub const ZONE_SNAPSHOT_VERSION: SchemaVersion = SchemaVersion::new(1, 0);

/// A saved world snapshot of one zone.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ZoneSnapshot {
    /// Version written when the snapshot was created.
    pub schema_version: SchemaVersion,
    /// Static configuration the zone was running with.
    pub config: ZoneConfig,
    /// The authoritative tick when the snapshot was taken.
    pub tick: Tick,
    /// Entities whose persistence policy is
    /// [`PersistencePolicy::PersistentEntity`], in stable-ID order.
    pub entities: Vec<EntityRecord>,
    /// Links whose endpoints are all saved entities, in graph order.
    pub links: Vec<EntityLink>,
    /// Action result cache in dedupe order, for idempotent retries.
    pub action_results: Vec<(String, ActionResult)>,
    /// Link-activation result cache in dedupe order.
    pub link_results: Vec<(String, Vec<ActionEffect>)>,
    /// Active action cooldowns keyed by actor, verb, and target.
    pub cooldowns: Vec<(ActionKey, Tick)>,
    /// Active link cooldowns keyed by source, target, and event.
    pub link_cooldowns: Vec<((StableId, StableId, LinkEvent), Tick)>,
    /// Applied inventory-transfer receipts, for idempotent retries.
    pub transfers: Vec<TransferReceipt>,
    /// Actions applied since the zone started.
    pub applied_actions: u64,
    /// Link activations since the zone started.
    pub link_activations: u64,
}

impl Zone {
    /// A snapshot of the zone's persistent state.
    ///
    /// Entities without the [`PersistencePolicy::PersistentEntity`] policy and
    /// every link touching them are excluded. Sorted storage gives one
    /// canonical ordering for identical world state.
    #[must_use]
    pub fn snapshot(&self) -> ZoneSnapshot {
        let saved: BTreeSet<StableId> = self
            .entities
            .values()
            .filter(|entity| entity.persistence == PersistencePolicy::PersistentEntity)
            .map(|entity| entity.id)
            .collect();

        let entities = self
            .entities
            .values()
            .filter(|entity| saved.contains(&entity.id))
            .cloned()
            .collect();

        let links = self
            .links
            .links()
            .iter()
            .filter(|link| saved.contains(&link.source) && saved.contains(&link.target))
            .cloned()
            .collect();

        let action_results: Vec<(String, ActionResult)> = self
            .action_order
            .iter()
            .filter_map(|request| {
                self.action_results
                    .get(request)
                    .map(|result| (request.clone(), result.clone()))
            })
            .collect();

        let link_results: Vec<(String, Vec<ActionEffect>)> = self
            .link_order
            .iter()
            .filter_map(|request| {
                self.link_results
                    .get(request)
                    .map(|effects| (request.clone(), effects.clone()))
            })
            .collect();

        let cooldowns = self
            .cooldowns
            .iter()
            .filter(|(key, _)| {
                saved.contains(&key.0) && key.2.is_none_or(|target| saved.contains(&target))
            })
            .map(|(key, until)| (key.clone(), *until))
            .collect();

        let link_cooldowns = self
            .link_cooldowns
            .iter()
            .filter(|((source, target, _), _)| saved.contains(source) && saved.contains(target))
            .map(|(key, until)| (*key, *until))
            .collect();

        let transfers = self.transfers.applied().cloned().collect();

        ZoneSnapshot {
            schema_version: ZONE_SNAPSHOT_VERSION,
            config: self.config.clone(),
            tick: self.tick,
            entities,
            links,
            action_results,
            link_results,
            cooldowns,
            link_cooldowns,
            transfers,
            applied_actions: self.stats.applied_actions,
            link_activations: self.stats.link_activations,
        }
    }

    /// Rebuild a zone from a snapshot plus runtime-only state.
    ///
    /// The snapshot supplies the configuration; the caller supplies navigation
    /// and item rules, which are never serialized. Every entity is re-validated
    /// through [`Zone::spawn`], so the same bounds, capacity, and duplicate
    /// checks apply.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] for an unsupported snapshot
    /// version or a non-zone instance ID, [`ErrorCode::NotFound`] when a link
    /// or cooldown references a missing entity, [`ErrorCode::Conflict`] for
    /// duplicate idempotency keys, and every error
    /// [`Zone::new`], [`Zone::spawn`], and [`Zone::add_link`] can produce.
    pub fn restore(
        snapshot: &ZoneSnapshot,
        nav: NavGrid,
        rules: Arc<dyn ItemRules + Send + Sync>,
    ) -> Result<Self, EngineError> {
        if snapshot.schema_version != ZONE_SNAPSHOT_VERSION {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "unsupported zone snapshot schema version",
            )
            .with_detail("snapshot", snapshot.schema_version.to_string())
            .with_detail("expected", ZONE_SNAPSHOT_VERSION.to_string()));
        }
        snapshot.config.instance.expect_kind(IdKind::Zone)?;

        let mut zone = Zone::new(snapshot.config.clone(), nav, rules)?;
        zone.tick = snapshot.tick;
        zone.stats.tick = snapshot.tick;
        zone.stats.applied_actions = snapshot.applied_actions;
        zone.stats.link_activations = snapshot.link_activations;

        for entity in &snapshot.entities {
            zone.spawn(entity.clone())?;
        }
        for link in &snapshot.links {
            zone.add_link(link.clone())?;
        }

        if snapshot.action_results.len() > ACTION_CACHE_LIMIT
            || snapshot.link_results.len() > LINK_CACHE_LIMIT
        {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "snapshot idempotency cache exceeds its limit",
            ));
        }
        for (request, result) in &snapshot.action_results {
            if request != &result.request_id {
                return Err(EngineError::new(
                    ErrorCode::InvalidRequest,
                    "snapshot action cache key does not match result",
                ));
            }
            if zone.action_results.contains_key(request) {
                return Err(EngineError::new(
                    ErrorCode::Conflict,
                    "duplicate snapshot action key",
                ));
            }
            zone.action_results.insert(request.clone(), result.clone());
            zone.action_order.push_back(request.clone());
        }
        for (request, effects) in &snapshot.link_results {
            if zone.link_results.contains_key(request) {
                return Err(EngineError::new(
                    ErrorCode::Conflict,
                    "duplicate snapshot link key",
                ));
            }
            zone.link_results.insert(request.clone(), effects.clone());
            zone.link_order.push_back(request.clone());
        }

        for (key, until) in &snapshot.cooldowns {
            if !zone.entities.contains_key(&key.0) {
                return Err(EngineError::not_found("cooldown actor", key.0));
            }
            if let Some(target) = key.2
                && !zone.entities.contains_key(&target)
            {
                return Err(EngineError::not_found("cooldown target", target));
            }
            zone.cooldowns.insert(key.clone(), *until);
        }
        for (key, until) in &snapshot.link_cooldowns {
            let (source, target, _) = key;
            if !zone.entities.contains_key(source) {
                return Err(EngineError::not_found("link source", *source));
            }
            if !zone.entities.contains_key(target) {
                return Err(EngineError::not_found("link target", *target));
            }
            zone.link_cooldowns.insert(*key, *until);
        }

        let mut transfer_keys = BTreeSet::new();
        for receipt in &snapshot.transfers {
            if receipt.key.is_empty() || receipt.quantity == 0 {
                return Err(EngineError::new(
                    ErrorCode::InvalidRequest,
                    "invalid snapshot transfer receipt",
                ));
            }
            receipt.source.expect_kind(IdKind::Entity)?;
            receipt.destination.expect_kind(IdKind::Entity)?;
            if !transfer_keys.insert(&receipt.key) {
                return Err(EngineError::new(
                    ErrorCode::Conflict,
                    "duplicate snapshot transfer key",
                ));
            }
        }
        zone.transfers.adopt(snapshot.transfers.clone());

        zone.stats.entity_count = zone.entities.len();
        zone.stats.controlled_entities = zone
            .entities
            .values()
            .filter(|entity| entity.is_controlled())
            .count();
        Ok(zone)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::Door;
    use crate::mind::Mind;
    use engine_core::id::DefinitionId;
    use engine_core::inventory::{Inventory, ItemStack, UniformItemRules};
    use engine_core::math::Vec3;

    fn rules() -> Arc<dyn ItemRules + Send + Sync> {
        Arc::new(UniformItemRules::new(9))
    }

    fn nav() -> NavGrid {
        NavGrid::new(10, 10, 2.0, Vec3::ZERO).unwrap()
    }

    fn zone() -> Zone {
        let config = ZoneConfig::new(
            DefinitionId::new("zone.test").unwrap(),
            StableId::from_parts(IdKind::Zone, 1, 1),
            Vec3::ZERO,
            Vec3::new(20.0, 4.0, 20.0),
        )
        .unwrap()
        .with_max_entities(4);
        Zone::new(config, nav(), rules()).unwrap()
    }

    fn entity(seed: u128, policy: PersistencePolicy) -> EntityRecord {
        // Keep every test position inside the 20 m zone; small seeds only.
        let offset = f32::from(u8::try_from(seed % 16).unwrap_or_default());
        EntityRecord::new(
            StableId::from_parts(IdKind::Entity, 1, seed),
            DefinitionId::new("prop.crate").unwrap(),
            Vec3::new(offset * 0.5, 0.0, offset * 0.25),
        )
        .with_persistence(policy)
    }

    fn link(source: StableId, target: StableId) -> EntityLink {
        EntityLink::new(
            DefinitionId::new("link.lever_to_door").unwrap(),
            source,
            target,
            [LinkEvent::Activated],
        )
    }

    #[test]
    fn a_zone_round_trips_through_a_snapshot() {
        let mut source = zone();
        let lever = source
            .spawn(entity(1, PersistencePolicy::PersistentEntity))
            .unwrap();
        let door = source
            .spawn(
                entity(2, PersistencePolicy::PersistentEntity)
                    .with_health(50)
                    .with_door(Door::new())
                    .with_mind(Mind::player(
                        StableId::from_parts(IdKind::Character, 3, 7),
                        "session-test",
                    )),
            )
            .unwrap();
        source
            .spawn(entity(3, PersistencePolicy::Transient))
            .unwrap();
        source.spawn(entity(4, PersistencePolicy::Session)).unwrap();
        source.add_link(link(lever, door)).unwrap();

        for _ in 0..3 {
            source.advance_tick();
        }
        let effects = source
            .activate_links("req-1", lever, LinkEvent::Activated)
            .unwrap();
        assert_eq!(effects.len(), 2, "the lever activates and toggles the door");

        let snapshot = source.snapshot();
        let mut restored = Zone::restore(&snapshot, nav(), rules()).unwrap();

        assert_eq!(restored.tick(), Tick(3));
        assert_eq!(restored.entity_count(), 2);
        assert_eq!(
            restored.entity(door).unwrap().door,
            Some(Door {
                open: true,
                locked: false,
                key: None,
            })
        );
        // The replay cache keeps retries idempotent across the restart: the
        // repeat returns the original effects, not a fresh activation.
        assert_eq!(
            restored
                .activate_links("req-1", lever, LinkEvent::Activated)
                .unwrap(),
            effects
        );
        assert_eq!(restored.snapshot(), snapshot);
    }

    #[test]
    fn an_unsupported_snapshot_version_is_rejected() {
        let mut source = zone();
        source
            .spawn(entity(1, PersistencePolicy::PersistentEntity))
            .unwrap();
        let mut snapshot = source.snapshot();
        snapshot.schema_version = SchemaVersion::new(99, 0);
        let error = Zone::restore(&snapshot, nav(), rules()).unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidRequest);
        assert!(error.details().contains_key("expected"));
    }

    #[test]
    fn a_stale_link_endpoint_fails_restore() {
        let mut source = zone();
        let lever = source
            .spawn(entity(1, PersistencePolicy::PersistentEntity))
            .unwrap();
        let door = source
            .spawn(entity(2, PersistencePolicy::PersistentEntity))
            .unwrap();
        source.add_link(link(lever, door)).unwrap();

        let snapshot = source.snapshot();
        let mut truncated = snapshot.clone();
        truncated.entities.retain(|entity| entity.id != door);
        let error = Zone::restore(&truncated, nav(), rules()).unwrap_err();
        assert_eq!(error.code(), ErrorCode::NotFound);
    }

    #[test]
    fn a_cooldown_for_a_missing_entity_fails_restore() {
        let mut source = zone();
        source
            .spawn(entity(1, PersistencePolicy::PersistentEntity))
            .unwrap();
        let mut snapshot = source.snapshot();
        snapshot.cooldowns.push((
            (
                StableId::from_parts(IdKind::Entity, 1, 42),
                crate::action::Verb::Open,
                None,
            ),
            Tick(5),
        ));
        let error = Zone::restore(&snapshot, nav(), rules()).unwrap_err();
        assert_eq!(error.code(), ErrorCode::NotFound);
    }

    #[test]
    fn restore_rejects_corrupt_idempotency_caches() {
        let mut snapshot = zone().snapshot();
        let result = ActionResult {
            request_id: "action-1".into(),
            actor: StableId::from_parts(IdKind::Entity, 1, 1),
            verb: crate::action::Verb::Open,
            server_tick: Tick::ZERO,
            outcome: crate::action::ActionOutcome::Applied { effects: vec![] },
        };
        snapshot
            .action_results
            .push(("action-1".into(), result.clone()));
        snapshot.action_results.push(("action-1".into(), result));
        assert_eq!(
            Zone::restore(&snapshot, nav(), rules()).unwrap_err().code(),
            ErrorCode::Conflict
        );
        snapshot.action_results.pop();
        snapshot.action_results[0].0 = "different".into();
        assert_eq!(
            Zone::restore(&snapshot, nav(), rules()).unwrap_err().code(),
            ErrorCode::InvalidRequest
        );

        snapshot.action_results.clear();
        snapshot.link_results = vec![("link-1".into(), vec![]); 2];
        assert_eq!(
            Zone::restore(&snapshot, nav(), rules()).unwrap_err().code(),
            ErrorCode::Conflict
        );
        snapshot.link_results = vec![("link-1".into(), vec![]); LINK_CACHE_LIMIT + 1];
        assert_eq!(
            Zone::restore(&snapshot, nav(), rules()).unwrap_err().code(),
            ErrorCode::InvalidRequest
        );
    }

    #[test]
    fn restore_validates_bounds_and_capacity() {
        let mut source = zone();
        source
            .spawn(entity(1, PersistencePolicy::PersistentEntity))
            .unwrap();
        let snapshot = source.snapshot();

        let mut outside = snapshot.clone();
        outside.entities[0].transform.position = Vec3::new(100.0, 0.0, 0.0);
        let error = Zone::restore(&outside, nav(), rules()).unwrap_err();
        assert_eq!(error.code(), ErrorCode::OutOfRange);

        let mut too_many = snapshot.clone();
        while too_many.entities.len() < 5 {
            too_many.entities.push(entity(
                10 + too_many.entities.len() as u128,
                PersistencePolicy::PersistentEntity,
            ));
        }
        let error = Zone::restore(&too_many, nav(), rules()).unwrap_err();
        assert_eq!(error.code(), ErrorCode::CapacityExceeded);
    }

    #[test]
    fn transient_entities_and_their_links_are_not_saved() {
        let mut source = zone();
        let lever = source
            .spawn(entity(1, PersistencePolicy::PersistentEntity))
            .unwrap();
        let transient = source
            .spawn(entity(2, PersistencePolicy::Transient))
            .unwrap();
        source.add_link(link(lever, transient)).unwrap();

        let snapshot = source.snapshot();
        assert_eq!(snapshot.entities.len(), 1);
        assert_eq!(snapshot.links.len(), 0);
        assert_eq!(
            Zone::restore(&snapshot, nav(), rules())
                .unwrap()
                .entity_count(),
            1
        );
    }

    #[test]
    fn transfer_receipts_survive_a_restart() {
        let mut source = zone();
        let mut giver = entity(1, PersistencePolicy::PersistentEntity);
        let mut taker = entity(2, PersistencePolicy::PersistentEntity);
        let mut giver_inventory = Inventory::with_capacity(2).unwrap();
        let taker_inventory = Inventory::with_capacity(2).unwrap();
        let coin = DefinitionId::new("item.coin").unwrap();
        giver_inventory
            .try_add(ItemStack::new(coin.clone(), 3), &*source.rules)
            .unwrap();
        giver.inventory = Some(giver_inventory);
        taker.inventory = Some(taker_inventory);
        let giver_id = source.spawn(giver).unwrap();
        let taker_id = source.spawn(taker).unwrap();

        let mut from = source
            .entity_mut(giver_id)
            .unwrap()
            .inventory
            .take()
            .unwrap();
        let mut to = source
            .entity_mut(taker_id)
            .unwrap()
            .inventory
            .take()
            .unwrap();
        let receipt = source
            .transfers
            .transfer(
                "move-1",
                &mut from,
                giver_id,
                0,
                &mut to,
                taker_id,
                1,
                &*source.rules,
            )
            .unwrap();
        source.entity_mut(giver_id).unwrap().inventory = Some(from);
        source.entity_mut(taker_id).unwrap().inventory = Some(to);

        let snapshot = source.snapshot();
        assert_eq!(snapshot.transfers.len(), 1);
        let restored = Zone::restore(&snapshot, nav(), rules()).unwrap();
        assert_eq!(restored.snapshot(), snapshot);
        assert_eq!(restored.transfers.receipt("move-1"), Some(&receipt));
        assert_eq!(restored.transfers.applied_count(), 1);

        let mut corrupt = snapshot.clone();
        corrupt.transfers.push(receipt);
        assert_eq!(
            Zone::restore(&corrupt, nav(), rules()).unwrap_err().code(),
            ErrorCode::Conflict
        );
        corrupt.transfers.pop();
        corrupt.transfers[0].quantity = 0;
        assert_eq!(
            Zone::restore(&corrupt, nav(), rules()).unwrap_err().code(),
            ErrorCode::InvalidRequest
        );
    }
}
