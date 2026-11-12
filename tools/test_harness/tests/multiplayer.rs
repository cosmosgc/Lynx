use std::num::NonZeroU64;
use std::time::Duration;

use engine_core::math::Vec3;
use engine_net::replication::ReplicationCategory;
use lynx_test_harness::{
    HeadlessClient, NetworkConditions, TestServer, assert_shared_entity_consistency, demo_actor_id,
};
use serde_json::from_value;
use world_gateway::EntitySnapshot;

const TOKENS: [&str; 2] = ["harness-client-one", "harness-client-two"];
const WAIT: Duration = Duration::from_secs(5);

async fn until(mut condition: impl FnMut() -> anyhow::Result<bool>) -> anyhow::Result<()> {
    tokio::time::timeout(WAIT, async {
        loop {
            if condition()? {
                return Ok::<(), anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await??;
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "The end-to-end harness scenario keeps its multi-client assertions ordered together."
)]
async fn two_headless_clients_share_authoritative_state_and_resume() -> anyhow::Result<()> {
    let server = TestServer::start_with_zone(&TOKENS, |grants, zone| {
        let first = demo_actor_id(&grants[0]);
        let second = demo_actor_id(&grants[1]);
        let first_position = zone.entity(first).unwrap().position();
        zone.entity_mut(second).unwrap().transform.position = first_position;
    })
    .await?;
    let handshake = lynx_world::demo::handshake()?;
    let mut first = HeadlessClient::connect(server.url(), TOKENS[0], None, handshake.clone());
    let mut second = HeadlessClient::connect(server.url(), TOKENS[1], None, handshake);
    first.wait_ready(WAIT).await?;
    second.wait_ready(WAIT).await?;

    let actor = first.actor().expect("world.ready includes the actor");
    let barrel = first
        .updates()
        .rev()
        .flat_map(|observed| observed.update.batch.updates.iter().rev())
        .find(|update| {
            matches!(
                update.category,
                ReplicationCategory::InterestManaged | ReplicationCategory::StateSnapshot
            ) && update.payload["prototype"] == "prop.barrel"
        })
        .map(|update| from_value::<EntitySnapshot>(update.payload.clone()).unwrap())
        .expect("the demo scene contains a visible barrel");
    let initial = assert_shared_entity_consistency(&[&first, &second], barrel.entity)?;
    let initial_health = initial.health.expect("barrel is damageable").current;

    first.send_action(Some(barrel.entity), "attack", &[])?;
    until(|| {
        first.poll()?;
        second.poll()?;
        let left = first.entity_snapshot(barrel.entity);
        let right = second.entity_snapshot(barrel.entity);
        Ok(left
            .as_ref()
            .and_then(|state| state.health)
            .is_some_and(|health| health.current < initial_health)
            && right
                .as_ref()
                .and_then(|state| state.health)
                .is_some_and(|health| health.current < initial_health))
    })
    .await?;
    let after_attack = assert_shared_entity_consistency(&[&first, &second], barrel.entity)?;
    assert!(after_attack.health.unwrap().current < initial_health);

    let actor_state = first.entity_snapshot(actor).expect("actor is in own view");
    let movement_sequence = first.move_to(Vec3::new(
        actor_state.position.x + 0.25,
        actor_state.position.y,
        actor_state.position.z,
    ))?;
    until(|| {
        first.poll()?;
        second.poll()?;
        Ok(first.observed_movement_sequence(movement_sequence))
    })
    .await?;
    second.poll()?;
    assert!(first.observed_movement_sequence(movement_sequence));
    assert!(
        !second.observed_movement_sequence(movement_sequence),
        "movement reconciliation is session-private"
    );

    first.send_chat("headless harness chat")?;
    until(|| {
        first.poll()?;
        second.poll()?;
        Ok([&first, &second].iter().all(|client| {
            client.messages().any(|message| {
                message.kind.as_str() == "chat.message"
                    && message.body["content"] == "headless harness chat"
            })
        }))
    })
    .await?;

    second.reconnect(WAIT).await?;
    assert_eq!(second.status(), lynx_client_net::Status::Ready);
    assert_eq!(
        assert_shared_entity_consistency(&[&first, &second], barrel.entity)?,
        after_attack
    );

    // Deterministic envelope loss is useful for testing caller behavior without
    // introducing packet-level or wall-clock flakiness.
    first.set_network_conditions(NetworkConditions {
        outbound_latency: Duration::from_millis(10),
        drop_every_nth_outbound: NonZeroU64::new(1),
        ..NetworkConditions::default()
    });
    first.send_chat("intentionally dropped")?;
    tokio::time::sleep(Duration::from_millis(15)).await;
    first.poll()?;
    assert_eq!(first.dropped_outbound(), 1);
    first.set_network_conditions(NetworkConditions {
        inbound_latency: Duration::from_millis(10),
        drop_every_nth_inbound: NonZeroU64::new(1),
        ..NetworkConditions::default()
    });
    second.send_chat("incoming envelope to drop")?;
    until(|| {
        first.poll()?;
        second.poll()?;
        Ok(second.messages().any(|message| {
            message.kind.as_str() == "chat.message"
                && message.body["content"] == "incoming envelope to drop"
        }))
    })
    .await?;
    assert!(first.dropped_inbound() > 0);

    first.set_network_conditions(NetworkConditions::default());
    drop(first);
    drop(second);
    server.shutdown().await?;
    Ok(())
}
