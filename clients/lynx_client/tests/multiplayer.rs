//! Two independent clients sharing the real gateway, simulation, and sockets.
use engine_core::{
    id::{IdKind, StableId},
    time::Tick,
};
use lynx_client::online::{OnlineClient, OnlineStatus};
use lynx_client_net::Config;
use lynx_world::{
    auth::{Authenticator, digest},
    demo,
    service::Service,
    transport::{self, AppState},
};
use std::{sync::Arc, time::Duration};
use tokio::sync::{oneshot, watch};
use world_gateway::{GatewayConfig, SystemResumeTokens, WorldGateway};
use world_server::WorldServer;

async fn until(clients: &mut [OnlineClient; 2], ready: impl Fn(&[OnlineClient; 2]) -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            for client in clients.iter_mut() {
                client.poll();
                assert!(!matches!(client.status(), OnlineStatus::Failed(_)));
            }
            if ready(clients) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn two_clients_observe_combat_exchange_chat_and_resume_without_duplicates() {
    let tokens = ["multiplayer-first", "multiplayer-second"];
    let auth = Authenticator::new(&tokens.map(digest)).unwrap();
    let grants: Vec<_> = auth.grants().cloned().collect();
    let mut zone = demo::build_zone(&grants).unwrap();
    // Keep both players in one interest set; production demo spaces them apart.
    let actors: Vec<_> = grants
        .iter()
        .map(|grant| StableId::from_parts(IdKind::Entity, 1, grant.principal.random_bits()))
        .collect();
    let position = zone.entity(actors[0]).unwrap().position();
    zone.entity_mut(actors[1]).unwrap().transform.position = position;
    let gateway = WorldGateway::new(
        zone,
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
    let (stop, stopped) = oneshot::channel();
    let server = tokio::spawn(transport::serve_with_shutdown(
        listener,
        Arc::new(AppState::new(events, auth)),
        async {
            let _ = stopped.await;
        },
    ));
    let mut clients = tokens.map(|token| {
        let mut client = OnlineClient::new(1.0).unwrap();
        client
            .connect(Config {
                url: url.clone(),
                bearer: token.into(),
                character: None,
                handshake: demo::handshake().unwrap(),
                last_ack_tick: Tick::ZERO,
            })
            .unwrap();
        client
    });
    until(&mut clients, |clients| {
        clients
            .iter()
            .all(|client| client.status() == OnlineStatus::Active)
    })
    .await;
    let barrel = clients[0]
        .world()
        .entities()
        .find(|entity| entity.prototype.as_str() == "prop.barrel")
        .unwrap()
        .entity;
    clients[0].interact(barrel, "attack").unwrap();
    until(&mut clients, |clients| {
        clients.iter().all(|client| {
            client.world().entities().any(|entity| {
                entity.entity == barrel && entity.health.is_some_and(|health| health.current < 100)
            })
        })
    })
    .await;
    let health: Vec<_> = clients
        .iter()
        .map(|client| {
            client
                .world()
                .entities()
                .find(|entity| entity.entity == barrel)
                .unwrap()
                .health
        })
        .collect();
    assert_eq!(health[0], health[1]);
    clients[0].send_chat("hello from first").unwrap();
    clients[1].send_chat("hello from second").unwrap();
    for client in &mut clients {
        client.request_chat_presence().unwrap();
    }
    until(&mut clients, |clients| {
        clients.iter().all(|client| {
            client.chat().messages().count() == 2 && client.chat().members().len() == 2
        })
    })
    .await;
    let first: Vec<_> = clients[0].chat().messages().cloned().collect();
    assert_eq!(
        first,
        clients[1].chat().messages().cloned().collect::<Vec<_>>()
    );
    assert_ne!(first[0].sender_id, first[1].sender_id);
    clients[1].reconnect().unwrap();
    until(&mut clients, |clients| {
        clients[1].status() == OnlineStatus::Active
    })
    .await;
    clients[1].request_chat_history(1, None).unwrap();
    until(&mut clients, |clients| {
        clients[1].chat().next_before().is_some()
    })
    .await;
    let cursor = clients[1].chat().next_before();
    clients[1].request_chat_history(1, cursor).unwrap();
    until(&mut clients, |clients| {
        clients[1].chat().next_before().is_none()
    })
    .await;
    assert_eq!(
        first,
        clients[1].chat().messages().cloned().collect::<Vec<_>>()
    );
    clients[1].disconnect();
    // Presence is polled by the graphical client; request again until detach is processed.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            clients[0].request_chat_presence().unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            clients[0].poll();
            if clients[0].chat().members().len() == 1 {
                break;
            }
        }
    })
    .await
    .unwrap();
    clients[0].disconnect();
    let _ = shutdown.send(true);
    let _ = stop.send(());
    service.await.unwrap();
    server.await.unwrap().unwrap();
}
