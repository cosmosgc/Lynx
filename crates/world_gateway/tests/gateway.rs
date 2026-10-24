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
use engine_sim::action::{ActionEffect, ActionResult, Verb};
use engine_sim::entity::{Door, EntityRecord, Interactable, InteractionPermission, UiProvider};
use engine_sim::mind::Mind;
use engine_sim::minigame::Minigame;
use engine_sim::nav::NavGrid;
use engine_sim::zone::{Zone, ZoneConfig};
use game_api::AccountId;
use serde_json::json;
use world_gateway::{
    Connection, GatewayConfig, OpenPanel, OwnerState, SequentialResumeTokens, SessionClaims,
    SessionDelivery, UI_ACTION_KIND, UI_CLOSE_KIND, UI_STATE_KIND, UiState, WorldGateway,
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
    alice_terminal: StableId,
    bob_owned_terminal: StableId,
    alice_display: StableId,
    alice_vault: StableId,
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

fn spawn_panels(
    zone: &mut Zone,
    bob_principal: StableId,
) -> (StableId, StableId, StableId, StableId) {
    let alice_terminal = id(IdKind::Entity, 40);
    zone.spawn(
        EntityRecord::new(
            alice_terminal,
            text("prop.terminal"),
            Vec3::new(10.5, 0.0, 10.0),
        )
        .with_interactable(
            Interactable::new(3.0, [Verb::Use, Verb::Inspect]).without_line_of_sight(),
        )
        .with_ui(UiProvider::new(text("ui.terminal"), "Terminal")),
    )
    .unwrap();

    let bob_owned_terminal = id(IdKind::Entity, 41);
    zone.spawn(
        EntityRecord::new(
            bob_owned_terminal,
            text("prop.private_terminal"),
            Vec3::new(11.5, 0.0, 10.0),
        )
        .owned_by(bob_principal)
        .with_interactable(
            Interactable::new(3.0, [Verb::Use])
                .without_line_of_sight()
                .with_permission(InteractionPermission::Owner),
        )
        .with_ui(UiProvider::new(
            text("ui.private_terminal"),
            "Private Terminal",
        )),
    )
    .unwrap();

    let alice_display = id(IdKind::Entity, 42);
    zone.spawn(
        EntityRecord::new(
            alice_display,
            text("prop.display"),
            Vec3::new(9.5, 0.0, 10.0),
        )
        .with_interactable(Interactable::new(3.0, [Verb::Use]).without_line_of_sight())
        .with_ui(UiProvider::new(text("ui.display"), "Display")),
    )
    .unwrap();

    let alice_vault = id(IdKind::Entity, 43);
    zone.spawn(
        EntityRecord::new(alice_vault, text("prop.vault"), Vec3::new(8.5, 0.0, 10.0))
            .with_interactable(
                Interactable::new(3.0, [Verb::Use, Verb::Play, Verb::Open]).without_line_of_sight(),
            )
            .with_door(Door {
                open: false,
                locked: true,
                key: None,
            })
            .with_ui(UiProvider::new(text("ui.lockpick"), "Vault Lock"))
            .with_minigame(Minigame::new(text("minigame.lockpick"), 3, 4, 10, 7).unwrap()),
    )
    .unwrap();

    (
        alice_terminal,
        bob_owned_terminal,
        alice_display,
        alice_vault,
    )
}

fn spawn_props(zone: &mut Zone) -> (StableId, StableId, StableId, StableId) {
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

    (alice_door, bob_door, alice_barrel, bob_barrel)
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

    let (alice_door, bob_door, alice_barrel, bob_barrel) = spawn_props(&mut zone);

    let (alice_terminal, bob_owned_terminal, alice_display, alice_vault) =
        spawn_panels(&mut zone, bob_principal);

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
        alice_terminal,
        bob_owned_terminal,
        alice_display,
        alice_vault,
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

fn ui_state(envelope: &Envelope) -> UiState {
    assert_eq!(envelope.kind.as_str(), UI_STATE_KIND);
    serde_json::from_value(envelope.body.clone()).unwrap()
}

fn reply_of_kind<'a>(replies: &'a [Envelope], kind: &str) -> &'a Envelope {
    replies
        .iter()
        .find(|envelope| envelope.kind.as_str() == kind)
        .unwrap_or_else(|| panic!("expected a {kind} reply"))
}

fn open_provider(
    gateway: &mut WorldGateway,
    session: &SessionId,
    provider: StableId,
    request_id: &str,
) -> Vec<Envelope> {
    gateway
        .handle(
            session,
            &action(
                request_id,
                json!({ "verb": "use", "target": provider.to_string() }),
            ),
            Millis::new(2_000),
        )
        .unwrap()
}

fn secret_for(fixture: &Fixture, vault: StableId, principal: StableId) -> String {
    fixture
        .gateway
        .zone()
        .entity(vault)
        .unwrap()
        .minigame
        .as_ref()
        .unwrap()
        .secret_for(principal)
        .iter()
        .map(|digit| char::from(b'0' + digit))
        .collect()
}

fn play_guess(
    fixture: &mut Fixture,
    session: &SessionId,
    request_id: &str,
    guess: &str,
    now_ms: i64,
) -> Vec<Envelope> {
    fixture
        .gateway
        .handle(
            session,
            &envelope(
                request_id,
                UI_ACTION_KIND,
                json!({ "schema": "ui.lockpick", "action": "play", "params": { "guess": guess } }),
            ),
            Millis::new(now_ms),
        )
        .unwrap()
}

fn result_of(replies: &[Envelope]) -> ActionResult {
    serde_json::from_value(reply_of_kind(replies, "action.result").body.clone()).unwrap()
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

#[test]
fn opening_a_panel_emits_server_resolved_ui_state() {
    let mut fixture = fixture();
    let alice = connect(
        &mut fixture.gateway,
        "account-alice",
        fixture.alice_principal,
    );

    let replies = open_provider(
        &mut fixture.gateway,
        &alice.session,
        fixture.alice_terminal,
        "req_ui_open_0001",
    );
    assert_eq!(
        replies.len(),
        2,
        "an opened panel produces a result and state"
    );

    let result: ActionResult =
        serde_json::from_value(reply_of_kind(&replies, "action.result").body.clone()).unwrap();
    assert!(result.is_applied());
    assert!(matches!(
        result.effects(),
        [ActionEffect::UiOpened { target, .. }] if *target == fixture.alice_terminal
    ));

    let state = ui_state(reply_of_kind(&replies, UI_STATE_KIND));
    let expected = OpenPanel {
        target: fixture.alice_terminal,
        schema: text("ui.terminal"),
        title: "Terminal".to_string(),
    };
    assert_eq!(state.tick, fixture.gateway.zone().tick());
    assert_eq!(state.panel, Some(expected.clone()));
    assert_eq!(fixture.gateway.panel(&alice.session), Some(expected));
}

#[test]
fn panel_actions_use_the_open_target_and_revalidate_range() {
    let mut fixture = fixture();
    let alice = connect(
        &mut fixture.gateway,
        "account-alice",
        fixture.alice_principal,
    );
    let _ = open_provider(
        &mut fixture.gateway,
        &alice.session,
        fixture.alice_terminal,
        "req_ui_open_0001",
    );

    fixture
        .gateway
        .zone_mut()
        .entity_mut(fixture.alice_actor)
        .unwrap()
        .transform
        .position = Vec3::new(80.0, 0.0, 80.0);
    let out_of_range = fixture
        .gateway
        .handle(
            &alice.session,
            &envelope(
                "req_ui_action_0001",
                UI_ACTION_KIND,
                json!({ "schema": "ui.terminal", "action": "inspect" }),
            ),
            Millis::new(2_001),
        )
        .unwrap();
    let result: ActionResult =
        serde_json::from_value(reply_of_kind(&out_of_range, "action.result").body.clone()).unwrap();
    assert_eq!(result.actor, fixture.alice_actor);
    assert_eq!(result.error().unwrap().code(), ErrorCode::OutOfRange);
    let state = ui_state(reply_of_kind(&out_of_range, UI_STATE_KIND));
    assert_eq!(
        state.panel.as_ref().map(|panel| panel.target),
        Some(fixture.alice_terminal),
        "a rejected action keeps the panel open"
    );

    fixture
        .gateway
        .zone_mut()
        .entity_mut(fixture.alice_actor)
        .unwrap()
        .transform
        .position = Vec3::new(10.0, 0.0, 10.0);
    let applied = fixture
        .gateway
        .handle(
            &alice.session,
            &envelope(
                "req_ui_action_0002",
                UI_ACTION_KIND,
                json!({ "schema": "ui.terminal", "action": "inspect" }),
            ),
            Millis::new(2_002),
        )
        .unwrap();
    let result: ActionResult =
        serde_json::from_value(reply_of_kind(&applied, "action.result").body.clone()).unwrap();
    assert!(result.is_applied());
    assert!(matches!(
        result.effects(),
        [ActionEffect::Inspected { target, .. }] if *target == fixture.alice_terminal
    ));
    assert_eq!(result.request_id, "req_ui_action_0002");

    let unsupported = fixture
        .gateway
        .handle(
            &alice.session,
            &envelope(
                "req_ui_action_0003",
                UI_ACTION_KIND,
                json!({ "schema": "ui.terminal", "action": "attack" }),
            ),
            Millis::new(2_003),
        )
        .unwrap();
    let result: ActionResult =
        serde_json::from_value(reply_of_kind(&unsupported, "action.result").body.clone()).unwrap();
    assert_eq!(result.error().unwrap().code(), ErrorCode::Unsupported);
    assert!(fixture.gateway.panel(&alice.session).is_some());
}

#[test]
fn a_panel_whose_target_disappeared_is_cleared() {
    let mut fixture = fixture();
    let alice = connect(
        &mut fixture.gateway,
        "account-alice",
        fixture.alice_principal,
    );
    let _ = open_provider(
        &mut fixture.gateway,
        &alice.session,
        fixture.alice_terminal,
        "req_ui_open_0001",
    );

    fixture
        .gateway
        .zone_mut()
        .despawn(fixture.alice_terminal)
        .unwrap();
    let missing = fixture
        .gateway
        .handle(
            &alice.session,
            &envelope(
                "req_ui_action_0001",
                UI_ACTION_KIND,
                json!({ "schema": "ui.terminal", "action": "inspect" }),
            ),
            Millis::new(2_001),
        )
        .unwrap();
    let result: ActionResult =
        serde_json::from_value(reply_of_kind(&missing, "action.result").body.clone()).unwrap();
    assert_eq!(result.error().unwrap().code(), ErrorCode::NotFound);
    let state = ui_state(reply_of_kind(&missing, UI_STATE_KIND));
    assert!(
        state.panel.is_none(),
        "a panel whose target disappeared is cleared"
    );
    assert!(fixture.gateway.panel(&alice.session).is_none());
}

#[test]
fn panel_messages_reject_client_targets_and_schema_mismatches() {
    let mut fixture = fixture();
    let alice = connect(
        &mut fixture.gateway,
        "account-alice",
        fixture.alice_principal,
    );
    let _ = open_provider(
        &mut fixture.gateway,
        &alice.session,
        fixture.alice_terminal,
        "req_ui_open_0001",
    );

    let spoof = fixture
        .gateway
        .handle(
            &alice.session,
            &envelope(
                "req_ui_spoof_0001",
                UI_ACTION_KIND,
                json!({
                    "schema": "ui.terminal",
                    "action": "inspect",
                    "target": fixture.alice_display.to_string(),
                }),
            ),
            Millis::new(2_001),
        )
        .unwrap();
    assert_eq!(spoof.len(), 1);
    let error: ErrorPayload = serde_json::from_value(spoof[0].body.clone()).unwrap();
    assert_eq!(error.code, ErrorCode::InvalidRequest);
    assert!(fixture.gateway.panel(&alice.session).is_some());

    let wrong_schema = fixture
        .gateway
        .handle(
            &alice.session,
            &envelope(
                "req_ui_stale_0001",
                UI_ACTION_KIND,
                json!({ "schema": "ui.other", "action": "inspect" }),
            ),
            Millis::new(2_002),
        )
        .unwrap();
    let error: ErrorPayload = serde_json::from_value(wrong_schema[0].body.clone()).unwrap();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(
        fixture
            .gateway
            .panel(&alice.session)
            .unwrap()
            .schema
            .as_str(),
        "ui.terminal"
    );

    let bad_close = fixture
        .gateway
        .handle(
            &alice.session,
            &envelope(
                "req_ui_close_0001",
                UI_CLOSE_KIND,
                json!({ "schema": "ui.terminal" }),
            ),
            Millis::new(2_003),
        )
        .unwrap();
    let error: ErrorPayload = serde_json::from_value(bad_close[0].body.clone()).unwrap();
    assert_eq!(error.code, ErrorCode::InvalidRequest);
    assert!(fixture.gateway.panel(&alice.session).is_some());

    let closed = fixture
        .gateway
        .handle(
            &alice.session,
            &envelope("req_ui_close_0002", UI_CLOSE_KIND, json!({})),
            Millis::new(2_004),
        )
        .unwrap();
    let state = ui_state(reply_of_kind(&closed, UI_STATE_KIND));
    assert!(state.panel.is_none());
    assert!(fixture.gateway.panel(&alice.session).is_none());

    let no_panel = fixture
        .gateway
        .handle(
            &alice.session,
            &envelope(
                "req_ui_action_0001",
                UI_ACTION_KIND,
                json!({ "schema": "ui.terminal", "action": "inspect" }),
            ),
            Millis::new(2_005),
        )
        .unwrap();
    let error: ErrorPayload = serde_json::from_value(no_panel[0].body.clone()).unwrap();
    assert_eq!(error.code, ErrorCode::InvalidState);
}

#[test]
fn panel_state_is_cleared_on_disconnect_and_resume() {
    let mut fixture = fixture();
    let alice = connect(
        &mut fixture.gateway,
        "account-alice",
        fixture.alice_principal,
    );
    let _ = open_provider(
        &mut fixture.gateway,
        &alice.session,
        fixture.alice_terminal,
        "req_ui_open_0001",
    );
    assert!(fixture.gateway.panel(&alice.session).is_some());

    fixture
        .gateway
        .disconnect(&alice.session, Tick(0), Millis::new(3_000))
        .unwrap();
    assert!(
        fixture.gateway.panel(&alice.session).is_none(),
        "a detached transport must not leave a panel open"
    );

    let resumed = fixture
        .gateway
        .resume(&alice.token, &handshake(), Tick(0), Millis::new(4_000))
        .unwrap();
    assert!(fixture.gateway.panel(&resumed.session).is_none());

    let replies = fixture
        .gateway
        .handle(
            &resumed.session,
            &envelope(
                "req_ui_action_0001",
                UI_ACTION_KIND,
                json!({ "schema": "ui.terminal", "action": "inspect" }),
            ),
            Millis::new(4_001),
        )
        .unwrap();
    let error: ErrorPayload = serde_json::from_value(replies[0].body.clone()).unwrap();
    assert_eq!(error.code, ErrorCode::InvalidState);
}

#[test]
fn panel_open_requires_permission_and_range() {
    let mut fixture = fixture();
    let alice = connect(
        &mut fixture.gateway,
        "account-alice",
        fixture.alice_principal,
    );
    let bob = connect(&mut fixture.gateway, "account-bob", fixture.bob_principal);

    let denied = fixture
        .gateway
        .handle(
            &alice.session,
            &action(
                "req_ui_denied_0001",
                json!({ "verb": "use", "target": fixture.bob_owned_terminal.to_string() }),
            ),
            Millis::new(2_000),
        )
        .unwrap();
    let result: ActionResult =
        serde_json::from_value(reply_of_kind(&denied, "action.result").body.clone()).unwrap();
    assert_eq!(result.error().unwrap().code(), ErrorCode::PermissionDenied);
    assert!(fixture.gateway.panel(&alice.session).is_none());

    let far = fixture
        .gateway
        .handle(
            &bob.session,
            &action(
                "req_ui_far_0001",
                json!({ "verb": "use", "target": fixture.alice_terminal.to_string() }),
            ),
            Millis::new(2_001),
        )
        .unwrap();
    let result: ActionResult =
        serde_json::from_value(reply_of_kind(&far, "action.result").body.clone()).unwrap();
    assert_eq!(result.error().unwrap().code(), ErrorCode::OutOfRange);
    assert!(fixture.gateway.panel(&bob.session).is_none());
}

#[test]
fn opening_another_panel_replaces_the_previous_state() {
    let mut fixture = fixture();
    let alice = connect(
        &mut fixture.gateway,
        "account-alice",
        fixture.alice_principal,
    );
    let _ = open_provider(
        &mut fixture.gateway,
        &alice.session,
        fixture.alice_terminal,
        "req_ui_open_0001",
    );
    let _ = open_provider(
        &mut fixture.gateway,
        &alice.session,
        fixture.alice_display,
        "req_ui_open_0002",
    );
    assert_eq!(
        fixture.gateway.panel(&alice.session).unwrap().schema,
        text("ui.display")
    );

    let stale = fixture
        .gateway
        .handle(
            &alice.session,
            &envelope(
                "req_ui_action_0001",
                UI_ACTION_KIND,
                json!({ "schema": "ui.terminal", "action": "inspect" }),
            ),
            Millis::new(2_001),
        )
        .unwrap();
    let error: ErrorPayload = serde_json::from_value(stale[0].body.clone()).unwrap();
    assert_eq!(error.code, ErrorCode::Conflict);

    let refreshed = fixture
        .gateway
        .handle(
            &alice.session,
            &envelope(
                "req_ui_action_0002",
                UI_ACTION_KIND,
                json!({ "schema": "ui.display", "action": "use" }),
            ),
            Millis::new(2_002),
        )
        .unwrap();
    let result: ActionResult =
        serde_json::from_value(reply_of_kind(&refreshed, "action.result").body.clone()).unwrap();
    assert!(result.is_applied());
    assert_eq!(
        fixture.gateway.panel(&alice.session).unwrap().target,
        fixture.alice_display
    );
}

#[test]
fn minigame_panel_runs_turns_and_closes_when_solved() {
    let mut fixture = fixture();
    let alice = connect(
        &mut fixture.gateway,
        "account-alice",
        fixture.alice_principal,
    );

    let opened = open_provider(
        &mut fixture.gateway,
        &alice.session,
        fixture.alice_vault,
        "req_vault_open_0001",
    );
    let result = result_of(&opened);
    assert!(result.is_applied());
    assert!(matches!(
        result.effects(),
        [ActionEffect::UiOpened { target, schema, .. }, ActionEffect::MinigameStarted {
            target: started,
            length: 3,
            attempts_left: 4,
            solved: false,
            ..
        }] if *target == fixture.alice_vault
            && *started == fixture.alice_vault
            && schema.as_str() == "ui.lockpick"
    ));
    assert_eq!(
        ui_state(reply_of_kind(&opened, UI_STATE_KIND))
            .panel
            .unwrap()
            .schema,
        text("ui.lockpick")
    );

    let secret = secret_for(&fixture, fixture.alice_vault, fixture.alice_principal);
    let wrong = if secret == "000" { "111" } else { "000" };
    let turn = play_guess(
        &mut fixture,
        &alice.session,
        "req_vault_play_0001",
        wrong,
        2_001,
    );
    let result = result_of(&turn);
    assert!(result.is_applied());
    let ActionEffect::MinigameAttempt {
        attempts_left,
        solved,
        ..
    } = &result.effects()[0]
    else {
        panic!("expected a minigame attempt effect");
    };
    assert!(!solved);
    assert_eq!(*attempts_left, 3);
    assert!(
        ui_state(reply_of_kind(&turn, UI_STATE_KIND))
            .panel
            .is_some(),
        "a failed guess keeps the panel open"
    );

    let solved = play_guess(
        &mut fixture,
        &alice.session,
        "req_vault_play_0002",
        &secret,
        2_002,
    );
    let result = result_of(&solved);
    assert!(result.is_applied());
    assert!(result.effects().iter().any(
        |effect| matches!(effect, ActionEffect::MinigameSolved { target } if *target == fixture.alice_vault)
    ));
    assert!(result.effects().iter().any(
        |effect| matches!(effect, ActionEffect::DoorUnlocked { target } if *target == fixture.alice_vault)
    ));
    assert!(
        ui_state(reply_of_kind(&solved, UI_STATE_KIND))
            .panel
            .is_none(),
        "solving the minigame closes the panel"
    );
    let vault = fixture.gateway.zone().entity(fixture.alice_vault).unwrap();
    assert!(!vault.door.as_ref().unwrap().locked);

    let replay = play_guess(
        &mut fixture,
        &alice.session,
        "req_vault_play_0003",
        &secret,
        2_003,
    );
    let error: ErrorPayload = serde_json::from_value(replay[0].body.clone()).unwrap();
    assert_eq!(error.code, ErrorCode::InvalidState);
}

#[test]
fn minigame_actions_require_the_panel_channel_and_an_open_panel() {
    let mut fixture = fixture();
    let alice = connect(
        &mut fixture.gateway,
        "account-alice",
        fixture.alice_principal,
    );
    let secret = secret_for(&fixture, fixture.alice_vault, fixture.alice_principal);

    let no_panel = fixture
        .gateway
        .handle(
            &alice.session,
            &envelope(
                "req_vault_play_0001",
                UI_ACTION_KIND,
                json!({ "schema": "ui.lockpick", "action": "play", "params": { "guess": secret } }),
            ),
            Millis::new(2_000),
        )
        .unwrap();
    let error: ErrorPayload = serde_json::from_value(no_panel[0].body.clone()).unwrap();
    assert_eq!(error.code, ErrorCode::InvalidState);

    let direct = fixture
        .gateway
        .handle(
            &alice.session,
            &action(
                "req_vault_direct_0001",
                json!({ "verb": "play", "target": fixture.alice_vault.to_string(), "params": { "guess": secret } }),
            ),
            Millis::new(2_001),
        )
        .unwrap();
    let error: ErrorPayload = serde_json::from_value(direct[0].body.clone()).unwrap();
    assert_eq!(error.code, ErrorCode::Unsupported);
    assert!(fixture.gateway.panel(&alice.session).is_none());
    assert!(
        fixture
            .gateway
            .zone()
            .entity(fixture.alice_vault)
            .unwrap()
            .door
            .as_ref()
            .unwrap()
            .locked
    );
}

#[test]
fn reopening_a_minigame_panel_preserves_the_attempt_budget() {
    let mut fixture = fixture();
    let alice = connect(
        &mut fixture.gateway,
        "account-alice",
        fixture.alice_principal,
    );
    let _ = open_provider(
        &mut fixture.gateway,
        &alice.session,
        fixture.alice_vault,
        "req_vault_open_0001",
    );
    let secret = secret_for(&fixture, fixture.alice_vault, fixture.alice_principal);
    let wrong = if secret == "000" { "111" } else { "000" };
    let _ = fixture
        .gateway
        .handle(
            &alice.session,
            &envelope(
                "req_vault_play_0001",
                UI_ACTION_KIND,
                json!({ "schema": "ui.lockpick", "action": "play", "params": { "guess": wrong } }),
            ),
            Millis::new(2_001),
        )
        .unwrap();
    let _ = fixture
        .gateway
        .handle(
            &alice.session,
            &envelope("req_vault_close_0001", UI_CLOSE_KIND, json!({})),
            Millis::new(2_002),
        )
        .unwrap();

    let reopened = open_provider(
        &mut fixture.gateway,
        &alice.session,
        fixture.alice_vault,
        "req_vault_open_0002",
    );
    let result: ActionResult =
        serde_json::from_value(reply_of_kind(&reopened, "action.result").body.clone()).unwrap();
    assert!(matches!(
        result.effects().get(1),
        Some(ActionEffect::MinigameStarted {
            attempts_left: 3,
            solved: false,
            ..
        })
    ));
}
