use std::{sync::Arc, time::Duration};

use engine_core::id::{DefinitionId, IdKind, StableId};
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
#[allow(clippy::too_many_lines)]
async fn delayed_private_corrections_remain_bounded_over_websocket() {
    for delay_ms in [100, 200] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/world", listener.local_addr().unwrap());
        let (stop, stopped) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            socket.next().await.unwrap().unwrap();
            for envelope in [
                message(
                    "world.ready",
                    serde_json::json!({"session":"delayed", "actor":StableId::from_parts(IdKind::Entity, 1, 1), "resumed":false}),
                ),
                message(
                    "world.snapshot",
                    serde_json::json!({"tick":10,"interest":{"set":[],"added":[],"removed":[]},"batch":{"tick":10,"updates":[]},"movement":{"tick":10,"position":{"x":0,"y":0,"z":0},"last_consumed_sequence":null,"applied":null}}),
                ),
            ] {
                socket
                    .send(Message::Binary(encode_frame(&envelope).unwrap().into()))
                    .await
                    .unwrap();
            }
            let request = loop {
                let Message::Binary(bytes) = socket.next().await.unwrap().unwrap() else {
                    continue;
                };
                let envelope = decode_frame(&bytes).unwrap();
                if envelope.kind.as_str() == "movement.request" {
                    break envelope;
                }
            };
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            let sequence = request.body["sequence"].as_u64().unwrap();
            let mut queued = message("movement.queued", serde_json::json!({"sequence":sequence}));
            queued.correlation = Some(request.id);
            socket
                .send(Message::Binary(encode_frame(&queued).unwrap().into()))
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            let correction = message(
                "world.delta",
                serde_json::json!({"tick":11,"interest":{"set":[],"added":[],"removed":[]},"batch":{"tick":11,"updates":[]},"movement":{"tick":11,"position":{"x":0,"y":0,"z":0},"last_consumed_sequence":sequence,"applied":false}}),
            );
            socket
                .send(Message::Binary(encode_frame(&correction).unwrap().into()))
                .await
                .unwrap();
            let _ = stopped.await;
        });
        let mut client = OnlineClient::new(1.0).unwrap();
        client
            .connect(Config {
                url,
                bearer: "test-secret".into(),
                character: None,
                handshake: demo::handshake().unwrap(),
                last_ack_tick: Tick::ZERO,
            })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while client.status() != OnlineStatus::Active {
                client.poll();
                assert!(!matches!(client.status(), OnlineStatus::Failed(_)));
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let origin = client
            .world()
            .reconciliation()
            .authoritative_position()
            .unwrap();
        let destination = Position { x: 0.75, ..origin };
        client.move_to(destination).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while client.pending_movement() {
                // A delayed acknowledgement/result must not allow another send
                // or accumulate prediction beyond the one permitted endpoint.
                assert!(client.move_to(Position { x: 1.0, ..origin }).is_err());
                assert_eq!(
                    client.world().reconciliation().predicted_position(),
                    Some(destination)
                );
                assert_eq!(
                    client.world().reconciliation().authoritative_position(),
                    Some(origin)
                );
                client.poll();
                assert_eq!(client.status(), OnlineStatus::Active);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(client.last_movement_applied(), Some(false));
        client.smooth_presentation(0.5);
        assert_eq!(
            client
                .world()
                .reconciliation()
                .presentation_position()
                .unwrap()
                .x,
            0.375
        );
        assert_eq!(
            client.world().reconciliation().authoritative_position(),
            Some(origin)
        );
        client.smooth_presentation(1.0);
        assert_eq!(
            client.world().reconciliation().presentation_position(),
            Some(origin)
        );
        stop.send(()).unwrap();
        server.await.unwrap();
    }
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
            character: None,
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
                character: None,
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
        // Owner-only state is supported now, but this incomplete inventory
        // payload is still rejected atomically.
        let expected = if case == "reliable" {
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
#[allow(clippy::too_many_lines)] // Keep the end-to-end headless session in one readable flow.
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
        Service::new(WorldServer::new(gateway), Duration::from_millis(50))
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
            character: None,
            handshake: demo::handshake().unwrap(),
            last_ack_tick: Tick::ZERO,
        })
        .unwrap();
    let mut app = headless_app();
    app.insert_resource(online);
    #[cfg(feature = "window")]
    {
        use bevy::prelude::*;
        use bevy::window::PrimaryWindow;
        app.init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<bevy::input_focus::InputFocus>()
            .init_resource::<UiScale>()
            .add_plugins((
                lynx_client::windowed::OnlineWindowPlugin,
                lynx_client::inventory_panel::InventoryPanelPlugin,
                lynx_client::lock_panel::LockPanelPlugin,
                lynx_client::ui::chat::ChatPanelPlugin,
            ));
        app.world_mut().spawn((Window::default(), PrimaryWindow));
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
    let inventory = app
        .world()
        .resource::<OnlineClient>()
        .world()
        .inventory()
        .expect("the demo actor receives its owner inventory");
    assert_eq!(
        inventory.total_quantity(&DefinitionId::new("item.torch").unwrap()),
        3
    );
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
        assert_ne!(expected.len(), 0);
        let world = app.world_mut();
        let displayed: Vec<_> = world
            .query_filtered::<(&Sprite, &Transform), Without<ChildOf>>()
            .iter(world)
            .map(|(_, t)| (t.translation.x, t.translation.y))
            .collect();
        assert_eq!(displayed.len(), expected.len());
        // Remote actors may already be interpolating toward a newer tick.
        assert!(
            displayed
                .iter()
                .all(|(x, y)| x.is_finite() && y.is_finite())
        );
        assert_eq!(
            world
                .resource::<lynx_client::offline::OfflineScene>()
                .avatar_position,
            Vec3::ZERO
        );

        // Held WASD input issues bounded movement intents through the online
        // client; the server remains authoritative and reports the outcome.
        let start = app
            .world()
            .resource::<OnlineClient>()
            .world()
            .reconciliation()
            .authoritative_position()
            .unwrap();
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyW);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                app.update();
                let client = app.world().resource::<OnlineClient>();
                assert!(!matches!(client.status(), OnlineStatus::Failed(_)));
                if let Some(position) = client.world().reconciliation().authoritative_position()
                    && position.z < start.z
                    && client.last_movement_applied() == Some(true)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .release(KeyCode::KeyW);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                app.update();
                if !app.world().resource::<OnlineClient>().pending_movement() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
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
            x: position.x + 0.09,
            ..position
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            app.update();
            let client = app.world().resource::<OnlineClient>();
            assert!(!matches!(client.status(), OnlineStatus::Failed(_)));
            if client.last_movement_applied() == Some(true)
                && client.world().reconciliation().predicted_position()
                    == client.world().reconciliation().authoritative_position()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        app.world()
            .resource::<OnlineClient>()
            .world()
            .reconciliation()
            .authoritative_position()
            .unwrap()
            .x
            > position.x
    );
    // Predict a locally permitted endpoint that exceeds the server's 50 ms
    // movement budget. Admission succeeds, but authoritative processing rejects
    // it. Exercise the real socket/private-result path, not a fabricated result.
    {
        let mut client = app.world_mut().resource_mut::<OnlineClient>();
        let authority = client
            .world()
            .reconciliation()
            .authoritative_position()
            .unwrap();
        client
            .move_to(Position {
                x: authority.x + 0.9,
                ..authority
            })
            .unwrap();
        assert_ne!(
            client.world().reconciliation().presentation_position(),
            Some(authority)
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            while client.pending_movement() {
                client.poll();
                assert_eq!(client.status(), OnlineStatus::Active);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(client.last_movement_applied(), Some(false));
        assert_eq!(
            client.world().reconciliation().authoritative_position(),
            Some(authority)
        );
        client.smooth_presentation(0.5);
        assert_ne!(
            client.world().reconciliation().presentation_position(),
            Some(authority)
        );
        assert_eq!(
            client.world().reconciliation().authoritative_position(),
            Some(authority)
        );
        client.smooth_presentation(1.0);
        assert_eq!(
            client.world().reconciliation().presentation_position(),
            Some(authority)
        );
    }
    {
        let mut client = app.world_mut().resource_mut::<OnlineClient>();
        let barrel = client
            .world()
            .entities()
            .find(|entity| entity.prototype.as_str() == "prop.barrel")
            .unwrap()
            .entity;
        let vault = client
            .world()
            .entities()
            .find(|entity| entity.prototype.as_str() == "prop.vault")
            .unwrap()
            .entity;
        let door = client
            .world()
            .entities()
            .find(|entity| entity.prototype.as_str() == "prop.door")
            .unwrap()
            .entity;
        assert!(client.interact(door, "attack").is_err());
        client.interact(barrel, "use").unwrap();
        wait_for_interaction(&mut client).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while client.interaction().panel().is_none() {
                client.poll();
                assert_eq!(client.status(), OnlineStatus::Active);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            client.interaction().panel().unwrap().schema.as_str(),
            "ui.display"
        );
        client.close_panel().unwrap();
        wait_for_interaction(&mut client).await;
        assert!(client.interaction().panel().is_none());
        client.interact(vault, "use").unwrap();
        assert!(client.interact(door, "open").is_err());
        wait_for_interaction(&mut client).await;
        assert!(client.interaction().panel().is_none());
        assert_ne!(
            client.interaction().status(),
            "Ação confirmada pelo servidor."
        );
        client.interact(door, "open").unwrap();
        wait_for_interaction(&mut client).await;
        assert_eq!(
            client.interaction().status(),
            "Ação confirmada pelo servidor."
        );
        for _ in 0..18 {
            let position = client
                .world()
                .reconciliation()
                .authoritative_position()
                .unwrap();
            client
                .move_to(Position {
                    z: position.z + 0.09,
                    ..position
                })
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while client.pending_movement() {
                    client.poll();
                    assert_eq!(client.status(), OnlineStatus::Active);
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
        }
        client.interact(vault, "use").unwrap();
        wait_for_interaction(&mut client).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while client.interaction().panel().is_none() {
                client.poll();
                assert_eq!(client.status(), OnlineStatus::Active);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            client.interaction().panel().unwrap().schema.as_str(),
            "ui.lockpick"
        );
        let abandoned_panel = client.interaction().panel().unwrap().clone();
        #[cfg(feature = "window")]
        {
            app.update();
            app.update();
            assert!(has_lock_dialog(&mut app));
            click_lock_button(&mut app, "submit");
            assert!(
                !app.world()
                    .resource::<OnlineClient>()
                    .interaction()
                    .pending()
            );
            {
                use bevy::prelude::*;
                let world = app.world_mut();
                world
                    .query_filtered::<&mut Window, With<bevy::window::PrimaryWindow>>()
                    .single_mut(world)
                    .unwrap()
                    .focused = false;
            }
            click_lock_button(&mut app, "5");
            assert!(
                app.world_mut()
                    .query::<&bevy::prelude::Text>()
                    .iter(app.world())
                    .any(|text| text.0 == "[ ___ ]")
            );
            {
                use bevy::prelude::*;
                let world = app.world_mut();
                world
                    .query_filtered::<&mut Window, With<bevy::window::PrimaryWindow>>()
                    .single_mut(world)
                    .unwrap()
                    .focused = true;
            }
            click_lock_button(&mut app, "5");
            assert!(
                app.world_mut()
                    .query::<&bevy::prelude::Text>()
                    .iter(app.world())
                    .any(|text| text.0 == "[ 5__ ]")
            );
            click_lock_button(&mut app, "erase");
            app.update();
            assert!(has_named_entity(&mut app, "lock-dialog-titlebar"));
            assert!(has_named_entity(&mut app, "lock-button-close"));
            click_lock_button(&mut app, "close");
            {
                let mut client = app.world_mut().resource_mut::<OnlineClient>();
                wait_for_interaction(&mut client).await;
                assert!(client.interaction().panel().is_none());
            }
            app.update();
            app.update();
            assert!(!has_lock_dialog(&mut app));
        }
        #[cfg(feature = "window")]
        let mut client = app.world_mut().resource_mut::<OnlineClient>();
        client.interact(vault, "use").unwrap();
        wait_for_interaction(&mut client).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while client.interaction().panel().is_none() {
                client.poll();
                assert_eq!(client.status(), OnlineStatus::Active);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        // Walking away closes the server's panel without submitting any action.
        move_z_steps(&mut client, -0.09, 12).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while client.interaction().panel().is_some() {
                client.poll();
                assert_eq!(client.status(), OnlineStatus::Active);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(client.interaction().lock().is_none());
        assert!(client.submit_lock_guess(&abandoned_panel, "555").is_err());
        #[cfg(feature = "window")]
        {
            app.update();
            app.update();
            assert!(!has_lock_dialog(&mut app));
        }
        #[cfg(feature = "window")]
        let mut client = app.world_mut().resource_mut::<OnlineClient>();
        move_z_steps(&mut client, 0.09, 12).await;
        client.interact(vault, "use").unwrap();
        wait_for_interaction(&mut client).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while client.interaction().panel().is_none() {
                client.poll();
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let panel = client.interaction().panel().unwrap().clone();
        let mut stale = panel.clone();
        stale.target = door;
        assert!(client.submit_lock_guess(&stale, "555").is_err());
        assert!(client.submit_lock_guess(&panel, "xx").is_err());
        // Solve using only the server's per-digit comparisons, never its seed.
        let mut low = [0_u8; 3];
        let mut high = [9_u8; 3];
        for _ in 0..4 {
            let digits = std::array::from_fn::<_, 3, _>(|i| u8::midpoint(low[i], high[i]));
            let guess: String = digits
                .iter()
                .map(|digit| char::from(b'0' + digit))
                .collect();
            #[cfg(feature = "window")]
            {
                app.update();
                app.update();
                for digit in guess.chars() {
                    let id = if digit == '0' {
                        "zero".to_owned()
                    } else {
                        digit.to_string()
                    };
                    click_lock_button(&mut app, &id);
                }
                click_lock_button(&mut app, "submit");
                client = app.world_mut().resource_mut::<OnlineClient>();
                assert!(client.interaction().pending());
            }
            #[cfg(not(feature = "window"))]
            client.submit_lock_guess(&panel, &guess).unwrap();
            wait_for_interaction(&mut client).await;
            if let Some(lock) = client.interaction().lock() {
                if lock.solved {
                    break;
                }
                assert_eq!(lock.hints.len(), 3);
                for (i, hint) in lock.hints.iter().enumerate() {
                    match hint {
                        lynx_client::interaction::DigitHint::Exact => {
                            low[i] = digits[i];
                            high[i] = digits[i];
                        }
                        lynx_client::interaction::DigitHint::Higher => low[i] = digits[i] + 1,
                        lynx_client::interaction::DigitHint::Lower => high[i] = digits[i] - 1,
                    }
                }
            } else {
                break;
            }
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            while client.interaction().panel().is_some() {
                client.poll();
                assert_eq!(client.status(), OnlineStatus::Active);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(client.submit_lock_guess(&panel, "555").is_err());
        client.close_panel().unwrap();
        wait_for_interaction(&mut client).await;
        assert!(client.interaction().panel().is_none());
    }
    assert!(!format!("{:?}", app.world().resource::<OnlineClient>()).contains(TOKEN));
    {
        let mut client = app.world_mut().resource_mut::<OnlineClient>();
        client.disconnect();
        assert_eq!(client.status(), OnlineStatus::Disconnected);
        assert_eq!(client.world().tick(), None);
    }
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

#[tokio::test]
#[allow(clippy::too_many_lines)] // The reconnect flow needs a real in-process server.
async fn reconnects_with_rotated_resume_token_and_keeps_moving() {
    const TOKEN: &str = "resume-test-secret";
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
        Service::new(WorldServer::new(gateway), Duration::from_millis(50))
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

    let mut client = OnlineClient::new(1.0).unwrap();
    client
        .connect(Config {
            url,
            bearer: TOKEN.into(),
            character: None,
            handshake: demo::handshake().unwrap(),
            last_ack_tick: Tick::ZERO,
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            client.poll();
            assert!(!matches!(client.status(), OnlineStatus::Failed(_)));
            if client.status() == OnlineStatus::Active && client.resume_available() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(!client.resumed());
    let before = client
        .world()
        .reconciliation()
        .authoritative_position()
        .unwrap();
    assert!(client.world().tick().is_some());
    assert_eq!(client.last_ack_tick(), client.world().tick().unwrap());

    // Reopening the transport races the server's detach notification; the
    // client must treat a conflict as retryable and then resume the session.
    client.reconnect().unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            client.poll();
            assert!(!matches!(client.status(), OnlineStatus::Failed(_)));
            if client.status() == OnlineStatus::Active
                && client.resumed()
                && client.resume_available()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(client.resumed());
    let resumed = client
        .world()
        .reconciliation()
        .authoritative_position()
        .unwrap();
    assert_eq!(resumed, before);

    let position = resumed;
    client
        .move_to(Position {
            x: position.x + 0.09,
            ..position
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            client.poll();
            assert!(!matches!(client.status(), OnlineStatus::Failed(_)));
            if client.last_movement_applied() == Some(true) && !client.pending_movement() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        client
            .world()
            .reconciliation()
            .authoritative_position()
            .unwrap()
            .x
            > position.x
    );
    assert!(!format!("{client:?}").contains(TOKEN));
    let actor = client.world().actor().unwrap();
    let facing = client
        .world()
        .entities()
        .find(|entity| entity.entity == actor)
        .unwrap()
        .yaw_degrees;
    assert!(
        (facing - 90.0).abs() < 0.001,
        "accepted +X movement must replicate facing"
    );

    // A second successful resume proves that the rotated credential replaced
    // the now-consumed token from the first admission.
    client.reconnect().unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            client.poll();
            assert!(!matches!(client.status(), OnlineStatus::Failed(_)));
            if client.status() == OnlineStatus::Active
                && client.resumed()
                && client.resume_available()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();

    let _ = shutdown.send(true);
    let _ = stop.send(());
    service.await.unwrap();
    server.await.unwrap().unwrap();
}

/// TCP proxy used to inject a transport drop without stopping the authority.
/// Each accepted connection is forwarded byte-for-byte until the watch value
/// changes, at which point both halves are dropped to close the sockets.
async fn spawn_drop_proxy(
    target: std::net::SocketAddr,
) -> (
    std::net::SocketAddr,
    watch::Sender<u64>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (drop_signal, drop_rx) = watch::channel(0_u64);
    let task = tokio::spawn(async move {
        loop {
            let Ok((client, _)) = listener.accept().await else {
                return;
            };
            let mut connection_drop = drop_rx.clone();
            // Connections accepted after a drop must wait for the next signal,
            // not inherit the already-fired one.
            connection_drop.borrow_and_update();
            tokio::spawn(async move {
                let Ok(upstream) = tokio::net::TcpStream::connect(target).await else {
                    return;
                };
                let (mut client_read, mut client_write) = client.into_split();
                let (mut upstream_read, mut upstream_write) = upstream.into_split();
                tokio::select! {
                    _ = tokio::io::copy(&mut client_read, &mut upstream_write) => {}
                    _ = tokio::io::copy(&mut upstream_read, &mut client_write) => {}
                    _ = connection_drop.changed() => {}
                }
            });
        }
    });
    (address, drop_signal, task)
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // The injected-drop flow needs a real in-process server.
async fn automatically_reconnects_after_injected_transport_drop() {
    const TOKEN: &str = "drop-test-secret";
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
        Service::new(WorldServer::new(gateway), Duration::from_millis(50))
            .run(receiver, shutdown_rx),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_address = listener.local_addr().unwrap();
    let (proxy_address, drop_signal, proxy) = spawn_drop_proxy(server_address).await;
    let (stop, stop_rx) = oneshot::channel();
    let server = tokio::spawn(transport::serve_with_shutdown(
        listener,
        Arc::new(AppState::new(events, auth)),
        async {
            let _ = stop_rx.await;
        },
    ));
    let url = format!("ws://{proxy_address}/world");

    let mut client = OnlineClient::new(1.0).unwrap();
    client
        .connect(Config {
            url,
            bearer: TOKEN.into(),
            character: None,
            handshake: demo::handshake().unwrap(),
            last_ack_tick: Tick::ZERO,
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            client.poll();
            assert!(!matches!(client.status(), OnlineStatus::Failed(_)));
            if client.status() == OnlineStatus::Active && client.resume_available() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let before = client
        .world()
        .reconciliation()
        .authoritative_position()
        .unwrap();
    assert_eq!(client.reconnect_attempts(), 0);

    // Close the proxied transport. The client must notice on its own, back off,
    // reconnect with the rotated resume token, and restore the same baseline.
    drop_signal.send(1).unwrap();
    let mut saw_reconnecting = false;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            client.poll();
            assert!(!matches!(client.status(), OnlineStatus::Failed(_)));
            if matches!(client.status(), OnlineStatus::Reconnecting { .. }) {
                saw_reconnecting = true;
            }
            if client.status() == OnlineStatus::Active
                && client.resumed()
                && client.resume_available()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(saw_reconnecting, "drop must enter the automatic retry path");
    assert_eq!(client.reconnect_attempts(), 1);
    assert_eq!(
        client
            .world()
            .reconciliation()
            .authoritative_position()
            .unwrap(),
        before
    );

    let position = before;
    client
        .move_to(Position {
            x: position.x + 0.09,
            ..position
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            client.poll();
            assert!(!matches!(client.status(), OnlineStatus::Failed(_)));
            if client.last_movement_applied() == Some(true) && !client.pending_movement() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        client
            .world()
            .reconciliation()
            .authoritative_position()
            .unwrap()
            .x
            > position.x
    );

    let _ = shutdown.send(true);
    let _ = stop.send(());
    proxy.abort();
    service.await.unwrap();
    server.await.unwrap().unwrap();
}

async fn wait_for_interaction(client: &mut OnlineClient) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while client.interaction().pending() {
            client.poll();
            assert_eq!(client.status(), OnlineStatus::Active);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

async fn move_z_steps(client: &mut OnlineClient, step: f32, count: usize) {
    for _ in 0..count {
        let position = client
            .world()
            .reconciliation()
            .authoritative_position()
            .unwrap();
        client
            .move_to(Position {
                z: position.z + step,
                ..position
            })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while client.pending_movement() {
                client.poll();
                assert_eq!(client.status(), OnlineStatus::Active);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
}

#[cfg(feature = "window")]
fn has_named_entity(app: &mut bevy::prelude::App, expected: &str) -> bool {
    app.world_mut()
        .query::<&bevy::prelude::Name>()
        .iter(app.world())
        .any(|name| name.as_str() == expected)
}

#[cfg(feature = "window")]
fn has_lock_dialog(app: &mut bevy::prelude::App) -> bool {
    has_named_entity(app, "online-lock-dialog")
}

#[cfg(feature = "window")]
fn click_lock_button(app: &mut bevy::prelude::App, id: &str) {
    use bevy::picking::{
        backend::HitData,
        events::{Pointer, PointerClick},
        pointer::{Location, PointerButton, PointerId},
    };
    use bevy::prelude::*;
    let name = format!("lock-button-{id}");
    let button = app
        .world_mut()
        .query::<(Entity, &Name)>()
        .iter(app.world())
        .find(|(_, candidate)| candidate.as_str() == name)
        .unwrap()
        .0;
    let window = app
        .world_mut()
        .query_filtered::<Entity, With<bevy::window::PrimaryWindow>>()
        .single(app.world())
        .unwrap();
    let event = PointerClick {
        entity: button,
        pointer: Pointer::new(
            PointerId::Mouse,
            Location {
                target: bevy::camera::RenderTarget::Window(bevy::window::WindowRef::Primary)
                    .normalize(Some(window))
                    .unwrap(),
                position: Vec2::ZERO,
            },
        ),
        button: PointerButton::Primary,
        hit: HitData::new(Entity::PLACEHOLDER, 0.0, None, None),
        duration: Duration::from_millis(20),
        count: 1,
    };
    app.add_systems(
        PreUpdate,
        move |mut commands: Commands, mut sent: Local<bool>| {
            if !*sent {
                commands.trigger(event.clone());
                *sent = true;
            }
        },
    );
    app.update();
    app.update();
}

#[tokio::test]
#[allow(clippy::result_large_err)] // The tungstenite upgrade callback uses a large HTTP response error.
async fn rejected_nakama_access_token_refreshes_off_thread_and_retries_admission() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/world", listener.local_addr().unwrap());
    let (stop, stopped) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut rejected, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let length = rejected.read(&mut buffer).await.unwrap();
            assert_ne!(length, 0, "client closed before sending handshake");
            request.extend_from_slice(&buffer[..length]);
            if request.windows(4).any(|part| part == b"\r\n\r\n") {
                break;
            }
        }
        let request = String::from_utf8_lossy(&request).to_ascii_lowercase();
        assert!(request.contains("authorization: bearer expired-access"));
        rejected
            .write_all(
                b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();

        let (stream, _) = listener.accept().await.unwrap();
        let mut socket =
            tokio_tungstenite::accept_hdr_async(stream, |request: &Request, response: Response| {
                let bearer = request
                    .headers()
                    .get("authorization")
                    .and_then(|value| value.to_str().ok());
                assert_eq!(bearer, Some("Bearer refreshed-access"));
                Ok(response)
            })
            .await
            .unwrap();
        let actor = StableId::from_parts(IdKind::Entity, 1, 1);
        for envelope in [
            message(
                "world.ready",
                serde_json::json!({"session":"refreshed", "actor":actor, "resumed":false}),
            ),
            message(
                "world.snapshot",
                serde_json::json!({"tick":1,"interest":{"set":[],"added":[],"removed":[]},"batch":{"tick":1,"updates":[]},"movement":{"tick":1,"position":{"x":0,"y":0,"z":0},"last_consumed_sequence":null,"applied":null}}),
            ),
        ] {
            socket
                .send(Message::Binary(encode_frame(&envelope).unwrap().into()))
                .await
                .unwrap();
        }
        let _ = stopped.await;
    });

    let refresh_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let refresh_count_clone = refresh_count.clone();
    let mut client = OnlineClient::new(1.0).unwrap();
    client.set_credential_refresher(move || {
        refresh_count_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok("refreshed-access".to_owned())
    });
    client
        .connect(Config {
            url,
            bearer: "expired-access".into(),
            character: Some("character-1".into()),
            handshake: demo::handshake().unwrap(),
            last_ack_tick: Tick::ZERO,
        })
        .unwrap();

    let mut saw_refreshing = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        while client.status() != OnlineStatus::Active {
            client.poll();
            saw_refreshing |= client.status() == OnlineStatus::RefreshingCredentials;
            assert!(!matches!(client.status(), OnlineStatus::Failed(_)));
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();

    assert!(saw_refreshing);
    assert_eq!(refresh_count.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(client.status(), OnlineStatus::Active);
    stop.send(()).unwrap();
    server.await.unwrap();
}
