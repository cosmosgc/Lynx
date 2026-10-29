use std::{sync::Arc, time::Duration};

use engine_core::time::Tick;
use engine_net::envelope::{Envelope, MessageId, MessageKind};
use engine_net::framing::encode_frame;
use futures_util::{SinkExt, StreamExt};
use lynx_client_net::{Client, Config, Failure, SendError, Status};
use lynx_world::{
    auth::{Authenticator, digest},
    demo,
    service::Service,
    transport::{self, AppState},
};
use tokio::sync::{oneshot, watch};
use tokio_tungstenite::tungstenite::Message;
use world_gateway::{GatewayConfig, SystemResumeTokens, WorldGateway};
use world_server::WorldServer;

const TOKEN: &str = "client-net-integration-secret";

fn config(url: String, bearer: &str) -> Config {
    Config {
        url,
        bearer: bearer.into(),
        character: None,
        handshake: demo::handshake().unwrap(),
        last_ack_tick: Tick::ZERO,
    }
}

async fn receive(client: &mut Client) -> Envelope {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(envelope) = client.try_recv() {
                return envelope;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("expected envelope")
}

async fn finished(client: &Client) -> Status {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match client.status() {
                status @ (Status::Failed(_) | Status::Closed) => return status,
                _ => tokio::time::sleep(Duration::from_millis(5)).await,
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn plaintext_policy_and_redaction() {
    for url in [
        "ws://example.com/world",
        "ws://localhost/world",
        "ws://192.0.2.1/world",
    ] {
        let settings = config(url.into(), TOKEN);
        let debug = format!("{settings:?}");
        assert!(!debug.contains(url));
        assert!(!debug.contains(TOKEN));
        let client = Client::connect(settings);
        assert_eq!(
            finished(&client).await,
            Status::Failed(Failure::InvalidConfiguration)
        );
        assert!(!format!("{client:?}").contains(TOKEN));
    }
    // A closed local port demonstrates that the URLs pass configuration validation;
    // no TLS or plaintext connection is established in this test.
    for url in [
        "ws://127.0.0.1:0/world",
        "ws://[::1]:0/world",
        "wss://127.0.0.1:0/world",
    ] {
        let client = Client::connect(config(url.into(), TOKEN));
        assert_eq!(finished(&client).await, Status::Failed(Failure::Transport));
    }
}

#[tokio::test]
#[allow(clippy::result_large_err)] // Tungstenite's callback fixes the error type.
async fn character_header_is_sent_and_redacted() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/world", listener.local_addr().unwrap());
    let server =
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let callback = |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                        response: tokio_tungstenite::tungstenite::handshake::server::Response| {
            assert_eq!(request.headers()["authorization"], format!("Bearer {TOKEN}"));
            assert_eq!(request.headers()["x-lynx-character"], "chr_private");
            Ok(response)
        };
            let mut socket = tokio_tungstenite::accept_hdr_async(stream, callback)
                .await
                .unwrap();
            assert!(socket.next().await.unwrap().is_ok());
        });
    let mut settings = config(url, TOKEN);
    settings.character = Some("chr_private".into());
    let debug = format!("{settings:?}");
    assert!(!debug.contains("chr_private"));
    assert!(!debug.contains(TOKEN));
    let client = Client::connect(settings);
    server.await.unwrap();
    assert!(matches!(
        finished(&client).await,
        Status::Closed | Status::Failed(_)
    ));
}

#[test]
fn server_url_validation_is_available_before_connecting() {
    use lynx_client_net::validate_server_url;
    for url in [
        "wss://example.com:443/world",
        "ws://127.0.0.1:8090/world",
        "ws://[::1]:8090/world",
    ] {
        assert_eq!(validate_server_url(url), Ok(()));
    }
    for url in [
        "ws://localhost/world",
        "ws://192.168.1.1/world",
        "wss://user:pass@example.com/world",
        "wss://example.com/world?token=secret",
    ] {
        assert_eq!(validate_server_url(url), Err(Failure::InvalidConfiguration));
    }
}

#[tokio::test]
async fn ready_without_snapshot_times_out() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/world", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket.next().await.unwrap().unwrap();
        let ready = Envelope::new(
            MessageId::parse("ready").unwrap(),
            MessageKind::parse("world.ready").unwrap(),
            serde_json::json!({}),
        );
        socket
            .send(Message::Binary(encode_frame(&ready).unwrap().into()))
            .await
            .unwrap();
        while socket.next().await.is_some() {}
    });
    let mut client = Client::connect(config(url, TOKEN));
    assert_eq!(receive(&mut client).await.kind.as_str(), "world.ready");
    assert_eq!(finished(&client).await, Status::Failed(Failure::Timeout));
    server.await.unwrap();
}

#[tokio::test]
async fn malformed_and_oversize_frames_disconnect_safely() {
    for message in [
        Message::Binary(vec![0, 0, 0, 5, 1].into()),
        Message::Text(
            "x".repeat(
                engine_net::framing::MAX_FRAME_BYTES + engine_net::framing::FRAME_HEADER_BYTES + 1,
            )
            .into(),
        ),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/world", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            socket.next().await.unwrap().unwrap();
            let _ = socket.send(message).await;
        });
        let client = Client::connect(config(url, TOKEN));
        assert!(matches!(
            finished(&client).await,
            Status::Failed(Failure::Protocol | Failure::Transport)
        ));
        assert!(!format!("{client:?}").contains(TOKEN));
        server.await.unwrap();
    }
}

#[tokio::test]
async fn demo_world_handshake_and_request() {
    let auth = Authenticator::new(&[digest(TOKEN)]).unwrap();
    let grants: Vec<_> = auth.grants().cloned().collect();
    let gateway = WorldGateway::new(
        demo::build_zone(&grants).unwrap(),
        demo::handshake().unwrap(),
        GatewayConfig::default(),
        SystemResumeTokens::new(),
    )
    .unwrap();
    let (events, receiver) = transport::event_channel();
    let (shutdown, shutdown_rx) = watch::channel(false);
    let service = tokio::spawn(
        Service::new(WorldServer::new(gateway), Duration::from_millis(20))
            .run(receiver, shutdown_rx),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/world", listener.local_addr().unwrap());
    let (stop, stop_rx) = oneshot::channel();
    let server = tokio::spawn(transport::serve_with_shutdown(
        listener,
        Arc::new(AppState::new(events, auth)),
        async {
            let _ = stop_rx.await;
        },
    ));

    let settings = config(url, TOKEN);
    assert!(!format!("{settings:?}").contains(TOKEN));
    let mut client = Client::connect(settings);
    assert_eq!(receive(&mut client).await.kind.as_str(), "world.ready");
    assert_eq!(receive(&mut client).await.kind.as_str(), "world.snapshot");
    let token = receive(&mut client).await;
    assert_eq!(token.kind.as_str(), "world.token");
    assert!(token.body["resume"].as_str().is_some());
    assert_eq!(client.status(), Status::Ready);
    let request = Envelope::new(
        MessageId::parse("request-test").unwrap(),
        MessageKind::parse("bogus.kind").unwrap(),
        serde_json::json!({}),
    );
    client.try_send(&request).unwrap();
    let error = receive(&mut client).await;
    assert_eq!(error.kind.as_str(), "protocol.error");
    assert_eq!(error.correlation.unwrap().as_str(), "request-test");
    client.close();
    assert_eq!(client.status(), Status::Closed);
    assert_eq!(client.try_send(&request), Err(SendError::Closed));
    let _ = shutdown.send(true);
    let _ = stop.send(());
    tokio::time::timeout(Duration::from_secs(5), service)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn invalid_url_is_redacted_and_outgoing_queue_is_bounded() {
    let mut client = Client::connect(config("ws://user:secret@localhost/world".into(), TOKEN));
    let request = Envelope::new(
        MessageId::parse("request-test").unwrap(),
        MessageKind::parse("bogus.kind").unwrap(),
        serde_json::json!({}),
    );
    for _ in 0..64 {
        client.try_send(&request).unwrap();
    }
    assert_eq!(client.try_send(&request), Err(SendError::Full));
    tokio::time::timeout(Duration::from_secs(2), async {
        while client.status() == Status::Connecting {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        client.status(),
        Status::Failed(Failure::InvalidConfiguration)
    );
    assert!(!format!("{client:?}").contains(TOKEN));
    client.close();
}

#[tokio::test]
async fn incoming_overflow_disconnects_instead_of_silently_dropping() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/world", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        assert!(socket.next().await.unwrap().is_ok());
        let message = Envelope::new(
            MessageId::parse("server-test").unwrap(),
            MessageKind::parse("world.ready").unwrap(),
            serde_json::json!({}),
        );
        let frame = encode_frame(&message).unwrap();
        for _ in 0..65 {
            socket
                .send(Message::Binary(frame.clone().into()))
                .await
                .unwrap();
        }
    });
    let client = Client::connect(config(url, TOKEN));
    tokio::time::timeout(Duration::from_secs(5), async {
        while client.status() != Status::Failed(Failure::IncomingQueueFull) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    server.await.unwrap();
}
