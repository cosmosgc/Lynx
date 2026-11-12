#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, allow(unused_features))]

//! Content definitions, bundles, and validation for Lynx.

pub mod bundle;
pub mod definition;
pub mod diagnostic;
pub mod diff;
pub mod migration;
pub mod publishing;
pub mod validate;

pub use bundle::{
    ContentManifest, ContentVersionSnapshot, ContentVersionState, DefinitionEntry, LocaleEntry,
    is_sha256_hex, sha256_hex,
};
pub use definition::{
    BehaviorDefinition, BehaviorGraph, BehaviorInstruction, BehaviorKind, BehaviorNode,
    ContentDefinition, ContentKind, DisplayText, LinkAction, LinkDefinition,
    MAX_BEHAVIOR_GRAPH_NODES, MAX_BEHAVIOR_PATROL_POINTS, MapPosition, MapZone, MinigameDefinition,
    Presentation, PresentationMode, RouteKind, UiFieldBinding, UiPanelAction, UiPanelDefinition,
    WorldMapDefinition, ZoneEntityLink, ZoneLinkEvent, ZoneRoute, is_safe_asset_path,
};
pub use diagnostic::{Diagnostic, Severity, ValidationReport};
pub use diff::{ContentDiff, DefinitionChange, VersionBump, diff};
pub use migration::{
    CatalogMigration, ContentMigrationPlan, DefinitionMigration, MigrationError,
    MigrationOperation, PersistedComponentMigration, PersistedInstanceMigrationPlan,
    apply_operations,
};
pub use publishing::{
    AuthorId, LicenseId, ModerationAction, ModerationState, PublishingPolicy, PublishingRecord,
    ReviewNote, check_publish,
};
pub use validate::{ContentLoader, ContentSet, starter_component_registry};
