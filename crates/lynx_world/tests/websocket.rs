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
use lynx_world::service::{HELLO_KIND, Hello, Service, TOKEN_KIND};
use lynx_world::transport::{self, AppState, MessageBudgetConfig, OriginPolicy};
use serde_json::json;
use tokio::sync::{oneshot, watch};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, header};
use tokio_tungstenite::{WebSocketStream, connect_async};
use world_gateway::{GatewayConfig, SystemResumeTokens, WorldGateway};
use world_server::{READY_KIND, SNAPSHOT_KIND, WorldServer};

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
        let state = Arc::new(
            AppState::new(events, authenticator)
                .with_message_budget(MessageBudgetConfig::new(2, Duration::from_secs(60)).unwrap()),
        );
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
        assert_eq!(limited.body["code"], json!("budget_exhausted"));
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
