//! Loads the canonical faction-world fixture and verifies recovery invariants.
//!
//! The fixture is durable server state, not authored content: it records two
//! factions, one inter-faction treasury transfer, and one claimed territory.

use engine_core::faction::{
    FactionRole, FactionSnapshot, FactionWorld, TreasuryEntryKind, TreasuryTransferReceipt,
};
use engine_core::{DefinitionId, ErrorCode, IdKind, StableId};

const FIXTURE: &str = include_str!("fixtures/faction_world_v1.json");

fn id(value: &str) -> DefinitionId {
    DefinitionId::new(value).unwrap()
}

fn actor(seed: u128) -> StableId {
    StableId::from_parts(IdKind::Character, 0, seed)
}

#[test]
fn fixture_deserializes_validates_and_round_trips() {
    let snapshot: FactionSnapshot =
        serde_json::from_str(FIXTURE).expect("fixture must be valid faction-world JSON");
    snapshot
        .validate()
        .expect("fixture must pass recovery validation");

    let world = FactionWorld::from_snapshot(snapshot.clone()).expect("fixture must rebuild");
    assert_eq!(world.snapshot(), snapshot);

    let encoded = serde_json::to_string_pretty(&world.snapshot()).unwrap();
    let reparsed: FactionSnapshot = serde_json::from_str(&encoded).unwrap();
    assert_eq!(reparsed, snapshot);
}

#[test]
fn fixture_records_governance_and_membership() {
    let snapshot: FactionSnapshot = serde_json::from_str(FIXTURE).unwrap();
    let world = FactionWorld::from_snapshot(snapshot).unwrap();
    let guards = id("faction.town_guard");

    let faction = world.faction(&guards).expect("town guard faction");
    assert_eq!(faction.definition.display_name, "Town Guard");
    assert_eq!(faction.membership.len(), 2);
    assert_eq!(faction.role_of(actor(1)), Some(FactionRole::Leader));
    assert_eq!(faction.role_of(actor(2)), Some(FactionRole::Officer));
    assert_eq!(world.faction_of(actor(2)), Some(&guards));
}

#[test]
fn fixture_treasury_is_conserved_and_power_is_inspectable() {
    let snapshot: FactionSnapshot = serde_json::from_str(FIXTURE).unwrap();
    let world = FactionWorld::from_snapshot(snapshot).unwrap();
    let guards = id("faction.town_guard");
    let bandits = id("faction.road_bandits");

    let guard_faction = world.faction(&guards).unwrap();
    let bandit_faction = world.faction(&bandits).unwrap();
    assert_eq!(guard_faction.treasury.balance(), 8_500);
    assert_eq!(bandit_faction.treasury.balance(), 3_000);

    let transfer = guard_faction.treasury.entry("aid-1").unwrap();
    assert_eq!(transfer.kind, TreasuryEntryKind::TransferOut);
    assert_eq!(transfer.amount, 3_000);
    assert_eq!(transfer.counterparty, Some(bandits.clone()));

    let inbound = bandit_faction.treasury.entry("aid-1").unwrap();
    assert_eq!(inbound.kind, TreasuryEntryKind::TransferIn);
    assert_eq!(inbound.counterparty, Some(guards.clone()));

    let guard_power = world.power(&guards).unwrap();
    assert_eq!(guard_power.treasury_balance, 8_500);
    assert_eq!(guard_power.power, 850);
    assert!(!guard_power.applied_cap);
    assert_eq!(world.power(&bandits).unwrap().power, 300);

    assert_eq!(world.snapshot().transfers.len(), 1);
    let receipt = TreasuryTransferReceipt {
        key: "aid-1".into(),
        source: guards,
        target: bandits,
        amount: 3_000,
        at: engine_core::time::Millis::new(4_000),
        schema_version: TreasuryTransferReceipt::SCHEMA_VERSION,
    };
    assert_eq!(world.snapshot().transfers[0], receipt);
}

#[test]
fn fixture_territory_history_matches_current_ownership() {
    let snapshot: FactionSnapshot = serde_json::from_str(FIXTURE).unwrap();
    let world = FactionWorld::from_snapshot(snapshot).unwrap();
    let guards = id("faction.town_guard");
    let town = id("zone.town_square");
    let camp = id("zone.bandit_camp");

    assert_eq!(world.territory(&town).unwrap().owner, Some(guards.clone()));
    assert_eq!(world.territory(&camp).unwrap().owner, None);
    assert_eq!(world.territory_history().len(), 1);

    let change = &world.territory_history()[0];
    assert_eq!(change.key, "claim-1");
    assert_eq!(change.zone, town);
    assert_eq!(change.to, Some(guards));
    assert_eq!(change.actor, actor(1));
    assert_eq!(change.revision, 2);
}

#[test]
fn fixture_recovery_rejects_missing_transfer_receipts() {
    let mut snapshot: FactionSnapshot = serde_json::from_str(FIXTURE).unwrap();
    snapshot.transfers.clear();
    assert_eq!(
        FactionWorld::from_snapshot(snapshot).unwrap_err().code(),
        ErrorCode::InvalidState
    );
}

#[test]
fn fixture_recovery_rejects_a_territory_without_history() {
    let mut snapshot: FactionSnapshot = serde_json::from_str(FIXTURE).unwrap();
    snapshot.territory_history.clear();
    assert_eq!(
        FactionWorld::from_snapshot(snapshot).unwrap_err().code(),
        ErrorCode::InvalidState
    );
}
