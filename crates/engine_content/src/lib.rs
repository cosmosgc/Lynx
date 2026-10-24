//! Content definitions, bundles, and validation for Lynx.

pub mod bundle;
pub mod definition;
pub mod diagnostic;
pub mod validate;

pub use bundle::{ContentManifest, DefinitionEntry, LocaleEntry, is_sha256_hex, sha256_hex};
pub use definition::{
    ContentDefinition, ContentKind, DisplayText, Presentation, PresentationMode, is_safe_asset_path,
};
pub use diagnostic::{Diagnostic, Severity, ValidationReport};
pub use validate::{ContentLoader, ContentSet};
