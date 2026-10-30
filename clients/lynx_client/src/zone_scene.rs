//! Zone and content-bundle loading for the offline presentation layer.
//!
//! This module deliberately stops at presentation: it reads authored data,
//! validates it, and builds a deterministic local scene. It never decides
//! gameplay outcomes. The server remains authoritative for movement, combat,
//! interaction, and persistence.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use bevy::prelude::{Resource, Vec3};
use engine_content::{
    ContentDefinition, ContentKind, ContentLoader, ContentManifest, Presentation, PresentationMode,
    is_safe_asset_path, sha256_hex, starter_component_registry,
};
use engine_core::i18n::Locale;
use engine_core::id::DefinitionId;
use engine_core::schema::ComponentKey;

/// Presentation units are metres; the windowed adapter scales them to pixels.
pub const SCENE_HALF_EXTENT_M: f32 = 32.0;
/// Terrain is split into tiles no smaller than this, to bound draw calls.
pub const MIN_TERRAIN_TILE_M: f32 = 4.0;
/// Maximum rows or columns used by the placeholder terrain renderer.
pub const MAX_TERRAIN_TILES: usize = 48;
/// Maximum entity definitions placed around a zone spawn point.
pub const MAX_SCENE_OBJECTS: usize = 64;

/// Where to load a scene from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SceneLoadOptions {
    /// Project root or content root. `None` selects the built-in demo scene.
    pub content_root: Option<PathBuf>,
    /// Explicit zone definition ID. `None` chooses a deterministic default.
    pub zone: Option<String>,
}

/// A validated, local-only scene projection of one authored zone.
#[derive(Clone, Debug, Resource)]
pub struct ZoneScene {
    /// Authored zone definition ID.
    pub zone_id: String,
    /// Resolved display name in the source locale.
    pub name: String,
    /// Bundle content version when a manifest was present and verified.
    pub content_version: Option<String>,
    /// Placeholder world bounds in metres.
    pub bounds: SceneBounds,
    /// Local avatar spawn point in metres.
    pub spawn: Vec3,
    /// Bounded placeholder terrain tiles.
    pub terrain: Vec<SceneTile>,
    /// Props, characters, and interactables derived from definitions.
    pub objects: Vec<SceneObject>,
    /// Non-fatal presentation problems, including missing assets.
    pub issues: Vec<SceneIssue>,
}

impl Default for ZoneScene {
    fn default() -> Self {
        demo_scene()
    }
}

impl ZoneScene {
    /// Count of distinct non-fatal issues.
    #[must_use]
    pub fn issue_count(&self) -> usize {
        self.issues.len()
    }

    /// Objects that expose at least one interactable verb.
    pub fn interactables(&self) -> impl Iterator<Item = &SceneObject> {
        self.objects.iter().filter(|object| object.interactable)
    }

    /// Stable, human-readable scene summary used by `--describe` and diagnostics.
    #[must_use]
    pub fn summary(&self) -> String {
        let props = self
            .objects
            .iter()
            .filter(|object| object.kind == SceneObjectKind::Prop)
            .count();
        let characters = self
            .objects
            .iter()
            .filter(|object| object.kind == SceneObjectKind::Character)
            .count();
        format!(
            "zone: {} ({})\ncontent: {}\nbounds: {:?}..{:?}\nterrain tiles: {}\nprops: {props}\ncharacters: {characters}\ninteractables: {}\nwarnings: {}",
            self.zone_id,
            self.name,
            self.content_version
                .as_deref()
                .unwrap_or("unversioned source"),
            self.bounds.min,
            self.bounds.max,
            self.terrain.len(),
            self.interactables().count(),
            self.issue_count()
        )
    }
}

/// Axis-aligned placeholder world bounds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneBounds {
    /// Minimum corner in metres.
    pub min: Vec3,
    /// Maximum corner in metres.
    pub max: Vec3,
    /// Authored navigation cell size in metres.
    pub cell_size: f32,
}

impl SceneBounds {
    /// Clamp a position into the walkable X/Z rectangle.
    #[must_use]
    pub fn clamp(&self, position: Vec3) -> Vec3 {
        Vec3::new(
            position.x.clamp(self.min.x, self.max.x),
            position.y,
            position.z.clamp(self.min.z, self.max.z),
        )
    }
}

/// One placeholder ground tile.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneTile {
    /// Tile centre in metres.
    pub position: Vec3,
    /// Tile edge length in metres.
    pub size: f32,
}

/// Presentation category derived from authored components.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SceneObjectKind {
    /// A non-interactable prop.
    Prop,
    /// A character or NPC.
    Character,
    /// An interactable that is not a character.
    Interactable,
}

/// Whether a presentation asset was found on disk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AssetStatus {
    /// No usable asset was declared or found; render the placeholder.
    Placeholder,
    /// A declared asset exists and its format is supported by the policy.
    Verified(String),
}

/// A prop, character, or interactable placed by the placeholder layout policy.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneObject {
    /// Authored definition ID.
    pub id: String,
    /// Resolved display name in the source locale.
    pub name: String,
    /// Presentation category.
    pub kind: SceneObjectKind,
    /// Whether the object exposes interactable verbs.
    pub interactable: bool,
    /// Placeholder world position in metres.
    pub position: Vec3,
    /// Authored presentation mode.
    pub presentation: PresentationMode,
    /// Asset verification result.
    pub asset: AssetStatus,
}

/// A clear, non-fatal presentation diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SceneIssue {
    /// Stable diagnostic code.
    pub code: String,
    /// Human-readable explanation.
    pub message: String,
}

impl SceneIssue {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
        }
    }
}

/// Fatal scene-loading failure. Callers print this and exit cleanly.
#[derive(Debug)]
pub enum SceneError {
    /// The configured content root does not exist.
    MissingContentRoot(PathBuf),
    /// The configured content root exists but is not a directory.
    ContentRootNotDirectory(PathBuf),
    /// The manifest could not be read or parsed.
    Manifest(String),
    /// The bundle targets an incompatible engine version.
    EngineIncompatible {
        /// Requirement declared by the bundle.
        required: String,
        /// Client engine version.
        actual: String,
    },
    /// A declared bundle file is missing, unreadable, or has the wrong hash.
    BundleFile {
        /// Relative path in the bundle.
        path: String,
        /// Why the file could not be verified.
        reason: String,
    },
    /// Content validation reported one or more errors.
    ContentInvalid {
        /// Number of errors.
        count: usize,
        /// First reported error.
        first: String,
    },
    /// The requested zone definition is absent.
    UnknownZone(String),
    /// No zone definitions were loaded.
    NoZones,
    /// The starter component registry could not be created.
    Registry(String),
}

impl fmt::Display for SceneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingContentRoot(path) => {
                write!(f, "content root does not exist: {}", path.display())
            }
            Self::ContentRootNotDirectory(path) => {
                write!(f, "content root is not a directory: {}", path.display())
            }
            Self::Manifest(error) => write!(f, "invalid content manifest: {error}"),
            Self::EngineIncompatible { required, actual } => write!(
                f,
                "content bundle requires engine {required}, but this client is {actual}"
            ),
            Self::BundleFile { path, reason } => {
                write!(f, "content bundle file {path}: {reason}")
            }
            Self::ContentInvalid { count, first } => write!(
                f,
                "content has {count} validation error(s); first error: {first}"
            ),
            Self::UnknownZone(zone) => {
                write!(f, "zone {zone} was not found in the content bundle")
            }
            Self::NoZones => write!(f, "content bundle contains no zone definitions"),
            Self::Registry(error) => {
                write!(
                    f,
                    "could not create the starter component registry: {error}"
                )
            }
        }
    }
}

impl std::error::Error for SceneError {}

/// Load a scene from an explicit content root, or the built-in demo scene.
///
/// # Errors
///
/// Returns [`SceneError`] for missing roots, incompatible manifests, invalid
/// content, or an unknown requested zone. Caveat: this is a presentation-only
/// projection; it does not prove the content is accepted by a running server.
pub fn load_scene(options: &SceneLoadOptions) -> Result<ZoneScene, SceneError> {
    match &options.content_root {
        Some(root) => load_content_scene(root, options.zone.as_deref()),
        None => Ok(demo_scene()),
    }
}

/// Load and validate a generated project or content bundle.
///
/// The root may be either a content directory or a project directory containing
/// a `content/` directory.
///
/// # Errors
///
/// See [`load_scene`].
pub fn load_content_scene(
    requested_root: &Path,
    requested_zone: Option<&str>,
) -> Result<ZoneScene, SceneError> {
    if !requested_root.exists() {
        return Err(SceneError::MissingContentRoot(requested_root.to_path_buf()));
    }
    if !requested_root.is_dir() {
        return Err(SceneError::ContentRootNotDirectory(
            requested_root.to_path_buf(),
        ));
    }
    let root = resolve_content_root(requested_root);
    if !root.is_dir() {
        return Err(SceneError::ContentRootNotDirectory(root));
    }

    let mut issues = Vec::new();
    let mut content_version = None;
    let manifest_path = root.join("manifest.json");
    if manifest_path.is_file() {
        let manifest = ContentManifest::read(&manifest_path)
            .map_err(|error| SceneError::Manifest(error.to_string()))?;
        check_engine_compatibility(&manifest)?;
        verify_bundle_files(&root, &manifest)?;
        content_version = Some(manifest.content_version.to_string());
    } else {
        issues.push(SceneIssue::new(
            "content.manifest.missing",
            "no manifest.json was found; the content version cannot be verified",
        ));
    }

    let registry =
        starter_component_registry().map_err(|error| SceneError::Registry(error.to_string()))?;
    let (set, report) = ContentLoader::new(registry).load_project(&root);
    let errors: Vec<String> = report.errors().map(ToString::to_string).collect();
    if let Some(first) = errors.first() {
        return Err(SceneError::ContentInvalid {
            count: errors.len(),
            first: first.clone(),
        });
    }

    let zone_id = select_zone(&set, requested_zone)?;
    let definition = set
        .definitions
        .get(&zone_id)
        .ok_or_else(|| SceneError::UnknownZone(zone_id.to_string()))?;
    let source_catalog = set.catalogs.catalog(&Locale::default());
    let name = source_catalog
        .and_then(|catalog| catalog.get(&definition.display.name))
        .map_or_else(|| definition.display.name.to_string(), str::to_owned);

    let cell_size = zone_cell_size(definition, &mut issues);
    let bounds = SceneBounds {
        min: Vec3::new(-SCENE_HALF_EXTENT_M, 0.0, -SCENE_HALF_EXTENT_M),
        max: Vec3::new(SCENE_HALF_EXTENT_M, 8.0, SCENE_HALF_EXTENT_M),
        cell_size,
    };
    let spawn = Vec3::ZERO;
    let terrain = build_terrain(&bounds);
    let objects = build_objects(&set, &root, source_catalog, &mut issues);

    Ok(ZoneScene {
        zone_id: zone_id.to_string(),
        name,
        content_version,
        bounds,
        spawn,
        terrain,
        objects,
        issues,
    })
}

/// The built-in offline scene. It mirrors the server demo's prop categories but
/// is local presentation data, not the server's authoritative `zone.demo`.
#[must_use]
pub fn demo_scene() -> ZoneScene {
    let bounds = SceneBounds {
        min: Vec3::new(-64.0, 0.0, -64.0),
        max: Vec3::new(64.0, 8.0, 64.0),
        cell_size: 8.0,
    };
    let objects = vec![
        demo_object(
            "prop.door",
            "Porta",
            SceneObjectKind::Interactable,
            Vec3::new(2.0, 0.0, 0.0),
            PresentationMode::IconAbstract,
        ),
        demo_object(
            "prop.barrel",
            "Barril",
            SceneObjectKind::Prop,
            Vec3::new(-2.0, 0.0, 2.0),
            PresentationMode::IconAbstract,
        ),
        demo_object(
            "prop.owner_crate",
            "Caixa",
            SceneObjectKind::Prop,
            Vec3::new(0.0, 0.0, -2.0),
            PresentationMode::IconAbstract,
        ),
        demo_object(
            "prop.vault",
            "Cofre",
            SceneObjectKind::Interactable,
            Vec3::new(0.0, 0.0, 4.0),
            PresentationMode::IconAbstract,
        ),
        demo_object(
            "character.tavern_keeper",
            "Dona Rosa",
            SceneObjectKind::Character,
            Vec3::new(-5.0, 0.0, -5.0),
            PresentationMode::Billboard2d,
        ),
        demo_object(
            "character.training_master",
            "Mestre de treino",
            SceneObjectKind::Character,
            Vec3::new(5.0, 0.0, -5.0),
            PresentationMode::Billboard2d,
        ),
    ];
    ZoneScene {
        zone_id: "zone.demo".to_owned(),
        name: "Praça de demonstração".to_owned(),
        content_version: None,
        bounds,
        spawn: Vec3::ZERO,
        terrain: build_terrain(&bounds),
        objects,
        issues: Vec::new(),
    }
}

fn demo_object(
    id: &str,
    name: &str,
    kind: SceneObjectKind,
    position: Vec3,
    presentation: PresentationMode,
) -> SceneObject {
    SceneObject {
        id: id.to_owned(),
        name: name.to_owned(),
        kind,
        interactable: matches!(
            kind,
            SceneObjectKind::Interactable | SceneObjectKind::Character
        ),
        position,
        presentation,
        asset: AssetStatus::Placeholder,
    }
}

fn resolve_content_root(root: &Path) -> PathBuf {
    if root.join("localization").is_dir() || root.join("manifest.json").is_file() {
        root.to_path_buf()
    } else if root.join("content").join("localization").is_dir() {
        root.join("content")
    } else {
        root.to_path_buf()
    }
}

fn check_engine_compatibility(manifest: &ContentManifest) -> Result<(), SceneError> {
    let actual = semver::Version::parse(env!("CARGO_PKG_VERSION")).map_err(|error| {
        SceneError::Manifest(format!("client version is not valid semver: {error}"))
    })?;
    if !manifest.engine.matches(&actual) {
        return Err(SceneError::EngineIncompatible {
            required: manifest.engine.to_string(),
            actual: actual.to_string(),
        });
    }
    Ok(())
}

fn verify_bundle_files(root: &Path, manifest: &ContentManifest) -> Result<(), SceneError> {
    for entry in &manifest.definitions {
        verify_bundle_file(root, &entry.path, &entry.sha256)?;
    }
    for entry in &manifest.locales {
        verify_bundle_file(root, &entry.path, &entry.sha256)?;
    }
    Ok(())
}

fn verify_bundle_file(root: &Path, relative: &str, expected: &str) -> Result<(), SceneError> {
    if !is_safe_asset_path(relative) {
        return Err(SceneError::BundleFile {
            path: relative.to_owned(),
            reason: "path must be relative and must not contain '..'".to_owned(),
        });
    }
    let bytes = std::fs::read(root.join(relative)).map_err(|error| SceneError::BundleFile {
        path: relative.to_owned(),
        reason: error.to_string(),
    })?;
    if sha256_hex(&bytes) != expected {
        return Err(SceneError::BundleFile {
            path: relative.to_owned(),
            reason: "sha256 does not match the manifest".to_owned(),
        });
    }
    Ok(())
}

fn select_zone(
    set: &engine_content::ContentSet,
    requested: Option<&str>,
) -> Result<DefinitionId, SceneError> {
    let zone_ids: Vec<&DefinitionId> = set
        .definitions
        .iter()
        .filter(|(_, definition)| definition.kind() == ContentKind::Zone)
        .map(|(id, _)| id)
        .collect();
    if zone_ids.is_empty() {
        return Err(SceneError::NoZones);
    }
    if let Some(requested) = requested {
        let id = DefinitionId::new(requested)
            .map_err(|_| SceneError::UnknownZone(requested.to_owned()))?;
        if zone_ids.contains(&&id) {
            return Ok(id);
        }
        return Err(SceneError::UnknownZone(requested.to_owned()));
    }
    for candidate in ["zone.demo", "zone.starter"] {
        if let Ok(id) = DefinitionId::new(candidate)
            && zone_ids.contains(&&id)
        {
            return Ok(id);
        }
    }
    Ok((*zone_ids[0]).clone())
}

fn zone_cell_size(definition: &ContentDefinition, issues: &mut Vec<SceneIssue>) -> f32 {
    let Ok(key) = ComponentKey::new("core.zone") else {
        return MIN_TERRAIN_TILE_M;
    };
    let Some(component) = definition.components.get(&key) else {
        issues.push(SceneIssue::new(
            "zone.missing_component",
            format!(
                "zone {} has no core.zone component; using a {MIN_TERRAIN_TILE_M} m terrain cell",
                definition.id
            ),
        ));
        return MIN_TERRAIN_TILE_M;
    };
    component
        .get("cell_size_m")
        .and_then(serde_json::Value::as_f64)
        .filter(|value| value.is_finite() && (0.25..=64.0).contains(value))
        .map_or_else(
            || {
                issues.push(SceneIssue::new(
                    "zone.invalid_cell_size",
                    format!(
                        "zone {} has an invalid cell_size_m; using {MIN_TERRAIN_TILE_M} m",
                        definition.id
                    ),
                ));
                MIN_TERRAIN_TILE_M
            },
            |value| {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "the value is range-checked to 0.25..=64.0"
                )]
                let value = value as f32;
                value.max(MIN_TERRAIN_TILE_M)
            },
        )
}

fn build_terrain(bounds: &SceneBounds) -> Vec<SceneTile> {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "bounds are fixed positive extents and the count is capped"
    )]
    let columns =
        ((bounds.max.x - bounds.min.x) / bounds.cell_size.max(MIN_TERRAIN_TILE_M)).ceil() as usize;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "bounds are fixed positive extents and the count is capped"
    )]
    let rows =
        ((bounds.max.z - bounds.min.z) / bounds.cell_size.max(MIN_TERRAIN_TILE_M)).ceil() as usize;
    let columns = columns.clamp(1, MAX_TERRAIN_TILES);
    let rows = rows.clamp(1, MAX_TERRAIN_TILES);
    #[expect(
        clippy::cast_precision_loss,
        reason = "terrain dimensions are capped at 48"
    )]
    let size_x = (bounds.max.x - bounds.min.x) / columns as f32;
    #[expect(
        clippy::cast_precision_loss,
        reason = "terrain dimensions are capped at 48"
    )]
    let size_z = (bounds.max.z - bounds.min.z) / rows as f32;
    let mut tiles = Vec::with_capacity(columns * rows);
    for row in 0..rows {
        for column in 0..columns {
            #[expect(
                clippy::cast_precision_loss,
                reason = "terrain dimensions are capped at 48"
            )]
            let x = bounds.min.x + (column as f32 + 0.5) * size_x;
            #[expect(
                clippy::cast_precision_loss,
                reason = "terrain dimensions are capped at 48"
            )]
            let z = bounds.min.z + (row as f32 + 0.5) * size_z;
            tiles.push(SceneTile {
                position: Vec3::new(x, 0.0, z),
                size: size_x.min(size_z),
            });
        }
    }
    tiles
}

fn build_objects(
    set: &engine_content::ContentSet,
    root: &Path,
    source_catalog: Option<&engine_core::i18n::MessageCatalog>,
    issues: &mut Vec<SceneIssue>,
) -> Vec<SceneObject> {
    let mut objects = Vec::new();
    let mut character_index = 0_usize;
    let mut interactable_index = 0_usize;
    let mut prop_index = 0_usize;
    let mut seen = BTreeSet::new();

    for (id, definition) in &set.definitions {
        if definition.kind() != ContentKind::Entity || !seen.insert(id.as_str()) {
            continue;
        }
        if objects.len() >= MAX_SCENE_OBJECTS {
            issues.push(SceneIssue::new(
                "scene.object_limit",
                format!(
                    "content has more than {MAX_SCENE_OBJECTS} entity definitions; extra objects were not placed"
                ),
            ));
            break;
        }
        let interactable = ComponentKey::new("core.interactable")
            .is_ok_and(|key| definition.components.contains_key(&key));
        let is_character = id.as_str().starts_with("character.")
            || (interactable && interactable_has_talk(definition))
            || definition.tags.contains("character")
            || definition.tags.contains("npc");
        let kind = match (is_character, interactable) {
            (true, _) => SceneObjectKind::Character,
            (false, true) => SceneObjectKind::Interactable,
            (false, false) => SceneObjectKind::Prop,
        };
        let (index, radius) = match kind {
            SceneObjectKind::Character => {
                let index = character_index;
                character_index += 1;
                (index, 7.0)
            }
            SceneObjectKind::Interactable => {
                let index = interactable_index;
                interactable_index += 1;
                (index, 11.0)
            }
            SceneObjectKind::Prop => {
                let index = prop_index;
                prop_index += 1;
                (index, 15.0)
            }
        };
        let name = source_catalog
            .and_then(|catalog| catalog.get(&definition.display.name))
            .map_or_else(|| definition.display.name.to_string(), str::to_owned);
        let presentation = definition
            .presentation
            .as_ref()
            .map_or(PresentationMode::IconAbstract, |value| value.mode);
        let asset = evaluate_assets(definition, root, issues);
        objects.push(SceneObject {
            id: id.to_string(),
            name,
            kind,
            interactable,
            position: ring_position(index, radius),
            presentation,
            asset,
        });
    }
    objects
}

fn interactable_has_talk(definition: &ContentDefinition) -> bool {
    let Ok(key) = ComponentKey::new("core.interactable") else {
        return false;
    };
    definition
        .components
        .get(&key)
        .and_then(|component| component.get("verbs"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|verbs| verbs.iter().any(|verb| verb.as_str() == Some("talk")))
}

fn ring_position(index: usize, radius: f32) -> Vec3 {
    const TAU: f32 = std::f32::consts::TAU;
    #[expect(
        clippy::cast_precision_loss,
        reason = "the number of placed objects is capped at 64"
    )]
    let count = MAX_SCENE_OBJECTS as f32;
    #[expect(
        clippy::cast_precision_loss,
        reason = "the number of placed objects is capped at 64"
    )]
    let index = index as f32;
    let angle = TAU * index / count;
    Vec3::new(angle.cos() * radius, 0.0, angle.sin() * radius)
}

fn evaluate_assets(
    definition: &ContentDefinition,
    root: &Path,
    issues: &mut Vec<SceneIssue>,
) -> AssetStatus {
    let Some(presentation) = &definition.presentation else {
        return AssetStatus::Placeholder;
    };
    let mut verified = None;
    let paths: Vec<(&str, Option<&str>, &[&str])> = match presentation.mode {
        PresentationMode::Rigged3d => {
            vec![("model", presentation.model.as_deref(), &["glb", "gltf"])]
        }
        PresentationMode::Billboard2d => {
            vec![("portrait", presentation.portrait.as_deref(), &["png"])]
        }
        PresentationMode::Hybrid => vec![
            ("model", presentation.model.as_deref(), &["glb", "gltf"]),
            ("portrait", presentation.portrait.as_deref(), &["png"]),
        ],
        PresentationMode::IconAbstract => Vec::new(),
    };
    for (field, path, extensions) in paths {
        let Some(path) = path else {
            continue;
        };
        if !is_safe_asset_path(path) {
            issues.push(SceneIssue::new(
                "asset.unsafe_path",
                format!(
                    "{} {field} path '{path}' is not a safe relative asset path; using a placeholder",
                    definition.id
                ),
            ));
            continue;
        }
        let extension_supported = Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extensions
                    .iter()
                    .any(|allowed| extension.eq_ignore_ascii_case(allowed))
            });
        if !extension_supported {
            let allowed = extensions.join(", ");
            issues.push(SceneIssue::new(
                "asset.unsupported_format",
                format!(
                    "{} {field} asset '{path}' is not a supported format ({allowed}); using a placeholder",
                    definition.id
                ),
            ));
            continue;
        }
        if !root.join(path).is_file() {
            issues.push(SceneIssue::new(
                "asset.missing",
                format!(
                    "{} {field} asset '{path}' is missing from the content bundle; using a placeholder",
                    definition.id
                ),
            ));
            continue;
        }
        verified.get_or_insert_with(|| path.to_owned());
    }
    verified.map_or(AssetStatus::Placeholder, AssetStatus::Verified)
}

/// Presentation policy for an authored asset path. Exposed for tooling and
/// tests; scene loading applies it automatically.
#[must_use]
pub fn unsupported_asset_reason(presentation: &Presentation, root: &Path) -> Option<String> {
    let paths = [
        presentation.model.as_deref(),
        presentation.portrait.as_deref(),
        presentation.animation_set.as_deref(),
    ];
    for path in paths.into_iter().flatten() {
        if !is_safe_asset_path(path) {
            return Some(format!("unsafe asset path '{path}'"));
        }
        if !root.join(path).is_file() {
            return Some(format!("missing asset '{path}'"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "lynx-zone-scene-{name}-{}-{unique}",
            std::process::id()
        ))
    }

    fn write_content(root: &Path, entity: &str) {
        std::fs::create_dir_all(root.join("zones")).unwrap();
        std::fs::create_dir_all(root.join("entities")).unwrap();
        std::fs::create_dir_all(root.join("localization")).unwrap();
        std::fs::write(
            root.join("zones/starter.ron"),
            r#"(
                id: "zone.starter",
                version: (major: 1, minor: 0),
                display: (name: "zone.starter.name", description: "zone.starter.description"),
                components: {
                    "core.zone": {"max_players": 64, "cell_size_m": 1.0},
                },
            )"#,
        )
        .unwrap();
        std::fs::write(root.join("entities/object.ron"), entity).unwrap();
        std::fs::write(
            root.join("localization/pt-BR.ron"),
            r#"(
                locale: "pt-BR",
                messages: {
                    "zone.starter.name": "Início",
                    "zone.starter.description": "Uma zona.",
                    "entity.keeper.name": "Dona Rosa",
                    "entity.keeper.description": "A tavern keeper.",
                },
            )"#,
        )
        .unwrap();
    }

    #[test]
    fn demo_scene_has_terrain_characters_props_and_interactables() {
        let scene = demo_scene();
        assert_ne!(scene.terrain.len(), 0);
        assert!(
            scene
                .objects
                .iter()
                .any(|object| object.kind == SceneObjectKind::Prop)
        );
        assert!(
            scene
                .objects
                .iter()
                .any(|object| object.kind == SceneObjectKind::Character)
        );
        assert!(scene.interactables().count() >= 2);
    }

    #[test]
    fn missing_content_root_is_a_clear_error() {
        let root = temp_root("missing");
        let error = load_content_scene(&root, None).unwrap_err();
        assert!(matches!(error, SceneError::MissingContentRoot(_)));
        assert!(error.to_string().contains("does not exist"));
    }

    #[test]
    fn incompatible_engine_manifest_is_rejected_before_parsing_content() {
        let root = temp_root("engine");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("manifest.json"),
            r#"{
                "content_version": "0.1.0",
                "engine": "^99.0",
                "definitions": [],
                "locales": []
            }"#,
        )
        .unwrap();
        let error = load_content_scene(&root, None).unwrap_err();
        assert!(matches!(error, SceneError::EngineIncompatible { .. }));
        assert!(error.to_string().contains("requires engine"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_declared_asset_reports_an_issue_and_uses_a_placeholder() {
        let root = temp_root("asset");
        write_content(
            &root,
            r#"(
                id: "entity.keeper",
                version: (major: 1, minor: 0),
                display: (name: "entity.keeper.name", description: "entity.keeper.description"),
                components: {
                    "core.interactable": {
                        "verbs": ["inspect", "talk"],
                        "range_m": 2.0,
                        "requires_line_of_sight": true,
                    },
                },
                presentation: Some((mode: Billboard2d, portrait: Some("assets/keeper.png"))),
            )"#,
        );
        let scene = load_content_scene(&root, None).unwrap();
        assert_eq!(scene.zone_id, "zone.starter");
        assert_eq!(scene.name, "Início");
        assert_eq!(scene.objects.len(), 1);
        assert_eq!(scene.objects[0].kind, SceneObjectKind::Character);
        assert_eq!(scene.objects[0].asset, AssetStatus::Placeholder);
        assert!(
            scene
                .issues
                .iter()
                .any(|issue| issue.code == "asset.missing")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_verified_asset_is_recorded_without_loading_it() {
        let root = temp_root("verified");
        write_content(
            &root,
            r#"(
                id: "entity.keeper",
                version: (major: 1, minor: 0),
                display: (name: "entity.keeper.name", description: "entity.keeper.description"),
                components: {
                    "core.interactable": {
                        "verbs": ["inspect", "talk"],
                        "range_m": 2.0,
                        "requires_line_of_sight": true,
                    },
                },
                presentation: Some((mode: Billboard2d, portrait: Some("assets/keeper.png"))),
            )"#,
        );
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("assets/keeper.png"), b"placeholder").unwrap();
        let scene = load_content_scene(&root, None).unwrap();
        assert_eq!(
            scene.objects[0].asset,
            AssetStatus::Verified("assets/keeper.png".to_owned())
        );
        assert!(
            !scene
                .issues
                .iter()
                .any(|issue| issue.code == "asset.missing")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unsupported_asset_format_is_a_clear_diagnostic() {
        let root = temp_root("format");
        write_content(
            &root,
            r#"(
                id: "entity.keeper",
                version: (major: 1, minor: 0),
                display: (name: "entity.keeper.name", description: "entity.keeper.description"),
                components: {
                    "core.interactable": {
                        "verbs": ["inspect"],
                        "range_m": 2.0,
                        "requires_line_of_sight": true,
                    },
                },
                presentation: Some((mode: Billboard2d, portrait: Some("assets/keeper.psd"))),
            )"#,
        );
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("assets/keeper.psd"), b"placeholder").unwrap();
        let scene = load_content_scene(&root, None).unwrap();
        assert_eq!(scene.objects[0].asset, AssetStatus::Placeholder);
        assert!(
            scene
                .issues
                .iter()
                .any(|issue| issue.code == "asset.unsupported_format")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_root_resolves_its_content_directory_and_requested_zone() {
        let root = temp_root("project");
        let content = root.join("content");
        write_content(
            &content,
            r#"(
                id: "entity.keeper",
                version: (major: 1, minor: 0),
                display: (name: "entity.keeper.name", description: "entity.keeper.description"),
                components: {
                    "core.interactable": {
                        "verbs": ["inspect"],
                        "range_m": 2.0,
                        "requires_line_of_sight": true,
                    },
                },
            )"#,
        );
        let scene = load_content_scene(&root, Some("zone.starter")).unwrap();
        assert_eq!(scene.zone_id, "zone.starter");
        assert_eq!(scene.objects[0].kind, SceneObjectKind::Interactable);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unknown_zone_is_reported_without_panicking() {
        let root = temp_root("unknown");
        write_content(
            &root,
            r#"(
                id: "entity.keeper",
                version: (major: 1, minor: 0),
                display: (name: "entity.keeper.name", description: "entity.keeper.description"),
                components: {
                    "core.interactable": {
                        "verbs": ["inspect"],
                        "range_m": 2.0,
                        "requires_line_of_sight": true,
                    },
                },
            )"#,
        );
        let error = load_content_scene(&root, Some("zone.absent")).unwrap_err();
        assert!(matches!(error, SceneError::UnknownZone(_)));
        std::fs::remove_dir_all(root).unwrap();
    }
}
