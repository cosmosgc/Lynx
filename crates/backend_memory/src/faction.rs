//! Read-only faction access and faction-world persistence for the in-memory adapter.

use engine_core::DefinitionId;
use engine_core::faction::{FactionSnapshot, FactionWorld};
use game_api::{
    ApiError, FactionService, FactionStore, FactionView, SessionToken, TerritoryView,
    validate_snapshot,
};

use crate::MemoryBackend;

impl FactionService for MemoryBackend {
    fn factions(&self, token: &SessionToken) -> Result<Vec<FactionView>, ApiError> {
        let state = self.lock_state()?;
        Self::account_for_token(&state, token)?;
        Ok(state
            .faction_world
            .factions()
            .map(FactionView::from_faction)
            .collect())
    }

    fn faction(
        &self,
        token: &SessionToken,
        faction: &DefinitionId,
    ) -> Result<FactionView, ApiError> {
        let state = self.lock_state()?;
        Self::account_for_token(&state, token)?;
        state
            .faction_world
            .faction(faction)
            .map(FactionView::from_faction)
            .ok_or_else(|| ApiError::NotFound(format!("faction: {faction}")))
    }

    fn territories(&self, token: &SessionToken) -> Result<Vec<TerritoryView>, ApiError> {
        let state = self.lock_state()?;
        Self::account_for_token(&state, token)?;
        Ok(state
            .faction_world
            .territories()
            .map(TerritoryView::from_territory)
            .collect())
    }
}

impl FactionStore for MemoryBackend {
    fn load_faction_world(&self) -> Result<FactionSnapshot, ApiError> {
        let state = self.lock_state()?;
        Ok(state.faction_world.snapshot())
    }

    fn replace_faction_world(&self, snapshot: &FactionSnapshot) -> Result<(), ApiError> {
        validate_snapshot(snapshot)?;
        let world = FactionWorld::from_snapshot(snapshot.clone())
            .map_err(|error| ApiError::Conflict(error.to_string()))?;
        let mut state = self.lock_state()?;
        state.faction_world = world;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::faction::{
        Faction, FactionDefinition, FactionRole, GovernancePolicy, PowerRules, TerritoryRules,
    };
    use engine_core::time::Millis;
    use engine_core::{IdKind, StableId};
    use game_api::AuthService;

    fn backend() -> (MemoryBackend, SessionToken) {
        let backend = MemoryBackend::new();
        let session = backend
            .authenticate_device(&crate::test_support::login("faction-tests", true))
            .unwrap();
        (backend, session.token)
    }

    fn faction_id(value: &str) -> DefinitionId {
        DefinitionId::new(value).unwrap()
    }

    fn seeded_world() -> FactionSnapshot {
        let mut world = FactionWorld::new(TerritoryRules::default());
        let guards = faction_id("faction.town_guard");
        let leader = StableId::from_parts(IdKind::Character, 0, 1);
        world
            .add_faction(
                Faction::new(
                    FactionDefinition::new(guards.clone(), "Town Guard"),
                    GovernancePolicy::default(),
                    PowerRules::default(),
                    leader,
                    Millis::new(1),
                )
                .unwrap(),
            )
            .unwrap();
        world
            .deposit("seed", leader, &guards, 4_000, "city taxes", Millis::new(2))
            .unwrap();
        world
            .register_territory(faction_id("zone.town_square"))
            .unwrap();
        world
            .claim_territory(
                "claim-1",
                leader,
                &guards,
                &faction_id("zone.town_square"),
                Millis::new(3),
            )
            .unwrap();
        world.snapshot()
    }

    #[test]
    fn services_require_authentication() {
        let backend = MemoryBackend::new();
        let token = SessionToken::new("not-a-token").unwrap();
        assert_eq!(
            backend.factions(&token).unwrap_err(),
            ApiError::Unauthenticated
        );
        assert_eq!(
            backend.territories(&token).unwrap_err(),
            ApiError::Unauthenticated
        );
    }

    #[test]
    fn service_returns_views_from_the_persisted_world() {
        let (backend, token) = backend();
        backend
            .replace_faction_world(&seeded_world())
            .expect("valid snapshot");

        let factions = backend.factions(&token).unwrap();
        assert_eq!(factions.len(), 1);
        assert_eq!(factions[0].display_name, "Town Guard");
        assert_eq!(factions[0].members[0].role, FactionRole::Leader);
        assert_eq!(factions[0].treasury_balance, 4_000);
        assert_eq!(factions[0].power.power, 400);

        let territories = backend.territories(&token).unwrap();
        assert_eq!(territories.len(), 1);
        assert_eq!(territories[0].owner, Some(faction_id("faction.town_guard")));
        assert_eq!(backend.load_faction_world().unwrap(), seeded_world());
    }

    #[test]
    fn unknown_factions_are_not_found() {
        let (backend, token) = backend();
        assert_eq!(
            backend
                .faction(&token, &faction_id("faction.unknown"))
                .unwrap_err(),
            ApiError::NotFound("faction: faction.unknown".into())
        );
    }

    #[test]
    fn invalid_snapshots_are_rejected_without_replacing_state() {
        let (backend, token) = backend();
        backend.replace_faction_world(&seeded_world()).unwrap();
        let before = backend.load_faction_world().unwrap();

        let mut broken = before.clone();
        broken.territories[0].owner = Some(faction_id("faction.ghost"));
        assert_eq!(
            backend.replace_faction_world(&broken).unwrap_err().code(),
            engine_core::ErrorCode::Conflict
        );

        let mut broken = before.clone();
        broken.territories[0].zone = faction_id("faction.town_guard");
        assert_eq!(
            backend.replace_faction_world(&broken).unwrap_err().code(),
            engine_core::ErrorCode::InvalidRequest
        );

        assert_eq!(backend.load_faction_world().unwrap(), before);
        assert_eq!(backend.factions(&token).unwrap().len(), 1);
    }
}
