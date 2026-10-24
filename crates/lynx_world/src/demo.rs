//! The built-in demo zone.
//!
//! One character, one door, one barrel, and one owner-only crate are spawned
//! per configured token. Characters are spaced far enough apart that interest
//! filtering is visible with the default view radius: a player does not receive
//! another player's props, and the owner-only crate never appears in a
//! stranger's snapshot.

use std::sync::Arc;

use engine_core::error::{EngineError, ErrorCode};
use engine_core::id::{DefinitionId, IdKind, StableId};
use engine_core::inventory::UniformItemRules;
use engine_core::math::Vec3;
use engine_net::protocol::Handshake;
use engine_sim::action::Verb;
use engine_sim::entity::{Door, EntityRecord, Interactable};
use engine_sim::mind::Mind;
use engine_sim::nav::NavGrid;
use engine_sim::zone::{Zone, ZoneConfig};
use world_gateway::VISIBILITY_OWNER_ONLY_TAG;

use crate::auth::Grant;

const CHARACTER_SPACING_M: f32 = 120.0;
const COLUMNS: usize = 6;
const BASE_HEIGHT_M: u16 = 240;
const WIDTH_M: u16 = 800;
const MAX_CHARACTERS: usize = 32;

/// Build the demo zone with one character per authenticated grant.
///
/// # Errors
///
/// Returns [`ErrorCode::InvalidRequest`] when no grant is configured or a
/// definition identifier is malformed, [`ErrorCode::CapacityExceeded`] above
/// [`MAX_CHARACTERS`], and propagates zone spawn failures.
pub fn build_zone(grants: &[Grant]) -> Result<Zone, EngineError> {
    if grants.len() > MAX_CHARACTERS {
        return Err(EngineError::new(
            ErrorCode::CapacityExceeded,
            "the demo zone supports at most 32 characters",
        )
        .with_detail("configured", grants.len().to_string())
        .with_detail("max", MAX_CHARACTERS.to_string()));
    }
    let rows = grants.len().div_ceil(COLUMNS).max(1);
    let rows = u16::try_from(rows)
        .map_err(|_| EngineError::new(ErrorCode::CapacityExceeded, "too many demo rows"))?;
    let height_m = BASE_HEIGHT_M + (rows - 1) * 120;

    let config = ZoneConfig::new(
        definition("zone.demo")?,
        StableId::from_parts(IdKind::Zone, 1, 1),
        Vec3::ZERO,
        Vec3::new(f32::from(WIDTH_M), 4.0, f32::from(height_m)),
    )?;
    let nav = NavGrid::new(
        usize::from(WIDTH_M / 2),
        usize::from(height_m / 2),
        2.0,
        Vec3::ZERO,
    )
    .ok_or_else(|| {
        EngineError::new(ErrorCode::InvalidRequest, "demo navigation grid is invalid")
    })?;
    let mut zone = Zone::new(config, nav, Arc::new(UniformItemRules::new(99)))?;

    let character_definition = definition("character.player")?;
    let door_definition = definition("prop.door")?;
    let barrel_definition = definition("prop.barrel")?;
    let crate_definition = definition("prop.owner_crate")?;

    for (index, grant) in grants.iter().enumerate() {
        let column = u16::try_from(index % COLUMNS).expect("column fits in u16");
        let row = u16::try_from(index / COLUMNS).expect("row fits in u16");
        let position = Vec3::new(
            60.0 + f32::from(column) * CHARACTER_SPACING_M,
            0.0,
            60.0 + f32::from(row) * CHARACTER_SPACING_M,
        );

        let seed = grant.principal.random_bits();
        let actor = StableId::from_parts(IdKind::Entity, 1, seed);
        zone.spawn(
            EntityRecord::new(actor, character_definition.clone(), position)
                .with_health(100)
                .with_mind(Mind::player(grant.principal, "unbound")),
        )?;

        zone.spawn(
            EntityRecord::new(
                StableId::from_parts(IdKind::Entity, 1, seed ^ 0x100),
                door_definition.clone(),
                position.translated(Vec3::new(2.0, 0.0, 0.0)),
            )
            .with_interactable(Interactable::new(3.0, [Verb::Open]).without_line_of_sight())
            .with_door(Door::new()),
        )?;

        zone.spawn(
            EntityRecord::new(
                StableId::from_parts(IdKind::Entity, 1, seed ^ 0x200),
                barrel_definition.clone(),
                position.translated(Vec3::new(-2.0, 0.0, 2.0)),
            )
            .with_health(100)
            .with_interactable(
                Interactable::new(4.0, [Verb::Inspect, Verb::Attack]).without_line_of_sight(),
            ),
        )?;

        zone.spawn(
            EntityRecord::new(
                StableId::from_parts(IdKind::Entity, 1, seed ^ 0x300),
                crate_definition.clone(),
                position.translated(Vec3::new(0.0, 0.0, -2.0)),
            )
            .owned_by(grant.principal)
            .with_tag(VISIBILITY_OWNER_ONLY_TAG),
        )?;
    }

    Ok(zone)
}

/// The server handshake advertised to clients.
///
/// # Errors
///
/// Returns an error only if the crate version is not valid semver.
pub fn handshake() -> Result<Handshake, EngineError> {
    let version = env!("CARGO_PKG_VERSION");
    let version = semver::Version::parse(version).map_err(|error| {
        EngineError::new(ErrorCode::Internal, "invalid crate version")
            .with_detail("error", error.to_string())
    })?;
    Ok(Handshake::new(version.clone(), version))
}

fn definition(value: &str) -> Result<DefinitionId, EngineError> {
    DefinitionId::new(value).map_err(|error| {
        EngineError::new(
            ErrorCode::Internal,
            "invalid built-in definition identifier",
        )
        .with_detail("error", error.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{Authenticator, digest};

    fn grants(count: usize) -> Vec<Grant> {
        let hashes: Vec<[u8; 32]> = (0..count)
            .map(|index| digest(&format!("token-{index}")))
            .collect();
        Authenticator::new(&hashes)
            .unwrap()
            .grants()
            .cloned()
            .collect()
    }

    #[test]
    fn every_grant_gets_a_character_and_nearby_props() {
        let grants = grants(2);
        let zone = build_zone(&grants).unwrap();
        // Per grant: character, door, barrel, owner-only crate.
        assert_eq!(zone.entity_count(), grants.len() * 4);
        for grant in &grants {
            let seed = grant.principal.random_bits();
            let actor = StableId::from_parts(IdKind::Entity, 1, seed);
            let entity = zone.entity(actor).expect("character is spawned");
            assert_eq!(
                entity.mind.as_ref().unwrap().controller.player_character(),
                Some(grant.principal)
            );
            assert!(entity.position().x >= 0.0);
        }
    }

    #[test]
    fn characters_are_far_enough_apart_for_a_sixty_meter_view() {
        let grants = grants(2);
        let zone = build_zone(&grants).unwrap();
        let first = StableId::from_parts(IdKind::Entity, 1, grants[0].principal.random_bits());
        let second = StableId::from_parts(IdKind::Entity, 1, grants[1].principal.random_bits());
        let distance = zone
            .entity(first)
            .unwrap()
            .position()
            .distance(zone.entity(second).unwrap().position());
        assert!(distance > 60.0, "characters must not share a view");
    }

    #[test]
    fn the_handshake_is_valid_semver() {
        let handshake = handshake().unwrap();
        assert_eq!(handshake.protocol, engine_net::protocol::protocol_version());
    }
}
