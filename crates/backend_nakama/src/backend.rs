//! Production composition of Nakama authentication and Lynx PostgreSQL state.
//!
//! Nakama validates credentials and supplies a user identity. This adapter
//! provisions/resolves the durable Lynx account before passing only that account
//! to the account-scoped character repository. No Nakama type reaches gameplay.

use std::fmt;

use engine_core::IdKind;
use engine_core::id::{IdGenerator, SystemIdGenerator};
use game_api::{
    Account, ApiError, AuthService, Character, CharacterCreationKey, CharacterCreationReceipt,
    CharacterId, CharacterIntentService, CharacterService, CharacterWorldService, CreateCharacter,
    DeviceLogin, HealthService, HealthStatus, Session, SessionToken,
};

use crate::{NakamaClient, PostgresCharacterStore, PostgresIdentityStore};

/// Production adapter joining Nakama sessions to Lynx-owned character data.
pub struct NakamaPostgresBackend {
    nakama: NakamaClient,
    identities: PostgresIdentityStore,
    characters: PostgresCharacterStore,
}

impl fmt::Debug for NakamaPostgresBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NakamaPostgresBackend")
            .field("nakama", &self.nakama)
            .field("identities", &self.identities)
            .field("characters", &self.characters)
            .finish()
    }
}

impl NakamaPostgresBackend {
    /// Compose established Nakama and PostgreSQL adapters.
    #[must_use]
    pub fn new(
        nakama: NakamaClient,
        identities: PostgresIdentityStore,
        characters: PostgresCharacterStore,
    ) -> Self {
        Self {
            nakama,
            identities,
            characters,
        }
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
