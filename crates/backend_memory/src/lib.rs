//! In-memory backend adapter for tests and offline development.
//!
//! This adapter is deliberately not a production identity provider. It has no
//! persistence, password verification, cryptographic token generation, or
//! security boundary. Do not use it to authenticate real players.

mod admin;
mod character;
mod chat;
mod inventory;
mod social;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex};

use game_api::{
    Account, AccountId, ApiError, AuditRecord, AuthService, Character, CharacterId, ChatChannel,
    ChatChannelKind, ChatMessage, DeviceLogin, FriendState, HealthService, HealthStatus,
    InventorySlot, Session, SessionToken, WorldService, WorldSession, Zone,
};

/// Default number of non-deleted characters one account may own.
pub const DEFAULT_CHARACTER_LIMIT: usize = 8;

/// The single local chat room provided for offline development.
const LOCAL_CHANNEL_ID: &str = "global";

/// A local in-memory implementation of the shared backend contracts.
///
/// Cloned instances share their state. A new instance starts empty and does not
/// persist state across process restarts.
pub struct MemoryBackend {
    state: Arc<Mutex<State>>,
    zones: BTreeMap<String, Zone>,
    character_limit: usize,
}

impl fmt::Debug for MemoryBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryBackend")
            .field("zones", &self.zones)
            .finish_non_exhaustive()
    }
}

impl Clone for MemoryBackend {
    fn clone(&self) -> Self {
        Self {
            state: Arc::clone(&self.state),
            zones: self.zones.clone(),
            character_limit: self.character_limit,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct State {
    accounts: BTreeMap<AccountId, Account>,
    device_accounts: BTreeMap<String, AccountId>,
    access_tokens: BTreeMap<String, AccountId>,
    access_refresh_tokens: BTreeMap<String, String>,
    refresh_tokens: BTreeMap<String, AccountId>,
    refresh_access_tokens: BTreeMap<String, String>,
    world_sessions: BTreeMap<AccountId, WorldSession>,
    characters: BTreeMap<CharacterId, Character>,
    inventory: BTreeMap<CharacterId, Vec<InventorySlot>>,
    friends: BTreeMap<AccountId, BTreeMap<AccountId, FriendState>>,
    blocked: BTreeMap<AccountId, BTreeSet<AccountId>>,
    channels: BTreeMap<String, ChatChannel>,
    messages: BTreeMap<String, VecDeque<ChatMessage>>,
    audit: Vec<AuditRecord>,
    next_event_id: u64,
    next_id: u64,
}

fn initial_state() -> State {
    let mut state = State::default();
    state.channels.insert(
        LOCAL_CHANNEL_ID.to_owned(),
        ChatChannel {
            id: LOCAL_CHANNEL_ID.to_owned(),
            name: Some("Global".into()),
            kind: ChatChannelKind::Room,
            member_count: 0,
        },
    );
    state
        .messages
        .insert(LOCAL_CHANNEL_ID.to_owned(), VecDeque::new());
    state
}

impl MemoryBackend {
    /// Create an empty backend with one local development zone.
    #[must_use]
    pub fn new() -> Self {
        let zone = Zone {
            id: "local".into(),
            name: "Local Zone".into(),
            player_count: 0,
            capacity: Some(100),
        };
        Self {
            state: Arc::new(Mutex::new(initial_state())),
            zones: BTreeMap::from([(zone.id.clone(), zone)]),
            character_limit: DEFAULT_CHARACTER_LIMIT,
        }
    }

    /// Create an empty backend with configured zones.
    ///
    /// Player counts are maintained by the adapter and start at zero,
    /// regardless of the values in the supplied zones.
    pub fn with_zones(zones: impl IntoIterator<Item = Zone>) -> Result<Self, ApiError> {
        let mut configured = BTreeMap::new();
        for mut zone in zones {
            if zone.id.is_empty()
                || zone.id.chars().any(char::is_control)
                || zone.id.chars().any(char::is_whitespace)
                || zone.name.trim().is_empty()
                || zone.name.chars().any(char::is_control)
            {
                return Err(ApiError::InvalidRequest(
                    "zone IDs and names must be non-empty and contain no control characters".into(),
                ));
            }
            if configured.contains_key(&zone.id) {
                return Err(ApiError::Conflict(format!(
                    "duplicate zone ID: {}",
                    zone.id
                )));
            }
            zone.player_count = 0;
            configured.insert(zone.id.clone(), zone);
        }
        if configured.is_empty() {
            return Err(ApiError::InvalidRequest(
                "at least one zone must be configured".into(),
            ));
        }
        Ok(Self {
            state: Arc::new(Mutex::new(initial_state())),
            zones: configured,
            character_limit: DEFAULT_CHARACTER_LIMIT,
        })
    }

    /// Override the number of non-deleted characters one account may own.
    ///
    /// Values below one are treated as one.
    #[must_use]
    pub const fn with_character_limit(mut self, limit: usize) -> Self {
        self.character_limit = if limit == 0 { 1 } else { limit };
        self
    }

    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, State>, ApiError> {
        self.state
            .lock()
            .map_err(|_| ApiError::Unavailable("in-memory backend state is unavailable".into()))
    }

    fn next_token(state: &mut State, kind: &str) -> Result<SessionToken, ApiError> {
        state.next_id = state
            .next_id
            .checked_add(1)
            .ok_or_else(|| ApiError::Unavailable("in-memory ID space exhausted".into()))?;
        SessionToken::new(format!("memory-{kind}-{}", state.next_id))
    }

    fn account_for_token(state: &State, token: &SessionToken) -> Result<AccountId, ApiError> {
        state
            .access_tokens
            .get(token.expose_secret())
            .cloned()
            .ok_or(ApiError::Unauthenticated)
    }
}

impl Default for MemoryBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl HealthService for MemoryBackend {
    fn health(&self) -> Result<HealthStatus, ApiError> {
        Ok(HealthStatus {
            healthy: true,
            message: Some("in-memory backend (non-persistent)".into()),
        })
    }
}

impl AuthService for MemoryBackend {
    fn authenticate_device(&self, request: &DeviceLogin) -> Result<Session, ApiError> {
        request.validate()?;
        let mut state = self.lock_state()?;

        let (account_id, created, new_account) =
            match state.device_accounts.get(&request.device_id) {
                Some(account_id) => (account_id.clone(), false, None),
                None if request.create => {
                    state.next_id = state.next_id.checked_add(1).ok_or_else(|| {
                        ApiError::Unavailable("in-memory ID space exhausted".into())
                    })?;
                    let account_id = AccountId::new(format!("memory-account-{}", state.next_id))?;
                    let account = Account {
                        id: account_id.clone(),
                        username: request.username.clone(),
                        display_name: None,
                        locale: request.locale.clone(),
                        created_at: None,
                        avatar_url: None,
                    };
                    (account_id, true, Some(account))
                }
                None => {
                    return Err(ApiError::NotFound(
                        "no in-memory account exists for this device".into(),
                    ));
                }
            };

        let token = Self::next_token(&mut state, "access")?;
        let refresh_token = Self::next_token(&mut state, "refresh")?;
        state
            .access_tokens
            .insert(token.expose_secret().to_owned(), account_id.clone());
        state.access_refresh_tokens.insert(
            token.expose_secret().to_owned(),
            refresh_token.expose_secret().to_owned(),
        );
        state
            .refresh_tokens
            .insert(refresh_token.expose_secret().to_owned(), account_id.clone());
        state.refresh_access_tokens.insert(
            refresh_token.expose_secret().to_owned(),
            token.expose_secret().to_owned(),
        );
        if let Some(account) = new_account {
            state
                .device_accounts
                .insert(request.device_id.clone(), account_id.clone());
            state.accounts.insert(account_id.clone(), account);
        }

        Ok(Session {
            account_id,
            token,
            refresh_token,
            created,
            expires_at: None,
        })
    }

    fn refresh_session(&self, refresh_token: &SessionToken) -> Result<Session, ApiError> {
        let mut state = self.lock_state()?;
        let account_id = state
            .refresh_tokens
            .get(refresh_token.expose_secret())
            .cloned()
            .ok_or(ApiError::SessionExpired)?;
        let previous_access_token = state
            .refresh_access_tokens
            .get(refresh_token.expose_secret())
            .cloned();
        let token = Self::next_token(&mut state, "access")?;
        let next_refresh_token = Self::next_token(&mut state, "refresh")?;
        state.refresh_tokens.remove(refresh_token.expose_secret());
        state
            .refresh_access_tokens
            .remove(refresh_token.expose_secret());
        if let Some(previous_access_token) = previous_access_token {
            state.access_tokens.remove(&previous_access_token);
            state.access_refresh_tokens.remove(&previous_access_token);
        }
        state
            .access_tokens
            .insert(token.expose_secret().to_owned(), account_id.clone());
        state.access_refresh_tokens.insert(
            token.expose_secret().to_owned(),
            next_refresh_token.expose_secret().to_owned(),
        );
        state.refresh_tokens.insert(
            next_refresh_token.expose_secret().to_owned(),
            account_id.clone(),
        );
        state.refresh_access_tokens.insert(
            next_refresh_token.expose_secret().to_owned(),
            token.expose_secret().to_owned(),
        );
        Ok(Session {
            account_id,
            token,
            refresh_token: next_refresh_token,
            created: false,
            expires_at: None,
        })
    }

    fn account(&self, token: &SessionToken) -> Result<Account, ApiError> {
        let state = self.lock_state()?;
        let account_id = Self::account_for_token(&state, token)?;
        state
            .accounts
            .get(&account_id)
            .cloned()
            .ok_or_else(|| ApiError::Unavailable("in-memory account record is missing".into()))
    }

    fn logout(&self, token: &SessionToken) -> Result<(), ApiError> {
        let mut state = self.lock_state()?;
        let account_id = Self::account_for_token(&state, token)?;
        state.access_tokens.remove(token.expose_secret());
        if let Some(refresh) = state.access_refresh_tokens.remove(token.expose_secret()) {
            state.refresh_tokens.remove(&refresh);
            state.refresh_access_tokens.remove(&refresh);
        }
        if !state
            .access_tokens
            .values()
            .any(|owner| owner == &account_id)
        {
            state.world_sessions.remove(&account_id);
        }
        Ok(())
    }
}

impl WorldService for MemoryBackend {
    fn zones(&self, token: &SessionToken) -> Result<Vec<Zone>, ApiError> {
        let state = self.lock_state()?;
        Self::account_for_token(&state, token)?;
        let mut zones: Vec<_> = self.zones.values().cloned().collect();
        for zone in &mut zones {
            zone.player_count = u32::try_from(
                state
                    .world_sessions
                    .values()
                    .filter(|session| session.zone_id == zone.id)
                    .count(),
            )
            .map_err(|_| {
                ApiError::Unavailable("zone player count exceeds supported range".into())
            })?;
        }
        Ok(zones)
    }

    fn enter_world(&self, token: &SessionToken, zone_id: &str) -> Result<WorldSession, ApiError> {
        let mut state = self.lock_state()?;
        let account_id = Self::account_for_token(&state, token)?;
        let zone = self
            .zones
            .get(zone_id)
            .ok_or_else(|| ApiError::NotFound(format!("zone not found: {zone_id}")))?;

        if let Some(existing) = state.world_sessions.get(&account_id)
            && existing.zone_id == zone_id
        {
            return Ok(existing.clone());
        }

        let occupancy = state
            .world_sessions
            .values()
            .filter(|session| session.zone_id == zone_id)
            .count();
        if zone
            .capacity
            .is_some_and(|capacity| occupancy >= capacity as usize)
        {
            return Err(ApiError::Unavailable(format!(
                "zone is at capacity: {zone_id}"
            )));
        }

        state.next_id = state
            .next_id
            .checked_add(1)
            .ok_or_else(|| ApiError::Unavailable("in-memory ID space exhausted".into()))?;
        let session = WorldSession {
            zone_id: zone_id.to_owned(),
            session_id: format!("memory-world-{}", state.next_id),
            endpoint: "local://world".into(),
        };
        state.world_sessions.insert(account_id, session.clone());
        Ok(session)
    }

    fn leave_world(&self, token: &SessionToken, session: &WorldSession) -> Result<(), ApiError> {
        let mut state = self.lock_state()?;
        let account_id = Self::account_for_token(&state, token)?;
        match state.world_sessions.get(&account_id) {
            Some(current) if current == session => {
                state.world_sessions.remove(&account_id);
                Ok(())
            }
            Some(_) => Err(ApiError::Conflict(
                "world session does not match the active session".into(),
            )),
            None => Err(ApiError::NotFound("no active world session".into())),
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use engine_core::Locale;
    use game_api::DeviceLogin;

    pub(crate) fn login(device_id: &str, create: bool) -> DeviceLogin {
        DeviceLogin {
            device_id: device_id.into(),
            username: None,
            create,
            locale: Locale::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::Locale;

    fn login(device_id: &str, create: bool) -> DeviceLogin {
        DeviceLogin {
            device_id: device_id.into(),
            username: None,
            create,
            locale: Locale::default(),
        }
    }

    fn backend_with_capacity(capacity: u32) -> MemoryBackend {
        MemoryBackend::with_zones([Zone {
            id: "test-zone".into(),
            name: "Test Zone".into(),
            player_count: 99,
            capacity: Some(capacity),
        }])
        .unwrap()
    }

    #[test]
    fn device_authentication_creates_and_reuses_an_account() {
        let backend = MemoryBackend::new();
        let first = backend
            .authenticate_device(&login("device-1", true))
            .unwrap();
        let again = backend
            .authenticate_device(&login("device-1", true))
            .unwrap();
        assert!(first.created);
        assert!(!again.created);
        assert_eq!(first.account_id, again.account_id);
        assert_eq!(
            backend.account(&first.token).unwrap().locale,
            Locale::default()
        );
    }

    #[test]
    fn device_authentication_honors_create_and_validation_options() {
        let backend = MemoryBackend::new();
        assert!(matches!(
            backend.authenticate_device(&login("unknown", false)),
            Err(ApiError::NotFound(_))
        ));
        assert!(matches!(
            backend.authenticate_device(&DeviceLogin {
                username: Some("bad name".into()),
                ..login("device-1", true)
            }),
            Err(ApiError::InvalidRequest(_))
        ));
    }

    #[test]
    fn refresh_rotates_tokens_and_logout_revokes_them() {
        let backend = MemoryBackend::new();
        let session = backend
            .authenticate_device(&login("device-1", true))
            .unwrap();
        let other_session = backend
            .authenticate_device(&login("device-1", true))
            .unwrap();
        let refreshed = backend.refresh_session(&session.refresh_token).unwrap();
        assert_ne!(session.token, refreshed.token);
        assert!(matches!(
            backend.refresh_session(&session.refresh_token),
            Err(ApiError::SessionExpired)
        ));
        assert!(matches!(
            backend.account(&session.token),
            Err(ApiError::Unauthenticated)
        ));
        assert!(backend.account(&refreshed.token).is_ok());
        assert!(backend.account(&other_session.token).is_ok());
        backend.logout(&refreshed.token).unwrap();
        assert!(matches!(
            backend.account(&refreshed.token),
            Err(ApiError::Unauthenticated)
        ));
        assert!(matches!(
            backend.refresh_session(&refreshed.refresh_token),
            Err(ApiError::SessionExpired)
        ));
        assert!(backend.account(&other_session.token).is_ok());
    }

    #[test]
    fn world_entry_tracks_capacity_and_requires_the_active_session_to_leave() {
        let backend = backend_with_capacity(1);
        let first = backend
            .authenticate_device(&login("device-1", true))
            .unwrap();
        let second = backend
            .authenticate_device(&login("device-2", true))
            .unwrap();
        let world = backend.enter_world(&first.token, "test-zone").unwrap();
        assert_eq!(
            backend.enter_world(&first.token, "test-zone").unwrap(),
            world
        );
        assert_eq!(backend.zones(&first.token).unwrap()[0].player_count, 1);
        assert!(matches!(
            backend.enter_world(&second.token, "test-zone"),
            Err(ApiError::Unavailable(_))
        ));
        assert!(matches!(
            backend.leave_world(
                &first.token,
                &WorldSession {
                    session_id: "stale".into(),
                    ..world.clone()
                }
            ),
            Err(ApiError::Conflict(_))
        ));
        backend.leave_world(&first.token, &world).unwrap();
        assert_eq!(backend.zones(&second.token).unwrap()[0].player_count, 0);
        assert!(matches!(
            backend.enter_world(&second.token, "missing"),
            Err(ApiError::NotFound(_))
        ));
    }

    #[test]
    fn invalid_zone_configuration_is_rejected() {
        assert!(matches!(
            MemoryBackend::with_zones([]),
            Err(ApiError::InvalidRequest(_))
        ));
        assert!(matches!(
            MemoryBackend::with_zones([
                Zone {
                    id: "same".into(),
                    name: "First".into(),
                    player_count: 0,
                    capacity: None,
                },
                Zone {
                    id: "same".into(),
                    name: "Second".into(),
                    player_count: 0,
                    capacity: None,
                },
            ]),
            Err(ApiError::Conflict(_))
        ));
    }

    #[test]
    fn cloned_backend_instances_share_the_same_state() {
        let backend = MemoryBackend::new();
        let other = backend.clone();
        let session = backend
            .authenticate_device(&login("device-1", true))
            .unwrap();
        assert_eq!(
            other.account(&session.token).unwrap().id,
            session.account_id
        );
    }
}
