use engine_net::envelope::{Envelope, MessageId, MessageKind};
use engine_net::protocol::{ErrorPayload, Feature, Handshake, negotiate};
use serde_json::json;

fn envelope(id: &str, kind: &str, body: serde_json::Value) -> Envelope {
    Envelope::new(
        MessageId::parse(id).unwrap(),
        MessageKind::parse(kind).unwrap(),
        body,
    )
}

#[test]
fn hello_and_version_error_match_wire_fixtures() {
    let mut client = Handshake::new(semver::Version::new(0, 1, 0), semver::Version::new(0, 1, 0));
    client.features.insert(Feature::Chat);
    let hello = envelope(
        "req_1",
        "session.hello",
        json!({"handshake": client, "last_ack_tick": 0}),
    );
    assert_eq!(
        hello.encode().unwrap(),
        include_str!("fixtures/hello-1.0.0.json").trim()
    );
    assert_eq!(
        Envelope::decode(include_str!("fixtures/hello-1.0.0.json")).unwrap(),
        hello
    );

    let server = client.clone();
    client.protocol = semver::Version::new(2, 0, 0);
    let error = ErrorPayload::from(negotiate(&client, &server).unwrap_err());
    let reply = hello.reply(
        MessageId::parse("res_1").unwrap(),
        MessageKind::parse("protocol.error").unwrap(),
        serde_json::to_value(error).unwrap(),
    );
    assert_eq!(
        reply.encode().unwrap(),
        include_str!("fixtures/error-1.0.0.json").trim()
    );
    assert_eq!(
        Envelope::decode(include_str!("fixtures/error-1.0.0.json")).unwrap(),
        reply
    );
}

#[test]
fn older_and_newer_minor_peers_and_feature_degradation() {
    let mut server = Handshake::new(semver::Version::new(0, 1, 0), semver::Version::new(1, 2, 0));
    server.protocol = semver::Version::new(1, 2, 0);
    server.features.insert(Feature::Chat);
    for minor in [0, 2, 4] {
        let mut client = server.clone();
        client.protocol = semver::Version::new(1, minor, 7);
        client.features.insert(Feature::Groups);
        let result = negotiate(&client, &server).unwrap();
        assert_eq!(result.protocol, server.protocol);
        assert!(result.features.contains(Feature::Chat));
        assert!(!result.features.contains(Feature::Groups));
        assert!(result.degraded);
        assert_eq!(result.warnings.len(), if minor == 2 { 1 } else { 2 });
    }
    let mut client = server.clone();
    client.content = semver::Version::new(2, 0, 0);
    let error = negotiate(&client, &server).unwrap_err();
    assert_eq!(error.details()["kind"], "content");
}
