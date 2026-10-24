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
use engine_core::id::{IdKind, StableId};
use game_api::AccountId;
use sha2::{Digest, Sha256};
use world_gateway::SessionClaims;

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
