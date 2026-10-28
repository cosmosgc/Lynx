//! End-to-end test for the WebSocket transport.
//!
//! It exercises the same path a client uses: HTTP upgrade with a bearer
//! token, `session.hello`, ready/snapshot/token delivery, correlated protocol
//! errors, and shutdown. The authority loop is the real one.

use std::sync::Arc;
use std::time::Duration;

use engine_core::time::Tick;
use engine_net::envelope::{Envelope, MessageId, MessageKind};
use engine_net::framing::{decode_frame, encode_frame};
use futures_util::{SinkExt, StreamExt};
use lynx_world::auth::{Authenticator, digest};
use lynx_world::demo;
use lynx_world::rate_limit::{RateLimitConfig, TokenBucketConfig};
use lynx_world::service::{HELLO_KIND, Hello, Service, TOKEN_KIND};
use lynx_world::transport::{self, AppState, OriginPolicy};
use serde_json::json;
use tokio::sync::{oneshot, watch};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, header};
use tokio_tungstenite::{WebSocketStream, connect_async};
use world_gateway::{GatewayConfig, SystemResumeTokens, WorldGateway};
use world_server::{DELTA_KIND, READY_KIND, SNAPSHOT_KIND, WorldServer};

const TOKEN: &str = "websocket-integration-token";

#[tokio::test]
async fn origin_allowlist_rejects_before_websocket_upgrade() {
    let authenticator = Authenticator::new(&[digest(TOKEN)]).unwrap();
    let (events, mut receiver) = transport::event_channel();
    let state = Arc::new(
        AppState::new(events, authenticator)
            .with_origin_policy(OriginPolicy::new(vec!["https://game.example".into()]).unwrap()),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown, shutdown_rx) = oneshot::channel();
    let server = tokio::spawn(transport::serve_with_shutdown(listener, state, async {
        let _ = shutdown_rx.await;
    }));
    let request = |origin: Option<&str>| {
        let mut request = format!("ws://{address}/world")
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {TOKEN}")).unwrap(),
        );
        if let Some(origin) = origin {
            request
                .headers_mut()
                .insert(header::ORIGIN, HeaderValue::from_str(origin).unwrap());
        }
        request
    };
    let error = connect_async(request(Some("https://evil.example")))
        .await
        .unwrap_err();
    match error {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status(), 403);
            assert_eq!(
                response.body().as_deref(),
                Some(&b"{\"error\":\"origin_forbidden\"}"[..])
            );
        }
        other => panic!("unexpected error: {other}"),
    }
    assert!(receiver.try_recv().is_err());
    let (allowed, _) = connect_async(request(Some("https://game.example")))
        .await
        .unwrap();
    let (native, _) = connect_async(request(None)).await.unwrap();
    drop((allowed, native));
    let _ = shutdown.send(());
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

fn envelope(id: &str, kind: &str, body: serde_json::Value) -> Envelope {
    Envelope::new(
        MessageId::parse(id).unwrap(),
        MessageKind::parse(kind).unwrap(),
        body,
    )
}

async fn next_envelope<S>(socket: &mut WebSocketStream<S>) -> Envelope
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("the server should answer")
            .expect("the websocket should stay open")
            .expect("the frame should be valid");
        match message {
            Message::Binary(bytes) => return decode_frame(&bytes).unwrap(),
            Message::Text(text) => return Envelope::decode(text.as_str()).unwrap(),
            Message::Ping(_) | Message::Pong(_) => {}
            other => panic!("unexpected websocket message: {other:?}"),
        }
    }
}

async fn send_envelope<S>(socket: &mut WebSocketStream<S>, envelope: &Envelope)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    socket
        .send(Message::Binary(encode_frame(envelope).unwrap().into()))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_client_can_authenticate_play_and_receive_correlated_errors() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let authenticator = Authenticator::new(&[digest(TOKEN)]).unwrap();
        let grants: Vec<_> = authenticator.grants().cloned().collect();
        let zone = demo::build_zone(&grants).unwrap();
        let gateway = WorldGateway::new(
            zone,
            demo::handshake().unwrap(),
            GatewayConfig::default(),
            SystemResumeTokens::new(),
        )
        .unwrap();
        let service = Service::new(WorldServer::new(gateway), Duration::from_millis(20));

        let (events, receivers) = transport::event_channel();
        let (shutdown, shutdown_rx) = watch::channel(false);
        let state = Arc::new(AppState::new(events, authenticator).with_rate_limits(
            RateLimitConfig::default().with_session_total(TokenBucketConfig::new(2, 1).unwrap()),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        let service_task = tokio::spawn(service.run(receivers, shutdown_rx));
        let (server_shutdown, server_shutdown_rx) = oneshot::channel();
        let server_task = tokio::spawn(transport::serve_with_shutdown(
            listener,
            Arc::clone(&state),
            async move {
                let _ = server_shutdown_rx.await;
            },
        ));

        // Missing credentials never reach the WebSocket.
        let denied = connect_async(format!("ws://{address}/world").as_str()).await;
        assert!(denied.is_err(), "an unauthenticated upgrade must fail");

        let mut request = format!("ws://{address}/world")
            .as_str()
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {TOKEN}")).unwrap(),
        );
        let (mut socket, response) = connect_async(request).await.unwrap();
        assert_eq!(response.status(), 101);

        let hello = Hello {
            handshake: demo::handshake().unwrap(),
            last_ack_tick: Tick::ZERO,
        };
        let hello = envelope(
            "req_hello_0001",
            HELLO_KIND,
            serde_json::to_value(hello).unwrap(),
        );
        send_envelope(&mut socket, &hello).await;

        let ready = next_envelope(&mut socket).await;
        assert_eq!(ready.kind.as_str(), READY_KIND);
        assert_eq!(ready.body["resumed"], json!(false));
        assert!(ready.body["actor"].as_str().is_some());

        let snapshot = next_envelope(&mut socket).await;
        assert_eq!(snapshot.kind.as_str(), SNAPSHOT_KIND);
        assert!(snapshot.body["interest"]["set"].as_array().is_some());

        let token = next_envelope(&mut socket).await;
        assert_eq!(token.kind.as_str(), TOKEN_KIND);
        assert!(token.body["resume"].as_str().is_some());

        let unsupported = envelope("req_bad_0001", "bogus.kind", json!({}));
        send_envelope(&mut socket, &unsupported).await;
        let error = next_envelope(&mut socket).await;
        assert_eq!(error.kind.as_str(), "protocol.error");
        assert_eq!(error.correlation.as_ref().unwrap().as_str(), "req_bad_0001");
        assert_eq!(error.body["code"], json!("unsupported"));

        // Text and binary messages share the same post-admission budget.
        socket
            .send(Message::Text(unsupported.encode().unwrap().into()))
            .await
            .unwrap();
        assert_eq!(
            next_envelope(&mut socket).await.body["code"],
            json!("unsupported")
        );
        send_envelope(&mut socket, &unsupported).await;
        let limited = next_envelope(&mut socket).await;
        assert_eq!(limited.kind.as_str(), "protocol.error");
        assert_eq!(limited.body["code"], json!("rate_limited"));
        assert!(
            limited.body["params"]["retry_after_ms"]
                .as_str()
                .unwrap()
                .parse::<u64>()
                .unwrap()
                > 0
        );
        assert!(matches!(
            socket.next().await.unwrap().unwrap(),
            Message::Close(_)
        ));

        drop(socket);
        let _ = shutdown.send(true);
        let _ = server_shutdown.send(());
        let _ = tokio::time::timeout(Duration::from_secs(2), service_task).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;
    })
    .await
    .expect("the websocket integration test timed out");
}

#[tokio::test]
#[allow(
    clippy::items_after_statements,
    clippy::too_many_lines,
    reason = "The end-to-end scenario keeps its socket setup and assertions together."
)]
async fn movement_is_session_bound_ticked_replicated_and_cancelled_on_disconnect() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let authenticator = Authenticator::new(&[digest(TOKEN)]).unwrap();
        let grants: Vec<_> = authenticator.grants().cloned().collect();
        let gateway = WorldGateway::new(
            demo::build_zone(&grants).unwrap(),
            demo::handshake().unwrap(),
            GatewayConfig::default(),
            SystemResumeTokens::new(),
        )
        .unwrap();
        let service = Service::new(WorldServer::new(gateway), Duration::from_secs(1));
        let (events, receiver) = transport::event_channel();
        let (shutdown, shutdown_rx) = watch::channel(false);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let service_task = tokio::spawn(service.run(receiver, shutdown_rx));
        let (server_shutdown, server_shutdown_rx) = oneshot::channel();
        let server_task = tokio::spawn(transport::serve_with_shutdown(
            listener,
            Arc::new(AppState::new(events, authenticator)),
            async { let _ = server_shutdown_rx.await; },
        ));

        async fn connect(address: std::net::SocketAddr, token: &str) -> WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
            let mut request = format!("ws://{address}/world").into_client_request().unwrap();
            request.headers_mut().insert(header::AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {token}")).unwrap());
            let (mut socket, _) = connect_async(request).await.unwrap();
            send_envelope(&mut socket, &envelope("hello", HELLO_KIND, serde_json::to_value(Hello {
                handshake: demo::handshake().unwrap(), last_ack_tick: Tick::ZERO,
            }).unwrap())).await;
            socket
        }

        let mut socket = connect(address, TOKEN).await;
        let ready = next_envelope(&mut socket).await;
        assert_eq!(ready.kind.as_str(), READY_KIND);
        let actor = ready.body["actor"].as_str().unwrap().to_owned();
        let snapshot = next_envelope(&mut socket).await;
        assert_eq!(snapshot.kind.as_str(), SNAPSHOT_KIND);
        let token = next_envelope(&mut socket).await.body["resume"].as_str().unwrap().to_owned();
        let initial = snapshot.body["batch"]["updates"].as_array().unwrap().iter()
            .find(|update| update["entity"] == actor).unwrap()["payload"]["position"].clone();
        let x = initial["x"].as_f64().unwrap();
        let destination = json!({"x": x + 0.5, "y": initial["y"], "z": initial["z"]});

        // Authority, timing and actor identity are never client-supplied.
        for field in ["actor", "principal", "session", "speed", "duration"] {
            let mut body = json!({"sequence": 1, "destination": destination});
            body[field] = json!("forged");
            let id = format!("forged-{field}");
            send_envelope(&mut socket, &envelope(&id, "movement.request", body)).await;
            let error = next_envelope(&mut socket).await;
            assert_eq!(error.kind.as_str(), "protocol.error");
            assert_eq!(error.correlation.unwrap().as_str(), id);
            assert_eq!(error.body["code"], "invalid_request");
        }

        let request = envelope("move-1", "movement.request", json!({"sequence": 1, "destination": destination}));
        send_envelope(&mut socket, &request).await;
        let queued = next_envelope(&mut socket).await;
        assert_eq!(queued.kind.as_str(), "movement.queued");
        assert_eq!(queued.correlation.unwrap().as_str(), "move-1");
        assert_eq!(queued.body["sequence"], 1);
        let delta = next_envelope(&mut socket).await;
        assert_eq!(delta.kind.as_str(), DELTA_KIND);
        assert!(delta.body["tick"].as_u64().unwrap() > snapshot.body["tick"].as_u64().unwrap());
        let update = delta.body["batch"]["updates"].as_array().unwrap().iter()
            .find(|update| update["entity"] == actor).unwrap();
        assert_eq!(update["category"], "state_snapshot");
        assert_eq!(update["payload"]["position"], destination);

        send_envelope(&mut socket, &request).await;
        let stale = next_envelope(&mut socket).await;
        assert_eq!(stale.kind.as_str(), "protocol.error");
        assert_eq!(stale.body["code"], "invalid_request");

        // Queueing is not an acceptance of the endpoint: the tick must reject
        // an endpoint beyond its fixed two-metre movement budget.
        let too_far = envelope("move-far", "movement.request", json!({"sequence": 2, "destination": {"x": x + 10.0, "y": initial["y"], "z": initial["z"]}}));
        send_envelope(&mut socket, &too_far).await;
        assert_eq!(next_envelope(&mut socket).await.kind.as_str(), "movement.queued");
        tokio::time::sleep(Duration::from_millis(1200)).await;
        send_envelope(&mut socket, &too_far).await;
        let rejected = loop {
            let message = next_envelope(&mut socket).await;
            if message.kind.as_str() != DELTA_KIND {
                break message;
            }
            // Unrelated deltas may race the reply, but the rejected endpoint
            // must never appear as this actor's authoritative position.
            for update in message.body["batch"]["updates"].as_array().unwrap() {
                if update["entity"] == actor && update["payload"]["position"].is_object() {
                    assert_eq!(update["payload"]["position"], destination);
                }
            }
        };
        assert_eq!(rejected.kind.as_str(), "protocol.error");
        assert_eq!(rejected.body["code"], "invalid_request");

        // Queue a second move and detach before the next tick. Resuming after
        // that tick must still show the first authoritative position.
        let pending = envelope("move-3", "movement.request", json!({"sequence": 3, "destination": {"x": x + 1.0, "y": initial["y"], "z": initial["z"]}}));
        send_envelope(&mut socket, &pending).await;
        assert_eq!(next_envelope(&mut socket).await.kind.as_str(), "movement.queued");
        socket.close(None).await.unwrap();
        drop(socket);
        tokio::time::sleep(Duration::from_millis(1200)).await;
        let mut resumed = connect(address, &token).await;
        let ready = next_envelope(&mut resumed).await;
        assert_eq!(ready.kind.as_str(), READY_KIND);
        assert_eq!(ready.body["resumed"], true);
        let snapshot = next_envelope(&mut resumed).await;
        assert_eq!(snapshot.kind.as_str(), SNAPSHOT_KIND);
        let position = &snapshot.body["batch"]["updates"].as_array().unwrap().iter()
            .find(|update| update["entity"] == actor).unwrap()["payload"]["position"];
        assert_eq!(*position, destination);
        assert_eq!(next_envelope(&mut resumed).await.kind.as_str(), TOKEN_KIND);

        drop(resumed);
        shutdown.send(true).unwrap();
        server_shutdown.send(()).unwrap();
        service_task.await.unwrap();
        server_task.await.unwrap().unwrap();
    }).await.expect("movement websocket test timed out");
}
