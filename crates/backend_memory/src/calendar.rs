//! Read-only calendar access and calendar persistence for the in-memory adapter.

use engine_core::DefinitionId;
use engine_core::world_event::{CalendarSnapshot, WorldCalendar, WorldEvent};
use game_api::{
    ApiError, CalendarService, CalendarStore, ConflictResolutionView, EventEnrollmentView,
    SessionToken, WorldEventView, validate_calendar_snapshot,
};

use crate::MemoryBackend;

impl CalendarService for MemoryBackend {
    fn events(&self, token: &SessionToken) -> Result<Vec<WorldEventView>, ApiError> {
        let state = self.lock_state()?;
        Self::account_for_token(&state, token)?;
        Ok(state
            .calendar
            .public_events()
            .into_iter()
            .map(|event| WorldEventView::from_event(event, &state.calendar))
            .collect())
    }

    fn event(
        &self,
        token: &SessionToken,
        event: &DefinitionId,
    ) -> Result<WorldEventView, ApiError> {
        let state = self.lock_state()?;
        Self::account_for_token(&state, token)?;
        let definition = published_event(&state.calendar, event)?;
        Ok(WorldEventView::from_event(definition, &state.calendar))
    }

    fn enrollments(
        &self,
        token: &SessionToken,
        event: &DefinitionId,
    ) -> Result<Vec<EventEnrollmentView>, ApiError> {
        let state = self.lock_state()?;
        Self::account_for_token(&state, token)?;
        published_event(&state.calendar, event)?;
        Ok(state
            .calendar
            .enrollments(event)
            .map(EventEnrollmentView::from_enrollment)
            .collect())
    }

    fn resolution(
        &self,
        token: &SessionToken,
        event: &DefinitionId,
    ) -> Result<Option<ConflictResolutionView>, ApiError> {
        let state = self.lock_state()?;
        Self::account_for_token(&state, token)?;
        published_event(&state.calendar, event)?;
        Ok(state
            .calendar
            .resolution(event)
            .map(ConflictResolutionView::from_resolution))
    }
}

impl CalendarStore for MemoryBackend {
    fn load_calendar(&self) -> Result<CalendarSnapshot, ApiError> {
        let state = self.lock_state()?;
        Ok(state.calendar.snapshot())
    }

    fn replace_calendar(&self, snapshot: &CalendarSnapshot) -> Result<(), ApiError> {
        let mut state = self.lock_state()?;
        validate_calendar_snapshot(snapshot, &state.faction_world.snapshot())?;
        let calendar = WorldCalendar::from_snapshot(snapshot.clone())
            .map_err(|error| ApiError::Conflict(error.to_string()))?;
        state.calendar = calendar;
        Ok(())
    }
}

/// Look up a published event, treating drafts as absent.
fn published_event<'a>(
    calendar: &'a WorldCalendar,
    event: &DefinitionId,
) -> Result<&'a WorldEvent, ApiError> {
    calendar
        .event(event)
        .filter(|definition| definition.status.is_public())
        .ok_or_else(|| ApiError::NotFound(format!("event: {event}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::faction::{
        Faction, FactionDefinition, FactionRole, FactionWorld, GovernancePolicy, PowerRules,
        TerritoryRules,
    };
    use engine_core::time::{DurationMs, Millis};
    use engine_core::world_event::{
        CancellationPolicy, ConflictOutcome, ConflictRule, EnrollmentRules, EventSchedule,
        EventSide, EventSideRole, WORLD_EVENT_SCHEMA_VERSION, WorldEventStatus,
    };
    use engine_core::{IdKind, StableId};
    use game_api::{AuthService, FactionStore};

    fn backend() -> (MemoryBackend, SessionToken) {
        let backend = MemoryBackend::new();
        let session = backend
            .authenticate_device(&crate::test_support::login("calendar-tests", true))
            .unwrap();
        (backend, session.token)
    }

    fn id(value: &str) -> DefinitionId {
        DefinitionId::new(value).unwrap()
    }

    fn actor(seed: u128) -> StableId {
        StableId::from_parts(IdKind::Character, 0, seed)
    }

    fn event() -> WorldEvent {
        WorldEvent {
            id: id("event.siege"),
            title: "Siege of the square".into(),
            description: Some("Bandits test the town guard.".into()),
            schedule: EventSchedule {
                enrollment_opens_at: Millis::new(10),
                enrollment_closes_at: Millis::new(20),
                starts_at: Millis::new(20),
                ends_at: Millis::new(30),
            },
            target_zone: id("zone.town_square"),
            sides: vec![
                EventSide {
                    id: id("side.attackers"),
                    role: EventSideRole::Attacker,
                    display_name: "Bandits".into(),
                    faction: id("faction.bandits"),
                },
                EventSide {
                    id: id("side.defenders"),
                    role: EventSideRole::Defender,
                    display_name: "Town Guard".into(),
                    faction: id("faction.town_guard"),
                },
            ],
            enrollment: EnrollmentRules::default(),
            conflict: ConflictRule::default(),
            cancellation: CancellationPolicy::AnyTimeBeforeResolution,
            status: WorldEventStatus::Draft,
            schema_version: WORLD_EVENT_SCHEMA_VERSION,
        }
    }

    fn seed_world(backend: &MemoryBackend) -> FactionWorld {
        let mut world = FactionWorld::new(TerritoryRules {
            claim_protection: DurationMs::new(0),
            capture_cooldown: DurationMs::new(0),
            minimum_capture_power: 0,
            ..TerritoryRules::default()
        });
        let guards = id("faction.town_guard");
        let bandits = id("faction.bandits");
        for (faction, founder, name) in [
            (guards.clone(), actor(1), "Town Guard"),
            (bandits.clone(), actor(2), "Bandits"),
        ] {
            world
                .add_faction(
                    Faction::new(
                        FactionDefinition::new(faction, name),
                        GovernancePolicy::default(),
                        PowerRules::default(),
                        founder,
                        Millis::new(1),
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        world.register_territory(id("zone.town_square")).unwrap();
        world
            .claim_territory(
                "claim-1",
                actor(1),
                &guards,
                &id("zone.town_square"),
                Millis::new(3),
            )
            .unwrap();
        world
            .invite(
                actor(2),
                &bandits,
                actor(3),
                FactionRole::Member,
                Millis::new(3),
            )
            .unwrap();
        backend.replace_faction_world(&world.snapshot()).unwrap();
        world
    }

    fn scheduled(world: &FactionWorld) -> WorldCalendar {
        let mut calendar = WorldCalendar::new();
        let definition = event();
        let event = definition.id.clone();
        calendar
            .create(definition, actor(9), Millis::new(1))
            .unwrap();
        calendar
            .schedule(world, &event, actor(9), Millis::new(2))
            .unwrap();
        calendar
    }

    #[test]
    fn services_require_authentication() {
        let backend = MemoryBackend::new();
        let token = SessionToken::new("not-a-token").unwrap();
        assert_eq!(
            backend.events(&token).unwrap_err(),
            ApiError::Unauthenticated
        );
        assert_eq!(
            backend.event(&token, &id("event.siege")).unwrap_err(),
            ApiError::Unauthenticated
        );
        assert_eq!(
            backend.enrollments(&token, &id("event.siege")).unwrap_err(),
            ApiError::Unauthenticated
        );
        assert_eq!(
            backend.resolution(&token, &id("event.siege")).unwrap_err(),
            ApiError::Unauthenticated
        );
    }

    #[test]
    fn service_returns_public_calendar_and_result() {
        let (backend, token) = backend();
        let mut world = seed_world(&backend);
        let mut calendar = scheduled(&world);
        let event_id = id("event.siege");
        calendar.open(&event_id, actor(9), Millis::new(10)).unwrap();
        calendar
            .enroll(
                &world,
                &event_id,
                actor(2),
                &id("side.attackers"),
                Millis::new(11),
            )
            .unwrap();
        calendar
            .enroll(
                &world,
                &event_id,
                actor(3),
                &id("side.attackers"),
                Millis::new(11),
            )
            .unwrap();
        calendar
            .enroll(
                &world,
                &event_id,
                actor(1),
                &id("side.defenders"),
                Millis::new(11),
            )
            .unwrap();
        calendar
            .activate(&mut world, &event_id, actor(9), Millis::new(20))
            .unwrap();
        calendar
            .resolve(&mut world, &event_id, actor(9), Millis::new(30))
            .unwrap();
        backend.replace_faction_world(&world.snapshot()).unwrap();
        backend.replace_calendar(&calendar.snapshot()).unwrap();

        let events = backend.events(&token).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].title, "Siege of the square");
        assert_eq!(events[0].status, WorldEventStatus::Resolved);
        assert_eq!(events[0].sides[0].participants, 2);
        assert_eq!(events[0].sides[1].participants, 1);

        let enrollments = backend.enrollments(&token, &event_id).unwrap();
        assert_eq!(enrollments.len(), 3);

        let resolution = backend
            .resolution(&token, &event_id)
            .unwrap()
            .expect("resolved event");
        assert_eq!(resolution.outcome, ConflictOutcome::AttackerVictory);
        assert_eq!(
            resolution.territory_change.as_ref().unwrap().to,
            Some(id("faction.bandits"))
        );
        assert_eq!(backend.load_calendar().unwrap(), calendar.snapshot());
    }

    #[test]
    fn drafts_are_hidden_from_the_public_calendar() {
        let (backend, token) = backend();
        let _world = seed_world(&backend);
        let mut calendar = WorldCalendar::new();
        calendar.create(event(), actor(9), Millis::new(1)).unwrap();
        backend.replace_calendar(&calendar.snapshot()).unwrap();

        assert_eq!(backend.events(&token).unwrap().len(), 0);
        assert_eq!(
            backend.event(&token, &id("event.siege")).unwrap_err(),
            ApiError::NotFound("event: event.siege".into())
        );
    }

    #[test]
    fn calendar_and_factions_cannot_disagree_after_replacement() {
        let (backend, token) = backend();
        let world = seed_world(&backend);
        let calendar = scheduled(&world);
        backend.replace_calendar(&calendar.snapshot()).unwrap();
        let before = backend.load_calendar().unwrap();

        let mut broken = before.clone();
        broken.events[0].sides[1].faction = id("faction.guard");
        assert_eq!(
            backend.replace_calendar(&broken).unwrap_err().code(),
            engine_core::ErrorCode::NotFound
        );

        let mut broken = before.clone();
        broken.transitions.clear();
        assert_eq!(
            backend.replace_calendar(&broken).unwrap_err().code(),
            engine_core::ErrorCode::Conflict
        );

        assert_eq!(backend.load_calendar().unwrap(), before);
        assert_eq!(backend.events(&token).unwrap().len(), 1);
    }

    #[test]
    fn replacing_factions_rechecks_the_calendar() {
        let (backend, _) = backend();
        let mut world = seed_world(&backend);
        let calendar = scheduled(&world);
        backend.replace_calendar(&calendar.snapshot()).unwrap();

        let zone = id("zone.town_square");
        world
            .release_territory(
                "release-1",
                actor(1),
                &id("faction.town_guard"),
                &zone,
                Millis::new(4),
            )
            .unwrap();
        world
            .claim_territory(
                "claim-2",
                actor(2),
                &id("faction.bandits"),
                &zone,
                Millis::new(5),
            )
            .unwrap();

        assert_eq!(
            backend
                .replace_faction_world(&world.snapshot())
                .unwrap_err()
                .code(),
            engine_core::ErrorCode::Conflict
        );
        assert!(
            backend
                .replace_calendar(&backend.load_calendar().unwrap())
                .is_ok()
        );
    }

    #[test]
    fn replace_world_state_commits_a_consistent_pair_together() {
        let backend = MemoryBackend::new();
        let world = seed_world(&backend);
        let calendar = scheduled(&world);

        backend
            .replace_world_state(&calendar.snapshot(), &world.snapshot())
            .unwrap();
        assert_eq!(backend.load_calendar().unwrap(), calendar.snapshot());
        assert_eq!(backend.load_faction_world().unwrap(), world.snapshot());
    }

    #[test]
    fn replace_world_state_rejects_a_mismatched_pair() {
        let backend = MemoryBackend::new();
        let world = seed_world(&backend);
        let mut world_copy = world.clone();
        let mut calendar = scheduled(&world);
        let event_id = id("event.siege");
        calendar.open(&event_id, actor(9), Millis::new(10)).unwrap();
        calendar
            .activate(&mut world_copy, &event_id, actor(9), Millis::new(20))
            .unwrap();

        // The activated calendar demands a contested target, but the supplied
        // faction snapshot does not carry the contest, so the pair is
        // rejected and persisted state stays untouched.
        assert!(matches!(
            backend.replace_world_state(&calendar.snapshot(), &world.snapshot()),
            Err(ApiError::Conflict(_))
        ));
        assert_eq!(backend.load_calendar().unwrap().resolutions, Vec::new());
    }

    #[test]
    fn replace_world_state_accepts_a_resolved_pair() {
        let backend = MemoryBackend::new();
        let mut world = seed_world(&backend);
        let mut calendar = scheduled(&world);
        let event_id = id("event.siege");
        calendar.open(&event_id, actor(9), Millis::new(10)).unwrap();
        calendar
            .activate(&mut world, &event_id, actor(9), Millis::new(20))
            .unwrap();
        calendar
            .resolve(&mut world, &event_id, actor(9), Millis::new(30))
            .unwrap();

        // Attacker victory captured the target, so calendar and territory
        // history must move at once.
        backend
            .replace_world_state(&calendar.snapshot(), &world.snapshot())
            .unwrap();
        assert_eq!(backend.load_calendar().unwrap(), calendar.snapshot());
    }
}
