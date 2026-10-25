//! Content bundles, manifests, and SHA-256 hashing.
//!
//! A bundle is a directory plus a manifest. The manifest records the content
//! version, engine compatibility, per-file hashes, and the locales included.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use engine_core::i18n::{DEFAULT_LOCALE, Locale};
use engine_core::id::DefinitionId;
use engine_core::schema::SchemaVersion;
use engine_core::{EngineError, ErrorCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::definition::{
    ContentDefinition, ContentKind, MinigameDefinition, UiPanelDefinition, is_safe_asset_path,
};
use crate::diagnostic::ValidationReport;
use crate::publishing::{ModerationState, PublishingRecord};

/// Hash bytes with SHA-256 and return lowercase hexadecimal.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

/// Whether a string is a 64-character SHA-256 hex digest.
#[must_use]
pub fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// One definition file in a bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefinitionEntry {
    /// Path relative to the bundle root, using forward slashes.
    pub path: String,
    /// Definition kind.
    pub kind: ContentKind,
    /// Definition ID.
    pub id: DefinitionId,
    /// Definition version.
    pub version: SchemaVersion,
    /// SHA-256 of the file bytes.
    pub sha256: String,
}

/// One localization catalog in a bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocaleEntry {
    /// Locale tag.
    pub locale: Locale,
    /// Path relative to the bundle root, using forward slashes.
    pub path: String,
    /// SHA-256 of the catalog bytes.
    pub sha256: String,
}

/// The bundle manifest written as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentManifest {
    /// Monotonic content version.
    pub content_version: semver::Version,
    /// Compatible engine versions.
    pub engine: semver::VersionReq,
    /// Definition files.
    pub definitions: Vec<DefinitionEntry>,
    /// Localization catalogs. The `pt-BR` source catalog must be present.
    pub locales: Vec<LocaleEntry>,
    /// Ownership and moderation metadata for user content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publishing: Option<PublishingRecord>,
}

impl ContentManifest {
    /// Create an empty manifest.
    #[must_use]
    pub fn new(content_version: semver::Version, engine: semver::VersionReq) -> Self {
        Self {
            content_version,
            engine,
            definitions: Vec::new(),
            locales: Vec::new(),
            publishing: None,
        }
    }

    /// Attach ownership and moderation metadata for user content.
    #[must_use]
    pub fn with_publishing(mut self, record: PublishingRecord) -> Self {
        self.publishing = Some(record);
        self
    }

    /// Record a definition file.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::Conflict`] for a duplicate path or definition ID,
    /// and [`ErrorCode::InvalidRequest`] for an unsafe path.
    pub fn add_definition(
        &mut self,
        path: impl Into<String>,
        definition: &ContentDefinition,
        bytes: &[u8],
    ) -> Result<(), EngineError> {
        self.add_entry(
            path.into(),
            definition.kind(),
            definition.id.clone(),
            definition.version,
            bytes,
        )
    }

    /// Record a UI panel file.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::Conflict`] for a duplicate path or definition ID,
    /// and [`ErrorCode::InvalidRequest`] for an unsafe path.
    pub fn add_panel(
        &mut self,
        path: impl Into<String>,
        panel: &UiPanelDefinition,
        bytes: &[u8],
    ) -> Result<(), EngineError> {
        self.add_entry(
            path.into(),
            ContentKind::Ui,
            panel.id.clone(),
            panel.version,
            bytes,
        )
    }

    /// Record a minigame file.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::Conflict`] for a duplicate path or definition ID,
    /// and [`ErrorCode::InvalidRequest`] for an unsafe path.
    pub fn add_minigame(
        &mut self,
        path: impl Into<String>,
        minigame: &MinigameDefinition,
        bytes: &[u8],
    ) -> Result<(), EngineError> {
        self.add_entry(
            path.into(),
            ContentKind::Minigame,
            minigame.id.clone(),
            minigame.version,
            bytes,
        )
    }

    pub(crate) fn add_entry(
        &mut self,
        path: String,
        kind: ContentKind,
        id: DefinitionId,
        version: SchemaVersion,
        bytes: &[u8],
    ) -> Result<(), EngineError> {
        if !is_safe_asset_path(&path) {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "definition path must be relative and must not contain '..'",
            )
            .with_detail("path", &path));
        }
        if self.definitions.iter().any(|entry| entry.path == path) {
            return Err(
                EngineError::new(ErrorCode::Conflict, "duplicate definition path")
                    .with_detail("path", &path),
            );
        }
        if self.definitions.iter().any(|entry| entry.id == id) {
            return Err(
                EngineError::new(ErrorCode::Conflict, "duplicate definition id")
                    .with_detail("id", id.to_string())
                    .with_detail("path", &path),
            );
        }
        self.definitions.push(DefinitionEntry {
            path,
            kind,
            id,
            version,
            sha256: sha256_hex(bytes),
        });
        Ok(())
    }

    /// Record a localization catalog.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::Conflict`] for a duplicate path or locale, and
    /// [`ErrorCode::InvalidRequest`] for an unsafe path.
    pub fn add_locale(
        &mut self,
        path: impl Into<String>,
        locale: Locale,
        bytes: &[u8],
    ) -> Result<(), EngineError> {
        let path = path.into();
        if !is_safe_asset_path(&path) {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "catalog path must be relative and must not contain '..'",
            )
            .with_detail("path", &path));
        }
        if self.locales.iter().any(|entry| entry.path == path) {
            return Err(
                EngineError::new(ErrorCode::Conflict, "duplicate catalog path")
                    .with_detail("path", &path),
            );
        }
        if self.locales.iter().any(|entry| entry.locale == locale) {
            return Err(
                EngineError::new(ErrorCode::Conflict, "duplicate locale catalog")
                    .with_detail("locale", locale.to_string()),
            );
        }
        self.locales.push(LocaleEntry {
            locale,
            path,
            sha256: sha256_hex(bytes),
        });
        Ok(())
    }

    /// Find a definition entry by ID.
    #[must_use]
    pub fn definition(&self, id: &DefinitionId) -> Option<&DefinitionEntry> {
        self.definitions.iter().find(|entry| entry.id == *id)
    }

    /// Find a locale entry by tag.
    #[must_use]
    pub fn locale(&self, locale: &Locale) -> Option<&LocaleEntry> {
        self.locales.iter().find(|entry| entry.locale == *locale)
    }

    /// Validate internal consistency. File hashes and paths are verified, but
    /// the bundle directory is not read here.
    #[must_use]
    pub fn validate(&self) -> ValidationReport {
        let mut report = ValidationReport::new();
        if self.definitions.is_empty() {
            report.warning(
                "content.empty_bundle",
                "manifest",
                "bundle contains no definition files",
            );
        }

        let mut paths = BTreeSet::new();
        let mut ids = BTreeSet::new();
        for entry in &self.definitions {
            if !is_safe_asset_path(&entry.path) {
                report.error(
                    "content.invalid_path",
                    &entry.path,
                    "definition path must be relative and must not contain '..'",
                );
            }
            if !paths.insert(entry.path.as_str()) {
                report.error(
                    "content.duplicate_path",
                    &entry.path,
                    "two definition entries use the same path",
                );
            }
            if !ids.insert(entry.id.clone()) {
                report.error(
                    "content.duplicate_id",
                    entry.id.to_string(),
                    "two definition entries use the same ID",
                );
            }
            if !is_sha256_hex(&entry.sha256) {
                report.error(
                    "content.invalid_hash",
                    &entry.path,
                    "sha256 must be 64 hexadecimal characters",
                );
            }
            if entry.kind != ContentKind::infer(&entry.id) {
                report.warning(
                    "content.kind_mismatch",
                    &entry.path,
                    format!(
                        "manifest kind {} does not match the ID namespace ({})",
                        entry.kind,
                        ContentKind::infer(&entry.id)
                    ),
                );
            }
        }

        let mut locale_paths = BTreeSet::new();
        let mut locales = BTreeSet::new();
        for entry in &self.locales {
            if !is_safe_asset_path(&entry.path) {
                report.error(
                    "content.invalid_path",
                    &entry.path,
                    "catalog path must be relative and must not contain '..'",
                );
            }
            if !locale_paths.insert(entry.path.as_str()) {
                report.error(
                    "content.duplicate_path",
                    &entry.path,
                    "two locale entries use the same path",
                );
            }
            if !locales.insert(entry.locale.clone()) {
                report.error(
                    "content.duplicate_locale",
                    entry.locale.to_string(),
                    "two locale entries use the same locale",
                );
            }
            if !is_sha256_hex(&entry.sha256) {
                report.error(
                    "content.invalid_hash",
                    &entry.path,
                    "sha256 must be 64 hexadecimal characters",
                );
            }
        }

        let default_locale = Locale::default();
        if self.locale(&default_locale).is_none() {
            report.error(
                "content.missing_default_locale",
                "locales",
                format!("the default source catalog ({DEFAULT_LOCALE}) is required"),
            );
        }

        if let Some(publishing) = &self.publishing {
            validate_publishing(publishing, &mut report);
        }

        report
    }

    /// Serialize the manifest as pretty JSON.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::Internal`] if serialization fails.
    pub fn to_json_pretty(&self) -> Result<String, EngineError> {
        serde_json::to_string_pretty(self).map_err(Into::into)
    }

    /// Parse a manifest from JSON.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ContentInvalid`] when the JSON is malformed or does
    /// not match the manifest schema.
    pub fn from_json_str(text: &str) -> Result<Self, EngineError> {
        serde_json::from_str(text).map_err(|error| {
            EngineError::new(ErrorCode::ContentInvalid, "invalid content manifest")
                .with_detail("error", error.to_string())
        })
    }

    /// Read a manifest from disk.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ContentInvalid`] when the file cannot be read or
    /// parsed.
    pub fn read(path: impl AsRef<Path>) -> Result<Self, EngineError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|error| {
            EngineError::new(ErrorCode::ContentInvalid, "failed to read content manifest")
                .with_detail("path", path.display().to_string())
                .with_detail("error", error.to_string())
        })?;
        Self::from_json_str(&text)
    }

    /// Write the manifest as pretty JSON, creating its parent directory.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::Internal`] when the file cannot be written.
    pub fn write(&self, path: impl AsRef<Path>) -> Result<(), EngineError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                EngineError::new(ErrorCode::Internal, "failed to create manifest directory")
                    .with_detail("path", parent.display().to_string())
                    .with_detail("error", error.to_string())
            })?;
        }
        let json = self.to_json_pretty()?;
        std::fs::write(path, json).map_err(|error| {
            EngineError::new(ErrorCode::Internal, "failed to write content manifest")
                .with_detail("path", path.display().to_string())
                .with_detail("error", error.to_string())
        })
    }

    /// The manifest path inside a bundle directory.
    #[must_use]
    pub fn path_in(root: impl AsRef<Path>) -> PathBuf {
        root.as_ref().join("manifest.json")
    }
}

/// Check moderation metadata for internal consistency.
fn validate_publishing(publishing: &PublishingRecord, report: &mut ValidationReport) {
    if matches!(
        publishing.state,
        ModerationState::Approved | ModerationState::Rejected
    ) && publishing.reviewer.is_none()
    {
        report.error(
            "content.publishing.missing_reviewer",
            "publishing",
            "approved or rejected content must record the reviewing moderator",
        );
    }

    if publishing.state == ModerationState::Rejected && publishing.note.is_none() {
        report.error(
            "content.publishing.missing_rejection_note",
            "publishing",
            "rejected content must include a moderation note",
        );
    }

    if publishing.reviewer.is_some()
        && !matches!(
            publishing.state,
            ModerationState::Approved | ModerationState::Rejected
        )
    {
        report.error(
            "content.publishing.reviewer_without_review",
            "publishing",
            "a reviewer is recorded for content that is not approved or rejected",
        );
    }
    if publishing.note.is_some() && publishing.state != ModerationState::Rejected {
        report.error(
            "content.publishing.note_without_rejection",
            "publishing",
            "a moderation note is recorded for content that is not rejected",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::{
        DisplayText, MinigameDefinition, Presentation, PresentationMode, UiPanelDefinition,
    };
    use crate::publishing::{AuthorId, LicenseId, ReviewNote};
    use engine_core::i18n::MessageKey;
    use engine_core::schema::ComponentKey;
    use std::collections::{BTreeMap, BTreeSet};

    fn key(value: &str) -> MessageKey {
        MessageKey::parse(value).unwrap()
    }

    fn definition() -> ContentDefinition {
        let mut components = BTreeMap::new();
        components.insert(
            ComponentKey::new("core.interactable").unwrap(),
            serde_json::Map::new(),
        );
        ContentDefinition {
            id: DefinitionId::new("door.wooden").unwrap(),
            version: SchemaVersion::new(1, 0),
            display: DisplayText {
                name: key("entity.door.wooden.name"),
                description: key("entity.door.wooden.description"),
            },
            components,
            presentation: Some(Presentation {
                mode: PresentationMode::IconAbstract,
                model: None,
                portrait: None,
                animation_set: None,
            }),
            tags: BTreeSet::new(),
        }
    }

    fn manifest() -> ContentManifest {
        ContentManifest::new(
            semver::Version::new(0, 1, 0),
            semver::VersionReq::parse("^0.1").unwrap(),
        )
    }

    #[test]
    fn sha256_matches_a_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(is_sha256_hex(&sha256_hex(b"abc")));
        assert!(!is_sha256_hex("abc"));
        assert!(!is_sha256_hex(&"z".repeat(64)));
    }

    #[test]
    fn entries_are_hashed_and_deduplicated() {
        let definition = definition();
        let mut manifest = manifest();
        manifest
            .add_definition("entities/door.ron", &definition, b"door")
            .unwrap();
        assert_eq!(manifest.definitions[0].kind, ContentKind::Entity);
        assert_eq!(manifest.definitions[0].sha256, sha256_hex(b"door"));

        assert_eq!(
            manifest
                .add_definition("entities/door.ron", &definition, b"other")
                .unwrap_err()
                .code(),
            ErrorCode::Conflict
        );

        let locale = Locale::default();
        manifest
            .add_locale("localization/pt-BR.ron", locale.clone(), b"catalog")
            .unwrap();
        assert_eq!(
            manifest
                .add_locale("localization/other.ron", locale, b"catalog")
                .unwrap_err()
                .code(),
            ErrorCode::Conflict
        );
    }

    #[test]
    fn validation_requires_the_default_catalog_and_valid_hashes() {
        let mut manifest = manifest();
        manifest
            .add_definition("entities/door.ron", &definition(), b"door")
            .unwrap();
        manifest.definitions[0].sha256 = "not-a-hash".into();
        let report = manifest.validate();
        let codes: Vec<_> = report
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect();
        assert!(codes.contains(&"content.invalid_hash"));
        assert!(codes.contains(&"content.missing_default_locale"));

        manifest
            .add_locale("localization/pt-BR.ron", Locale::default(), b"catalog")
            .unwrap();
        manifest.definitions[0].sha256 = sha256_hex(b"door");
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn manifest_round_trips_through_json() {
        let mut manifest = manifest();
        manifest
            .add_definition("entities/door.ron", &definition(), b"door")
            .unwrap();
        manifest
            .add_locale("localization/pt-BR.ron", Locale::default(), b"catalog")
            .unwrap();
        let json = manifest.to_json_pretty().unwrap();
        let parsed = ContentManifest::from_json_str(&json).unwrap();
        assert_eq!(parsed, manifest);
    }

    #[test]
    fn publishing_metadata_round_trips_and_is_optional() {
        let mut manifest = manifest();
        manifest
            .add_definition("entities/door.ron", &definition(), b"door")
            .unwrap();
        manifest
            .add_locale("localization/pt-BR.ron", Locale::default(), b"catalog")
            .unwrap();
        let json = manifest.to_json_pretty().unwrap();
        assert!(!json.contains("\"publishing\""));
        assert!(
            ContentManifest::from_json_str(&json)
                .unwrap()
                .publishing
                .is_none()
        );

        let record = PublishingRecord::new(AuthorId::new("alice").unwrap())
            .with_license(LicenseId::new("CC-BY-4.0").unwrap());
        let manifest = manifest.with_publishing(record);
        let json = manifest.to_json_pretty().unwrap();
        assert!(json.contains("\"publishing\""));
        assert_eq!(ContentManifest::from_json_str(&json).unwrap(), manifest);
    }

    #[test]
    fn publishing_records_reject_inconsistent_reviewer_and_note() {
        let mut manifest = manifest();
        manifest
            .add_definition("entities/door.ron", &definition(), b"door")
            .unwrap();
        manifest
            .add_locale("localization/pt-BR.ron", Locale::default(), b"catalog")
            .unwrap();

        let mut record = PublishingRecord::new(AuthorId::new("alice").unwrap());
        record.reviewer = Some(AuthorId::new("moderator").unwrap());
        record.note = Some(ReviewNote::new("missing collision").unwrap());
        let report = manifest.with_publishing(record).validate();
        let codes: Vec<_> = report
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect();
        assert!(codes.contains(&"content.publishing.reviewer_without_review"));
        assert!(codes.contains(&"content.publishing.note_without_rejection"));
    }

    #[test]
    fn publishing_records_require_reviewer_and_note_when_needed() {
        let mut manifest = manifest();
        manifest
            .add_definition("entities/door.ron", &definition(), b"door")
            .unwrap();
        manifest
            .add_locale("localization/pt-BR.ron", Locale::default(), b"catalog")
            .unwrap();

        let mut approved = PublishingRecord::new(AuthorId::new("alice").unwrap());
        approved.state = ModerationState::Approved;
        let approved_report = manifest.clone().with_publishing(approved).validate();
        let approved_codes: Vec<_> = approved_report
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect();
        assert!(approved_codes.contains(&"content.publishing.missing_reviewer"));

        let mut rejected = PublishingRecord::new(AuthorId::new("alice").unwrap());
        rejected.state = ModerationState::Rejected;
        rejected.reviewer = Some(AuthorId::new("moderator").unwrap());
        let rejected_report = manifest.clone().with_publishing(rejected).validate();
        let rejected_codes: Vec<_> = rejected_report
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect();
        assert!(rejected_codes.contains(&"content.publishing.missing_rejection_note"));
    }

    #[test]
    fn unsafe_manifest_paths_are_rejected() {
        let mut manifest = manifest();
        assert_eq!(
            manifest
                .add_definition("../door.ron", &definition(), b"door")
                .unwrap_err()
                .code(),
            ErrorCode::InvalidRequest
        );
    }

    #[test]
    fn panels_and_minigames_are_recorded_with_their_kinds() {
        let panel = UiPanelDefinition {
            id: DefinitionId::new("ui.terminal").unwrap(),
            version: SchemaVersion::new(1, 0),
            display: DisplayText {
                name: key("ui.terminal.title"),
                description: key("ui.terminal.description"),
            },
            schema: "ui.terminal".to_string(),
            actions: Vec::new(),
            fields: Vec::new(),
            fallback_schema: Some("ui.inspect".to_string()),
        };
        let minigame = MinigameDefinition {
            id: DefinitionId::new("minigame.lockpick").unwrap(),
            version: SchemaVersion::new(1, 0),
            display: DisplayText {
                name: key("minigame.lockpick.name"),
                description: key("minigame.lockpick.description"),
            },
            panel_schema: "ui.lockpick".to_string(),
            authority: "server".to_string(),
            rewards: Vec::new(),
        };

        let mut manifest = manifest();
        manifest
            .add_panel("ui/terminal.ron", &panel, b"panel")
            .unwrap();
        manifest
            .add_minigame("minigames/lockpick.ron", &minigame, b"game")
            .unwrap();

        assert_eq!(manifest.definitions[0].kind, ContentKind::Ui);
        assert_eq!(manifest.definitions[1].kind, ContentKind::Minigame);
        assert_eq!(
            manifest
                .add_panel("ui/duplicate.ron", &panel, b"panel")
                .unwrap_err()
                .code(),
            ErrorCode::Conflict
        );
    }
}
