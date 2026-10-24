//! End-to-end tests for session authority, interest isolation, and reconnect.

use std::sync::Arc;

use engine_core::error::ErrorCode;
use engine_core::id::{DefinitionId, IdKind, StableId};
use engine_core::inventory::{Inventory, ItemStack, UniformItemRules};
use engine_core::math::Vec3;
use engine_core::time::{Millis, Tick};
use engine_net::envelope::{Envelope, MessageId, MessageKind};
use engine_net::protocol::{ErrorPayload, Handshake};
use engine_net::replication::{ReplicationBatch, SessionId};
use engine_sim::action::{ActionResult, Verb};
use engine_sim::entity::{Door, EntityRecord, Interactable};
use engine_sim::mind::Mind;
use engine_sim::nav::NavGrid;
use engine_sim::zone::{Zone, ZoneConfig};
use game_api::AccountId;
use serde_json::json;
use world_gateway::{
    Connection, GatewayConfig, OwnerState, SequentialResumeTokens, SessionClaims, SessionDelivery,
    WorldGateway,
};

struct Fixture {
    gateway: WorldGateway,
    alice_actor: StableId,
    alice_principal: StableId,
    bob_actor: StableId,
    bob_principal: StableId,
    alice_door: StableId,
    bob_door: StableId,
    alice_barrel: StableId,
    bob_barrel: StableId,
    hidden_crate: StableId,
    torch: DefinitionId,
}

fn id(kind: IdKind, seed: u128) -> StableId {
    StableId::from_parts(kind, 1, seed)
}

fn text(value: &str) -> DefinitionId {
    DefinitionId::new(value).unwrap()
}

fn handshake() -> Handshake {
    Handshake::new(semver::Version::new(0, 1, 0), semver::Version::new(1, 0, 0))
}

fn claims(account: &str, principal: StableId) -> SessionClaims {
    SessionClaims::new(AccountId::new(account).unwrap(), principal).unwrap()
}

fn fixture() -> Fixture {
    let config = ZoneConfig::new(
        text("zone.gateway_test"),
        id(IdKind::Zone, 1),
        Vec3::ZERO,
        Vec3::new(200.0, 4.0, 200.0),
    )
    .unwrap();
    let nav = NavGrid::new(200, 200, 1.0, Vec3::ZERO).unwrap();
    let mut zone = Zone::new(config, nav, Arc::new(UniformItemRules::new(99))).unwrap();

    let alice_actor = id(IdKind::Entity, 1);
    let alice_principal = id(IdKind::Character, 1);
    let bob_actor = id(IdKind::Entity, 2);
    let bob_principal = id(IdKind::Character, 2);

    let torch = text("item.torch");
    let mut inventory = Inventory::with_capacity(8).unwrap();
    inventory
        .try_add(ItemStack::new(torch.clone(), 1), &UniformItemRules::new(99))
        .unwrap();

    zone.spawn(
        EntityRecord::new(
            alice_actor,
            text("character.player"),
            Vec3::new(10.0, 0.0, 10.0),
        )
        .with_health(100)
        .with_mind(Mind::player(alice_principal, "unbound"))
        .with_inventory(inventory),
    )
    .unwrap();
    zone.spawn(
        EntityRecord::new(
            bob_actor,
            text("character.player"),
            Vec3::new(150.0, 0.0, 150.0),
        )
        .with_health(100)
        .with_mind(Mind::player(bob_principal, "unbound"))
        .with_inventory(Inventory::with_capacity(8).unwrap()),
    )
    .unwrap();

    let alice_door = id(IdKind::Entity, 10);
    let bob_door = id(IdKind::Entity, 11);
    for (door, position) in [
        (alice_door, Vec3::new(11.0, 0.0, 10.0)),
        (bob_door, Vec3::new(152.0, 0.0, 150.0)),
    ] {
        zone.spawn(
            EntityRecord::new(door, text("prop.door"), position)
                .with_interactable(Interactable::new(3.0, [Verb::Open]).without_line_of_sight())
                .with_door(Door::new()),
        )
        .unwrap();
    }

    let alice_barrel = id(IdKind::Entity, 20);
    let bob_barrel = id(IdKind::Entity, 21);
    for (barrel, position) in [
        (alice_barrel, Vec3::new(12.0, 0.0, 10.0)),
        (bob_barrel, Vec3::new(149.0, 0.0, 150.0)),
    ] {
        zone.spawn(
            EntityRecord::new(barrel, text("prop.barrel"), position)
                .with_health(100)
                .with_interactable(
                    Interactable::new(4.0, [Verb::Inspect, Verb::Attack]).without_line_of_sight(),
                ),
        )
        .unwrap();
    }

    let hidden_crate = id(IdKind::Entity, 30);
    zone.spawn(
        EntityRecord::new(
            hidden_crate,
            text("prop.hidden_crate"),
            Vec3::new(10.5, 0.0, 10.5),
        )
        .owned_by(bob_principal)
        .with_tag(world_gateway::VISIBILITY_OWNER_ONLY_TAG),
    )
    .unwrap();

    let gateway = WorldGateway::new(
        zone,
        handshake(),
        GatewayConfig::default(),
        SequentialResumeTokens::new(1),
    )
    .unwrap();

    Fixture {
        gateway,
        alice_actor,
        alice_principal,
        bob_actor,
        bob_principal,
        alice_door,
        bob_door,
        alice_barrel,
        bob_barrel,
        hidden_crate,
        torch,
    }
}

fn connect(gateway: &mut WorldGateway, account: &str, principal: StableId) -> Connection {
    gateway
        .connect(claims(account, principal), &handshake(), Millis::new(1_000))
        .unwrap()
}

fn envelope(id: &str, kind: &str, body: serde_json::Value) -> Envelope {
    Envelope::new(
        MessageId::parse(id).unwrap(),
        MessageKind::parse(kind).unwrap(),
        body,
    )
}

fn action(id: &str, body: serde_json::Value) -> Envelope {
    envelope(id, "action.request", body)
}

fn delivery<'a>(deliveries: &'a [SessionDelivery], session: &SessionId) -> &'a SessionDelivery {
    deliveries
        .iter()
        .find(|delivery| &delivery.session == session)
        .expect("the session should receive a delivery")
}

fn batch_has_entity(batch: &ReplicationBatch, entity: StableId) -> bool {
    batch
        .updates
        .iter()
        .any(|update| update.entity == Some(entity))
}

#[test]
fn action_actor_and_principal_come_from_the_session() {
    let mut fixture = fixture();
    let alice = connect(
        &mut fixture.gateway,
        "account-alice",
        fixture.alice_principal,
    );
    let bob = connect(&mut fixture.gateway, "account-bob", fixture.bob_principal);

    let spoofed = action(
        "req_spoof_0001",
        json!({
            "actor": fixture.bob_actor.to_string(),
            "principal": fixture.bob_principal.to_string(),
            "verb": "open",
            "target": fixture.alice_door.to_string(),
        }),
    );
    let replies = fixture
        .gateway
        .handle(&alice.session, &spoofed, Millis::new(2_000))
        .unwrap();
    let error: ErrorPayload = serde_json::from_value(replies[0].body.clone()).unwrap();
    assert_eq!(error.code, ErrorCode::InvalidRequest);
    assert!(
        !fixture
            .gateway
            .zone()
            .entity(fixture.alice_door)
            .unwrap()
            .door
            .as_ref()
            .unwrap()
            .open,
        "a rejected body must not mutate the zone"
    );

    let bob_attempt = action(
        "req_bob_0001",
        json!({ "verb": "open", "target": fixture.alice_door.to_string() }),
    );
    let replies = fixture
        .gateway
        .handle(&bob.session, &bob_attempt, Millis::new(2_001))
        .unwrap();
    let result: ActionResult = serde_json::from_value(replies[0].body.clone()).unwrap();
    assert_eq!(result.actor, fixture.bob_actor);
    assert_eq!(result.error().unwrap().code(), ErrorCode::OutOfRange);

    let alice_attempt = action(
        "req_alice_0001",
        json!({ "verb": "open", "target": fixture.alice_door.to_string() }),
    );
    let replies = fixture
        .gateway
        .handle(&alice.session, &alice_attempt, Millis::new(2_002))
        .unwrap();
    let result: ActionResult = serde_json::from_value(replies[0].body.clone()).unwrap();
    assert!(result.is_applied());
    assert_eq!(result.actor, fixture.alice_actor);
    assert!(
        fixture
            .gateway
            .zone()
            .entity(fixture.alice_door)
            .unwrap()
            .door
            .as_ref()
            .unwrap()
            .open
    );
}

#[test]
fn replication_is_isolated_by_interest_and_visibility() {
    let mut fixture = fixture();
    let alice = connect(
        &mut fixture.gateway,
        "account-alice",
        fixture.alice_principal,
    );
    let bob = connect(&mut fixture.gateway, "account-bob", fixture.bob_principal);

    assert!(alice.snapshot.interest.set.contains(&fixture.alice_actor));
    assert!(!alice.snapshot.interest.set.contains(&fixture.bob_actor));
    assert!(alice.snapshot.interest.set.contains(&fixture.alice_door));
    assert!(
        !alice.snapshot.interest.set.contains(&fixture.hidden_crate),
        "an owner-only entity must not enter a stranger's interest set"
    );
    assert!(bob.snapshot.interest.set.contains(&fixture.bob_actor));
    assert!(bob.snapshot.interest.set.contains(&fixture.bob_door));
    assert!(!bob.snapshot.interest.set.contains(&fixture.alice_actor));

    fixture
        .gateway
        .zone_mut()
        .entity_mut(fixture.bob_door)
        .unwrap()
        .door
        .as_mut()
        .unwrap()
        .open = true;

    let deliveries = fixture.gateway.tick(Millis::new(2_000)).unwrap();
    let alice_delivery = deliveries
        .iter()
        .find(|delivery| delivery.session == alice.session);
    let bob_delivery = delivery(&deliveries, &bob.session);

    assert!(
        alice_delivery.is_none_or(|delivery| !batch_has_entity(&delivery.batch, fixture.bob_door)),
        "a far door update must not reach Alice"
    );
    assert!(
        batch_has_entity(&bob_delivery.batch, fixture.bob_door),
        "the changed door should reach its nearby observer"
    );
    assert!(
        alice_delivery
            .is_none_or(|delivery| !batch_has_entity(&delivery.batch, fixture.hidden_crate))
    );
    assert!(!batch_has_entity(&bob_delivery.batch, fixture.alice_actor));
}

#[test]
fn reconnect_resumes_then_expires_and_rotates_the_token() {
    let mut fixture = fixture();
    let first = connect(
        &mut fixture.gateway,
        "account-alice",
        fixture.alice_principal,
    );
    let session = first.session.clone();

    let duplicate = fixture.gateway.connect(
        claims("account-alice", fixture.alice_principal),
        &handshake(),
        Millis::new(1_500),
    );
    assert_eq!(duplicate.unwrap_err().code(), ErrorCode::Conflict);

    fixture
        .gateway
        .disconnect(&session, Tick(0), Millis::new(2_000))
        .unwrap();
    let resumed = fixture
        .gateway
        .resume(&first.token, &handshake(), Tick(0), Millis::new(3_000))
        .unwrap();
    assert_eq!(resumed.session, session);
    assert!(resumed.resumed);
    assert_ne!(resumed.token, first.token);
    assert!(resumed.snapshot.interest.set.contains(&fixture.alice_actor));
    assert_eq!(
        fixture.gateway.session(&session).unwrap().last_ack_tick,
        Tick(0)
    );

    let replay = fixture
        .gateway
        .resume(&first.token, &handshake(), Tick(0), Millis::new(3_100))
        .unwrap_err();
    assert_eq!(replay.code(), ErrorCode::SessionExpired);

    let concurrent = fixture
        .gateway
        .resume(&resumed.token, &handshake(), Tick(0), Millis::new(3_100))
        .unwrap_err();
    assert_eq!(concurrent.code(), ErrorCode::Conflict);

    fixture
        .gateway
        .disconnect(&session, Tick(1), Millis::new(4_000))
        .unwrap();
    let expired = fixture
        .gateway
        .resume(&resumed.token, &handshake(), Tick(1), Millis::new(34_000))
        .unwrap_err();
    assert_eq!(expired.code(), ErrorCode::SessionExpired);
    assert!(fixture.gateway.session(&session).is_none());
}

#[test]
fn resume_fails_when_the_character_left_the_zone() {
    let mut fixture = fixture();
    let first = connect(
        &mut fixture.gateway,
        "account-alice",
        fixture.alice_principal,
    );
    let session = first.session.clone();
    fixture
        .gateway
        .disconnect(&session, Tick(0), Millis::new(2_000))
        .unwrap();
    fixture
        .gateway
        .zone_mut()
        .despawn(fixture.alice_actor)
        .unwrap();

    let error = fixture
        .gateway
        .resume(&first.token, &handshake(), Tick(0), Millis::new(3_000))
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::SessionExpired);
    assert!(
        fixture.gateway.session(&session).is_none(),
        "a session whose character is gone must be discarded"
    );
    assert_eq!(
        fixture
            .gateway
            .resume(&first.token, &handshake(), Tick(0), Millis::new(3_100))
            .unwrap_err()
            .code(),
        ErrorCode::SessionExpired
    );
}

#[test]
fn duplicate_action_ids_are_idempotent_and_session_scoped() {
    let mut fixture = fixture();
    let alice = connect(
        &mut fixture.gateway,
        "account-alice",
        fixture.alice_principal,
    );
    let bob = connect(&mut fixture.gateway, "account-bob", fixture.bob_principal);

    let attack = action(
        "req_attack_0001",
        json!({
            "verb": "attack",
            "target": fixture.alice_barrel.to_string(),
            "params": { "damage": "10" },
        }),
    );

    let first: ActionResult = serde_json::from_value(
        fixture
            .gateway
            .handle(&alice.session, &attack, Millis::new(2_000))
            .unwrap()[0]
            .body
            .clone(),
    )
    .unwrap();
    let second: ActionResult = serde_json::from_value(
        fixture
            .gateway
            .handle(&alice.session, &attack, Millis::new(2_001))
            .unwrap()[0]
            .body
            .clone(),
    )
    .unwrap();
    assert!(first.is_applied());
    assert_eq!(first, second, "a retry returns the original result");
    assert_eq!(first.request_id, "req_attack_0001");
    assert_eq!(
        fixture
            .gateway
            .zone()
            .entity(fixture.alice_barrel)
            .unwrap()
            .health
            .unwrap()
            .current,
        90,
        "the action must apply exactly once"
    );

    let bob_attack = action(
        "req_attack_0001",
        json!({
            "verb": "attack",
            "target": fixture.bob_barrel.to_string(),
            "params": { "damage": "10" },
        }),
    );
    let bob_result: ActionResult = serde_json::from_value(
        fixture
            .gateway
            .handle(&bob.session, &bob_attack, Millis::new(2_002))
            .unwrap()[0]
            .body
            .clone(),
    )
    .unwrap();
    assert!(bob_result.is_applied());
    assert_eq!(bob_result.actor, fixture.bob_actor);
    assert_eq!(
        fixture
            .gateway
            .zone()
            .entity(fixture.bob_barrel)
            .unwrap()
            .health
            .unwrap()
            .current,
        90,
        "another session's identical request id must not reuse the cache"
    );
}

#[test]
fn unknown_kinds_produce_correlated_errors_without_closing_the_session() {
    let mut fixture = fixture();
    let alice = connect(
        &mut fixture.gateway,
        "account-alice",
        fixture.alice_principal,
    );

    let request = envelope("req_unknown_0001", "chat.send", json!({ "text": "hello" }));
    let replies = fixture
        .gateway
        .handle(&alice.session, &request, Millis::new(2_000))
        .unwrap();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].kind.as_str(), world_gateway::ERROR_KIND);
    assert_eq!(replies[0].correlation.as_ref(), Some(&request.id));
    let error: ErrorPayload = serde_json::from_value(replies[0].body.clone()).unwrap();
    assert_eq!(error.code, ErrorCode::Unsupported);

    let follow_up = action(
        "req_followup_0001",
        json!({ "verb": "inspect", "target": fixture.alice_barrel.to_string() }),
    );
    let replies = fixture
        .gateway
        .handle(&alice.session, &follow_up, Millis::new(2_001))
        .unwrap();
    let result: ActionResult = serde_json::from_value(replies[0].body.clone()).unwrap();
    assert!(result.is_applied());
}

#[test]
fn owner_snapshots_include_only_the_owner_inventory() {
    let mut fixture = fixture();
    let alice = connect(
        &mut fixture.gateway,
        "account-alice",
        fixture.alice_principal,
    );
    let bob = connect(&mut fixture.gateway, "account-bob", fixture.bob_principal);

    let alice_owner = alice
        .snapshot
        .batch
        .updates
        .iter()
        .find(|update| {
            update.entity == Some(fixture.alice_actor)
                && update.category == engine_net::replication::ReplicationCategory::OwnerOnly
        })
        .expect("Alice receives her own inventory");
    let owner: OwnerState = serde_json::from_value(alice_owner.payload.clone()).unwrap();
    assert_eq!(owner.inventory.total_quantity(&fixture.torch), 1);

    assert!(
        !bob.snapshot.batch.updates.iter().any(|update| {
            update.entity == Some(fixture.alice_actor)
                && update.category == engine_net::replication::ReplicationCategory::OwnerOnly
        }),
        "Bob must never receive Alice's inventory"
    );

    fixture
        .gateway
        .zone_mut()
        .entity_mut(fixture.alice_actor)
        .unwrap()
        .inventory
        .as_mut()
        .unwrap()
        .try_add(
            ItemStack::new(fixture.torch.clone(), 2),
            &UniformItemRules::new(99),
        )
        .unwrap();

    let deliveries = fixture.gateway.tick(Millis::new(2_000)).unwrap();
    let alice_delivery = delivery(&deliveries, &alice.session);
    let update = alice_delivery
        .batch
        .updates
        .iter()
        .find(|update| {
            update.entity == Some(fixture.alice_actor)
                && update.category == engine_net::replication::ReplicationCategory::OwnerOnly
        })
        .expect("an inventory change is replicated to its owner");
    let owner: OwnerState = serde_json::from_value(update.payload.clone()).unwrap();
    assert_eq!(owner.inventory.total_quantity(&fixture.torch), 3);
    assert!(
        deliveries
            .iter()
            .find(|delivery| delivery.session == bob.session)
            .is_none_or(|delivery| !batch_has_entity(&delivery.batch, fixture.alice_actor)),
        "the inventory update must not reach another session"
    );
}

#[test]
fn acknowledgement_cursors_are_clamped_to_the_authoritative_tick() {
    let mut fixture = fixture();
    let alice = connect(
        &mut fixture.gateway,
        "account-alice",
        fixture.alice_principal,
    );

    let ack = envelope("req_ack_0001", "session.ack", json!({ "tick": 999 }));
    let replies = fixture
        .gateway
        .handle(&alice.session, &ack, Millis::new(2_000))
        .unwrap();
    assert_eq!(replies.len(), 0, "an acknowledgement has no reply");
    assert_eq!(
        fixture
            .gateway
            .session(&alice.session)
            .unwrap()
            .last_ack_tick,
        Tick(0)
    );

    let _ = fixture.gateway.tick(Millis::new(2_001)).unwrap();
    let _ = fixture.gateway.tick(Millis::new(2_002)).unwrap();
    let ack = envelope("req_ack_0002", "session.ack", json!({ "tick": 1 }));
    let _ = fixture
        .gateway
        .handle(&alice.session, &ack, Millis::new(2_003))
        .unwrap();
    assert_eq!(
        fixture
            .gateway
            .session(&alice.session)
            .unwrap()
            .last_ack_tick,
        Tick(1)
    );
}

#[test]
fn incompatible_handshakes_never_create_sessions() {
    let mut fixture = fixture();
    let mut client = handshake();
    client.protocol = semver::Version::new(2, 0, 0);

    let error = fixture
        .gateway
        .connect(
            claims("account-alice", fixture.alice_principal),
            &client,
            Millis::new(1_000),
        )
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::VersionMismatch);
    assert_eq!(fixture.gateway.retained_sessions(), 0);
}
