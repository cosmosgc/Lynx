//! Content manifest diffs and version compatibility.
//!
//! A bundle manifest records the content version, engine requirement, and one
//! hash per definition and locale. Publishing compares the manifest a client
//! already has with the candidate manifest: removed definitions and changed
//! kinds are breaking, additions are additive, and any byte change must bump
//! the affected definition and the content version.
//!
//! The rules are deliberately conservative. A diff is a publish gate, not a
//! merge tool: it reports what changed and the smallest safe version bump, and
//! it refuses to certify a manifest whose declared versions are too small.

use std::collections::BTreeMap;

use engine_core::i18n::Locale;
use engine_core::id::DefinitionId;
use serde::{Deserialize, Serialize};

use crate::bundle::{ContentManifest, DefinitionEntry};
use crate::diagnostic::ValidationReport;

/// The smallest semantic version bump a change set requires.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum VersionBump {
    /// No content change.
    #[default]
    None,
    /// Documentation or metadata only; no definition bytes changed.
    Patch,
    /// Additive, backward-compatible content.
    Minor,
    /// Breaking removal, kind change, or locale removal.
    Major,
}

impl VersionBump {
    /// The wire and report name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Patch => "patch",
            Self::Minor => "minor",
            Self::Major => "major",
        }
    }

    /// Classify the bump between two content versions.
    ///
    /// A version regression returns [`VersionBump::None`]; the diff reports it
    /// as an error separately.
    #[must_use]
    pub fn classify(base: &semver::Version, next: &semver::Version) -> Self {
        if next < base {
            Self::None
        } else if next.major > base.major {
            Self::Major
        } else if next.minor > base.minor {
            Self::Minor
        } else if next.patch > base.patch {
            Self::Patch
        } else {
            Self::None
        }
    }
}

impl std::fmt::Display for VersionBump {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One definition whose bytes or metadata changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefinitionChange {
    /// Stable definition ID.
    pub id: DefinitionId,
    /// Entry in the base manifest.
    pub from: DefinitionEntry,
    /// Entry in the candidate manifest.
    pub to: DefinitionEntry,
}

/// The result of comparing two content manifests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentDiff {
    /// Base content version.
    pub base_version: semver::Version,
    /// Candidate content version.
    pub next_version: semver::Version,
    /// Bump actually declared by the candidate.
    pub actual_bump: VersionBump,
    /// Smallest bump the change set requires.
    pub required_bump: VersionBump,
    /// Definitions present only in the candidate.
    pub added: Vec<DefinitionEntry>,
    /// Definitions present only in the base.
    pub removed: Vec<DefinitionEntry>,
    /// Definitions present in both with different bytes or metadata.
    pub changed: Vec<DefinitionChange>,
    /// Locales added by the candidate.
    pub added_locales: Vec<Locale>,
    /// Locales removed by the candidate.
    pub removed_locales: Vec<Locale>,
    /// Locales present in both with a different catalog hash.
    pub changed_locales: Vec<Locale>,
    /// Compatibility diagnostics.
    pub report: ValidationReport,
}

impl ContentDiff {
    /// Whether the candidate is a valid successor to the base.
    #[must_use]
    pub fn is_compatible(&self) -> bool {
        self.report.is_ok()
    }

    /// Whether any definition or locale changed.
    #[must_use]
    pub fn has_changes(&self) -> bool {
        !self.added.is_empty()
            || !self.removed.is_empty()
            || !self.changed.is_empty()
            || !self.added_locales.is_empty()
            || !self.removed_locales.is_empty()
            || !self.changed_locales.is_empty()
    }
}

/// Compare a base manifest with a candidate manifest.
///
/// The result always describes the full change set. Version problems are
/// reported as diagnostics instead of panics or silent acceptance, so a
/// publisher can show the exact rule that failed.
#[must_use]
pub fn diff(base: &ContentManifest, next: &ContentManifest) -> ContentDiff {
    let mut report = ValidationReport::new();

    if next.content_version < base.content_version {
        report.error(
            "content.diff.version_regressed",
            "manifest",
            format!(
                "candidate version {} is older than base version {}",
                next.content_version, base.content_version
            ),
        );
    }

    let (added, removed, changed, definition_bump) = diff_definitions(base, next, &mut report);
    let (added_locales, removed_locales, changed_locales, locale_bump) =
        diff_locales(base, next, &mut report);
    let required = definition_bump.max(locale_bump);

    let actual = VersionBump::classify(&base.content_version, &next.content_version);
    if actual < required {
        report.error(
            "content.diff.version_bump_too_small",
            "manifest",
            format!(
                "content version {} -> {} is a {} bump, but the change set requires {}",
                base.content_version, next.content_version, actual, required
            ),
        );
    }
    if base.engine != next.engine {
        report.warning(
            "content.diff.engine_requirement_changed",
            "manifest",
            format!(
                "engine requirement changed from {} to {}",
                base.engine, next.engine
            ),
        );
    }

    ContentDiff {
        base_version: base.content_version.clone(),
        next_version: next.content_version.clone(),
        actual_bump: actual,
        required_bump: required,
        added,
        removed,
        changed,
        added_locales,
        removed_locales,
        changed_locales,
        report,
    }
}

/// Compare definition entries and return the change set plus its required bump.
fn diff_definitions(
    base: &ContentManifest,
    next: &ContentManifest,
    report: &mut ValidationReport,
) -> (
    Vec<DefinitionEntry>,
    Vec<DefinitionEntry>,
    Vec<DefinitionChange>,
    VersionBump,
) {
    let base_definitions: BTreeMap<&DefinitionId, &DefinitionEntry> = base
        .definitions
        .iter()
        .map(|entry| (&entry.id, entry))
        .collect();
    let next_definitions: BTreeMap<&DefinitionId, &DefinitionEntry> = next
        .definitions
        .iter()
        .map(|entry| (&entry.id, entry))
        .collect();

    let mut required = VersionBump::None;
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();

    for (id, from) in &base_definitions {
        let Some(to) = next_definitions.get(id) else {
            required = required.max(VersionBump::Major);
            report.warning(
                "content.diff.definition_removed",
                id.to_string(),
                "removing a definition is a breaking content change",
            );
            removed.push((*from).clone());
            continue;
        };

        required = required.max(compare_definition(id, from, to, report));

        if from.sha256 != to.sha256
            || from.version != to.version
            || from.kind != to.kind
            || from.path != to.path
        {
            changed.push(DefinitionChange {
                id: (*id).clone(),
                from: (*from).clone(),
                to: (*to).clone(),
            });
        }
    }

    for (id, entry) in &next_definitions {
        if !base_definitions.contains_key(*id) {
            required = required.max(VersionBump::Minor);
            added.push((*entry).clone());
        }
    }

    (added, removed, changed, required)
}

/// Compare one definition present in both manifests.
fn compare_definition(
    id: &DefinitionId,
    from: &DefinitionEntry,
    to: &DefinitionEntry,
    report: &mut ValidationReport,
) -> VersionBump {
    let mut required = VersionBump::None;

    if from.kind != to.kind {
        required = VersionBump::Major;
        report.warning(
            "content.diff.kind_changed",
            id.to_string(),
            format!("definition kind changed from {} to {}", from.kind, to.kind),
        );
    }

    if from.sha256 != to.sha256 {
        required = required.max(VersionBump::Minor);
        if to.version <= from.version {
            report.error(
                "content.diff.definition_version_not_bumped",
                id.to_string(),
                format!(
                    "definition bytes changed but version stayed at {}.{}",
                    to.version.major, to.version.minor
                ),
            );
        }
    } else if to.version > from.version {
        report.warning(
            "content.diff.definition_version_bumped_without_change",
            id.to_string(),
            "definition version increased without a byte change",
        );
    } else if to.version < from.version {
        report.error(
            "content.diff.definition_version_regressed",
            id.to_string(),
            "definition version moved backwards",
        );
    }

    if from.path != to.path {
        report.warning(
            "content.diff.path_changed",
            id.to_string(),
            format!("definition path moved from {} to {}", from.path, to.path),
        );
    }

    required
}

/// Compare locale entries and return the change set plus its required bump.
fn diff_locales(
    base: &ContentManifest,
    next: &ContentManifest,
    report: &mut ValidationReport,
) -> (Vec<Locale>, Vec<Locale>, Vec<Locale>, VersionBump) {
    let base_locales: BTreeMap<&Locale, &crate::bundle::LocaleEntry> = base
        .locales
        .iter()
        .map(|entry| (&entry.locale, entry))
        .collect();
    let next_locales: BTreeMap<&Locale, &crate::bundle::LocaleEntry> = next
        .locales
        .iter()
        .map(|entry| (&entry.locale, entry))
        .collect();

    let mut required = VersionBump::None;
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();

    for (locale, from) in &base_locales {
        match next_locales.get(locale) {
            None => {
                required = required.max(VersionBump::Major);
                report.warning(
                    "content.diff.locale_removed",
                    locale.to_string(),
                    "removing a locale catalog is a breaking content change",
                );
                removed.push((*locale).clone());
            }
            Some(to) => {
                if from.sha256 != to.sha256 {
                    required = required.max(VersionBump::Minor);
                    changed.push((*locale).clone());
                }
                if from.path != to.path {
                    report.warning(
                        "content.diff.locale_path_changed",
                        locale.to_string(),
                        format!("catalog path moved from {} to {}", from.path, to.path),
                    );
                }
            }
        }
    }

    for locale in next_locales.keys() {
        if !base_locales.contains_key(locale) {
            required = required.max(VersionBump::Minor);
            added.push((*locale).clone());
        }
    }

    (added, removed, changed, required)
}

#[cfg(test)]
mod tests {
    use engine_core::schema::SchemaVersion;

    use super::*;
    use crate::bundle::ContentManifest;

    fn version(major: u64, minor: u64, patch: u64) -> semver::Version {
        semver::Version::new(major, minor, patch)
    }

    fn engine() -> semver::VersionReq {
        semver::VersionReq::parse(">=0.1, <0.2").unwrap()
    }

    fn id(value: &str) -> DefinitionId {
        DefinitionId::new(value).unwrap()
    }

    fn manifest(version: semver::Version) -> ContentManifest {
        ContentManifest::new(version, engine())
    }

    fn add(
        manifest: &mut ContentManifest,
        path: &str,
        definition: &str,
        kind: crate::definition::ContentKind,
        schema: SchemaVersion,
        bytes: &[u8],
    ) {
        manifest
            .add_entry(path.to_string(), kind, id(definition), schema, bytes)
            .unwrap();
    }

    fn base_manifest() -> ContentManifest {
        let mut manifest = manifest(version(1, 0, 0));
        add(
            &mut manifest,
            "entities/door.ron",
            "prop.door",
            crate::definition::ContentKind::Entity,
            SchemaVersion::new(1, 0),
            b"door-v1",
        );
        manifest
            .add_locale("locales/pt-BR.ron", Locale::default(), b"pt-v1")
            .unwrap();
        manifest
    }

    #[test]
    fn identical_manifests_need_no_bump() {
        let base = base_manifest();
        let result = diff(&base, &base);
        assert!(result.is_compatible());
        assert!(!result.has_changes());
        assert_eq!(result.required_bump, VersionBump::None);
        assert_eq!(result.actual_bump, VersionBump::None);
    }

    #[test]
    fn adding_a_definition_requires_a_minor_bump() {
        let base = base_manifest();
        let mut next = base.clone();
        next.content_version = version(1, 1, 0);
        add(
            &mut next,
            "entities/barrel.ron",
            "prop.barrel",
            crate::definition::ContentKind::Entity,
            SchemaVersion::new(1, 0),
            b"barrel-v1",
        );

        let result = diff(&base, &next);
        assert!(result.is_compatible());
        assert_eq!(result.required_bump, VersionBump::Minor);
        assert_eq!(result.actual_bump, VersionBump::Minor);
        assert_eq!(result.added.len(), 1);
    }

    #[test]
    fn removing_a_definition_requires_a_major_bump() {
        let base = base_manifest();
        let mut next = manifest(version(2, 0, 0));
        next.add_locale("locales/pt-BR.ron", Locale::default(), b"pt-v1")
            .unwrap();

        let result = diff(&base, &next);
        assert!(result.is_compatible());
        assert_eq!(result.required_bump, VersionBump::Major);
        assert_eq!(result.removed.len(), 1);

        let mut too_small = next.clone();
        too_small.content_version = version(1, 1, 0);
        let result = diff(&base, &too_small);
        assert!(!result.is_compatible());
        assert!(
            result
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "content.diff.version_bump_too_small")
        );
    }

    #[test]
    fn changing_definition_bytes_requires_a_definition_and_content_bump() {
        let base = base_manifest();

        let mut next = manifest(version(1, 0, 1));
        add(
            &mut next,
            "entities/door.ron",
            "prop.door",
            crate::definition::ContentKind::Entity,
            SchemaVersion::new(1, 0),
            b"door-v2",
        );
        next.add_locale("locales/pt-BR.ron", Locale::default(), b"pt-v1")
            .unwrap();

        let result = diff(&base, &next);
        assert!(!result.is_compatible());
        assert_eq!(result.required_bump, VersionBump::Minor);
        assert_eq!(result.changed.len(), 1);
        assert!(
            result
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "content.diff.definition_version_not_bumped")
        );
        assert!(
            result
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "content.diff.version_bump_too_small")
        );

        let mut bumped = manifest(version(1, 1, 0));
        add(
            &mut bumped,
            "entities/door.ron",
            "prop.door",
            crate::definition::ContentKind::Entity,
            SchemaVersion::new(1, 1),
            b"door-v2",
        );
        bumped
            .add_locale("locales/pt-BR.ron", Locale::default(), b"pt-v1")
            .unwrap();
        let result = diff(&base, &bumped);
        assert!(result.is_compatible());
    }

    #[test]
    fn changing_a_definition_kind_is_breaking() {
        let base = base_manifest();
        let mut next = manifest(version(2, 0, 0));
        add(
            &mut next,
            "entities/door.ron",
            "prop.door",
            crate::definition::ContentKind::Item,
            SchemaVersion::new(1, 0),
            b"door-v1",
        );
        next.add_locale("locales/pt-BR.ron", Locale::default(), b"pt-v1")
            .unwrap();

        let result = diff(&base, &next);
        assert!(result.is_compatible());
        assert_eq!(result.required_bump, VersionBump::Major);
        assert!(
            result
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "content.diff.kind_changed")
        );

        let mut too_small = next.clone();
        too_small.content_version = version(1, 1, 0);
        let result = diff(&base, &too_small);
        assert!(!result.is_compatible());
        assert!(
            result
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "content.diff.version_bump_too_small")
        );
    }

    #[test]
    fn a_version_regression_is_rejected() {
        let base = base_manifest();
        let mut next = base.clone();
        next.content_version = version(0, 9, 0);
        let result = diff(&base, &next);
        assert!(!result.is_compatible());
        assert!(
            result
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "content.diff.version_regressed")
        );
    }

    #[test]
    fn a_definition_version_regression_is_rejected() {
        let base = base_manifest();
        let mut next = manifest(version(1, 0, 0));
        add(
            &mut next,
            "entities/door.ron",
            "prop.door",
            crate::definition::ContentKind::Entity,
            SchemaVersion::new(0, 9),
            b"door-v1",
        );
        next.add_locale("locales/pt-BR.ron", Locale::default(), b"pt-v1")
            .unwrap();

        let result = diff(&base, &next);
        assert!(!result.is_compatible());
        assert!(
            result
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "content.diff.definition_version_regressed")
        );
    }

    #[test]
    fn a_definition_bump_without_a_byte_change_warns() {
        let base = base_manifest();
        let mut next = manifest(version(1, 0, 0));
        add(
            &mut next,
            "entities/door.ron",
            "prop.door",
            crate::definition::ContentKind::Entity,
            SchemaVersion::new(1, 1),
            b"door-v1",
        );
        next.add_locale("locales/pt-BR.ron", Locale::default(), b"pt-v1")
            .unwrap();

        let result = diff(&base, &next);
        assert!(result.is_compatible());
        assert_eq!(result.required_bump, VersionBump::None);
        assert!(
            result
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code
                    == "content.diff.definition_version_bumped_without_change")
        );
    }

    #[test]
    fn removing_a_locale_is_breaking() {
        let base = base_manifest();
        let mut next = manifest(version(2, 0, 0));
        add(
            &mut next,
            "entities/door.ron",
            "prop.door",
            crate::definition::ContentKind::Entity,
            SchemaVersion::new(1, 0),
            b"door-v1",
        );

        let result = diff(&base, &next);
        assert_eq!(result.required_bump, VersionBump::Major);
        assert_eq!(result.removed_locales, vec![Locale::default()]);
    }

    #[test]
    fn metadata_only_changes_warn_without_forcing_a_bump() {
        let base = base_manifest();
        let mut next = base.clone();
        next.engine = semver::VersionReq::parse(">=0.1.1, <0.2").unwrap();
        let result = diff(&base, &next);
        assert!(result.is_compatible());
        assert_eq!(result.required_bump, VersionBump::None);
        assert!(
            result
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "content.diff.engine_requirement_changed")
        );
    }
}
