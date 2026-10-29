use engine_net::envelope::{Envelope, MessageId, MessageKind};
use engine_net::protocol::{ErrorPayload, Feature, Handshake, Negotiated, negotiate};
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

#[test]
fn negotiated_reply_matches_wire_fixture() {
    let mut server = Handshake::new(semver::Version::new(0, 1, 0), semver::Version::new(1, 2, 0));
    server.features.insert(Feature::Chat);
    let mut client = server.clone();
    client.protocol = semver::Version::new(1, 1, 0);
    client.features.insert(Feature::Groups);
    let negotiated = negotiate(&client, &server).unwrap();
    let fixture = include_str!("fixtures/negotiated-1.0.0.json").trim();
    assert_eq!(serde_json::to_string(&negotiated).unwrap(), fixture);
    let parsed: Negotiated = serde_json::from_str(fixture).unwrap();
    assert_eq!(parsed, negotiated);
}

#[test]
fn compatibility_matrix_pins_warnings_and_errors() {
    let server = Handshake::new(semver::Version::new(3, 0, 0), semver::Version::new(1, 2, 3));
    for (protocol, content, expected_warnings) in [
        ((1, 0, 99), (1, 2, 99), 0),
        ((1, 1, 0), (1, 2, 3), 1),
        ((1, 0, 0), (1, 1, 0), 1),
        ((1, 5, 0), (1, 9, 0), 2),
    ] {
        let mut client = server.clone();
        client.protocol = semver::Version::new(protocol.0, protocol.1, protocol.2);
        client.content = semver::Version::new(content.0, content.1, content.2);
        let negotiated = negotiate(&client, &server).unwrap();
        assert_eq!(negotiated.warnings.len(), expected_warnings);
        assert!(!negotiated.degraded);
        assert_eq!(negotiated.content, server.content);
    }

    for (protocol_major, content_major, kind) in [
        (0, 1, "protocol"),
        (2, 1, "protocol"),
        (1, 0, "content"),
        (1, 2, "content"),
    ] {
        let mut client = server.clone();
        client.protocol.major = protocol_major;
        client.content.major = content_major;
        let error = ErrorPayload::from(negotiate(&client, &server).unwrap_err());
        assert_eq!(
            serde_json::to_value(error.code).unwrap(),
            "version_mismatch"
        );
        assert_eq!(error.params["kind"], kind);
        let (client_version, server_version) = if kind == "protocol" {
            (&client.protocol, &server.protocol)
        } else {
            (&client.content, &server.content)
        };
        assert_eq!(error.params["client"], client_version.to_string());
        assert_eq!(error.params["server"], server_version.to_string());
    }
}

#[test]
fn unknown_feature_names_fail_but_unknown_envelope_fields_are_ignored() {
    let mut client = serde_json::to_value(Handshake::new(
        semver::Version::new(0, 1, 0),
        semver::Version::new(0, 1, 0),
    ))
    .unwrap();
    client["features"] = json!(["chat", "future_feature"]);
    assert!(serde_json::from_value::<Handshake>(client).is_err());

    let text = r#"{"v":"1.0.0","id":"req_1","kind":"session.hello","future_envelope_field":true}"#;
    let decoded = Envelope::decode(text).unwrap();
    assert_eq!(decoded.body, json!({}));
    assert!(!decoded.encode().unwrap().contains("future_envelope_field"));
}
