//! Content diagnostics with severities.

use std::fmt;

use engine_core::schema::ValidationIssue;
use serde::{Deserialize, Serialize};

/// How serious a diagnostic is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// The content must not load or publish.
    Error,
    /// The content loads, but the issue should be fixed before release.
    Warning,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Error => "error",
            Self::Warning => "warning",
        })
    }
}

/// One validation finding, with a stable code and a diagnostic path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Error or warning.
    pub severity: Severity,
    /// Stable machine-readable code, for example `content.duplicate_id`.
    pub code: String,
    /// File and field path, for example `entities/door.ron:components.core.door`.
    pub path: String,
    /// Human-readable explanation.
    pub message: String,
}

impl Diagnostic {
    /// Create an error.
    #[must_use]
    pub fn error(
        code: impl Into<String>,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity: Severity::Error,
            code: code.into(),
            path: path.into(),
            message: message.into(),
        }
    }

    /// Create a warning.
    #[must_use]
    pub fn warning(
        code: impl Into<String>,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity: Severity::Warning,
            code: code.into(),
            path: path.into(),
            message: message.into(),
        }
    }

    /// Convert a schema issue into an error at `prefix`.
    #[must_use]
    pub fn from_issue(prefix: &str, issue: ValidationIssue) -> Self {
        let path = if prefix.is_empty() {
            issue.path
        } else {
            format!("{prefix}:{}", issue.path)
        };
        Self::error(issue.code, path, issue.message)
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} at {}: {}",
            self.severity, self.code, self.path, self.message
        )
    }
}

/// A collection of diagnostics produced by one validation pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationReport {
    /// Findings in insertion order.
    pub diagnostics: Vec<Diagnostic>,
}

impl ValidationReport {
    /// Create an empty report.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a diagnostic.
    pub fn push(&mut self, diagnostic: Diagnostic) {
        self.diagnostics.push(diagnostic);
    }

    /// Add an error.
    pub fn error(
        &mut self,
        code: impl Into<String>,
        path: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.push(Diagnostic::error(code, path, message));
    }

    /// Add a warning.
    pub fn warning(
        &mut self,
        code: impl Into<String>,
        path: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.push(Diagnostic::warning(code, path, message));
    }

    /// Merge another report into this one.
    pub fn extend(&mut self, other: Self) {
        self.diagnostics.extend(other.diagnostics);
    }

    /// Whether there are no errors. Warnings do not make a report invalid.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        !self.has_errors()
    }

    /// Whether at least one error is present.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
    }

    /// Errors only.
    pub fn errors(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Error)
    }

    /// Warnings only.
    pub fn warnings(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Warning)
    }

    /// Number of diagnostics.
    #[must_use]
    pub fn len(&self) -> usize {
        self.diagnostics.len()
    }

    /// Whether there are no diagnostics.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.diagnostics.is_empty()
    }
}

impl fmt::Display for ValidationReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, diagnostic) in self.diagnostics.iter().enumerate() {
            if index > 0 {
                f.write_str("\n")?;
            }
            write!(f, "{diagnostic}")?;
        }
        Ok(())
    }
}

impl From<ValidationIssue> for Diagnostic {
    fn from(issue: ValidationIssue) -> Self {
        Self::error(issue.code, issue.path, issue.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_make_a_report_invalid_but_warnings_do_not() {
        let mut report = ValidationReport::new();
        report.warning("i18n.unused_key", "pt-BR.ron", "unused");
        assert!(report.is_ok());
        assert_eq!(report.warnings().count(), 1);

        report.error("content.duplicate_id", "door.ron", "duplicate");
        assert!(!report.is_ok());
        assert_eq!(report.errors().count(), 1);
        assert_eq!(report.len(), 2);
    }
}
