//! Lookup of existing Lynx accounts by authenticated Nakama identity.

use std::sync::Mutex;

use game_api::{AccountId, ApiError};
use postgres::Client;

/// Read-only identity mapping over a migrated Lynx database.
///
/// This is not an authenticator or an account provisioner. The caller must
/// validate the Nakama session and obtain its user ID from authenticated claims
/// or a trusted Nakama response, never from a client-supplied account or user ID.
pub struct PostgresIdentityStore {
    client: Mutex<Client>,
}

impl std::fmt::Debug for PostgresIdentityStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresIdentityStore")
            .finish_non_exhaustive()
    }
}

impl PostgresIdentityStore {
    /// Wrap an established connection to the migrated Lynx database.
    #[must_use]
    pub fn new(client: Client) -> Self {
        Self {
            client: Mutex::new(client),
        }
    }

    /// Resolve an authenticated Nakama user to an existing Lynx account.
    ///
    /// Malformed or unmapped identities are rejected; database and invalid
    /// stored-account errors are sanitized. This method does not verify the
    /// provenance of `nakama_user_id` and must not be exposed directly to clients.
    pub fn resolve_account(&self, nakama_user_id: &str) -> Result<AccountId, ApiError> {
        if !is_uuid(nakama_user_id) {
            return Err(ApiError::Unauthenticated);
        }
        let mut client = self.client.lock().map_err(|_| unavailable())?;
        let row = client
            .query_opt(
                "SELECT account_id FROM lynx.accounts WHERE nakama_user_id = $1::text::uuid",
                &[&nakama_user_id],
            )
            .map_err(|_| unavailable())?
            .ok_or(ApiError::Unauthenticated)?;
        let account: String = row.try_get(0).map_err(|_| unavailable())?;
        AccountId::new(account).map_err(|_| unavailable())
    }
}

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, byte)| match i {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

fn unavailable() -> ApiError {
    ApiError::Unavailable("identity store unavailable".into())
}

#[cfg(test)]
mod tests {
    use super::is_uuid;

    #[test]
    fn accepts_only_canonical_uuid_shape() {
        assert!(is_uuid("12345678-1234-ABCD-9876-123456789abc"));
        for invalid in [
            "",
            "123456781234abcd9876123456789abc",
            "12345678-1234-abcd-9876-123456789abz",
            "12345678-1234-abcd-9876-123456789abc' OR true--",
            "é2345678-1234-abcd-9876-123456789abc",
        ] {
            assert!(!is_uuid(invalid));
        }
    }
}
