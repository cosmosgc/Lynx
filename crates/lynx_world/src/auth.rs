//! Static bearer-token authentication for the demo world service.
//!
//! Operators configure SHA-256 hashes, never plaintext tokens. Each accepted
//! token is bound to one server-chosen account and character; the client never
//! supplies either. A successful login starts a fresh session, while a
//! reconnect presents the gateway-issued resume token instead.
//!
//! This is a development and single-node deployment mechanism. Production
//! authentication belongs to Nakama (see `docs/adr/0002-backend-topology.md`);
//! the world service must validate the Nakama session before opening a
//! gateway session.

use anyhow::{Result, bail};
use backend_nakama::NakamaPostgresBackend;
use engine_core::id::{IdKind, StableId};
use game_api::{AccountId, ApiError, AuthService, CharacterId, SessionToken};
use sha2::{Digest, Sha256};
use world_gateway::SessionClaims;

/// Failure categories at the identity boundary. They contain no backend details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityError {
    /// The session is malformed, unknown, expired, or revoked.
    InvalidSession,
    /// The selected character is invalid or not owned by the account.
    CharacterForbidden,
    /// The identity or ownership service could not make a trustworthy decision.
    Unavailable,
}

impl IdentityError {
    /// Stable, non-sensitive category for callers to map to a protocol error.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidSession => "invalid_session",
            Self::CharacterForbidden => "character_forbidden",
            Self::Unavailable => "identity_unavailable",
        }
    }
}

/// Trusted identity and character ownership boundary, not a client interface.
///
/// Implementations must validate authenticity, expiry and revocation on every
/// call, fail closed on uncertainty, and never log the presented credential.
/// Ownership must be obtained from an authoritative source, not token claims or
/// client input. Backend errors must be reduced to the categories above.
/// This synchronous seam does not prescribe a production provider or runtime.
pub trait IdentityBackend {
    /// Validate a session and return its server-verified account.
    fn validate_session(&self, credential: &str) -> Result<AccountId, IdentityError>;

    /// Check current ownership of the selected character for that account.
    fn owns_character(
        &self,
        account: &AccountId,
        character: StableId,
    ) -> Result<bool, IdentityError>;
}

/// Identity adapter for the selected Nakama/PostgreSQL production backend.
///
/// Nakama validates the bearer session while the Lynx PostgreSQL repository
/// proves character ownership. Only sanitized [`IdentityError`] values cross
/// this boundary, so neither a provider response nor a database error can be
/// returned to a peer or put in an admission log.
#[derive(Debug)]
pub struct NakamaIdentityBackend {
    backend: NakamaPostgresBackend,
}

impl NakamaIdentityBackend {
    /// Wrap the production adapter used for authentication and ownership.
    #[must_use]
    pub const fn new(backend: NakamaPostgresBackend) -> Self {
        Self { backend }
    }
}

impl IdentityBackend for NakamaIdentityBackend {
    fn validate_session(&self, credential: &str) -> Result<AccountId, IdentityError> {
        let token =
            SessionToken::new(credential.to_owned()).map_err(|_| IdentityError::InvalidSession)?;
        self.backend
            .account(&token)
            .map(|account| account.id)
            .map_err(identity_error)
    }

    fn owns_character(
        &self,
        account: &AccountId,
        character: StableId,
    ) -> Result<bool, IdentityError> {
        let character =
            CharacterId::new(character).map_err(|_| IdentityError::CharacterForbidden)?;
        self.backend
            .owns_active_character(account, character)
            .map_err(identity_error)
    }
}

fn identity_error(error: ApiError) -> IdentityError {
    match error {
        ApiError::Unauthenticated | ApiError::SessionExpired | ApiError::PermissionDenied => {
            IdentityError::InvalidSession
        }
        ApiError::NotFound(_) | ApiError::InvalidRequest(_) => IdentityError::CharacterForbidden,
        ApiError::Unavailable(_)
        | ApiError::Transport(_)
        | ApiError::Conflict(_)
        | ApiError::RateLimited
        | ApiError::Backend { .. } => IdentityError::Unavailable,
    }
}

/// Authenticate and authorize a selection before creating a gateway session.
///
/// The returned claims carry no staff authority. Refresh, ongoing revalidation,
/// auditing, and admission wiring remain responsibilities of the caller.
///
/// # Errors
///
/// Rejects invalid sessions, invalid or unowned characters, and backend failures.
pub fn authenticate_selection(
    backend: &(impl IdentityBackend + ?Sized),
    credential: &str,
    character: StableId,
) -> Result<SessionClaims, IdentityError> {
    if credential.is_empty() {
        return Err(IdentityError::InvalidSession);
    }
    let account = backend.validate_session(credential)?;
    if character.kind() != IdKind::Character || !backend.owns_character(&account, character)? {
        return Err(IdentityError::CharacterForbidden);
    }
    SessionClaims::new(account, character).map_err(|_| IdentityError::CharacterForbidden)
}

/// A trusted identity granted by one configured token.
#[derive(Debug, Clone)]
pub struct Grant {
    /// Backend account that owns the character.
    pub account: AccountId,
    /// Character principal the session may drive.
    pub principal: StableId,
    /// Whether the account carries a staff role.
    pub staff: bool,
}

impl Grant {
    /// Build the trusted claims passed to the gateway.
    ///
    /// # Errors
    ///
    /// Returns [`engine_core::error::EngineError`] if the principal is not a
    /// character ID.
    pub fn claims(&self) -> Result<SessionClaims, engine_core::error::EngineError> {
        SessionClaims::new(self.account.clone(), self.principal)
            .map(|claims| claims.with_staff(self.staff))
    }
}

#[derive(Debug)]
struct Entry {
    digest: [u8; 32],
    grant: Grant,
}

/// The set of hashed tokens accepted by this process.
#[derive(Debug)]
pub struct Authenticator {
    entries: Vec<Entry>,
}

impl Authenticator {
    /// Build one demo character per configured token hash.
    ///
    /// Character identities are deterministic: entry `i` owns character
    /// principal seed `i + 1`. Operators should keep this mapping stable across
    /// restarts until durable character storage exists.
    ///
    /// # Errors
    ///
    /// Returns an error when no hash is configured, a hash is duplicated, or a
    /// generated account identifier is rejected.
    pub fn new(hashes: &[[u8; 32]]) -> Result<Self> {
        if hashes.is_empty() {
            bail!("at least one token hash is required");
        }
        let mut entries = Vec::with_capacity(hashes.len());
        for (index, digest) in hashes.iter().enumerate() {
            if entries
                .iter()
                .any(|entry: &Entry| constant_time_eq(&entry.digest, digest))
            {
                bail!("token hash {index} is configured more than once");
            }
            let seed = u128::try_from(index + 1)?;
            let principal = StableId::from_parts(IdKind::Character, 1, seed);
            let account = AccountId::new(format!("lynx-demo-{index:02}"))
                .map_err(|error| anyhow::anyhow!("invalid demo account: {error}"))?;
            entries.push(Entry {
                digest: *digest,
                grant: Grant {
                    account,
                    principal,
                    staff: false,
                },
            });
        }
        Ok(Self { entries })
    }

    /// Whether demo sessions carry a staff role.
    ///
    /// This exists for loopback development only: a staff session may drive
    /// world calendar lifecycle commands. Never enable it on a service that
    /// real players can reach.
    #[must_use]
    pub fn with_staff(mut self, staff: bool) -> Self {
        for entry in &mut self.entries {
            entry.grant.staff = staff;
        }
        self
    }

    /// Resolve a presented bearer token, if it matches a configured hash.
    ///
    /// The compared value is the SHA-256 digest, so a leak of this process's
    /// configuration does not expose usable tokens.
    #[must_use]
    pub fn grant_for(&self, presented: &str) -> Option<&Grant> {
        let digest = digest(presented);
        self.entries
            .iter()
            .find(|entry| constant_time_eq(&entry.digest, &digest))
            .map(|entry| &entry.grant)
    }

    /// The configured grants, in token order. The demo zone uses this to spawn
    /// one character per grant.
    pub fn grants(&self) -> impl Iterator<Item = &Grant> {
        self.entries.iter().map(|entry| &entry.grant)
    }
}

/// Compute the SHA-256 digest of a token.
#[must_use]
pub fn digest(token: &str) -> [u8; 32] {
    let hash = Sha256::digest(token.as_bytes());
    let mut bytes = [0_u8; 32];
    bytes.copy_from_slice(&hash);
    bytes
}

/// Format a token hash for logs and configuration examples.
#[must_use]
pub fn hash_hex(token: &str) -> String {
    hex(&digest(token))
}

/// Parse a 64-character hexadecimal SHA-256 digest.
///
/// # Errors
///
/// Returns an error for the wrong length or non-hexadecimal characters. The
/// input is never echoed, so a mistyped plaintext token does not reach logs.
pub fn parse_hash(text: &str) -> Result<[u8; 32]> {
    let text = text.trim();
    if text.len() != 64 {
        bail!("a token hash must be 64 hexadecimal characters");
    }
    let mut bytes = [0_u8; 32];
    let encoded = text.as_bytes();
    for index in 0..bytes.len() {
        let high = decode_nibble(encoded[index * 2])?;
        let low = decode_nibble(encoded[index * 2 + 1])?;
        bytes[index] = (high << 4) | low;
    }
    Ok(bytes)
}

fn decode_nibble(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => bail!("a token hash may only contain hexadecimal characters"),
    }
}

fn hex(bytes: &[u8; 32]) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(64);
    for byte in bytes {
        write!(out, "{byte:02x}").expect("writing to a string cannot fail");
    }
    out
}

fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    let mut difference = 0_u8;
    for (a, b) in left.iter().zip(right) {
        difference |= a ^ b;
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "correct horse battery staple";

    struct FakeIdentity;

    impl IdentityBackend for FakeIdentity {
        fn validate_session(&self, credential: &str) -> Result<AccountId, IdentityError> {
            match credential {
                TOKEN => Ok(AccountId::new("verified-account").unwrap()),
                "offline" => Err(IdentityError::Unavailable),
                // Includes expired, revoked, malformed, and unknown credentials.
                _ => Err(IdentityError::InvalidSession),
            }
        }

        fn owns_character(
            &self,
            account: &AccountId,
            character: StableId,
        ) -> Result<bool, IdentityError> {
            assert_eq!(account.as_str(), "verified-account");
            if character == StableId::from_parts(IdKind::Character, 1, 3) {
                return Err(IdentityError::Unavailable);
            }
            Ok(character == StableId::from_parts(IdKind::Character, 1, 1))
        }
    }

    #[test]
    fn identity_selection_fails_closed_and_returns_only_verified_claims() {
        let owned = StableId::from_parts(IdKind::Character, 1, 1);
        let other = StableId::from_parts(IdKind::Character, 1, 2);
        let claims = authenticate_selection(&FakeIdentity, TOKEN, owned).unwrap();
        assert_eq!(claims.account.as_str(), "verified-account");
        assert_eq!(claims.principal, owned);
        assert!(!claims.staff);

        for token in ["", "expired", "revoked", "malformed", "unknown"] {
            assert_eq!(
                authenticate_selection(&FakeIdentity, token, owned).unwrap_err(),
                IdentityError::InvalidSession
            );
        }
        assert_eq!(
            authenticate_selection(&FakeIdentity, TOKEN, other).unwrap_err(),
            IdentityError::CharacterForbidden
        );
        assert_eq!(
            authenticate_selection(
                &FakeIdentity,
                TOKEN,
                StableId::from_parts(IdKind::Character, 1, 3)
            )
            .unwrap_err(),
            IdentityError::Unavailable
        );
        assert_eq!(
            authenticate_selection(&FakeIdentity, "offline", owned).unwrap_err(),
            IdentityError::Unavailable
        );
        assert_eq!(
            authenticate_selection(
                &FakeIdentity,
                TOKEN,
                StableId::from_parts(IdKind::Entity, 1, 1)
            )
            .unwrap_err(),
            IdentityError::CharacterForbidden
        );
    }

    #[test]
    fn identity_errors_have_stable_redacted_codes_and_debug_output() {
        for error in [
            IdentityError::InvalidSession,
            IdentityError::CharacterForbidden,
            IdentityError::Unavailable,
        ] {
            assert!(!error.code().contains(TOKEN));
            assert!(!format!("{error:?}").contains(TOKEN));
        }
        assert_eq!(IdentityError::InvalidSession.code(), "invalid_session");
        assert_eq!(
            IdentityError::CharacterForbidden.code(),
            "character_forbidden"
        );
        assert_eq!(IdentityError::Unavailable.code(), "identity_unavailable");
    }

    #[test]
    fn hashes_round_trip_through_configuration_text() {
        let text = hash_hex(TOKEN);
        assert_eq!(text.len(), 64);
        assert_eq!(parse_hash(&text).unwrap(), digest(TOKEN));
        assert_eq!(parse_hash(&text.to_uppercase()).unwrap(), digest(TOKEN));
        assert_eq!(parse_hash(&format!("  {text}\n")).unwrap(), digest(TOKEN));
    }

    #[test]
    fn malformed_hashes_are_rejected_without_echoing_input() {
        assert!(parse_hash("").is_err());
        assert!(parse_hash("abcd").is_err());
        let plaintext = "z".repeat(64);
        assert!(parse_hash(&format!("{plaintext}z")).is_err());

        let error = parse_hash(&plaintext).err().unwrap().to_string();
        assert!(!error.contains(&plaintext));
    }

    #[test]
    fn only_configured_digests_are_accepted() {
        let authenticator = Authenticator::new(&[digest(TOKEN)]).unwrap();
        let grant = authenticator.grant_for(TOKEN).unwrap();
        assert_eq!(grant.account.as_str(), "lynx-demo-00");
        assert_eq!(grant.principal.kind(), IdKind::Character);
        assert!(!grant.staff);
        assert!(authenticator.grant_for("wrong token").is_none());
        assert!(authenticator.grant_for("").is_none());
    }

    #[test]
    fn duplicate_and_empty_hash_sets_are_rejected() {
        assert!(Authenticator::new(&[]).is_err());
        let hash = digest(TOKEN);
        assert!(Authenticator::new(&[hash, hash]).is_err());
        assert!(Authenticator::new(&[hash, digest("another")]).is_ok());
    }

    #[test]
    fn grants_produce_valid_claims_and_distinct_characters() {
        let authenticator = Authenticator::new(&[digest(TOKEN), digest("second token")]).unwrap();
        let grants: Vec<&Grant> = authenticator.grants().collect();
        assert_eq!(grants.len(), 2);
        assert_ne!(grants[0].principal, grants[1].principal);
        for grant in grants {
            let claims = grant.claims().unwrap();
            assert_eq!(claims.principal, grant.principal);
            assert_eq!(claims.account, grant.account);
        }
    }
}
