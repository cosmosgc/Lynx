//! Stable, prefixed identifiers and authored definition keys.
//!
//! Durable identity uses a 128-bit value: a 48-bit millisecond timestamp and
//! 80 bits of randomness, rendered as a 26-character Crockford Base32 string
//! with a kind prefix, for example `ent_01J8Z3K4Q7Y2N5V6B8C0D1E2F3`.
//!
//! See `docs/adr/0007-stable-entity-ids.md`.

use std::fmt;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{EngineError, ErrorCode};

const ENCODED_LEN: usize = 26;
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const RANDOM_BITS: u32 = 80;
const RANDOM_MASK: u128 = (1u128 << RANDOM_BITS) - 1;
const MAX_TIMESTAMP_MS: u64 = (1u64 << 48) - 1;

/// The namespace of a durable identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdKind {
    /// A world entity instance.
    Entity,
    /// A player or AI character.
    Character,
    /// A running zone instance.
    Zone,
    /// A player property or plot.
    Property,
    /// A unique item instance.
    Item,
}

impl IdKind {
    /// The wire prefix, without the underscore.
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Entity => "ent",
            Self::Character => "chr",
            Self::Zone => "zone",
            Self::Property => "prop",
            Self::Item => "itm",
        }
    }

    /// Resolve a prefix.
    #[must_use]
    pub fn from_prefix(prefix: &str) -> Option<Self> {
        match prefix {
            "ent" => Some(Self::Entity),
            "chr" => Some(Self::Character),
            "zone" => Some(Self::Zone),
            "prop" => Some(Self::Property),
            "itm" => Some(Self::Item),
            _ => None,
        }
    }

    /// Human-readable name for diagnostics.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Entity => "entity",
            Self::Character => "character",
            Self::Zone => "zone",
            Self::Property => "property",
            Self::Item => "item",
        }
    }
}

impl fmt::Display for IdKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.prefix())
    }
}

/// A durable, globally unique identifier that survives saves and restarts.
///
/// [`StableId`] values are never reused. The internal `u128` is opaque; use
/// the parsing and formatting APIs rather than raw arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableId {
    kind: IdKind,
    value: u128,
}

impl StableId {
    /// Assemble an ID from a kind, timestamp, and 80 random bits.
    ///
    /// Prefer a generator in production. This constructor exists for loaders,
    /// migrations, and deterministic tests.
    #[must_use]
    pub const fn from_parts(kind: IdKind, timestamp_ms: u64, random: u128) -> Self {
        let ts = if timestamp_ms > MAX_TIMESTAMP_MS {
            MAX_TIMESTAMP_MS
        } else {
            timestamp_ms
        };
        Self {
            kind,
            value: ((ts as u128) << RANDOM_BITS) | (random & RANDOM_MASK),
        }
    }

    /// The identifier's namespace.
    #[must_use]
    pub const fn kind(self) -> IdKind {
        self.kind
    }

    /// Creation timestamp in milliseconds, as encoded in the ID.
    #[must_use]
    pub const fn timestamp_ms(self) -> u64 {
        (self.value >> RANDOM_BITS) as u64
    }

    /// The 80 random bits, used to verify uniqueness in tests.
    #[must_use]
    pub const fn random_bits(self) -> u128 {
        self.value & RANDOM_MASK
    }

    /// Fail with [`ErrorCode::InvalidRequest`] if this ID is not `expected`.
    pub fn expect_kind(self, expected: IdKind) -> Result<Self, EngineError> {
        if self.kind == expected {
            Ok(self)
        } else {
            Err(EngineError::new(
                ErrorCode::InvalidRequest,
                format!(
                    "expected a {} id, got a {} id",
                    expected.label(),
                    self.kind.label()
                ),
            )
            .with_detail("id", self.to_string())
            .with_detail("expected_kind", expected.prefix())
            .with_detail("actual_kind", self.kind.prefix()))
        }
    }

    /// Parse a prefixed ID string.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] for a malformed prefix, length, or
    /// encoding.
    pub fn parse(input: &str) -> Result<Self, EngineError> {
        let invalid = |reason: &str| {
            EngineError::new(
                ErrorCode::InvalidRequest,
                format!("invalid stable id: {reason}"),
            )
            .with_detail("reason", reason)
            .with_detail("value", input)
        };

        let (prefix, encoded) = input
            .split_once('_')
            .ok_or_else(|| invalid("missing prefix"))?;
        let kind = IdKind::from_prefix(prefix).ok_or_else(|| invalid("unknown prefix"))?;
        if encoded.len() != ENCODED_LEN {
            return Err(invalid("expected 26 encoded characters"));
        }

        let mut value: u128 = 0;
        for (index, ch) in encoded.bytes().enumerate() {
            let digit = decode_digit(ch).ok_or_else(|| invalid("invalid encoded character"))?;
            if index == 0 && digit > 7 {
                return Err(invalid("value exceeds 128 bits"));
            }
            value = (value << 5) | u128::from(digit);
        }

        Ok(Self { kind, value })
    }
}

impl fmt::Display for StableId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut encoded = [0u8; ENCODED_LEN];
        let mut value = self.value;
        for slot in encoded.iter_mut().rev() {
            *slot = ALPHABET[(value & 31) as usize];
            value >>= 5;
        }
        // The buffer is ASCII by construction.
        let text = std::str::from_utf8(&encoded).map_err(|_| fmt::Error)?;
        write!(f, "{}_{text}", self.kind.prefix())
    }
}

impl FromStr for StableId {
    type Err = EngineError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::parse(input)
    }
}

impl Serialize for StableId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for StableId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct IdVisitor;

        impl Visitor<'_> for IdVisitor {
            type Value = StableId;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a prefixed stable id such as ent_01J8Z3K4Q7Y2N5V6B8C0D1E2F3")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                StableId::parse(value).map_err(|error| E::custom(error.message()))
            }
        }

        deserializer.deserialize_str(IdVisitor)
    }
}

/// Generates durable IDs. Inject a deterministic implementation in tests.
pub trait IdGenerator {
    /// Generate a new unique ID in `kind`'s namespace.
    fn generate(&mut self, kind: IdKind) -> StableId;
}

/// The production generator: time-ordered with 80 random bits per millisecond.
#[derive(Debug)]
pub struct SystemIdGenerator {
    last_ms: u64,
    last_random: u128,
}

impl SystemIdGenerator {
    /// Create a generator seeded from the system clock.
    ///
    /// # Panics
    ///
    /// Panics if the OS entropy source is unavailable or the clock is before
    /// the Unix epoch. Both indicate a broken environment rather than a
    /// recoverable game state.
    #[must_use]
    pub fn new() -> Self {
        let mut random = [0u8; 10];
        getrandom::fill(&mut random)
            .expect("the operating system entropy source must be available to generate entity ids");
        Self {
            last_ms: 0,
            last_random: u128::from_be_bytes(pad_random(random)),
        }
    }

    /// Generate at an explicit timestamp. Used by loaders and tests.
    pub fn generate_at(&mut self, kind: IdKind, now_ms: u64) -> StableId {
        let now_ms = now_ms.min(MAX_TIMESTAMP_MS).max(self.last_ms);
        let mut random = if now_ms == self.last_ms {
            self.last_random.wrapping_add(1)
        } else {
            random_u80()
        };

        if random > RANDOM_MASK {
            // The 80-bit random space for this millisecond is exhausted.
            self.last_ms = (self.last_ms + 1).min(MAX_TIMESTAMP_MS);
            random = random_u80();
        }

        self.last_ms = now_ms.max(self.last_ms);
        self.last_random = random;
        StableId::from_parts(kind, self.last_ms, random)
    }
}

impl Default for SystemIdGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl IdGenerator for SystemIdGenerator {
    fn generate(&mut self, kind: IdKind) -> StableId {
        let now_ms = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(elapsed) => u64::try_from(elapsed.as_millis()).unwrap_or(MAX_TIMESTAMP_MS),
            Err(_) => 0,
        };
        self.generate_at(kind, now_ms)
    }
}

/// A deterministic generator for tests, replays, and fixtures.
#[derive(Debug, Clone)]
pub struct DeterministicIdGenerator {
    next: u128,
}

impl DeterministicIdGenerator {
    /// Start at `seed`. IDs are sequential from that value.
    #[must_use]
    pub const fn new(seed: u128) -> Self {
        Self { next: seed }
    }
}

impl IdGenerator for DeterministicIdGenerator {
    fn generate(&mut self, kind: IdKind) -> StableId {
        let value = self.next;
        self.next = self.next.wrapping_add(1);
        StableId::from_parts(kind, 0, value)
    }
}

fn pad_random(bytes: [u8; 10]) -> [u8; 16] {
    let mut padded = [0u8; 16];
    padded[6..].copy_from_slice(&bytes);
    padded
}

fn random_u80() -> u128 {
    let mut bytes = [0u8; 10];
    if getrandom::fill(&mut bytes).is_err() {
        // Entropy failure is fatal for uniqueness. Mix the clock with a
        // process-local counter as a last resort, still inside 80 bits.
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| {
                u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX)
            });
        let mixed = now ^ count.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        return u128::from(mixed);
    }
    u128::from_be_bytes(pad_random(bytes))
}

fn decode_digit(byte: u8) -> Option<u8> {
    let upper = byte.to_ascii_uppercase();
    match upper {
        b'0' | b'O' => Some(0),
        b'1' | b'I' | b'L' => Some(1),
        b'2'..=b'9' => Some(upper - b'0'),
        b'A' => Some(10),
        b'B' => Some(11),
        b'C' => Some(12),
        b'D' => Some(13),
        b'E' => Some(14),
        b'F' => Some(15),
        b'G' => Some(16),
        b'H' => Some(17),
        b'J' => Some(18),
        b'K' => Some(19),
        b'M' => Some(20),
        b'N' => Some(21),
        b'P' => Some(22),
        b'Q' => Some(23),
        b'R' => Some(24),
        b'S' => Some(25),
        b'T' => Some(26),
        b'V' => Some(27),
        b'W' => Some(28),
        b'X' => Some(29),
        b'Y' => Some(30),
        b'Z' => Some(31),
        _ => None,
    }
}

/// A stable, human-writable key for authored content, such as `door.wooden`.
///
/// Definition IDs are not entity instances. They are validated lowercase slugs
/// with dot-separated segments.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DefinitionId(String);

impl DefinitionId {
    /// The maximum accepted length.
    pub const MAX_LEN: usize = 96;

    /// Validate and create a definition key.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] when the key is empty, too long,
    /// uses uppercase or unsupported characters, or has empty segments.
    pub fn new(value: impl Into<String>) -> Result<Self, EngineError> {
        let value = value.into();
        let invalid = |reason: &str| {
            EngineError::new(ErrorCode::InvalidRequest, "invalid definition id")
                .with_detail("reason", reason)
                .with_detail("value", &value)
        };

        if value.is_empty() {
            return Err(invalid("must not be empty"));
        }
        if value.len() > Self::MAX_LEN {
            return Err(invalid("must be at most 96 characters"));
        }
        if !value.as_bytes()[0].is_ascii_lowercase() && !value.as_bytes()[0].is_ascii_digit() {
            return Err(invalid("must start with a lowercase letter or digit"));
        }
        if value.ends_with('.') {
            return Err(invalid("must not end with a dot"));
        }
        if value.contains("..") {
            return Err(invalid("must not contain empty segments"));
        }
        if let Some(bad) = value.chars().find(|c| {
            !(c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
        }) {
            return Err(invalid(&format!("unsupported character {bad:?}")));
        }

        Ok(Self(value))
    }

    /// The key as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The first namespace segment, for example `door` in `door.wooden`.
    #[must_use]
    pub fn namespace(&self) -> Option<&str> {
        self.0.split_once('.').map(|(namespace, _)| namespace)
    }

    /// Whether the key is inside `namespace`.
    #[must_use]
    pub fn is_in(&self, namespace: &str) -> bool {
        self.namespace() == Some(namespace)
    }
}

impl fmt::Display for DefinitionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for DefinitionId {
    type Err = EngineError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::new(input)
    }
}

impl Serialize for DefinitionId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for DefinitionId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct KeyVisitor;

        impl Visitor<'_> for KeyVisitor {
            type Value = DefinitionId;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a lowercase definition key such as door.wooden")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                DefinitionId::new(value).map_err(|error| E::custom(error.message()))
            }
        }

        deserializer.deserialize_str(KeyVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_ids_are_stable_and_ordered() {
        let mut ids = DeterministicIdGenerator::new(1);
        let first = ids.generate(IdKind::Entity);
        let second = ids.generate(IdKind::Entity);
        assert_eq!(first.kind(), IdKind::Entity);
        assert_eq!(first.timestamp_ms(), 0);
        assert!(first < second);
        assert_eq!(
            first.to_string(),
            parse_round_trip(&first.to_string()).to_string()
        );
    }

    #[test]
    fn system_generator_is_monotonic_within_a_millisecond() {
        let mut generator = SystemIdGenerator {
            last_ms: 0,
            last_random: 0,
        };
        let a = generator.generate_at(IdKind::Character, 1_700_000_000_000);
        let b = generator.generate_at(IdKind::Character, 1_700_000_000_000);
        let c = generator.generate_at(IdKind::Character, 1_700_000_000_000);
        assert!(a < b && b < c);
        assert_eq!(a.timestamp_ms(), 1_700_000_000_000);
        assert_eq!(b.random_bits(), a.random_bits() + 1);
    }

    #[test]
    fn system_generator_never_goes_backwards_when_the_clock_does() {
        let mut generator = SystemIdGenerator {
            last_ms: 0,
            last_random: 0,
        };
        let a = generator.generate_at(IdKind::Zone, 2_000);
        let b = generator.generate_at(IdKind::Zone, 1_000);
        assert_eq!(b.timestamp_ms(), 2_000);
        assert!(a < b);
    }

    #[test]
    fn id_prefixes_are_preserved_and_checked() {
        let id = StableId::from_parts(IdKind::Item, 5, 7);
        assert!(id.to_string().starts_with("itm_"));
        assert!(id.expect_kind(IdKind::Item).is_ok());
        let error = id.expect_kind(IdKind::Entity).unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidRequest);
    }

    #[test]
    fn parsing_rejects_unknown_prefixes_lengths_and_characters() {
        let error = StableId::parse("bad_01J8Z3K4Q7Y2N5V6B8C0D1E2F3").unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidRequest);

        let id = StableId::from_parts(IdKind::Entity, 1, 2).to_string();
        assert!(StableId::parse(&id).is_ok());
        assert!(StableId::parse(&format!("{id}0")).is_err());
        assert!(StableId::parse("ent_01J8Z3K4Q7Y2N5V6B8C0D1E2F!").is_err());
    }

    #[test]
    fn parsing_accepts_ambiguous_crockford_characters_and_lowercase() {
        let canonical = StableId::from_parts(IdKind::Entity, 42, 12345).to_string();
        let lowered = canonical.to_ascii_lowercase();
        assert_eq!(
            StableId::parse(&lowered).unwrap(),
            StableId::parse(&canonical).unwrap()
        );
    }

    #[test]
    fn rejects_values_wider_than_128_bits() {
        let error = StableId::parse("ent_80000000000000000000000000").unwrap_err();
        assert!(error.message().contains("128 bits"));
    }

    #[test]
    fn definition_ids_are_validated() {
        for good in [
            "item.torch",
            "door.wooden",
            "a",
            "zone.town.square_1",
            "x-y_z",
        ] {
            assert!(DefinitionId::new(good).is_ok(), "{good} should be valid");
        }
        for bad in [
            "",
            "Door.Wooden",
            ".leading",
            "trailing.",
            "double..dot",
            "space key",
            "sla/sh",
        ] {
            assert!(DefinitionId::new(bad).is_err(), "{bad} should be invalid");
        }
        assert_eq!(
            DefinitionId::new("door.wooden").unwrap().namespace(),
            Some("door")
        );
        assert!(DefinitionId::new("door.wooden").unwrap().is_in("door"));
    }

    #[test]
    fn ids_round_trip_through_serde() {
        let id = StableId::from_parts(IdKind::Property, 1_700_000_000_123, 99);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{id}\""));
        let parsed: StableId = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, id);

        let key = DefinitionId::new("item.torch").unwrap();
        let json = serde_json::to_string(&key).unwrap();
        assert_eq!(json, "\"item.torch\"");
        assert_eq!(serde_json::from_str::<DefinitionId>(&json).unwrap(), key);
        assert!(serde_json::from_str::<DefinitionId>("\"Bad Key\"").is_err());
    }

    fn parse_round_trip(value: &str) -> StableId {
        StableId::parse(value).unwrap()
    }
}
