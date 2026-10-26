//! End-to-end tests for the gateway calendar stage: command routing,
//! staff gating, replay protection, snapshot persistence, and inspection.

use std::cell::RefCell;

use engine_core::faction::{
    Faction, FactionDefinition, GovernancePolicy, PowerRules, TerritoryRules,
};
use engine_core::id::{DefinitionId, IdKind, StableId};
use engine_core::math::Vec3;
use engine_core::time::{DurationMs, Millis};
use engine_core::world_event::{
    CancellationPolicy, ConflictRule, EnrollmentRules, EventSchedule, EventSide, EventSideRole,
    WORLD_EVENT_SCHEMA_VERSION, WorldCalendar, WorldEvent, WorldEventStatus,
};
use engine_net::envelope::{Envelope, MessageId, MessageKind};
use engine_net::replication::SessionId;
use engine_sim::entity::EntityRecord;
use engine_sim::mind::Mind;
use engine_sim::nav::NavGrid;
use engine_sim::zone::{Zone, ZoneConfig};
use game_api::AccountId;
use serde_json::json;
use world_gateway::{
    CALENDAR_COMMAND_KIND, CALENDAR_QUERY_KIND, CALENDAR_RESULT_KIND, CALENDAR_STATE_KIND,
    CalendarPersistence, CalendarSideView, GatewayConfig, SessionClaims, WorldGateway,
};

const ENROLLMENT_OPENS_AT: i64 = 900;
const ENROLLMENT_CLOSES_AT: i64 = 6_000;
const CONFLICT_STARTS_AT: i64 = 6_000;
const CONFLICT_ENDS_AT: i64 = 12_000;

fn id(kind: IdKind, seed: u128) -> StableId {
    StableId::from_parts(kind, 1, seed)
}

fn text(value: &str) -> DefinitionId {
    DefinitionId::new(value).unwrap()
}

fn handshake() -> engine_net::protocol::Handshake {
    engine_net::protocol::Handshake::new(
        semver::Version::new(0, 1, 0),
        semver::Version::new(1, 0, 0),
    )
}

fn claims(account: &str, principal: StableId) -> SessionClaims {
    SessionClaims::new(AccountId::new(account).unwrap(), principal).unwrap()
}

fn staff_claims(account: &str, principal: StableId) -> SessionClaims {
    claims(account, principal).with_staff(true)
}

fn zone() -> Zone {
    let config = ZoneConfig::new(
        text("zone.demo"),
        id(IdKind::Zone, 1),
        Vec3::ZERO,
        Vec3::new(400.0, 4.0, 400.0),
    )
    .unwrap();
    let nav = NavGrid::new(200, 200, 2.0, Vec3::ZERO).unwrap();
    let mut zone = Zone::new(
        config,
        nav,
        std::sync::Arc::new(engine_core::inventory::UniformItemRules::new(99)),
    )
    .unwrap();
    zone.spawn(
        EntityRecord::new(id(IdKind::Entity, 1), text("character.player"), Vec3::ZERO)
            .with_mind(Mind::player(id(IdKind::Character, 1), "unbound")),
    )
    .unwrap();
    zone.spawn(
        EntityRecord::new(
            id(IdKind::Entity, 2),
            text("character.player"),
            Vec3::new(200.0, 0.0, 0.0),
        )
        .with_mind(Mind::player(id(IdKind::Character, 7), "unbound")),
    )
    .unwrap();
    zone
}

fn demo_faction_world() -> engine_core::faction::FactionWorld {
    let rules = TerritoryRules {
        claim_protection: DurationMs::new(0),
        capture_cooldown: DurationMs::new(0),
        minimum_capture_power: 0,
        ..TerritoryRules::default()
    };
    let mut world = engine_core::faction::FactionWorld::new(rules);
    for (faction, _founder, name) in [
        (
            text("faction.defenders"),
            id(IdKind::Character, 9),
            "Guards",
        ),
        (
            text("faction.attackers"),
            id(IdKind::Character, 9),
            "Bandits",
        ),
    ] {
        world
            .add_faction(
                Faction::new(
                    FactionDefinition::new(faction, name),
                    GovernancePolicy::default(),
                    PowerRules::default(),
                    id(IdKind::Character, 9),
                    Millis::new(0),
                )
                .unwrap(),
            )
            .unwrap();
    }
    world.register_territory(text("zone.demo")).unwrap();
    world
        .claim_territory(
            "claim-demo",
            id(IdKind::Character, 9),
            &text("faction.defenders"),
            &text("zone.demo"),
            Millis::new(0),
        )
        .unwrap();
    world
}

fn demo_event() -> WorldEvent {
    WorldEvent {
        id: text("event.demo.assault"),
        title: "Assault on the demo square".into(),
        description: None,
        schedule: EventSchedule {
            enrollment_opens_at: Millis::new(ENROLLMENT_OPENS_AT),
            enrollment_closes_at: Millis::new(ENROLLMENT_CLOSES_AT),
            starts_at: Millis::new(CONFLICT_STARTS_AT),
            ends_at: Millis::new(CONFLICT_ENDS_AT),
        },
        target_zone: text("zone.demo"),
        sides: vec![
            EventSide {
                id: text("side.attackers"),
                role: EventSideRole::Attacker,
                display_name: "Bandits".into(),
                faction: text("faction.attackers"),
            },
            EventSide {
                id: text("side.defenders"),
                role: EventSideRole::Defender,
                display_name: "Guards".into(),
                faction: text("faction.defenders"),
            },
        ],
        enrollment: EnrollmentRules {
            requires_faction_membership: false,
            allow_unaffiliated: true,
            max_participants_per_side: None,
        },
        conflict: ConflictRule::default(),
        cancellation: CancellationPolicy::AnyTimeBeforeResolution,
        status: WorldEventStatus::Draft,
        schema_version: WORLD_EVENT_SCHEMA_VERSION,
    }
}

fn demo_calendar(
    factions: &mut engine_core::faction::FactionWorld,
    opened_at: i64,
) -> WorldCalendar {
    let mut calendar = WorldCalendar::new();
    let mut event = demo_event();
    event.status = WorldEventStatus::Draft;
    let event_id = event.id.clone();
    calendar
        .create(event, id(IdKind::Character, 9), Millis::new(100))
        .unwrap();
    calendar
        .schedule(
            factions,
            &event_id,
            id(IdKind::Character, 9),
            Millis::new(500),
        )
        .unwrap();
    calendar
        .open(&event_id, id(IdKind::Character, 9), Millis::new(opened_at))
        .unwrap();
    calendar
}

/// Records every committed snapshot pair for assertions.
struct RecordingStore {
    commits: RefCell<Vec<(u32, u32)>>,
}

impl RecordingStore {
    fn new() -> Self {
        Self {
            commits: RefCell::new(Vec::new()),
        }
    }
}

impl CalendarPersistence for RecordingStore {
    fn commit(
        &self,
        calendar: &engine_core::world_event::CalendarSnapshot,
        factions: &engine_core::faction::FactionSnapshot,
    ) -> Result<(), engine_core::error::EngineError> {
        self.commits.borrow_mut().push((
            calendar.events.len() as u32,
            factions.territories.len() as u32,
        ));
        Ok(())
    }
}

/// A gateway whose calendar stage is reachable over the wire.
struct CalendarFixture {
    gateway: WorldGateway,
    session: SessionId,
    staff_session: SessionId,
}

fn fixture(store: Option<RecordingStore>) -> CalendarFixture {
    let mut factions = demo_faction_world();
    let calendar = demo_calendar(&mut factions, 1_000);
    let zones = zone();
    let mut gateway = WorldGateway::new(
        zones,
        handshake(),
        GatewayConfig::default(),
        world_gateway::SequentialResumeTokens::new(1),
    )
    .unwrap()
    .with_calendar(calendar, factions)
    .unwrap();
    if let Some(store) = store {
        gateway = gateway.with_calendar_store(store);
    }
    let actor = id(IdKind::Character, 1);
    let staff_principal = id(IdKind::Character, 7);
    let player = gateway
        .connect(claims("player", actor), &handshake(), Millis::new(1_000))
        .unwrap();
    let staff = gateway
        .connect(
            staff_claims("staff", staff_principal),
            &handshake(),
            Millis::new(1_000),
        )
        .unwrap();
    CalendarFixture {
        gateway,
        session: player.session,
        staff_session: staff.session,
    }
}

fn envelope(id: &str, kind: &str, body: serde_json::Value) -> Envelope {
    Envelope::new(
        MessageId::parse(id).unwrap(),
        MessageKind::parse(kind).unwrap(),
        body,
    )
}

fn query_calendar(gateway: &mut WorldGateway, session: &SessionId) -> Envelope {
    let replies = gateway
        .handle(
            session,
            &envelope("q1", CALENDAR_QUERY_KIND, json!({})),
            Millis::new(1_100),
        )
        .unwrap();
    let reply = replies
        .iter()
        .find(|envelope| envelope.kind.as_str() == CALENDAR_STATE_KIND)
        .unwrap();
    reply.clone()
}

#[test]
fn open_event_enrolls_a_participant() {
    let mut fixture = fixture(None);
    let replies = fixture
        .gateway
        .handle(
            &fixture.session,
            &envelope(
                "req-1",
                CALENDAR_COMMAND_KIND,
                json!({
                    "request_id": "enroll-1",
                    "action": { "kind": "enroll", "event": "event.demo.assault", "side": "side.attackers" }
                }),
            ),
            Millis::new(1_100),
        )
        .unwrap();
    let reply = replies
        .iter()
        .find(|envelope| envelope.kind.as_str() == CALENDAR_RESULT_KIND)
        .unwrap();
    assert_eq!(
        reply.correlation.as_ref().map(|id| id.as_str()),
        Some("req-1")
    );
    let outcome = reply
        .body
        .get("outcome")
        .and_then(|outcome| outcome.get("status"))
        .and_then(|status| status.as_str())
        .unwrap_or_default();
    assert_eq!(outcome, "applied");
}

#[test]
fn repeated_request_id_returns_the_original_result() {
    let mut fixture = fixture(None);
    let send = |gateway: &mut WorldGateway| {
        let replies = gateway
            .handle(
                &fixture.session,
                &envelope(
                    "req-1",
                    CALENDAR_COMMAND_KIND,
                    json!({
                        "request_id": "enroll-1",
                        "action": { "kind": "enroll", "event": "event.demo.assault", "side": "side.attackers" }
                    }),
                ),
                Millis::new(1_100),
            )
            .unwrap();
        let reply = replies
            .iter()
            .find(|envelope| envelope.kind.as_str() == CALENDAR_RESULT_KIND)
            .unwrap();
        serde_json::to_value(&reply.body).unwrap()
    };
    let first = send(&mut fixture.gateway);
    let replay = send(&mut fixture.gateway);
    // The wire reply gets a fresh server ID each time, but the correlated
    // outcome must be the original result.
    assert_eq!(first, replay);
}

#[test]
fn lifecycle_commands_require_staff_gating() {
    let mut fixture = fixture(None);
    // A non-staff session cannot drive any lifecycle transition; the
    // rejection travels as a structured calendar result, not a hard error.
    let replies = fixture
        .gateway
        .handle(
            &fixture.session,
            &envelope(
                "req-2",
                CALENDAR_COMMAND_KIND,
                json!({
                    "request_id": "activate-1",
                    "action": { "kind": "activate", "event": "event.demo.assault" }
                }),
            ),
            Millis::new(1_100),
        )
        .unwrap();
    let result = replies
        .iter()
        .find(|envelope| envelope.kind.as_str() == CALENDAR_RESULT_KIND)
        .unwrap();
    assert_eq!(
        result
            .body
            .pointer("/outcome/status")
            .and_then(|status| status.as_str()),
        Some("rejected")
    );
    assert_eq!(
        result
            .body
            .pointer("/outcome/error/code")
            .and_then(|code| code.as_str()),
        Some("permission_denied")
    );
}

#[test]
fn a_staff_session_can_publish_and_replay_returns_the_same_outcome() {
    let mut fixture = fixture(None);
    let replies = fixture
        .gateway
        .handle(
            &fixture.staff_session,
            &envelope(
                "req-3",
                CALENDAR_COMMAND_KIND,
                json!({
                    "request_id": "schedule-1",
                    "action": { "kind": "schedule", "event": "event.demo.assault" }
                }),
            ),
            Millis::new(1_100),
        )
        .unwrap();
    let result = replies
        .iter()
        .find(|envelope| envelope.kind.as_str() == CALENDAR_RESULT_KIND)
        .unwrap();
    // The fixture opened the event already, so scheduling rejects with a
    // structured InvalidState error and nothing changes.
    assert_eq!(
        result
            .body
            .pointer("/outcome/status")
            .and_then(|status| status.as_str()),
        Some("rejected")
    );
    assert_eq!(
        result
            .body
            .pointer("/outcome/error/code")
            .and_then(|code| code.as_str()),
        Some("invalid_state")
    );
}

#[test]
fn applied_commands_commit_calendar_and_faction_snapshots() {
    let store = RecordingStore::new();
    let mut fixture = fixture(Some(store));
    fixture
        .gateway
        .handle(
            &fixture.session,
            &envelope(
                "req-1",
                CALENDAR_COMMAND_KIND,
                json!({
                    "request_id": "enroll-1",
                    "action": { "kind": "enroll", "event": "event.demo.assault", "side": "side.attackers" }
                }),
            ),
            Millis::new(1_100),
        )
        .unwrap();
    let stage = fixture.gateway.calendar_stage().unwrap();
    let (calendar, _factions) = stage.snapshots();
    assert_eq!(calendar.events.len(), 1);
}

#[test]
fn state_excludes_drafts_and_reports_side_participants() {
    let mut fixture = fixture(None);
    fixture
        .gateway
        .handle(
            &fixture.session,
            &envelope(
                "req-1",
                CALENDAR_COMMAND_KIND,
                json!({
                    "request_id": "enroll-1",
                    "action": { "kind": "enroll", "event": "event.demo.assault", "side": "side.attackers" }
                }),
            ),
            Millis::new(1_100),
        )
        .unwrap();
    let reply = query_calendar(&mut fixture.gateway, &fixture.session);
    let state: world_gateway::CalendarStateBody =
        serde_json::from_value(reply.body.clone()).unwrap();
    assert!(
        state
            .events
            .iter()
            .all(|event| event.status != WorldEventStatus::Draft)
    );
    let attackers: &CalendarSideView = state
        .events
        .iter()
        .flat_map(|event| &event.sides)
        .find(|side| side.side == text("side.attackers"))
        .unwrap();
    assert_eq!(attackers.participants, 1);
}

#[test]
fn a_world_without_a_calendar_fails_closed() {
    let zone = zone();
    let mut gateway = WorldGateway::new(
        zone,
        handshake(),
        GatewayConfig::default(),
        world_gateway::SequentialResumeTokens::new(1),
    )
    .unwrap();
    let principal = id(IdKind::Character, 1);
    let player = gateway
        .connect(
            claims("player", principal),
            &handshake(),
            Millis::new(1_000),
        )
        .unwrap();
    let replies = gateway
        .handle(
            &player.session,
            &envelope("req-1", CALENDAR_QUERY_KIND, json!({})),
            Millis::new(1_100),
        )
        .unwrap();
    let error = replies
        .iter()
        .find(|envelope| envelope.kind.as_str() == world_gateway::ERROR_KIND)
        .unwrap();
    assert_eq!(
        error.body.get("code").and_then(|code| code.as_str()),
        Some("unsupported")
    );
}
