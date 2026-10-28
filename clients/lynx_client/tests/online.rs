use std::{sync::Arc, time::Duration};

use engine_core::id::{IdKind, StableId};
use engine_core::time::Tick;
use engine_net::{
    envelope::{Envelope, MessageId, MessageKind},
    framing::{decode_frame, encode_frame},
};
use futures_util::{SinkExt, StreamExt};
use lynx_client::{
    headless_app,
    online::{OnlineClient, OnlineStatus},
    reconciliation::Position,
};
use lynx_client_net::Config;
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

fn message(kind: &str, body: serde_json::Value) -> Envelope {
    Envelope::new(
        MessageId::parse("server-test").unwrap(),
        MessageKind::parse(kind).unwrap(),
        body,
    )
}

#[tokio::test]
async fn acknowledges_applied_snapshot_and_fails_on_stale_delta() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/world", listener.local_addr().unwrap());
    let (ack_tx, ack_rx) = oneshot::channel();
    let (send_stale, wait_stale) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket.next().await.unwrap().unwrap();
        let actor = StableId::from_parts(IdKind::Entity, 1, 1);
        let ready = message(
            "world.ready",
            serde_json::json!({"session":"session-1", "actor": actor, "resumed":false}),
        );
        let snapshot = message(
            "world.snapshot",
            serde_json::json!({"tick":10, "interest":{"set":[],"added":[],"removed":[]}, "batch":{"tick":10,"updates":[]}, "movement":{"tick":10,"position":{"x":0,"y":0,"z":0},"last_consumed_sequence":null,"applied":null}}),
        );
        for envelope in [&ready, &snapshot] {
            socket
                .send(Message::Binary(encode_frame(envelope).unwrap().into()))
                .await
                .unwrap();
        }
        let Message::Binary(bytes) = socket.next().await.unwrap().unwrap() else {
            panic!("expected ack");
        };
        let ack = decode_frame(&bytes).unwrap();
        ack_tx
            .send((ack.kind.as_str().to_owned(), ack.body))
            .unwrap();
        wait_stale.await.unwrap();
        let stale = message(
            "world.delta",
            serde_json::json!({"tick":10,"interest":{"set":[],"added":[],"removed":[]},"batch":{"tick":10,"updates":[]},"movement":null}),
        );
        socket
            .send(Message::Binary(encode_frame(&stale).unwrap().into()))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    });
    let mut client = OnlineClient::new(1.0).unwrap();
    client
        .connect(Config {
            url,
            bearer: "test-secret".into(),
            handshake: demo::handshake().unwrap(),
            last_ack_tick: Tick::ZERO,
        })
        .unwrap();
    let ack = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::pin!(ack_rx);
        loop {
            client.poll();
            if let Ok(result) = ack_rx.try_recv() {
                break result;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(ack, ("session.ack".into(), serde_json::json!({"tick":10})));
    send_stale.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !matches!(client.status(), OnlineStatus::Failed(_)) {
            client.poll();
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        client.status(),
        OnlineStatus::Failed(lynx_client::online::OnlineError::State(
            lynx_client::world_state::Error::StaleTick
        ))
    );
    assert_eq!(client.world().tick(), None);
    server.await.unwrap();
}

#[tokio::test]
async fn rejected_updates_are_not_acknowledged() {
    for case in ["snapshot", "new_entity", "owner", "reliable"] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/world", listener.local_addr().unwrap());
        let (result_tx, result_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            socket.next().await.unwrap().unwrap();
            let actor = StableId::from_parts(IdKind::Entity, 1, 1);
            let ready = message(
                "world.ready",
                serde_json::json!({"session":"session-1", "actor":actor, "resumed":false}),
            );
            socket
                .send(Message::Binary(encode_frame(&ready).unwrap().into()))
                .await
                .unwrap();
            let movement = serde_json::json!({"tick":10,"position":{"x":0,"y":0,"z":0},"last_consumed_sequence":null,"applied":null});
            let set = if case == "snapshot" {
                serde_json::json!([actor])
            } else {
                serde_json::json!([])
            };
            let snapshot = message(
                "world.snapshot",
                serde_json::json!({"tick":10,"interest":{"set":set,"added":[],"removed":[]},"batch":{"tick":10,"updates":[]},"movement":movement}),
            );
            socket
                .send(Message::Binary(encode_frame(&snapshot).unwrap().into()))
                .await
                .unwrap();
            if case != "snapshot" {
                let Message::Binary(bytes) = socket.next().await.unwrap().unwrap() else {
                    panic!("expected snapshot ack")
                };
                assert_eq!(
                    decode_frame(&bytes).unwrap().body,
                    serde_json::json!({"tick":10})
                );
                let set = if case == "new_entity" {
                    serde_json::json!([actor])
                } else {
                    serde_json::json!([])
                };
                let updates = match case {
                    "owner" => {
                        serde_json::json!([{"category":"owner_only","entity":actor,"tick":11,"payload":{"inventory":{}}}])
                    }
                    "reliable" => {
                        serde_json::json!([{"category":"reliable_event","session":"session-1","tick":11,"payload":{}}])
                    }
                    _ => serde_json::json!([]),
                };
                let delta = message(
                    "world.delta",
                    serde_json::json!({"tick":11,"interest":{"set":set,"added":[],"removed":[]},"batch":{"tick":11,"updates":updates},"movement":null}),
                );
                socket
                    .send(Message::Binary(encode_frame(&delta).unwrap().into()))
                    .await
                    .unwrap();
            }
            let received = tokio::time::timeout(Duration::from_millis(300), socket.next()).await;
            let ack = matches!(received, Ok(Some(Ok(Message::Binary(bytes)))) if decode_frame(&bytes).is_ok_and(|e| e.kind.as_str() == "session.ack"));
            result_tx.send(ack).unwrap();
        });
        let mut client = OnlineClient::new(1.0).unwrap();
        client
            .connect(Config {
                url,
                bearer: "test-secret".into(),
                handshake: demo::handshake().unwrap(),
                last_ack_tick: Tick::ZERO,
            })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !matches!(client.status(), OnlineStatus::Failed(_)) {
                client.poll();
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let expected = if case == "owner" || case == "reliable" {
            lynx_client::world_state::Error::UnsupportedReplication
        } else {
            lynx_client::world_state::Error::InvalidMessage
        };
        assert_eq!(
            client.status(),
            OnlineStatus::Failed(lynx_client::online::OnlineError::State(expected)),
            "{case}"
        );
        assert!(!result_rx.await.unwrap(), "{case} was acknowledged");
        server.await.unwrap();
    }
}

#[tokio::test]
async fn headless_schedule_connects_moves_and_disconnects() {
    const TOKEN: &str = "headless-test-secret";
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

    let mut online = OnlineClient::new(1.0).unwrap();
    online
        .connect(Config {
            url,
            bearer: TOKEN.into(),
            handshake: demo::handshake().unwrap(),
            last_ack_tick: Tick::ZERO,
        })
        .unwrap();
    let mut app = headless_app();
    app.insert_resource(online);
    #[cfg(feature = "window")]
    {
        use bevy::prelude::*;
        app.init_resource::<ButtonInput<KeyCode>>()
            .add_plugins(lynx_client::windowed::OnlineWindowPlugin);
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyW);
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            app.update();
            let client = app.world().resource::<OnlineClient>();
            assert!(!matches!(client.status(), OnlineStatus::Failed(_)));
            if client.status() == OnlineStatus::Active {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    #[cfg(feature = "window")]
    {
        use bevy::prelude::*;
        // Presentation may run before the poller in the active frame.
        app.update();
        let expected: Vec<_> = app
            .world()
            .resource::<OnlineClient>()
            .world()
            .entities()
            .map(|e| (e.position.x * 32.0, -e.position.z * 32.0))
            .collect();
        assert!(!expected.is_empty());
        let world = app.world_mut();
        let displayed: Vec<_> = world
            .query::<(&Sprite, &Transform)>()
            .iter(world)
            .map(|(_, t)| (t.translation.x, t.translation.y))
            .collect();
        assert_eq!(displayed.len(), expected.len());
        for position in expected {
            assert!(displayed.contains(&position));
        }
        assert_eq!(
            world
                .resource::<lynx_client::offline::OfflineScene>()
                .avatar_position,
            Vec3::ZERO
        );
    }
    let position = app
        .world()
        .resource::<OnlineClient>()
        .world()
        .reconciliation()
        .authoritative_position()
        .unwrap();
    app.world_mut()
        .resource_mut::<OnlineClient>()
        .move_to(Position {
            x: position.x + 0.1,
            ..position
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            app.update();
            let client = app.world().resource::<OnlineClient>();
            assert!(!matches!(client.status(), OnlineStatus::Failed(_)));
            if client.world().reconciliation().predicted_position()
                == client.world().reconciliation().authoritative_position()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(!format!("{:?}", app.world().resource::<OnlineClient>()).contains(TOKEN));
    let mut client = app.world_mut().resource_mut::<OnlineClient>();
    client.disconnect();
    assert_eq!(client.status(), OnlineStatus::Disconnected);
    assert_eq!(client.world().tick(), None);
    drop(client);
    #[cfg(feature = "window")]
    {
        use bevy::prelude::*;
        app.update();
        let world = app.world_mut();
        assert_eq!(world.query::<&Sprite>().iter(world).count(), 0);
    }
    let _ = shutdown.send(true);
    let _ = stop.send(());
    service.await.unwrap();
    server.await.unwrap().unwrap();
}
