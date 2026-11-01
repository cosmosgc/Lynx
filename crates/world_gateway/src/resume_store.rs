//! Durable storage contract for resumable gateway sessions.
//!
//! This seam intentionally stores a digest rather than a [`ResumeToken`](crate::session::ResumeToken).
//! The gateway wires this seam only when durable session storage is configured.

use engine_core::error::EngineError;
use sha2::{Digest, Sha256};

/// SHA-256 digest of an opaque resume token. Its debug representation never
/// prints the credential digest.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TokenDigest([u8; 32]);

impl TokenDigest {
    /// Hash a raw token at the trusted service boundary.
    #[must_use]
    pub fn of(token: &str) -> Self {
        let hash = Sha256::digest(token.as_bytes());
        let mut bytes = [0_u8; 32];
        bytes.copy_from_slice(&hash);
        Self(bytes)
    }

    /// Borrow the fixed-size digest for storage adapters.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Construct a digest read from a database.
    ///
    /// Returns `None` for values that are not exactly a SHA-256 digest.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let bytes: [u8; 32] = bytes.try_into().ok()?;
        Some(Self(bytes))
    }
}

impl std::fmt::Debug for TokenDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TokenDigest([REDACTED])")
    }
}

#[cfg(test)]
mod tests {
    use super::TokenDigest;

    #[test]
    fn token_digest_is_stable_redacted_and_fixed_size() {
        let digest = TokenDigest::of("opaque bearer token");
        assert_eq!(digest, TokenDigest::of("opaque bearer token"));
        assert_eq!(TokenDigest::from_bytes(digest.as_bytes()), Some(digest));
        assert_eq!(TokenDigest::from_bytes(&[0; 31]), None);
        assert_eq!(format!("{digest:?}"), "TokenDigest([REDACTED])");
    }
}

/// Durable metadata required to route a retained session after restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeSessionRecord {
    /// Opaque gateway routing identifier, not a bearer credential.
    pub session_id: String,
    /// Authenticated account identifier.
    pub account_id: String,
    /// Verified character principal identifier.
    pub principal_id: String,
    /// Verified actor entity identifier.
    pub actor_id: String,
    /// Trusted staff authorization claim captured at authentication time.
    pub staff: bool,
    /// Digest of the currently accepted resume token.
    pub token_digest: TokenDigest,
    /// Resume deadline in Unix epoch milliseconds; `None` while attached.
    pub expires_at_ms: Option<i64>,
    /// Whether the session currently has a live transport attachment.
    pub connected: bool,
    /// Monotonically increasing fencing generation.
    pub generation: i64,
    /// Whether the session has been explicitly revoked.
    pub revoked: bool,
}

/// Durable resume-session operations used by the gateway lifecycle.
///
/// Adapters must ensure `consume_and_rotate` atomically claims exactly one
/// detached, unexpired, unrevoked token and increments its generation.
pub trait ResumeSessionStore: std::fmt::Debug + Send + Sync {
    /// Insert an initially connected record.
    fn create(&self, record: &ResumeSessionRecord) -> Result<(), EngineError>;

    /// Detach a connected record, setting its expiry and advancing its fence.
    /// Returns false if the current generation is stale or not connected.
    fn detach(
        &self,
        session_id: &str,
        expected_generation: i64,
        expires_at_ms: i64,
    ) -> Result<bool, EngineError>;

    /// Convert process-crash leftovers into detached sessions. Implementations
    /// must validate and lock the current zone lease in the same transaction.
    fn detach_stale_connected(
        &self,
        zone_id: &str,
        owner: &str,
        fencing_token: i64,
        expires_at_ms: i64,
    ) -> Result<u64, EngineError>;

    /// Atomically consume `current` and replace it with `next` if eligible.
    /// Returns the consumed detached metadata with the new digest and fence,
    /// preserving its checked expiry for restoration validation, or `None`
    /// when the token cannot be used.
    fn consume_and_rotate(
        &self,
        current: TokenDigest,
        next: TokenDigest,
        now_ms: i64,
    ) -> Result<Option<ResumeSessionRecord>, EngineError>;

    /// Revoke a session only when its generation still matches.
    fn revoke(&self, session_id: &str, expected_generation: i64) -> Result<bool, EngineError>;

    /// Revoke every retained session for a trusted principal, for example
    /// after its authorization claims change.
    fn revoke_principal(&self, principal_id: &str) -> Result<u64, EngineError>;

    /// Remove detached records whose expiry has elapsed.
    fn sweep_expired(&self, now_ms: i64) -> Result<u64, EngineError>;
}
