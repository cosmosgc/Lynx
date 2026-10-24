//! Ownership, moderation, and publishing controls for user content.
//!
//! A bundle can carry a [`PublishingRecord`] in its manifest. The record names
//! the author, an optional license, and the moderation state of the content.
//! State changes go through [`ModerationState::apply`], so a publisher cannot
//! jump from draft to approved without a review step.
//!
//! [`check_publish`] is the final gate. It combines the moderation record, a
//! manifest [`ContentDiff`], and a [`PublishingPolicy`], and reports stable
//! diagnostics instead of letting a caller decide which checks matter.

use engine_core::{EngineError, ErrorCode};
use serde::{Deserialize, Serialize};

use crate::diagnostic::ValidationReport;
use crate::diff::ContentDiff;

/// Maximum accepted author identifier length.
pub const MAX_AUTHOR_ID_LEN: usize = 128;
/// Maximum accepted license identifier length.
pub const MAX_LICENSE_LEN: usize = 64;
/// Maximum accepted moderation note length.
pub const MAX_REVIEW_NOTE_LEN: usize = 512;

/// Stable backend identifier of the account that authored the content.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct AuthorId(String);

impl AuthorId {
    /// Validate and wrap an author identifier.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] for an empty identifier, one
    /// longer than [`MAX_AUTHOR_ID_LEN`], or one containing whitespace or
    /// control characters.
    pub fn new(value: impl Into<String>) -> Result<Self, EngineError> {
        let value = value.into();
        if value.is_empty()
            || value.chars().count() > MAX_AUTHOR_ID_LEN
            || value
                .chars()
                .any(|ch| ch.is_control() || ch.is_whitespace())
        {
            return Err(
                EngineError::new(ErrorCode::InvalidRequest, "invalid author id")
                    .with_detail("value", value.as_str())
                    .with_detail("max_chars", MAX_AUTHOR_ID_LEN.to_string()),
            );
        }
        Ok(Self(value))
    }

    /// The identifier as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AuthorId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for AuthorId {
    type Error = EngineError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// A license identifier, normally an SPDX expression such as `CC-BY-SA-4.0`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct LicenseId(String);

impl LicenseId {
    /// Validate and wrap a license identifier.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] for an empty identifier, one
    /// longer than [`MAX_LICENSE_LEN`], or one containing characters outside
    /// ASCII letters, digits, `-`, `.`, `_`, and `+`.
    pub fn new(value: impl Into<String>) -> Result<Self, EngineError> {
        let value = value.into();
        if value.is_empty()
            || value.chars().count() > MAX_LICENSE_LEN
            || !value
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '.' | '_' | '+'))
        {
            return Err(
                EngineError::new(ErrorCode::InvalidRequest, "invalid license id")
                    .with_detail("value", value.as_str())
                    .with_detail("max_chars", MAX_LICENSE_LEN.to_string()),
            );
        }
        Ok(Self(value))
    }

    /// The identifier as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for LicenseId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for LicenseId {
    type Error = EngineError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// A moderator's note explaining a rejection.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct ReviewNote(String);

impl ReviewNote {
    /// Validate and wrap a moderation note.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] for an empty or whitespace-only
    /// note, one longer than [`MAX_REVIEW_NOTE_LEN`], or one containing control
    /// characters other than tab and newline.
    pub fn new(value: impl Into<String>) -> Result<Self, EngineError> {
        let value = value.into();
        let invalid = value.trim().is_empty()
            || value.chars().count() > MAX_REVIEW_NOTE_LEN
            || value
                .chars()
                .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\t'));
        if invalid {
            return Err(
                EngineError::new(ErrorCode::InvalidRequest, "invalid review note")
                    .with_detail("max_chars", MAX_REVIEW_NOTE_LEN.to_string()),
            );
        }
        Ok(Self(value))
    }

    /// The note as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ReviewNote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for ReviewNote {
    type Error = EngineError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// Where a user-content submission sits in the moderation workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModerationState {
    /// Authored but not submitted for review.
    Draft,
    /// Waiting for a moderator.
    InReview,
    /// Approved for publishing.
    Approved,
    /// Rejected by a moderator; the author may revise and resubmit.
    Rejected,
    /// Withdrawn by the author or publisher.
    Withdrawn,
}

impl ModerationState {
    /// The wire and report name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::InReview => "in_review",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Withdrawn => "withdrawn",
        }
    }

    /// Apply a moderation action.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidState`] when the action is not legal from
    /// this state.
    pub fn apply(self, action: ModerationAction) -> Result<Self, EngineError> {
        let next = match (self, action) {
            (Self::Draft | Self::Rejected | Self::Withdrawn, ModerationAction::Submit) => {
                Self::InReview
            }
            (Self::InReview, ModerationAction::Approve) => Self::Approved,
            (Self::InReview, ModerationAction::Reject) => Self::Rejected,
            (Self::InReview | Self::Approved, ModerationAction::Withdraw) => Self::Withdrawn,
            _ => {
                return Err(EngineError::new(
                    ErrorCode::InvalidState,
                    format!(
                        "cannot {} content that is {}",
                        action.as_str(),
                        self.as_str()
                    ),
                )
                .with_detail("state", self.as_str())
                .with_detail("action", action.as_str()));
            }
        };
        Ok(next)
    }
}

impl std::fmt::Display for ModerationState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A transition in the moderation workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModerationAction {
    /// Submit a draft, rejection, or withdrawal for review.
    Submit,
    /// Approve content that is in review.
    Approve,
    /// Reject content that is in review.
    Reject,
    /// Withdraw content that is in review or approved.
    Withdraw,
}

impl ModerationAction {
    /// The wire and report name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Submit => "submit",
            Self::Approve => "approve",
            Self::Reject => "reject",
            Self::Withdraw => "withdraw",
        }
    }
}

impl std::fmt::Display for ModerationAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Ownership and moderation metadata stored in a bundle manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishingRecord {
    /// Account that authored the content.
    pub author: AuthorId,
    /// License the author granted, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<LicenseId>,
    /// Current moderation state.
    pub state: ModerationState,
    /// Submission revision, incremented each time the content is submitted.
    pub revision: u64,
    /// Moderator who approved or rejected the current revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<AuthorId>,
    /// Rejection note, present only for rejected content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<ReviewNote>,
}

impl PublishingRecord {
    /// Start a draft record for an author.
    #[must_use]
    pub fn new(author: AuthorId) -> Self {
        Self {
            author,
            license: None,
            state: ModerationState::Draft,
            revision: 0,
            reviewer: None,
            note: None,
        }
    }

    /// Attach the license the author granted.
    #[must_use]
    pub fn with_license(mut self, license: LicenseId) -> Self {
        self.license = Some(license);
        self
    }

    /// Whether the content is approved for publishing.
    #[must_use]
    pub fn is_approved(&self) -> bool {
        self.state == ModerationState::Approved
    }

    /// Submit the current content for review.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidState`] when the record is already in review.
    pub fn submit(&mut self) -> Result<(), EngineError> {
        self.state = self.state.apply(ModerationAction::Submit)?;
        self.revision = self.revision.saturating_add(1);
        self.reviewer = None;
        self.note = None;
        Ok(())
    }

    /// Approve the content under review.
    ///
    /// Self-approval is recorded faithfully and rejected later by
    /// [`check_publish`] unless the policy allows it.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidState`] unless the record is in review.
    pub fn approve(&mut self, reviewer: AuthorId) -> Result<(), EngineError> {
        self.state = self.state.apply(ModerationAction::Approve)?;
        self.reviewer = Some(reviewer);
        self.note = None;
        Ok(())
    }

    /// Reject the content under review.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidState`] unless the record is in review.
    pub fn reject(&mut self, reviewer: AuthorId, note: ReviewNote) -> Result<(), EngineError> {
        self.state = self.state.apply(ModerationAction::Reject)?;
        self.reviewer = Some(reviewer);
        self.note = Some(note);
        Ok(())
    }

    /// Withdraw content that is in review or approved.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidState`] for drafts and rejected content.
    pub fn withdraw(&mut self) -> Result<(), EngineError> {
        self.state = self.state.apply(ModerationAction::Withdraw)?;
        self.reviewer = None;
        self.note = None;
        Ok(())
    }
}

/// Rules a publisher applies when accepting user content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PublishingPolicy {
    /// Require a moderator approval before publishing.
    pub require_review: bool,
    /// Allow the author to act as the approving moderator.
    pub allow_self_review: bool,
    /// Require the record to declare a license.
    pub require_license: bool,
}

impl Default for PublishingPolicy {
    fn default() -> Self {
        Self {
            require_review: true,
            allow_self_review: false,
            require_license: true,
        }
    }
}

/// Check whether a moderated candidate may be published.
///
/// The result combines three independent concerns: the moderation state, the
/// manifest's version compatibility, and whether the candidate changes
/// anything at all. Publishing an unchanged bundle is only a warning.
#[must_use]
pub fn check_publish(
    record: &PublishingRecord,
    diff: &ContentDiff,
    policy: &PublishingPolicy,
) -> ValidationReport {
    let mut report = ValidationReport::new();

    let publishable = if policy.require_review {
        record.state == ModerationState::Approved
    } else {
        matches!(
            record.state,
            ModerationState::Draft | ModerationState::Approved
        )
    };
    if !publishable {
        report.error(
            "content.publish.not_approved",
            "publishing",
            format!(
                "content is {} and may not be published under this policy",
                record.state
            ),
        );
    }

    if policy.require_review
        && record.state == ModerationState::Approved
        && record.reviewer.is_none()
    {
        report.error(
            "content.publish.unreviewed_approval",
            "publishing",
            "approved content has no recorded reviewer",
        );
    }

    if policy.require_review
        && !policy.allow_self_review
        && record.reviewer.as_ref() == Some(&record.author)
    {
        report.error(
            "content.publish.self_approved",
            "publishing",
            "authors cannot approve their own content",
        );
    }

    if policy.require_license && record.license.is_none() {
        report.error(
            "content.publish.missing_license",
            "publishing",
            "publishing requires a declared license",
        );
    }

    if !diff.is_compatible() {
        report.error(
            "content.publish.incompatible_version",
            "publishing",
            "candidate manifest is not a compatible successor to the base manifest",
        );
    }

    if !diff.has_changes() {
        report.warning(
            "content.publish.no_changes",
            "publishing",
            "candidate does not change any definition or locale",
        );
    }

    report
}

#[cfg(test)]
mod tests {
    use engine_core::i18n::Locale;
    use engine_core::id::DefinitionId;
    use engine_core::schema::SchemaVersion;

    use super::*;
    use crate::bundle::ContentManifest;
    use crate::definition::ContentKind;
    use crate::diff::{VersionBump, diff};

    fn author(value: &str) -> AuthorId {
        AuthorId::new(value).unwrap()
    }

    fn license() -> LicenseId {
        LicenseId::new("CC-BY-4.0").unwrap()
    }

    fn approved() -> PublishingRecord {
        let mut record = PublishingRecord::new(author("alice")).with_license(license());
        record.submit().unwrap();
        record.approve(author("moderator")).unwrap();
        record
    }

    fn manifest(version: semver::Version) -> ContentManifest {
        ContentManifest::new(version, semver::VersionReq::parse(">=0.1, <0.2").unwrap())
    }

    fn base_manifest() -> ContentManifest {
        let mut manifest = manifest(semver::Version::new(1, 0, 0));
        manifest
            .add_entry(
                "entities/door.ron".to_string(),
                ContentKind::Entity,
                DefinitionId::new("prop.door").unwrap(),
                SchemaVersion::new(1, 0),
                b"door-v1",
            )
            .unwrap();
        manifest
            .add_locale("locales/pt-BR.ron", Locale::default(), b"pt-v1")
            .unwrap();
        manifest
    }

    fn compatible_diff() -> ContentDiff {
        let base = base_manifest();
        let mut next = base.clone();
        next.content_version = semver::Version::new(1, 1, 0);
        next.add_entry(
            "entities/barrel.ron".to_string(),
            ContentKind::Entity,
            DefinitionId::new("prop.barrel").unwrap(),
            SchemaVersion::new(1, 0),
            b"barrel-v1",
        )
        .unwrap();
        diff(&base, &next)
    }

    #[test]
    fn identifiers_and_notes_are_validated() {
        assert_eq!(
            AuthorId::new("").unwrap_err().code(),
            ErrorCode::InvalidRequest
        );
        assert_eq!(
            AuthorId::new("two words").unwrap_err().code(),
            ErrorCode::InvalidRequest
        );
        assert_eq!(
            AuthorId::new("a".repeat(MAX_AUTHOR_ID_LEN + 1))
                .unwrap_err()
                .code(),
            ErrorCode::InvalidRequest
        );
        assert_eq!(
            LicenseId::new("CC BY").unwrap_err().code(),
            ErrorCode::InvalidRequest
        );
        assert_eq!(
            ReviewNote::new("   ").unwrap_err().code(),
            ErrorCode::InvalidRequest
        );
        assert!(ReviewNote::new("line one\nline two").is_ok());
    }

    #[test]
    fn moderation_requires_legal_transitions() {
        let mut record = PublishingRecord::new(author("alice"));
        assert_eq!(
            record.approve(author("moderator")).unwrap_err().code(),
            ErrorCode::InvalidState
        );
        assert_eq!(
            record.withdraw().unwrap_err().code(),
            ErrorCode::InvalidState
        );

        record.submit().unwrap();
        assert_eq!(record.state, ModerationState::InReview);
        assert_eq!(record.revision, 1);
        record.approve(author("moderator")).unwrap();
        assert!(record.is_approved());
        record.withdraw().unwrap();
        assert_eq!(record.state, ModerationState::Withdrawn);
    }

    #[test]
    fn rejection_records_a_note_and_can_be_resubmitted() {
        let mut record = PublishingRecord::new(author("alice"));
        record.submit().unwrap();
        record
            .reject(
                author("moderator"),
                ReviewNote::new("missing collision").unwrap(),
            )
            .unwrap();
        assert_eq!(record.state, ModerationState::Rejected);
        assert_eq!(record.reviewer, Some(author("moderator")));
        assert_eq!(
            record.note.as_ref().map(ReviewNote::as_str),
            Some("missing collision")
        );

        record.submit().unwrap();
        assert_eq!(record.state, ModerationState::InReview);
        assert_eq!(record.revision, 2);
        assert!(record.reviewer.is_none());
        assert!(record.note.is_none());
    }

    #[test]
    fn approved_content_passes_the_default_policy() {
        let report = check_publish(
            &approved(),
            &compatible_diff(),
            &PublishingPolicy::default(),
        );
        assert!(report.is_ok());
        assert_eq!(report.diagnostics.len(), 0);
    }

    #[test]
    fn draft_content_needs_approval_unless_review_is_disabled() {
        let mut record = PublishingRecord::new(author("alice")).with_license(license());
        let diff = compatible_diff();
        let report = check_publish(&record, &diff, &PublishingPolicy::default());
        assert!(!report.is_ok());
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "content.publish.not_approved")
        );

        let relaxed = PublishingPolicy {
            require_review: false,
            ..PublishingPolicy::default()
        };
        assert!(check_publish(&record, &diff, &relaxed).is_ok());

        record.submit().unwrap();
        assert!(!check_publish(&record, &diff, &relaxed).is_ok());
    }

    #[test]
    fn self_approval_is_rejected_unless_allowed() {
        let mut record = PublishingRecord::new(author("alice")).with_license(license());
        record.submit().unwrap();
        record.approve(author("alice")).unwrap();

        let report = check_publish(&record, &compatible_diff(), &PublishingPolicy::default());
        assert!(!report.is_ok());
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "content.publish.self_approved")
        );

        let relaxed = PublishingPolicy {
            allow_self_review: true,
            ..PublishingPolicy::default()
        };
        assert!(check_publish(&record, &compatible_diff(), &relaxed).is_ok());
    }

    #[test]
    fn missing_license_and_incompatible_versions_block_publishing() {
        let mut record = PublishingRecord::new(author("alice"));
        record.submit().unwrap();
        record.approve(author("moderator")).unwrap();

        let base = base_manifest();
        let mut next = base.clone();
        next.content_version = semver::Version::new(1, 0, 0);
        next.add_entry(
            "entities/barrel.ron".to_string(),
            ContentKind::Entity,
            DefinitionId::new("prop.barrel").unwrap(),
            SchemaVersion::new(1, 0),
            b"barrel-v1",
        )
        .unwrap();
        let incompatible = diff(&base, &next);
        assert_eq!(incompatible.required_bump, VersionBump::Minor);

        let report = check_publish(&record, &incompatible, &PublishingPolicy::default());
        let codes: Vec<_> = report
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect();
        assert!(codes.contains(&"content.publish.missing_license"));
        assert!(codes.contains(&"content.publish.incompatible_version"));
    }

    #[test]
    fn publishing_an_unchanged_bundle_warns() {
        let base = base_manifest();
        let unchanged = diff(&base, &base);
        let report = check_publish(&approved(), &unchanged, &PublishingPolicy::default());
        assert!(report.is_ok());
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "content.publish.no_changes")
        );
    }
}
