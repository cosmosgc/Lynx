//! End-to-end tests for the transport-facing server loop.

use std::sync::Arc;

use engine_core::error::ErrorCode;
use engine_core::id::{DefinitionId, IdKind, StableId};
use engine_core::inventory::UniformItemRules;
use engine_core::math::Vec3;
use engine_core::time::{Millis, Tick};
use engine_net::envelope::{Envelope, MessageId, MessageKind};
use engine_net::protocol::Handshake;
use engine_net::replication::{ReplicationBatch, SessionId};
use engine_sim::action::{ActionResult, Verb};
use engine_sim::entity::{Door, EntityRecord, Interactable};
use engine_sim::mind::Mind;
use engine_sim::nav::NavGrid;
use engine_sim::zone::{Zone, ZoneConfig};
use game_api::AccountId;
use serde_json::json;
use world_gateway::{
    GatewayConfig, SequentialResumeTokens, SessionClaims, SessionDelivery, WorldGateway,
};
use world_server::{
    CloseReason, DELTA_KIND, Opened, Outbound, READY_KIND, Ready, SNAPSHOT_KIND, WorldServer,
    WorldUpdate,
};

struct Fixture {
    server: WorldServer,
    alice_actor: StableId,
    alice_principal: StableId,
    bob_actor: StableId,
    bob_principal: StableId,
    alice_door: StableId,
    bob_door: StableId,
    hidden_crate: StableId,
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

fn fixture_with_ttl(ttl_ms: u64) -> Fixture {
    let config = ZoneConfig::new(
        text("zone.server_test"),
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

    zone.spawn(
        EntityRecord::new(
            alice_actor,
            text("character.player"),
            Vec3::new(10.0, 0.0, 10.0),
        )
        .with_health(100)
        .with_mind(Mind::player(alice_principal, "unbound")),
    )
    .unwrap();
    zone.spawn(
        EntityRecord::new(
            bob_actor,
            text("character.player"),
            Vec3::new(150.0, 0.0, 150.0),
        )
        .with_health(100)
        .with_mind(Mind::player(bob_principal, "unbound")),
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
        GatewayConfig::new(engine_core::time::DurationMs::new(ttl_ms)),
        SequentialResumeTokens::new(1),
    )
    .unwrap();

    Fixture {
        server: WorldServer::new(gateway),
        alice_actor,
        alice_principal,
        bob_actor,
        bob_principal,
        alice_door,
        bob_door,
        hidden_crate,
    }
}

fn fixture() -> Fixture {
    fixture_with_ttl(30_000)
}

fn open(server: &mut WorldServer, account: &str, principal: StableId) -> Opened {
    server
        .open(claims(account, principal), &handshake(), Millis::new(1_000))
        .unwrap()
}

fn action(id: &str, body: serde_json::Value) -> Envelope {
    Envelope::new(
        MessageId::parse(id).unwrap(),
        MessageKind::parse("action.request").unwrap(),
        body,
    )
}

fn envelope_with_kind<'a>(opened: &'a Opened, kind: &str) -> &'a Envelope {
    opened
        .messages
        .iter()
        .find(|envelope| envelope.kind.as_str() == kind)
        .expect("the expected envelope should be present")
}

fn update(envelope: &Envelope) -> WorldUpdate {
    serde_json::from_value(envelope.body.clone()).unwrap()
}

fn message_for(outbound: &[Outbound], connection: world_server::ConnectionId) -> Option<&Envelope> {
    outbound.iter().find_map(|output| match output {
        Outbound::Message {
            connection: target,
            envelope,
        } if *target == connection => Some(envelope),
        _ => None,
    })
}

fn batch_has_entity(batch: &ReplicationBatch, entity: StableId) -> bool {
    batch
        .updates
        .iter()
        .any(|update| update.entity == Some(entity))
}

#[test]
fn opening_a_session_sends_ready_and_a_filtered_snapshot() {
    let mut fixture = fixture();
    let alice = open(
        &mut fixture.server,
        "account-alice",
        fixture.alice_principal,
    );

    assert_eq!(alice.messages.len(), 2);
    assert_eq!(alice.messages[0].kind.as_str(), READY_KIND);
    assert_eq!(alice.messages[1].kind.as_str(), SNAPSHOT_KIND);
    let ready: Ready = serde_json::from_value(alice.messages[0].body.clone()).unwrap();
    assert_eq!(ready.session, alice.session);
    assert_eq!(ready.actor, fixture.alice_actor);
    assert!(!ready.resumed);

    let snapshot = update(envelope_with_kind(&alice, SNAPSHOT_KIND));
    assert!(snapshot.interest.set.contains(&fixture.alice_actor));
    assert!(snapshot.interest.set.contains(&fixture.alice_door));
    assert!(
        !snapshot.interest.set.contains(&fixture.hidden_crate),
        "owner-only entities never enter a stranger's snapshot"
    );
    assert_eq!(fixture.server.connection_count(), 1);
    assert_eq!(
        fixture.server.connection_for(&alice.session),
        Some(alice.connection)
    );

    // Every message must survive the wire framing codec.
    for envelope in &alice.messages {
        let frame = engine_net::encode_frame(envelope).unwrap();
        let decoded = engine_net::decode_frame(&frame).unwrap();
        assert_eq!(&decoded, envelope);
    }
}

#[test]
fn actions_route_back_to_their_connection() {
    let mut fixture = fixture();
    let alice = open(
        &mut fixture.server,
        "account-alice",
        fixture.alice_principal,
    );

    let outbound = fixture
        .server
        .receive(
            alice.connection,
            &action(
                "req_open_0001",
                json!({ "verb": "open", "target": fixture.alice_door.to_string() }),
            ),
            Millis::new(2_000),
        )
        .unwrap();
    assert_eq!(outbound.len(), 1);
    let reply = message_for(&outbound, alice.connection).expect("a reply is addressed to Alice");
    assert_eq!(reply.kind.as_str(), "action.result");
    assert_eq!(
        reply.correlation.as_ref().unwrap().as_str(),
        "req_open_0001"
    );
    let result: ActionResult = serde_json::from_value(reply.body.clone()).unwrap();
    assert!(result.is_applied());
    assert_eq!(result.actor, fixture.alice_actor);
    assert!(
        fixture
            .server
            .gateway()
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
fn ticks_route_deltas_only_to_interested_connections() {
    let mut fixture = fixture();
    let alice = open(
        &mut fixture.server,
        "account-alice",
        fixture.alice_principal,
    );
    let bob = open(&mut fixture.server, "account-bob", fixture.bob_principal);
    let bob_snapshot = update(envelope_with_kind(&bob, SNAPSHOT_KIND));
    assert!(bob_snapshot.interest.set.contains(&fixture.bob_actor));
    assert!(bob_snapshot.interest.set.contains(&fixture.bob_door));
    assert!(
        !bob_snapshot.interest.set.contains(&fixture.alice_door),
        "distant entities stay out of Bob's snapshot"
    );

    fixture
        .server
        .gateway_mut()
        .zone_mut()
        .entity_mut(fixture.bob_door)
        .unwrap()
        .door
        .as_mut()
        .unwrap()
        .open = true;

    let outbound = fixture.server.tick(Millis::new(2_000)).unwrap();
    let bob_update =
        update(message_for(&outbound, bob.connection).expect("Bob's nearby door is replicated"));
    assert!(batch_has_entity(&bob_update.batch, fixture.bob_door));

    if let Some(envelope) = message_for(&outbound, alice.connection) {
        assert_eq!(envelope.kind.as_str(), DELTA_KIND);
        let alice_update = update(envelope);
        assert!(
            !batch_has_entity(&alice_update.batch, fixture.bob_door),
            "a far door must not reach Alice"
        );
    }
}

#[test]
fn disconnect_keeps_the_session_resumable() {
    let mut fixture = fixture();
    let first = open(
        &mut fixture.server,
        "account-alice",
        fixture.alice_principal,
    );
    let session = first.session.clone();

    let outbound = fixture
        .server
        .disconnect(first.connection, Tick(0), Millis::new(2_000))
        .unwrap();
    assert!(matches!(
        outbound.as_slice(),
        [Outbound::Closed {
            connection,
            reason: CloseReason::ClientDisconnected,
            ..
        }] if *connection == first.connection
    ));
    assert_eq!(fixture.server.connection_count(), 0);
    assert!(
        !fixture
            .server
            .gateway()
            .session(&session)
            .unwrap()
            .connected
    );

    let resumed = fixture
        .server
        .resume(&first.token, &handshake(), Tick(0), Millis::new(3_000))
        .unwrap();
    assert_eq!(resumed.session, session);
    assert!(resumed.resumed);
    assert_ne!(resumed.connection, first.connection);
    assert_eq!(resumed.messages[0].kind.as_str(), READY_KIND);
    assert_eq!(resumed.messages[1].kind.as_str(), SNAPSHOT_KIND);
    assert_ne!(resumed.token, first.token);
    assert_eq!(
        fixture.server.connection_for(&resumed.session),
        Some(resumed.connection)
    );

    let replay = fixture
        .server
        .resume(&first.token, &handshake(), Tick(0), Millis::new(3_100))
        .unwrap_err();
    assert_eq!(replay.code(), ErrorCode::SessionExpired);

    let old_connection = fixture.server.receive(
        first.connection,
        &action("req_old_0001", json!({ "verb": "inspect" })),
        Millis::new(3_200),
    );
    assert_eq!(old_connection.unwrap_err().code(), ErrorCode::NotFound);
}

#[test]
fn ticks_close_connections_whose_actor_disappeared() {
    let mut fixture = fixture();
    let alice = open(
        &mut fixture.server,
        "account-alice",
        fixture.alice_principal,
    );
    fixture
        .server
        .gateway_mut()
        .zone_mut()
        .despawn(fixture.alice_actor)
        .unwrap();

    let outbound = fixture.server.tick(Millis::new(2_000)).unwrap();
    assert!(outbound.iter().any(|output| matches!(
        output,
        Outbound::Closed {
            connection,
            session,
            reason: CloseReason::SessionExpired,
        } if *connection == alice.connection && *session == alice.session
    )));
    assert_eq!(fixture.server.connection_count(), 0);
    assert!(fixture.server.gateway().session(&alice.session).is_none());

    let error = fixture
        .server
        .receive(
            alice.connection,
            &action("req_gone_0001", json!({ "verb": "inspect" })),
            Millis::new(2_100),
        )
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::NotFound);
}

#[test]
fn unknown_connections_are_rejected_and_disconnect_is_idempotent() {
    let mut fixture = fixture();
    let alice = open(
        &mut fixture.server,
        "account-alice",
        fixture.alice_principal,
    );
    let connection = alice.connection;
    let _ = fixture
        .server
        .disconnect(connection, Tick(0), Millis::new(2_000))
        .unwrap();

    let unknown = fixture
        .server
        .receive(
            connection,
            &action("req_stray_0001", json!({ "verb": "inspect" })),
            Millis::new(2_100),
        )
        .unwrap_err();
    assert_eq!(unknown.code(), ErrorCode::NotFound);
    assert!(
        fixture
            .server
            .disconnect(connection, Tick(0), Millis::new(2_200))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn incompatible_handshakes_create_no_connection() {
    let mut fixture = fixture();
    let mut client = handshake();
    client.protocol = semver::Version::new(2, 0, 0);

    let error = fixture
        .server
        .open(
            claims("account-alice", fixture.alice_principal),
            &client,
            Millis::new(1_000),
        )
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::VersionMismatch);
    assert_eq!(fixture.server.connection_count(), 0);
    assert_eq!(fixture.server.gateway().retained_sessions(), 0);
}

#[test]
fn world_updates_round_trip_through_the_wire_body() {
    let mut fixture = fixture();
    let alice = open(
        &mut fixture.server,
        "account-alice",
        fixture.alice_principal,
    );
    let snapshot = update(envelope_with_kind(&alice, SNAPSHOT_KIND));
    let update_json = serde_json::to_value(&snapshot).unwrap();
    let parsed: WorldUpdate = serde_json::from_value(update_json).unwrap();
    assert_eq!(parsed, snapshot);
    let delivery = SessionDelivery {
        session: SessionId::new("sess-test").unwrap(),
        tick: snapshot.tick,
        interest: snapshot.interest.clone(),
        batch: snapshot.batch.clone(),
    };
    assert_eq!(WorldUpdate::from_delivery(&delivery), snapshot);
}
