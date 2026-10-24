//! Protocol versioning, handshake negotiation, and server messages.
//!
//! Player-facing failures travel as stable codes and parameters. The client
//! renders them from its locale catalog; Brazilian Portuguese is the default.

use std::collections::{BTreeMap, BTreeSet};

use engine_core::error::{EngineError, ErrorCode};
use engine_core::i18n::{Locale, MessageKey};
use serde::{Deserialize, Serialize};

/// The protocol semantic version owned by this crate.
pub const PROTOCOL_VERSION: &str = "1.0.0";

/// The protocol major version. A mismatch is rejected.
pub const PROTOCOL_MAJOR: u16 = 1;

/// The protocol minor version. A mismatch is accepted with degradation.
pub const PROTOCOL_MINOR: u16 = 0;

/// The protocol patch version. Informational only.
pub const PROTOCOL_PATCH: u16 = 0;

/// The protocol version as a semantic version value.
#[must_use]
pub fn protocol_version() -> semver::Version {
    semver::Version::new(
        u64::from(PROTOCOL_MAJOR),
        u64::from(PROTOCOL_MINOR),
        u64::from(PROTOCOL_PATCH),
    )
}

/// Optional capabilities negotiated at handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    /// Client-side movement prediction and reconciliation.
    MovementPrediction,
    /// Interest-managed entity replication.
    InterestManagement,
    /// Localized content catalogs and key-based text.
    LocalizedContent,
    /// Player chat channels.
    Chat,
    /// Groups and guilds.
    Groups,
    /// Matchmaking queues.
    Matchmaking,
    /// Admin diagnostics and spectator tools.
    AdminConsole,
}

/// A set of negotiated features.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FeatureSet(BTreeSet<Feature>);

impl FeatureSet {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a feature and report whether it was newly inserted.
    pub fn insert(&mut self, feature: Feature) -> bool {
        self.0.insert(feature)
    }

    /// Remove a feature and report whether it was present.
    pub fn remove(&mut self, feature: Feature) -> bool {
        self.0.remove(&feature)
    }

    /// Whether the set contains a feature.
    #[must_use]
    pub fn contains(&self, feature: Feature) -> bool {
        self.0.contains(&feature)
    }

    /// Features present in both sets.
    #[must_use]
    pub fn intersection(&self, other: &Self) -> Self {
        Self(self.0.intersection(&other.0).copied().collect())
    }

    /// Number of features.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Iterate in stable order.
    pub fn iter(&self) -> impl Iterator<Item = Feature> + '_ {
        self.0.iter().copied()
    }
}

impl FromIterator<Feature> for FeatureSet {
    fn from_iter<T: IntoIterator<Item = Feature>>(iter: T) -> Self {
        Self(iter.into_iter().collect())
    }
}

/// The first message of a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handshake {
    /// Client or server protocol version.
    pub protocol: semver::Version,
    /// Engine build version, for diagnostics.
    pub engine: semver::Version,
    /// Content bundle version in use.
    pub content: semver::Version,
    /// Requested locale. Defaults to `pt-BR`.
    #[serde(default)]
    pub locale: Locale,
    /// Advertised capabilities.
    #[serde(default)]
    pub features: FeatureSet,
}

impl Handshake {
    /// A handshake with the current protocol version and default locale.
    #[must_use]
    pub fn new(engine: semver::Version, content: semver::Version) -> Self {
        Self {
            protocol: protocol_version(),
            engine,
            content,
            locale: Locale::default(),
            features: FeatureSet::new(),
        }
    }

    /// Set the requested locale.
    #[must_use]
    pub fn with_locale(mut self, locale: Locale) -> Self {
        self.locale = locale;
        self
    }

    /// Advertise features.
    #[must_use]
    pub fn with_features(mut self, features: FeatureSet) -> Self {
        self.features = features;
        self
    }
}

/// The server's accepted session parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Negotiated {
    /// Effective protocol version.
    pub protocol: semver::Version,
    /// Effective content version.
    pub content: semver::Version,
    /// Effective locale.
    pub locale: Locale,
    /// Features enabled for this session.
    pub features: FeatureSet,
    /// Whether any requested feature was disabled.
    pub degraded: bool,
    /// Non-fatal compatibility notes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Negotiate a client handshake against the server's capabilities.
///
/// # Errors
///
/// Returns [`ErrorCode::VersionMismatch`] when protocol or content major
/// versions differ.
pub fn negotiate(client: &Handshake, server: &Handshake) -> Result<Negotiated, EngineError> {
    if client.protocol.major != server.protocol.major {
        return Err(version_mismatch(
            "protocol",
            &client.protocol,
            &server.protocol,
        ));
    }
    if client.content.major != server.content.major {
        return Err(version_mismatch(
            "content",
            &client.content,
            &server.content,
        ));
    }

    let mut warnings = Vec::new();
    if client.protocol.minor != server.protocol.minor {
        warnings.push(format!(
            "protocol minor version differs (client {}, server {})",
            client.protocol, server.protocol
        ));
    }
    if client.content.minor != server.content.minor {
        warnings.push(format!(
            "content minor version differs (client {}, server {})",
            client.content, server.content
        ));
    }
    if client.engine.major != server.engine.major {
        warnings.push(format!(
            "engine major version differs (client {}, server {})",
            client.engine, server.engine
        ));
    }

    let features = client.features.intersection(&server.features);
    let degraded = features != client.features;
    if degraded {
        warnings.push("one or more requested features are unavailable".to_string());
    }

    Ok(Negotiated {
        protocol: server.protocol.clone(),
        content: server.content.clone(),
        locale: client.locale.clone(),
        features,
        degraded,
        warnings,
    })
}

fn version_mismatch(what: &str, client: &semver::Version, server: &semver::Version) -> EngineError {
    EngineError::new(
        ErrorCode::VersionMismatch,
        format!("incompatible {what} version"),
    )
    .with_detail("kind", what)
    .with_detail("client", client.to_string())
    .with_detail("server", server.to_string())
}

/// Player-facing text described by a key and typed parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalizedNotice {
    /// Catalog key, for example `chat.player_joined`.
    pub key: MessageKey,
    /// Placeholder values, as strings on the wire.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, String>,
}

impl LocalizedNotice {
    /// Create a notice with no parameters.
    #[must_use]
    pub fn new(key: MessageKey) -> Self {
        Self {
            key,
            params: BTreeMap::new(),
        }
    }

    /// Attach a placeholder value.
    #[must_use]
    pub fn with_param(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.params.insert(name.into(), value.into());
        self
    }
}

/// A server error on the wire. The client renders `error.<code>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorPayload {
    /// Stable error category.
    pub code: ErrorCode,
    /// Safe typed parameters for the client message.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, String>,
    /// English diagnostic for logs. Clients must not display it as-is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
}

impl From<&EngineError> for ErrorPayload {
    fn from(error: &EngineError) -> Self {
        Self {
            code: error.code(),
            params: error.details().clone(),
            diagnostic: Some(error.message().to_string()),
        }
    }
}

impl From<EngineError> for ErrorPayload {
    fn from(error: EngineError) -> Self {
        Self::from(&error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version() -> semver::Version {
        semver::Version::new(0, 1, 0)
    }

    fn features(items: impl IntoIterator<Item = Feature>) -> FeatureSet {
        items.into_iter().collect()
    }

    #[test]
    fn protocol_constants_are_consistent() {
        assert_eq!(PROTOCOL_VERSION, "1.0.0");
        assert_eq!(protocol_version(), semver::Version::new(1, 0, 0));
    }

    #[test]
    fn major_mismatches_are_rejected() {
        let server = Handshake::new(version(), version());
        let mut client = Handshake::new(version(), version());
        client.protocol = semver::Version::new(2, 0, 0);
        assert_eq!(
            negotiate(&client, &server).unwrap_err().code(),
            ErrorCode::VersionMismatch
        );

        let mut client = Handshake::new(version(), version());
        client.content = semver::Version::new(1, 0, 0);
        assert_eq!(
            negotiate(&client, &server).unwrap_err().code(),
            ErrorCode::VersionMismatch
        );
    }

    #[test]
    fn minor_mismatches_are_accepted_with_warnings() {
        let server = Handshake::new(version(), version());
        let mut client = Handshake::new(version(), version());
        client.protocol = semver::Version::new(1, 4, 0);
        let negotiated = negotiate(&client, &server).unwrap();
        assert_eq!(negotiated.protocol, server.protocol);
        assert_eq!(negotiated.warnings.len(), 1);
    }

    #[test]
    fn feature_negotiation_is_an_intersection() {
        let mut server = Handshake::new(version(), version());
        server.features = features([Feature::Chat, Feature::LocalizedContent]);
        let mut client = Handshake::new(version(), version());
        client.features = features([Feature::Chat, Feature::AdminConsole]);

        let negotiated = negotiate(&client, &server).unwrap();
        assert_eq!(negotiated.features, features([Feature::Chat]));
        assert!(negotiated.degraded);
        assert_eq!(negotiated.locale.as_str(), "pt-BR");
    }

    #[test]
    fn localized_notice_and_error_payload_round_trip() {
        let notice = LocalizedNotice::new(MessageKey::parse("chat.player_joined").unwrap())
            .with_param("player", "Ana");
        let json = serde_json::to_string(&notice).unwrap();
        assert!(json.contains("chat.player_joined"));
        assert!(
            !json.contains("pt-BR"),
            "notices carry keys and parameters, not localized text"
        );
        assert_eq!(
            serde_json::from_str::<LocalizedNotice>(&json).unwrap(),
            notice
        );

        let error = EngineError::new(ErrorCode::OutOfRange, "target is too far")
            .with_detail("distance_m", "12.5");
        let payload = ErrorPayload::from(&error);
        assert_eq!(payload.code, ErrorCode::OutOfRange);
        assert_eq!(payload.params["distance_m"], "12.5");
        assert!(payload.diagnostic.is_some());
    }
}
