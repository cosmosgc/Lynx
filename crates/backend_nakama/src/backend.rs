//! Production composition of Nakama authentication and Lynx PostgreSQL state.
//!
//! Nakama validates credentials and supplies a user identity. This adapter
//! provisions/resolves the durable Lynx account before passing only that account
//! to the account-scoped character repository. No Nakama type reaches gameplay.

use std::collections::BTreeSet;
use std::fmt;

use engine_core::faction::FactionSnapshot;
use engine_core::id::{IdGenerator, SystemIdGenerator};
use engine_core::world_event::{CalendarSnapshot, WorldCalendar};
use engine_core::{DefinitionId, IdKind};
use game_api::{
    Account, ApiError, AuthService, CalendarService, Character, CharacterCreationKey,
    CharacterCreationReceipt, CharacterId, CharacterIntentService, CharacterService,
    CharacterWorldService, ChatChannel, ChatChannelTarget, ChatChannelTargetPage, ChatMessagePage,
    ChatMessageReceipt, ChatRealtimeEvent, ConflictResolutionView, CreateCharacter, CreateGroup,
    DeviceLogin, EventEnrollmentView, FactionService, FactionView, Friend, GroupId, GroupMember,
    GroupMembership, GroupPage, GroupSearch, GroupService, HealthService, HealthStatus,
    InventoryService, InventorySlot, RealtimeChatService, RealtimeChatSession,
    RealtimePartyService, Session, SessionToken, SocialGroup, SocialService, TerritoryView,
    WorldEventView,
};

use crate::{
    NakamaClient, PostgresCharacterStore, PostgresFactionCalendarStore, PostgresIdentityStore,
    PostgresInventoryStore,
};

/// Production adapter joining Nakama sessions to Lynx-owned game data.
pub struct NakamaPostgresBackend {
    nakama: NakamaClient,
    identities: PostgresIdentityStore,
    characters: PostgresCharacterStore,
    inventories: PostgresInventoryStore,
    faction_calendar: PostgresFactionCalendarStore,
    realtime_chat_rooms: BTreeSet<String>,
}

impl fmt::Debug for NakamaPostgresBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NakamaPostgresBackend")
            .field("nakama", &self.nakama)
            .field("identities", &self.identities)
            .field("characters", &self.characters)
            .field("inventories", &self.inventories)
            .field("faction_calendar", &self.faction_calendar)
            .finish()
    }
}

impl NakamaPostgresBackend {
    /// Snapshot each measured underlying adapter independently. Concurrent
    /// updates may occur between reads; each individual snapshot is coherent.
    #[must_use]
    pub fn telemetry_snapshot(&self) -> BackendTelemetrySnapshot {
        BackendTelemetrySnapshot {
            nakama: self.nakama.telemetry().snapshot(),
            identity_store: self.identities.telemetry().snapshot(),
            character_store: self.characters.telemetry().snapshot(),
        }
    }

    /// Open a long-lived Nakama realtime session for a validated player token.
    /// The returned socket is separate from world admission; dropping it ends
    /// Nakama party membership without changing Lynx world state.
    pub async fn connect_realtime(
        &self,
        token: &SessionToken,
    ) -> Result<crate::realtime::NakamaRealtimeSession, ApiError> {
        self.account_for_token(token)?;
        self.nakama.connect_realtime(token).await
    }

    /// Compose established Nakama and PostgreSQL adapters.
    #[must_use]
    pub fn new(
        nakama: NakamaClient,
        identities: PostgresIdentityStore,
        characters: PostgresCharacterStore,
        inventories: PostgresInventoryStore,
        faction_calendar: PostgresFactionCalendarStore,
    ) -> Self {
        Self {
            nakama,
            identities,
            characters,
            inventories,
            faction_calendar,
            realtime_chat_rooms: BTreeSet::new(),
        }
    }

    /// Allow named room targets for application chat sessions. The default
    /// backend has no room targets; direct and group targets are account-checked.
    pub fn with_realtime_chat_rooms<I, S>(mut self, rooms: I) -> Result<Self, ApiError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let rooms = rooms
            .into_iter()
            .map(Into::into)
            .map(|name| {
                let target = ChatChannelTarget::Room(name.clone());
                target.validate()?;
                Ok(name)
            })
            .collect::<Result<BTreeSet<_>, ApiError>>()?;
        if rooms.len() > 256 {
            return Err(ApiError::InvalidRequest(
                "too many configured Nakama chat rooms".into(),
            ));
        }
        self.realtime_chat_rooms = rooms;
        Ok(self)
    }

    fn faction_snapshot(&self) -> Result<FactionSnapshot, ApiError> {
        self.faction_calendar
            .load_world_state()
            .map(|state| state.map_or_else(FactionSnapshot::default, |(_, factions)| factions))
    }

    fn world_calendar(&self) -> Result<WorldCalendar, ApiError> {
        let snapshot = self
            .faction_calendar
            .load_world_state()?
            .map_or_else(CalendarSnapshot::default, |(calendar, _)| calendar);
        WorldCalendar::from_snapshot(snapshot)
            .map_err(|_| ApiError::Unavailable("stored calendar is invalid".into()))
    }

    fn account_for_token(&self, token: &SessionToken) -> Result<game_api::AccountId, ApiError> {
        let account = self.nakama.account(token)?;
        self.identities
            .provision_account(account.id.as_str(), &account.locale)
    }

    fn provision_session(&self, mut session: Session) -> Result<Session, ApiError> {
        let account = self.account_for_token(&session.token)?;
        session.account_id = account;
        Ok(session)
    }

    fn legacy_creation_key() -> Result<CharacterCreationKey, ApiError> {
        let id = SystemIdGenerator::new().generate(IdKind::Item);
        CharacterCreationKey::new(format!("legacy-{id}"))
    }

    /// Check whether a trusted, already-authenticated Lynx account owns an
    /// active character. This is deliberately not exposed as a client API:
    /// callers must obtain `account` from a validated Nakama session.
    pub fn owns_active_character(
        &self,
        account: &game_api::AccountId,
        character: CharacterId,
    ) -> Result<bool, ApiError> {
        match self.characters.select_character(account, character) {
            Ok(_) => Ok(true),
            Err(ApiError::NotFound(_) | ApiError::PermissionDenied) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

impl RealtimeChatService for NakamaPostgresBackend {
    type Session = AuthorizedNakamaChatSession;

    fn discover_channels(
        &self,
        token: &SessionToken,
        limit: u16,
        cursor: Option<&str>,
    ) -> Result<ChatChannelTargetPage, ApiError> {
        game_api::validate_group_page(limit, cursor)?;
        let friends = SocialService::friends(self, token)?;
        let groups = GroupService::my_groups(self, token, limit, cursor)?;
        let mut targets = BTreeSet::new();
        targets.extend(
            friends
                .into_iter()
                .filter(|friend| friend.state == game_api::FriendState::Friend)
                .map(|friend| ChatChannelTarget::Direct(friend.account_id)),
        );
        targets.extend(
            groups
                .items
                .into_iter()
                .filter(|membership| {
                    matches!(
                        membership.role,
                        game_api::GroupRole::Owner
                            | game_api::GroupRole::Admin
                            | game_api::GroupRole::Member
                    )
                })
                .map(|membership| ChatChannelTarget::Group(membership.group.id)),
        );
        Ok(ChatChannelTargetPage {
            targets: targets.into_iter().collect(),
            next_cursor: groups.cursor,
        })
    }

    async fn connect_chat(&self, token: &SessionToken) -> Result<Self::Session, ApiError> {
        Ok(AuthorizedNakamaChatSession {
            session: self.connect_realtime(token).await?,
            nakama: self.nakama.clone(),
            token: token.clone(),
            allowed_rooms: self.realtime_chat_rooms.clone(),
        })
    }
}

impl RealtimePartyService for NakamaPostgresBackend {
    type Session = crate::realtime::NakamaRealtimeSession;

    async fn connect_party(&self, token: &SessionToken) -> Result<Self::Session, ApiError> {
        self.connect_realtime(token).await
    }
}

/// Chat session that only joins direct channels currently listed as mutual
/// friendships. Nakama independently enforces group
/// membership. Configured room names remain deployment policy.
pub struct AuthorizedNakamaChatSession {
    session: crate::realtime::NakamaRealtimeSession,
    nakama: NakamaClient,
    token: SessionToken,
    allowed_rooms: BTreeSet<String>,
}

impl fmt::Debug for AuthorizedNakamaChatSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthorizedNakamaChatSession")
            .field("session", &self.session)
            .field("nakama", &self.nakama)
            .field("token", &self.token)
            .field("allowed_room_count", &self.allowed_rooms.len())
            .finish()
    }
}

impl RealtimeChatSession for AuthorizedNakamaChatSession {
    async fn join_channel(
        &mut self,
        target: &ChatChannelTarget,
        persistence: bool,
        hidden: bool,
    ) -> Result<ChatChannel, ApiError> {
        target.validate()?;
        if let ChatChannelTarget::Room(name) = target
            && !self.allowed_rooms.contains(name)
        {
            return Err(ApiError::PermissionDenied);
        }
        if let ChatChannelTarget::Direct(account) = target {
            let mutual_friend = SocialService::friends(&self.nakama, &self.token)?
                .into_iter()
                .any(|friend| {
                    friend.account_id == *account && friend.state == game_api::FriendState::Friend
                });
            if !mutual_friend {
                return Err(ApiError::PermissionDenied);
            }
        }
        RealtimeChatSession::join_channel(&mut self.session, target, persistence, hidden).await
    }

    async fn leave_channel(&mut self, channel_id: &str) -> Result<(), ApiError> {
        RealtimeChatSession::leave_channel(&mut self.session, channel_id).await
    }

    async fn send_realtime_message(
        &mut self,
        channel_id: &str,
        content: &str,
    ) -> Result<ChatMessageReceipt, ApiError> {
        RealtimeChatSession::send_realtime_message(&mut self.session, channel_id, content).await
    }

    async fn realtime_history(
        &mut self,
        channel_id: &str,
        limit: u16,
        forward: bool,
        cursor: Option<&str>,
    ) -> Result<ChatMessagePage, ApiError> {
        RealtimeChatSession::realtime_history(&mut self.session, channel_id, limit, forward, cursor)
            .await
    }

    async fn next_chat_event(&mut self) -> Result<ChatRealtimeEvent, ApiError> {
        RealtimeChatSession::next_chat_event(&mut self.session).await
    }
}

/// Aggregate measurements by fixed adapter, with no user-supplied labels.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BackendTelemetrySnapshot {
    /// Nakama client operations.
    pub nakama: crate::TelemetrySnapshot,
    /// PostgreSQL identity repository operations.
    pub identity_store: crate::TelemetrySnapshot,
    /// PostgreSQL character repository operations.
    pub character_store: crate::TelemetrySnapshot,
}

impl HealthService for NakamaPostgresBackend {
    fn health(&self) -> Result<HealthStatus, ApiError> {
        self.nakama.health()
    }
}

impl AuthService for NakamaPostgresBackend {
    fn authenticate_device(&self, request: &DeviceLogin) -> Result<Session, ApiError> {
        self.provision_session(self.nakama.authenticate_device(request)?)
    }

    fn refresh_session(&self, refresh_token: &SessionToken) -> Result<Session, ApiError> {
        self.provision_session(self.nakama.refresh_session(refresh_token)?)
    }

    fn account(&self, token: &SessionToken) -> Result<Account, ApiError> {
        let mut account = self.nakama.account(token)?;
        account.id = self
            .identities
            .provision_account(account.id.as_str(), &account.locale)?;
        Ok(account)
    }

    fn logout(&self, token: &SessionToken) -> Result<(), ApiError> {
        self.nakama.logout(token)
    }
}

impl SocialService for NakamaPostgresBackend {
    fn friends(&self, token: &SessionToken) -> Result<Vec<Friend>, ApiError> {
        self.account_for_token(token)?;
        self.nakama.friends(token)
    }

    fn request_friend(
        &self,
        token: &SessionToken,
        friend: &game_api::AccountId,
    ) -> Result<(), ApiError> {
        self.account_for_token(token)?;
        self.nakama.request_friend(token, friend)
    }

    fn accept_friend_request(
        &self,
        token: &SessionToken,
        friend: &game_api::AccountId,
    ) -> Result<(), ApiError> {
        self.account_for_token(token)?;
        self.nakama.accept_friend_request(token, friend)
    }

    fn dismiss_friend_request(
        &self,
        token: &SessionToken,
        friend: &game_api::AccountId,
    ) -> Result<(), ApiError> {
        self.account_for_token(token)?;
        self.nakama.dismiss_friend_request(token, friend)
    }

    fn remove_friend(
        &self,
        token: &SessionToken,
        friend: &game_api::AccountId,
    ) -> Result<(), ApiError> {
        self.account_for_token(token)?;
        self.nakama.remove_friend(token, friend)
    }

    fn block_account(
        &self,
        token: &SessionToken,
        account: &game_api::AccountId,
    ) -> Result<(), ApiError> {
        self.account_for_token(token)?;
        self.nakama.block_account(token, account)
    }

    fn unblock_account(
        &self,
        token: &SessionToken,
        account: &game_api::AccountId,
    ) -> Result<(), ApiError> {
        self.account_for_token(token)?;
        self.nakama.unblock_account(token, account)
    }
}

impl GroupService for NakamaPostgresBackend {
    fn discover_groups(
        &self,
        token: &SessionToken,
        search: &GroupSearch,
        limit: u16,
        cursor: Option<&str>,
    ) -> Result<GroupPage<SocialGroup>, ApiError> {
        self.account_for_token(token)?;
        self.nakama.discover_groups(token, search, limit, cursor)
    }

    fn my_groups(
        &self,
        token: &SessionToken,
        limit: u16,
        cursor: Option<&str>,
    ) -> Result<GroupPage<GroupMembership>, ApiError> {
        self.account_for_token(token)?;
        self.nakama.my_groups(token, limit, cursor)
    }

    fn group_members(
        &self,
        token: &SessionToken,
        group: &GroupId,
        limit: u16,
        cursor: Option<&str>,
    ) -> Result<GroupPage<GroupMember>, ApiError> {
        self.account_for_token(token)?;
        self.nakama.group_members(token, group, limit, cursor)
    }

    fn create_group(
        &self,
        token: &SessionToken,
        request: &CreateGroup,
    ) -> Result<SocialGroup, ApiError> {
        self.account_for_token(token)?;
        self.nakama.create_group(token, request)
    }

    fn request_join(&self, token: &SessionToken, group: &GroupId) -> Result<(), ApiError> {
        self.account_for_token(token)?;
        self.nakama.request_join(token, group)
    }

    fn leave_group(&self, token: &SessionToken, group: &GroupId) -> Result<(), ApiError> {
        self.account_for_token(token)?;
        self.nakama.leave_group(token, group)
    }

    fn add_members(
        &self,
        token: &SessionToken,
        group: &GroupId,
        accounts: &[game_api::AccountId],
    ) -> Result<(), ApiError> {
        self.account_for_token(token)?;
        self.nakama.add_members(token, group, accounts)
    }

    fn kick_members(
        &self,
        token: &SessionToken,
        group: &GroupId,
        accounts: &[game_api::AccountId],
    ) -> Result<(), ApiError> {
        self.account_for_token(token)?;
        self.nakama.kick_members(token, group, accounts)
    }

    fn promote_members(
        &self,
        token: &SessionToken,
        group: &GroupId,
        accounts: &[game_api::AccountId],
    ) -> Result<(), ApiError> {
        self.account_for_token(token)?;
        self.nakama.promote_members(token, group, accounts)
    }

    fn demote_members(
        &self,
        token: &SessionToken,
        group: &GroupId,
        accounts: &[game_api::AccountId],
    ) -> Result<(), ApiError> {
        self.account_for_token(token)?;
        self.nakama.demote_members(token, group, accounts)
    }
}

impl CharacterService for NakamaPostgresBackend {
    fn characters(&self, token: &SessionToken) -> Result<Vec<Character>, ApiError> {
        self.characters.characters(&self.account_for_token(token)?)
    }

    fn create_character(
        &self,
        token: &SessionToken,
        request: &CreateCharacter,
    ) -> Result<Character, ApiError> {
        let account = self.account_for_token(token)?;
        let key = Self::legacy_creation_key()?;
        self.characters
            .create_character(&account, key.as_str(), request)
    }

    fn delete_character(
        &self,
        token: &SessionToken,
        character_id: CharacterId,
    ) -> Result<(), ApiError> {
        self.characters
            .delete_character(&self.account_for_token(token)?, character_id)
    }
}

impl CharacterIntentService for NakamaPostgresBackend {
    fn create_character_idempotent(
        &self,
        token: &SessionToken,
        key: &CharacterCreationKey,
        request: &CreateCharacter,
    ) -> Result<CharacterCreationReceipt, ApiError> {
        let character = self.characters.create_character(
            &self.account_for_token(token)?,
            key.as_str(),
            request,
        )?;
        Ok(CharacterCreationReceipt {
            character_id: character.id,
        })
    }

    fn select_character(
        &self,
        token: &SessionToken,
        character_id: CharacterId,
    ) -> Result<Character, ApiError> {
        self.characters
            .select_character(&self.account_for_token(token)?, character_id)
    }
}

impl InventoryService for NakamaPostgresBackend {
    fn inventory(
        &self,
        token: &SessionToken,
        character_id: CharacterId,
    ) -> Result<Vec<InventorySlot>, ApiError> {
        let account = self.account_for_token(token)?;
        // Ownership and active status are part of the inventory query itself;
        // foreign, deleted, suspended, and missing characters are not disclosed.
        self.inventories
            .load_inventory_for_account(&account, character_id)
    }
}

impl CharacterWorldService for NakamaPostgresBackend {
    fn enter_character(
        &self,
        token: &SessionToken,
        character_id: CharacterId,
        zone_id: &str,
    ) -> Result<Character, ApiError> {
        self.characters
            .enter_character(&self.account_for_token(token)?, character_id, zone_id)
    }

    fn leave_character(
        &self,
        token: &SessionToken,
        character_id: CharacterId,
    ) -> Result<Character, ApiError> {
        self.characters
            .leave_character(&self.account_for_token(token)?, character_id)
    }
}

impl FactionService for NakamaPostgresBackend {
    fn factions(&self, token: &SessionToken) -> Result<Vec<FactionView>, ApiError> {
        self.account_for_token(token)?;
        Ok(faction_views(&self.faction_snapshot()?))
    }

    fn faction(
        &self,
        token: &SessionToken,
        faction: &DefinitionId,
    ) -> Result<FactionView, ApiError> {
        self.account_for_token(token)?;
        let snapshot = self.faction_snapshot()?;
        snapshot
            .factions
            .iter()
            .find(|value| value.definition.id == *faction)
            .map(FactionView::from_faction)
            .ok_or_else(|| ApiError::NotFound(format!("faction: {faction}")))
    }

    fn territories(&self, token: &SessionToken) -> Result<Vec<TerritoryView>, ApiError> {
        self.account_for_token(token)?;
        Ok(territory_views(&self.faction_snapshot()?))
    }
}

impl CalendarService for NakamaPostgresBackend {
    fn events(&self, token: &SessionToken) -> Result<Vec<WorldEventView>, ApiError> {
        self.account_for_token(token)?;
        Ok(world_event_views(&self.world_calendar()?))
    }

    fn event(
        &self,
        token: &SessionToken,
        event: &DefinitionId,
    ) -> Result<WorldEventView, ApiError> {
        self.account_for_token(token)?;
        let calendar = self.world_calendar()?;
        let event = published_event(&calendar, event)?;
        Ok(WorldEventView::from_event(event, &calendar))
    }

    fn enrollments(
        &self,
        token: &SessionToken,
        event: &DefinitionId,
    ) -> Result<Vec<EventEnrollmentView>, ApiError> {
        self.account_for_token(token)?;
        let calendar = self.world_calendar()?;
        published_event(&calendar, event)?;
        Ok(calendar
            .enrollments(event)
            .map(EventEnrollmentView::from_enrollment)
            .collect())
    }

    fn resolution(
        &self,
        token: &SessionToken,
        event: &DefinitionId,
    ) -> Result<Option<ConflictResolutionView>, ApiError> {
        self.account_for_token(token)?;
        let calendar = self.world_calendar()?;
        published_event(&calendar, event)?;
        Ok(calendar
            .resolution(event)
            .map(ConflictResolutionView::from_resolution))
    }
}

fn faction_views(snapshot: &FactionSnapshot) -> Vec<FactionView> {
    snapshot
        .factions
        .iter()
        .map(FactionView::from_faction)
        .collect()
}

fn territory_views(snapshot: &FactionSnapshot) -> Vec<TerritoryView> {
    snapshot
        .territories
        .iter()
        .map(TerritoryView::from_territory)
        .collect()
}

fn world_event_views(calendar: &WorldCalendar) -> Vec<WorldEventView> {
    calendar
        .public_events()
        .into_iter()
        .map(|event| WorldEventView::from_event(event, calendar))
        .collect()
}

fn published_event<'a>(
    calendar: &'a WorldCalendar,
    event: &DefinitionId,
) -> Result<&'a engine_core::world_event::WorldEvent, ApiError> {
    calendar
        .event(event)
        .filter(|definition| definition.status.is_public())
        .ok_or_else(|| ApiError::NotFound(format!("event: {event}")))
}

#[cfg_attr(coverage_nightly, coverage(off))]
#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::faction::{
        Faction, FactionDefinition, FactionWorld, GovernancePolicy, PowerRules, TerritoryRules,
    };
    use engine_core::time::{DurationMs, Millis};
    use engine_core::world_event::{
        CancellationPolicy, ConflictRule, EnrollmentRules, EventSchedule, EventSide, EventSideRole,
        WORLD_EVENT_SCHEMA_VERSION, WorldEvent,
    };
    use engine_core::{IdKind, StableId};

    fn id(value: &str) -> DefinitionId {
        DefinitionId::new(value).expect("valid definition ID")
    }

    fn actor(seed: u128) -> StableId {
        StableId::from_parts(IdKind::Character, 0, seed)
    }

    fn seeded_world() -> FactionWorld {
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
            (bandits, actor(2), "Bandits"),
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
                    .expect("valid faction"),
                )
                .expect("add faction");
        }
        world
            .register_territory(id("zone.town_square"))
            .expect("register territory");
        world
            .claim_territory(
                "claim-1",
                actor(1),
                &guards,
                &id("zone.town_square"),
                Millis::new(3),
            )
            .expect("claim territory");
        world
    }

    fn draft_event() -> WorldEvent {
        WorldEvent {
            id: id("event.siege"),
            title: "Siege of the square".into(),
            description: None,
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
            status: engine_core::world_event::WorldEventStatus::Draft,
            schema_version: WORLD_EVENT_SCHEMA_VERSION,
        }
    }

    #[test]
    fn faction_views_preserve_order_and_territory_ownership() {
        let snapshot = seeded_world().snapshot();
        let factions = faction_views(&snapshot);
        assert_eq!(factions.len(), 2);
        assert_eq!(factions[0].id, id("faction.bandits"));
        assert_eq!(factions[1].id, id("faction.town_guard"));
        assert_eq!(factions[1].members.len(), 1);

        let territories = territory_views(&snapshot);
        assert_eq!(territories.len(), 1);
        assert_eq!(territories[0].owner, Some(id("faction.town_guard")));
    }

    #[test]
    fn draft_events_are_hidden_until_published() {
        let world = seeded_world();
        let mut calendar = WorldCalendar::new();
        let definition = draft_event();
        let event_id = definition.id.clone();
        calendar
            .create(definition, actor(9), Millis::new(1))
            .expect("create draft");

        assert_eq!(world_event_views(&calendar), Vec::<WorldEventView>::new());
        assert!(matches!(
            published_event(&calendar, &event_id),
            Err(ApiError::NotFound(_))
        ));

        calendar
            .schedule(&world, &event_id, actor(9), Millis::new(2))
            .expect("schedule event");
        assert_eq!(world_event_views(&calendar).len(), 1);
        assert!(published_event(&calendar, &event_id).is_ok());
    }
}
