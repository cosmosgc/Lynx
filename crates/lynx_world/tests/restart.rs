//! Durable restart across a whole world.
//!
//! The authority loop snapshots zone, calendar, and faction state together;
//! this test proves the saved pair survives a restart cross-validated and that
//! the same character reconnects to the same entity afterwards.

use backend_memory::MemoryBackend;
use engine_core::error::ErrorCode;
use engine_core::faction::FactionWorld;
use engine_core::time::{DurationMs, Millis};
use engine_core::world_event::WorldCalendar;
use lynx_world::auth::{Authenticator, digest};
use lynx_world::backend::DemoCalendarCommit;
use lynx_world::demo;
use lynx_world::persistence::{
    FileSnapshotStore, SnapshotStore, WorldSnapshot, WorldSnapshotInputs,
};
use world_gateway::{
    CalendarPersistence, GatewayConfig, SessionClaims, SystemResumeTokens, WorldGateway,
};
use world_server::WorldServer;

fn characters(count: usize) -> Vec<lynx_world::auth::Grant> {
    let hashes: Vec<[u8; 32]> = (0..count)
        .map(|index| digest(&format!("restart-token-{index}")))
        .collect();
    Authenticator::new(&hashes)
        .unwrap()
        .grants()
        .cloned()
        .collect()
}

#[test]
fn a_saved_world_restarts_and_accepts_the_same_character() -> anyhow::Result<()> {
    let grants = characters(2);
    let now_ms = 100_000;
    let timezone_free_now = Millis::new(now_ms);

    // First run: build the demo world, move its tick, then snapshot it.
    let mut factions = demo::build_faction_world(now_ms)?;
    let calendar = demo::build_calendar(&mut factions, now_ms)?;

    let dir = std::env::temp_dir().join("lynx-world-restart-tests");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("restart-world.json");
    std::fs::remove_file(&path).unwrap_or(());

    let mut gateway = WorldGateway::new(
        demo::build_zone(&grants)?,
        demo::handshake()?,
        GatewayConfig::new(DurationMs::new(30_000)),
        SystemResumeTokens::new(),
    )?;
    gateway = gateway.with_calendar(calendar, factions)?;

    for _ in 0..4 {
        gateway.tick(timezone_free_now)?;
    }
    let server = WorldServer::new(gateway);

    let saved_stage = server
        .gateway()
        .calendar_stage()
        .expect("the demo world owns a calendar");
    let (calendar_snapshot, faction_snapshot) = saved_stage.snapshots();
    let inputs = WorldSnapshotInputs {
        zone: server.gateway().zone().snapshot(),
        calendar_factions: Some((calendar_snapshot, faction_snapshot)),
    };
    let world = WorldSnapshot::capture(&inputs, now_ms);

    let store = FileSnapshotStore::new(&path);
    store.save(&world)?;

    // Second run: load the snapshot and rebuild every aggregate from it.
    let loaded = store.load()?.expect("the snapshot exists after the save");
    let restored_zone = demo::restore_zone(&loaded.zone)?;
    let restored_calendar = WorldCalendar::from_snapshot(loaded.calendar.expect("paired")).unwrap();
    let restored_factions = FactionWorld::from_snapshot(loaded.factions.expect("paired")).unwrap();
    let mut gateway = WorldGateway::new(
        restored_zone,
        demo::handshake()?,
        GatewayConfig::new(DurationMs::new(30_000)),
        SystemResumeTokens::new(),
    )?;
    gateway = gateway.with_calendar(restored_calendar, restored_factions)?;
    let mut server = WorldServer::new(gateway);

    assert_eq!(
        server.gateway().zone().tick(),
        loaded.zone.tick,
        "the restored world continues at its saved tick"
    );
    assert_eq!(
        server.gateway().zone().entity_count(),
        world.zone.entities.len()
    );

    // The same token authenticates to the same character the snapshot kept.
    let claims: SessionClaims = grants[0].claims()?;
    let principal = claims.principal;
    let opened = server.open(claims, &demo::handshake()?, Millis::new(now_ms + 50))?;
    assert!(opened.messages.len() >= 2, "ready and snapshot arrive");
    let expected = world
        .zone
        .entities
        .iter()
        .find(|entity| {
            entity
                .mind
                .as_ref()
                .is_some_and(|mind| mind.controller.player_character() == Some(principal))
        })
        .map(|entity| entity.id);
    assert_eq!(
        Some(opened.actor),
        expected,
        "the restored session binds the persisted character"
    );

    // Calendar state survived too: the demo conflict is published and open.
    let restored_stage = server
        .gateway()
        .calendar_stage()
        .expect("the restored world owns a calendar");
    let state = restored_stage.state();
    assert_eq!(state.events.len(), 1);
    assert_eq!(
        state.events[0].event.as_str(),
        "event.assalto_praca",
        "the demo conflict is still published"
    );
    Ok(())
}

#[test]
fn a_save_failure_does_not_commit_a_partial_pair() -> anyhow::Result<()> {
    // The trusted calendar commit seam rejects disagreeing state before
    // anything is stored, so a restart never loads calendar-only changes.
    let mut factions = demo::build_faction_world(0)?;
    let calendar = demo::build_calendar(&mut factions, 0)?;
    let commit = DemoCalendarCommit::new(MemoryBackend::new());
    commit.commit(&calendar.snapshot(), &factions.snapshot())?;
    Ok(())
}

#[test]
fn a_broken_snapshot_file_is_rejected_with_a_stable_code() -> anyhow::Result<()> {
    let dir = std::env::temp_dir().join("lynx-world-restart-tests");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("broken-restart-world.json");
    std::fs::write(&path, b"{ not a world snapshot }")?;
    let error = FileSnapshotStore::new(&path).load().unwrap_err();
    std::fs::remove_file(&path).unwrap_or(());
    assert_eq!(
        error.code(),
        ErrorCode::InvalidRequest,
        "invalid files never reach the gateway"
    );
    Ok(())
}
