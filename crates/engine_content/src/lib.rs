//! Content definitions, bundles, and validation for Lynx.

pub mod bundle;
pub mod definition;
pub mod diagnostic;
pub mod diff;
pub mod publishing;
pub mod validate;

pub use bundle::{
    ContentManifest, ContentVersionSnapshot, ContentVersionState, DefinitionEntry, LocaleEntry,
    is_sha256_hex, sha256_hex,
};
pub use definition::{
    ContentDefinition, ContentKind, DisplayText, MapPosition, MapZone, MinigameDefinition,
    Presentation, PresentationMode, RouteKind, UiFieldBinding, UiPanelAction, UiPanelDefinition,
    WorldMapDefinition, ZoneRoute, is_safe_asset_path,
};
pub use diagnostic::{Diagnostic, Severity, ValidationReport};
pub use diff::{ContentDiff, DefinitionChange, VersionBump, diff};
pub use publishing::{
    AuthorId, LicenseId, ModerationAction, ModerationState, PublishingPolicy, PublishingRecord,
    ReviewNote, check_publish,
};
pub use validate::{ContentLoader, ContentSet, starter_component_registry};
