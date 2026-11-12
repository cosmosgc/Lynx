use engine_core::id::StableId;
use engine_core::math::Vec3;
use engine_core::time::Tick;
use engine_net::envelope::{Envelope, MessageId, MessageKind};
use engine_net::interest::{InterestSet, InterestUpdate};
use engine_net::protocol::{Handshake, negotiate};
use engine_net::replication::{ReplicationBatch, SessionId};
use serde::Serialize;
use world_gateway::replication::MovementReconciliation;
use world_server::{Ready, WorldUpdate};

fn envelope<T: Serialize>(id: &str, kind: &str, body: &T) -> Envelope {
    Envelope::new(
        MessageId::parse(id).unwrap(),
        MessageKind::parse(kind).unwrap(),
        serde_json::to_value(body).unwrap(),
    )
}

fn assert_fixture(envelope: &Envelope, fixture: &str) {
    let fixture = fixture.trim();
    assert_eq!(envelope.encode().unwrap(), fixture);
    assert_eq!(Envelope::decode(fixture).unwrap(), *envelope);
}

#[test]
fn ready_snapshot_and_delta_envelopes_match_versioned_fixtures() {
    let handshake = Handshake::new(semver::Version::new(0, 1, 0), semver::Version::new(1, 0, 0));
    let ready = Ready {
        session: SessionId::new("session_1").unwrap(),
        actor: StableId::parse("ent_01J8Z3K4Q7Y2N5V6B8C0D1E2F3").unwrap(),
        resumed: false,
        negotiated: negotiate(&handshake, &handshake).unwrap(),
    };
    let ready = envelope("srv_ready", "world.ready", &ready);
    assert_fixture(&ready, include_str!("fixtures/world-ready-1.1.0.json"));

    let update = WorldUpdate {
        tick: Tick::ZERO,
        interest: InterestUpdate {
            set: InterestSet::new(),
            added: Vec::new(),
            removed: Vec::new(),
        },
        batch: ReplicationBatch::new(Tick::ZERO),
        movement: Some(MovementReconciliation {
            tick: Tick::ZERO,
            position: Vec3::ZERO,
            last_consumed_sequence: None,
            applied: None,
            reset: false,
            slow_multiplier: None,
        }),
    };
    let snapshot = envelope("srv_snapshot", "world.snapshot", &update);
    assert_fixture(
        &snapshot,
        include_str!("fixtures/world-snapshot-1.1.0.json"),
    );
    let delta = envelope("srv_delta", "world.delta", &update);
    assert_fixture(&delta, include_str!("fixtures/world-delta-1.1.0.json"));
}

#[test]
fn world_token_uses_only_the_fake_credential_golden() {
    const FAKE_TOKEN: &str = "FAKE_LYNX_TEST_TOKEN_DO_NOT_AUTHENTICATE";
    let token = envelope(
        "tok_1",
        "world.token",
        &serde_json::json!({"resume": FAKE_TOKEN}),
    );
    let fixture = include_str!("fixtures/world-token-1.1.0.json");
    assert_fixture(&token, fixture);

    // Exact sentinel equality rejects replacing this fixture with a captured
    // bearer credential. The synthetic value cannot authenticate to a server.
    let decoded = Envelope::decode(fixture).unwrap();
    assert_eq!(decoded.body["resume"], FAKE_TOKEN);
    assert!(FAKE_TOKEN.starts_with("FAKE_"));
    assert!(!format!("{decoded:?}").contains(FAKE_TOKEN));
}

#[test]
fn versioned_wire_kind_catalog_is_complete_and_round_trips() {
    const KINDS: &[&str] = &[
        "action.request",
        "action.result",
        "calendar.command",
        "calendar.query",
        "calendar.result",
        "calendar.state",
        "chat.channels",
        "chat.channels.result",
        "chat.history",
        "chat.history.result",
        "chat.message",
        "chat.presence",
        "chat.presence.result",
        "chat.send",
        "chat.send.result",
        "movement.queued",
        "movement.request",
        "protocol.error",
        "session.ack",
        "session.hello",
        "ui.action",
        "ui.close",
        "ui.state",
        "world.delta",
        "world.ready",
        "world.snapshot",
        "world.token",
    ];
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/wire-kinds-1.1.0.json")).unwrap();
    let envelopes = fixture.as_array().expect("catalog is a JSON array");
    let mut actual = envelopes
        .iter()
        .map(|value| value["kind"].as_str().expect("kind is a string"))
        .collect::<Vec<_>>();
    actual.sort_unstable();
    assert_eq!(actual, KINDS, "catalog kinds must be unique and exhaustive");

    for value in envelopes {
        let encoded = Envelope::decode(&value.to_string())
            .unwrap()
            .encode()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&encoded).unwrap(),
            *value
        );
    }
    let token = envelopes
        .iter()
        .find(|value| value["kind"] == "world.token")
        .unwrap();
    assert_eq!(
        token["body"]["resume"],
        "FAKE_LYNX_TEST_TOKEN_DO_NOT_AUTHENTICATE"
    );
}
