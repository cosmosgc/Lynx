use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use engine_content::{
    ContentDefinition, ContentLoader, ContentSet, MapPosition, MapZone, ValidationReport,
    WorldMapDefinition, starter_component_registry,
};
use engine_core::id::DefinitionId;
use engine_core::math::Vec3;
use serde::{Deserialize, Serialize};

const HISTORY_LIMIT: usize = 128;
const RECOVERY_VERSION: u32 = 1;

/// Errors produced by project/session operations.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// The project path or content directory is invalid.
    #[error("invalid project: {0}")]
    InvalidProject(String),
    /// The requested map or zone is unavailable.
    #[error("editor selection is invalid: {0}")]
    InvalidSelection(String),
    /// External content cannot replace in-memory edits that have not been saved.
    #[error("cannot reload external content while editor changes are unsaved")]
    UnsavedExternalChanges,
    /// Filesystem or serialization operation failed.
    #[error("editor project operation failed: {0}")]
    Io(#[from] anyhow::Error),
}

impl From<io::Error> for SessionError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.into())
    }
}

impl From<ron::Error> for SessionError {
    fn from(error: ron::Error) -> Self {
        Self::Io(error.into())
    }
}

impl From<ron::error::SpannedError> for SessionError {
    fn from(error: ron::error::SpannedError) -> Self {
        Self::Io(error.into())
    }
}

#[derive(Debug, Clone)]
enum Edit {
    Map {
        map_id: DefinitionId,
        before: Box<WorldMapDefinition>,
        after: Box<WorldMapDefinition>,
    },
    Definition {
        definition_id: DefinitionId,
        before: Box<ContentDefinition>,
        after: Box<ContentDefinition>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct RecoverySnapshot {
    version: u32,
    project_root: PathBuf,
    maps: Vec<RecoveryMap>,
    #[serde(default)]
    definitions: Vec<RecoveryDefinition>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RecoveryMap {
    source: String,
    contents: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct RecoveryDefinition {
    source: String,
    contents: String,
}

/// An open content project with selection, bounded undo/redo, validation, and
/// crash recovery. Open requires a directory with `content/`; edits remain in
/// memory until `save`, and autosave writes a separate recoverable document.
#[derive(Debug)]
pub struct EditorSession {
    project_root: PathBuf,
    content_root: PathBuf,
    recovery_path: PathBuf,
    content: ContentSet,
    validation: ValidationReport,
    selected_map: Option<DefinitionId>,
    selected_zone: Option<DefinitionId>,
    dirty_maps: BTreeSet<DefinitionId>,
    dirty_definitions: BTreeSet<DefinitionId>,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    revision: u64,
    content_revision: u64,
    saved_paths: Vec<PathBuf>,
}

impl EditorSession {
    /// Open a generated Lynx project, loading and validating its content with
    /// the shared `engine_content` implementation.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SessionError> {
        let root = path.as_ref().canonicalize().map_err(|error| {
            SessionError::InvalidProject(format!("{} ({error})", path.as_ref().display()))
        })?;
        if !root.is_dir() {
            return Err(SessionError::InvalidProject(format!(
                "{} is not a directory",
                root.display()
            )));
        }
        let content_root = root.join("content");
        if !content_root.is_dir() {
            return Err(SessionError::InvalidProject(format!(
                "{} has no content directory",
                root.display()
            )));
        }
        restore_atomic_backups(&content_root)?;
        let recovery_root = root.join(".lynx");
        if recovery_root.is_dir() {
            restore_atomic_backups(&recovery_root)?;
        }

        let loader = ContentLoader::new(starter_component_registry().map_err(anyhow::Error::from)?);
        let (mut content, validation) = loader.load_project(&content_root);
        let selected_map = content.maps.keys().next().cloned();
        let recovery_path = root.join(".lynx").join("editor-recovery.ron");
        let mut session = Self {
            project_root: root,
            content_root,
            recovery_path,
            selected_zone: None,
            selected_map,
            content: std::mem::take(&mut content),
            validation,
            dirty_maps: BTreeSet::new(),
            dirty_definitions: BTreeSet::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            revision: 0,
            content_revision: 0,
            saved_paths: Vec::new(),
        };
        session.recover_if_available()?;
        session.select_first_zone();
        Ok(session)
    }

    /// Project root opened by this session.
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// Current selected map.
    pub fn selected_map(&self) -> Option<&DefinitionId> {
        self.selected_map.as_ref()
    }

    /// Current selected zone placement.
    pub fn selected_zone(&self) -> Option<&DefinitionId> {
        self.selected_zone.as_ref()
    }

    /// Definition data for the selected zone, if it is loaded in the project.
    pub fn selected_zone_definition(&self) -> Option<&ContentDefinition> {
        self.selected_zone
            .as_ref()
            .and_then(|id| self.content.definitions.get(id))
    }

    /// Maps in the loaded project.
    pub fn maps(&self) -> impl Iterator<Item = &WorldMapDefinition> {
        self.content.maps.values()
    }

    /// Zones currently available for placement.
    pub fn placeable_zones(&self) -> impl Iterator<Item = &DefinitionId> {
        self.content
            .definitions
            .iter()
            .filter(|(id, _)| id.is_in("zone"))
            .map(|(id, _)| id)
    }

    /// Selected map definition.
    pub fn map(&self) -> Option<&WorldMapDefinition> {
        self.selected_map
            .as_ref()
            .and_then(|id| self.content.maps.get(id))
    }

    /// Latest diagnostics from the same project validator used by `lynx validate`.
    pub fn validation(&self) -> &ValidationReport {
        &self.validation
    }

    /// Whether any map contains unsaved edits.
    pub fn is_dirty(&self) -> bool {
        !self.dirty_maps.is_empty() || !self.dirty_definitions.is_empty()
    }

    /// Monotonic edit revision for view synchronization.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Monotonic reload revision for authored definitions and catalogs.
    pub fn content_revision(&self) -> u64 {
        self.content_revision
    }

    /// Select a map and its first placement.
    pub fn select_map(&mut self, map_id: &DefinitionId) -> Result<(), SessionError> {
        if !self.content.maps.contains_key(map_id) {
            return Err(SessionError::InvalidSelection(format!(
                "unknown map {map_id}"
            )));
        }
        self.selected_map = Some(map_id.clone());
        self.select_first_zone();
        Ok(())
    }

    /// Select a placed zone in the current map.
    pub fn select_zone(&mut self, zone_id: &DefinitionId) -> Result<(), SessionError> {
        let exists = self
            .map()
            .is_some_and(|map| map.zones.iter().any(|zone| &zone.zone == zone_id));
        if !exists {
            return Err(SessionError::InvalidSelection(format!(
                "zone {zone_id} is not placed on the selected map"
            )));
        }
        self.selected_zone = Some(zone_id.clone());
        Ok(())
    }

    /// Move the selected zone by map-cell offsets and record one undoable edit.
    pub fn move_selected(&mut self, dx: i32, dy: i32) -> Result<(), SessionError> {
        let map_id = self
            .selected_map
            .clone()
            .ok_or_else(|| SessionError::InvalidSelection("no map selected".into()))?;
        let zone_id = self
            .selected_zone
            .clone()
            .ok_or_else(|| SessionError::InvalidSelection("no zone selected".into()))?;
        self.edit_map(&map_id, |map| {
            let placement = map
                .zones
                .iter_mut()
                .find(|zone| zone.zone == zone_id)
                .ok_or_else(|| {
                    SessionError::InvalidSelection(format!("zone {zone_id} is not placed"))
                })?;
            placement.position.x = placement.position.x.checked_add(dx).ok_or_else(|| {
                SessionError::InvalidSelection("zone x coordinate overflow".into())
            })?;
            placement.position.y = placement.position.y.checked_add(dy).ok_or_else(|| {
                SessionError::InvalidSelection("zone y coordinate overflow".into())
            })?;
            Ok(())
        })
    }

    /// Set the selected zone's authored entity-placement transform as one undoable edit.
    ///
    /// Position coordinates and yaw must be finite. The shared content validator
    /// reports zone bounds and navigation constraints after the edit.
    pub fn set_selected_entity_placement_transform(
        &mut self,
        placement_id: &str,
        position: Vec3,
        yaw_degrees: f32,
    ) -> Result<(), SessionError> {
        if !position.x.is_finite()
            || !position.y.is_finite()
            || !position.z.is_finite()
            || !yaw_degrees.is_finite()
        {
            return Err(SessionError::InvalidSelection(
                "entity placement transforms must be finite".into(),
            ));
        }
        let definition_id = self
            .selected_zone
            .clone()
            .ok_or_else(|| SessionError::InvalidSelection("no zone selected".into()))?;
        if !definition_id.is_in("zone") {
            return Err(SessionError::InvalidSelection(format!(
                "selected definition {definition_id} is not a zone"
            )));
        }
        self.edit_definition(&definition_id, |definition| {
            let placement = definition
                .entity_placements
                .iter_mut()
                .find(|placement| placement.id == placement_id)
                .ok_or_else(|| {
                    SessionError::InvalidSelection(format!(
                        "entity placement {placement_id} is not in zone {definition_id}"
                    ))
                })?;
            placement.position = position;
            placement.yaw_degrees = yaw_degrees;
            Ok(())
        })
    }

    /// Entity prototypes available to the zone placement picker.
    pub fn entity_definitions(&self) -> impl Iterator<Item = &DefinitionId> {
        self.content
            .definitions
            .keys()
            .filter(|id| id.is_in("entity"))
    }

    /// Create an instance with a generated stable, zone-local ID.
    pub fn add_entity_placement(
        &mut self,
        prototype: &DefinitionId,
        position: Vec3,
    ) -> Result<String, SessionError> {
        if !prototype.is_in("entity") || !self.content.definitions.contains_key(prototype) {
            return Err(SessionError::InvalidSelection(format!(
                "unknown entity {prototype}"
            )));
        }
        if !position.x.is_finite() || !position.y.is_finite() || !position.z.is_finite() {
            return Err(SessionError::InvalidSelection(
                "position must be finite".into(),
            ));
        }
        let zone = self
            .selected_zone
            .clone()
            .ok_or_else(|| SessionError::InvalidSelection("no zone selected".into()))?;
        let mut created = String::new();
        self.edit_definition(&zone, |definition| {
            created = next_placement_id(definition);
            definition
                .entity_placements
                .push(engine_content::definition::ZoneEntityPlacement {
                    id: created.clone(),
                    definition: prototype.clone(),
                    position,
                    yaw_degrees: 0.0,
                });
            Ok(())
        })?;
        Ok(created)
    }

    /// Duplicate a placement without duplicating its incident links.
    pub fn duplicate_entity_placement(&mut self, id: &str) -> Result<String, SessionError> {
        let zone = self
            .selected_zone
            .clone()
            .ok_or_else(|| SessionError::InvalidSelection("no zone selected".into()))?;
        let mut created = String::new();
        self.edit_definition(&zone, |definition| {
            let mut placement = definition
                .entity_placements
                .iter()
                .find(|placement| placement.id == id)
                .cloned()
                .ok_or_else(|| SessionError::InvalidSelection(format!("unknown placement {id}")))?;
            created = next_placement_id(definition);
            placement.id.clone_from(&created);
            definition.entity_placements.push(placement);
            Ok(())
        })?;
        Ok(created)
    }

    /// Remove a placement and all incident links in one reversible command.
    pub fn delete_entity_placement(&mut self, id: &str) -> Result<(), SessionError> {
        self.delete_entity_placements(&BTreeSet::from([id.to_owned()]))
    }

    /// Translate the complete selection in one validated history entry.
    pub fn move_entity_placements(
        &mut self,
        ids: &BTreeSet<String>,
        delta: Vec3,
    ) -> Result<(), SessionError> {
        self.edit_placements(ids, |definition| {
            for p in &mut definition.entity_placements {
                if ids.contains(&p.id) {
                    p.position = Vec3::new(
                        p.position.x + delta.x,
                        p.position.y + delta.y,
                        p.position.z + delta.z,
                    );
                    if !p.position.x.is_finite()
                        || !p.position.y.is_finite()
                        || !p.position.z.is_finite()
                    {
                        return Err(SessionError::InvalidSelection(
                            "position must be finite".into(),
                        ));
                    }
                }
            }
            Ok(())
        })
    }

    /// Remove all selected placements and their incident links as one edit.
    pub fn delete_entity_placements(&mut self, ids: &BTreeSet<String>) -> Result<(), SessionError> {
        self.edit_placements(ids, |definition| {
            definition
                .entity_placements
                .retain(|p| !ids.contains(&p.id));
            definition
                .entity_links
                .retain(|link| !ids.contains(&link.source) && !ids.contains(&link.target));
            Ok(())
        })
    }

    fn edit_placements(
        &mut self,
        ids: &BTreeSet<String>,
        edit: impl FnOnce(&mut ContentDefinition) -> Result<(), SessionError>,
    ) -> Result<(), SessionError> {
        let zone = self
            .selected_zone
            .clone()
            .ok_or_else(|| SessionError::InvalidSelection("no zone selected".into()))?;
        self.edit_definition(&zone, |definition| {
            if ids.is_empty()
                || definition
                    .entity_placements
                    .iter()
                    .filter(|p| ids.contains(&p.id))
                    .count()
                    != ids.len()
            {
                return Err(SessionError::InvalidSelection(
                    "select existing placements in this zone".into(),
                ));
            }
            edit(definition)
        })
    }

    /// Place an existing zone definition on the selected map.
    pub fn place_zone(
        &mut self,
        zone_id: &DefinitionId,
        position: MapPosition,
    ) -> Result<(), SessionError> {
        if !self.content.definitions.contains_key(zone_id) || !zone_id.is_in("zone") {
            return Err(SessionError::InvalidSelection(format!(
                "unknown zone definition {zone_id}"
            )));
        }
        let map_id = self
            .selected_map
            .clone()
            .ok_or_else(|| SessionError::InvalidSelection("no map selected".into()))?;
        self.edit_map(&map_id, |map| {
            if map.zones.iter().any(|zone| zone.zone == *zone_id) {
                return Err(SessionError::InvalidSelection(format!(
                    "zone {zone_id} is already placed"
                )));
            }
            map.zones.push(MapZone {
                zone: zone_id.clone(),
                position,
            });
            Ok(())
        })?;
        self.selected_zone = Some(zone_id.clone());
        Ok(())
    }

    /// Delete the selected placement and its attached routes as one undoable edit.
    pub fn delete_selected(&mut self) -> Result<(), SessionError> {
        let map_id = self
            .selected_map
            .clone()
            .ok_or_else(|| SessionError::InvalidSelection("no map selected".into()))?;
        let zone_id = self
            .selected_zone
            .clone()
            .ok_or_else(|| SessionError::InvalidSelection("no zone selected".into()))?;
        self.edit_map(&map_id, |map| {
            let before = map.zones.len();
            map.zones.retain(|zone| zone.zone != zone_id);
            if map.zones.len() == before {
                return Err(SessionError::InvalidSelection(format!(
                    "zone {zone_id} is not placed"
                )));
            }
            map.routes
                .retain(|route| route.from != zone_id && route.to != zone_id);
            Ok(())
        })?;
        self.select_first_zone();
        Ok(())
    }

    /// Undo the latest placement or transform edit.
    pub fn undo(&mut self) -> bool {
        let Some(edit) = self.undo.pop() else {
            return false;
        };
        match &edit {
            Edit::Map { map_id, before, .. } => {
                self.content.maps.insert(map_id.clone(), (**before).clone());
                self.dirty_maps.insert(map_id.clone());
            }
            Edit::Definition {
                definition_id,
                before,
                ..
            } => {
                self.content
                    .definitions
                    .insert(definition_id.clone(), (**before).clone());
                self.dirty_definitions.insert(definition_id.clone());
            }
        }
        self.redo.push(edit);
        self.revision = self.revision.saturating_add(1);
        self.refresh_validation();
        self.select_first_zone_if_missing();
        true
    }

    /// Redo the latest undone authored-content edit.
    pub fn redo(&mut self) -> bool {
        let Some(edit) = self.redo.pop() else {
            return false;
        };
        match &edit {
            Edit::Map { map_id, after, .. } => {
                self.content.maps.insert(map_id.clone(), (**after).clone());
                self.dirty_maps.insert(map_id.clone());
            }
            Edit::Definition {
                definition_id,
                after,
                ..
            } => {
                self.content
                    .definitions
                    .insert(definition_id.clone(), (**after).clone());
                self.dirty_definitions.insert(definition_id.clone());
            }
        }
        self.undo.push(edit);
        self.revision = self.revision.saturating_add(1);
        self.refresh_validation();
        self.select_first_zone_if_missing();
        true
    }

    /// Re-run project validation, including in-memory map edits.
    pub fn validate(&mut self) -> &ValidationReport {
        self.refresh_validation();
        &self.validation
    }

    /// Reload content changed outside the editor when there are no unsaved edits.
    ///
    /// The current map and zone selection are retained when they still exist;
    /// edit history is cleared if authored maps or definitions changed on disk.
    pub fn reload_external_changes(&mut self) -> Result<(), SessionError> {
        if self.is_dirty() {
            return Err(SessionError::UnsavedExternalChanges);
        }

        let loader = ContentLoader::new(starter_component_registry().map_err(anyhow::Error::from)?);
        let (mut content, validation) = loader.load_project(&self.content_root);
        let authored_content_changed =
            content.maps != self.content.maps || content.definitions != self.content.definitions;
        self.content = std::mem::take(&mut content);
        self.validation = validation;

        let previous_map = self.selected_map.take();
        self.selected_map = previous_map
            .filter(|id| self.content.maps.contains_key(id))
            .or_else(|| self.content.maps.keys().next().cloned());

        let previous_zone = self.selected_zone.take();
        self.selected_zone = previous_zone
            .filter(|id| {
                self.map()
                    .is_some_and(|map| map.zones.iter().any(|zone| &zone.zone == id))
            })
            .or_else(|| {
                self.map()
                    .and_then(|map| map.zones.first().map(|zone| zone.zone.clone()))
            });

        if authored_content_changed {
            self.undo.clear();
            self.redo.clear();
        }
        self.revision = self.revision.saturating_add(1);
        self.content_revision = self.content_revision.saturating_add(1);
        Ok(())
    }

    /// Save changed maps and definitions with sibling staging files, preserving
    /// the previous complete file if serialization or writing fails.
    pub fn save(&mut self) -> Result<(), SessionError> {
        // Persist the full edited state first. If the process stops while
        // several map files are being replaced, reopening replays this snapshot
        // over any partially written project files.
        self.autosave()?;
        self.saved_paths.clear();
        let dirty: Vec<_> = self.dirty_maps.iter().cloned().collect();
        for id in dirty {
            let map = self.content.maps.get(&id).ok_or_else(|| {
                SessionError::InvalidSelection(format!("edited map {id} disappeared"))
            })?;
            let source = self.content.map_sources.get(&id).ok_or_else(|| {
                SessionError::InvalidSelection(format!("map {id} has no source path"))
            })?;
            let path = self.content_root.join(source);
            let text = ron::ser::to_string_pretty(map, ron::ser::PrettyConfig::new())?;
            atomic_write(&path, text.as_bytes())?;
            self.saved_paths.push(path);
        }
        let dirty_definitions: Vec<_> = self.dirty_definitions.iter().cloned().collect();
        for id in dirty_definitions {
            let definition = self.content.definitions.get(&id).ok_or_else(|| {
                SessionError::InvalidSelection(format!("edited definition {id} disappeared"))
            })?;
            let source = self.content.sources.get(&id).ok_or_else(|| {
                SessionError::InvalidSelection(format!("definition {id} has no source path"))
            })?;
            let path = self.content_root.join(source);
            let text = ron::ser::to_string_pretty(definition, ron::ser::PrettyConfig::new())?;
            atomic_write(&path, text.as_bytes())?;
            self.saved_paths.push(path);
        }
        let recovery_backup = backup_path(&self.recovery_path);
        if self.recovery_path.exists() {
            fs::remove_file(&self.recovery_path)?;
        }
        if recovery_backup.exists() {
            fs::remove_file(recovery_backup)?;
        }
        self.dirty_maps.clear();
        self.dirty_definitions.clear();
        self.refresh_validation();
        Ok(())
    }

    /// Files written by the most recent [`EditorSession::save`].
    ///
    /// The editor uses these paths to ignore its own filesystem events instead
    /// of reporting a save as an external content change.
    pub fn take_saved_paths(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.saved_paths)
    }

    /// Persist a separate recovery snapshot. It never writes over project content.
    pub fn autosave(&self) -> Result<(), SessionError> {
        if self.dirty_maps.is_empty() && self.dirty_definitions.is_empty() {
            return Ok(());
        }
        let maps = self
            .dirty_maps
            .iter()
            .map(|id| {
                let map = self
                    .content
                    .maps
                    .get(id)
                    .ok_or_else(|| anyhow::anyhow!("edited map {id} disappeared"))?;
                let source = self
                    .content
                    .map_sources
                    .get(id)
                    .ok_or_else(|| anyhow::anyhow!("map {id} has no source path"))?;
                Ok(RecoveryMap {
                    source: source.clone(),
                    contents: ron::ser::to_string_pretty(map, ron::ser::PrettyConfig::new())?,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let definitions = self
            .dirty_definitions
            .iter()
            .map(|id| {
                let definition = self
                    .content
                    .definitions
                    .get(id)
                    .ok_or_else(|| anyhow::anyhow!("edited definition {id} disappeared"))?;
                let source = self
                    .content
                    .sources
                    .get(id)
                    .ok_or_else(|| anyhow::anyhow!("definition {id} has no source path"))?;
                Ok(RecoveryDefinition {
                    source: source.clone(),
                    contents: ron::ser::to_string_pretty(
                        definition,
                        ron::ser::PrettyConfig::new(),
                    )?,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let snapshot = RecoverySnapshot {
            version: RECOVERY_VERSION,
            project_root: self.project_root.clone(),
            maps,
            definitions,
        };
        let contents = ron::ser::to_string_pretty(&snapshot, ron::ser::PrettyConfig::new())?;
        atomic_write(&self.recovery_path, contents.as_bytes())?;
        Ok(())
    }

    fn edit_map(
        &mut self,
        map_id: &DefinitionId,
        edit: impl FnOnce(&mut WorldMapDefinition) -> Result<(), SessionError>,
    ) -> Result<(), SessionError> {
        let before = self
            .content
            .maps
            .get(map_id)
            .cloned()
            .ok_or_else(|| SessionError::InvalidSelection(format!("unknown map {map_id}")))?;
        let mut after = before.clone();
        edit(&mut after)?;
        if before == after {
            return Ok(());
        }
        self.content.maps.insert(map_id.clone(), after.clone());
        self.dirty_maps.insert(map_id.clone());
        if self.undo.len() == HISTORY_LIMIT {
            self.undo.remove(0);
        }
        self.undo.push(Edit::Map {
            map_id: map_id.clone(),
            before: Box::new(before),
            after: Box::new(after),
        });
        self.redo.clear();
        self.revision = self.revision.saturating_add(1);
        self.refresh_validation();
        Ok(())
    }

    fn edit_definition(
        &mut self,
        definition_id: &DefinitionId,
        edit: impl FnOnce(&mut ContentDefinition) -> Result<(), SessionError>,
    ) -> Result<(), SessionError> {
        let before = self
            .content
            .definitions
            .get(definition_id)
            .cloned()
            .ok_or_else(|| {
                SessionError::InvalidSelection(format!("unknown definition {definition_id}"))
            })?;
        let mut after = before.clone();
        edit(&mut after)?;
        if before == after {
            return Ok(());
        }
        let registry = starter_component_registry().map_err(anyhow::Error::from)?;
        let mut candidate = self.content.clone();
        candidate
            .definitions
            .insert(definition_id.clone(), after.clone());
        let report = candidate.validate(&registry);
        let existing_errors: BTreeSet<_> =
            self.validation.errors().map(ToString::to_string).collect();
        if let Some(error) = report
            .errors()
            .find(|error| !existing_errors.contains(&error.to_string()))
        {
            return Err(SessionError::InvalidSelection(error.to_string()));
        }
        self.content
            .definitions
            .insert(definition_id.clone(), after.clone());
        self.dirty_definitions.insert(definition_id.clone());
        if self.undo.len() == HISTORY_LIMIT {
            self.undo.remove(0);
        }
        self.undo.push(Edit::Definition {
            definition_id: definition_id.clone(),
            before: Box::new(before),
            after: Box::new(after),
        });
        self.redo.clear();
        self.revision = self.revision.saturating_add(1);
        self.refresh_validation();
        Ok(())
    }

    fn refresh_validation(&mut self) {
        if let Ok(registry) = starter_component_registry() {
            self.validation = self.content.validate(&registry);
        }
    }

    fn select_first_zone(&mut self) {
        self.selected_zone = self
            .map()
            .and_then(|map| map.zones.first().map(|zone| zone.zone.clone()));
    }

    fn select_first_zone_if_missing(&mut self) {
        let still_exists = self.selected_zone.as_ref().is_some_and(|id| {
            self.map()
                .is_some_and(|map| map.zones.iter().any(|zone| &zone.zone == id))
        });
        if !still_exists {
            self.select_first_zone();
        }
    }

    fn recover_if_available(&mut self) -> Result<(), SessionError> {
        let recovery_path = if self.recovery_path.exists() {
            self.recovery_path.clone()
        } else {
            let backup = backup_path(&self.recovery_path);
            if backup.exists() {
                backup
            } else {
                return Ok(());
            }
        };
        let bytes = fs::read(&recovery_path)?;
        let snapshot: RecoverySnapshot = ron::de::from_bytes(&bytes)?;
        if snapshot.version != RECOVERY_VERSION || snapshot.project_root != self.project_root {
            return Err(SessionError::InvalidProject(
                "editor recovery file does not match this project".into(),
            ));
        }
        for saved in snapshot.maps {
            let map: WorldMapDefinition = ron::de::from_str(&saved.contents)?;
            let Some(id) = self.content.maps.keys().find(|id| *id == &map.id).cloned() else {
                return Err(SessionError::InvalidProject(format!(
                    "recovery map {} is not in this project",
                    map.id
                )));
            };
            if self.content.map_sources.get(&id) != Some(&saved.source) {
                return Err(SessionError::InvalidProject(format!(
                    "recovery source path does not match map {}",
                    map.id
                )));
            }
            self.dirty_maps.insert(id.clone());
            self.content.maps.insert(id, map);
        }
        for saved in snapshot.definitions {
            let definition: ContentDefinition = ron::de::from_str(&saved.contents)?;
            let Some(id) = self
                .content
                .definitions
                .keys()
                .find(|id| *id == &definition.id)
                .cloned()
            else {
                return Err(SessionError::InvalidProject(format!(
                    "recovery definition {} is not in this project",
                    definition.id
                )));
            };
            if self.content.sources.get(&id) != Some(&saved.source) {
                return Err(SessionError::InvalidProject(format!(
                    "recovery source path does not match definition {}",
                    definition.id
                )));
            }
            self.dirty_definitions.insert(id.clone());
            self.content.definitions.insert(id, definition);
        }
        self.refresh_validation();
        Ok(())
    }
}

fn next_placement_id(definition: &ContentDefinition) -> String {
    (1..=definition.entity_placements.len().saturating_add(1))
        .map(|index| format!("entity_{index}"))
        .find(|id| {
            !definition
                .entity_placements
                .iter()
                .any(|placement| &placement.id == id)
        })
        .expect("one more candidate than placements guarantees an unused ID")
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), SessionError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| {
            SessionError::InvalidProject("target path has no parent directory".into())
        })?;
    fs::create_dir_all(parent)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut temporary_name = path.as_os_str().to_owned();
    temporary_name.push(format!(".{}.{}.tmp", std::process::id(), stamp));
    let temporary = PathBuf::from(temporary_name);
    let backup = backup_path(path);
    let write_result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        if path.exists() {
            if backup.exists() {
                fs::remove_file(&backup)?;
            }
            fs::rename(path, &backup)?;
        }
        if let Err(error) = fs::rename(&temporary, path) {
            if backup.exists() {
                let _ = fs::rename(&backup, path);
            }
            return Err(error);
        }
        if backup.exists() {
            fs::remove_file(backup)?;
        }
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result.map_err(anyhow::Error::from)?;
    Ok(())
}

fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".lynx-editor.bak");
    PathBuf::from(name)
}

fn restore_atomic_backups(root: &Path) -> Result<(), SessionError> {
    let suffix = ".lynx-editor.bak";
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            restore_atomic_backups(&path)?;
        } else if file_type.is_file()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(suffix))
        {
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            let target =
                path.with_file_name(&file_name[..file_name.len().saturating_sub(suffix.len())]);
            if target.exists() {
                fs::remove_file(path)?;
            } else {
                fs::rename(path, target)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_content::definition::ZoneEntityPlacement;
    use engine_content::{DisplayText, MapPosition};
    use engine_core::i18n::MessageKey;
    use engine_core::math::Vec3 as AuthoredVec3;
    use engine_core::schema::SchemaVersion;

    fn map(position_x: i32) -> WorldMapDefinition {
        WorldMapDefinition {
            id: DefinitionId::new("map.editor_test").unwrap(),
            version: SchemaVersion::new(1, 0),
            display: DisplayText {
                name: MessageKey::parse("map.editor_test.name").unwrap(),
                description: MessageKey::parse("map.editor_test.description").unwrap(),
            },
            zones: vec![MapZone {
                zone: DefinitionId::new("zone.editor_test").unwrap(),
                position: MapPosition {
                    x: position_x,
                    y: 0,
                },
            }],
            routes: Vec::new(),
        }
    }

    fn write_map(content_root: &Path, position_x: i32) {
        let map_directory = content_root.join("maps");
        fs::create_dir_all(&map_directory).unwrap();
        fs::write(
            map_directory.join("editor_test.ron"),
            ron::ser::to_string(&map(position_x)).unwrap(),
        )
        .unwrap();
    }

    fn write_zone(content_root: &Path) {
        let zone = ContentDefinition {
            id: DefinitionId::new("zone.editor_test").unwrap(),
            version: SchemaVersion::new(1, 0),
            display: DisplayText {
                name: MessageKey::parse("zone.editor_test.name").unwrap(),
                description: MessageKey::parse("zone.editor_test.description").unwrap(),
            },
            components: std::collections::BTreeMap::new(),
            presentation: None,
            tags: std::collections::BTreeSet::new(),
            behavior: None,
            travel_triggers: Vec::new(),
            entity_placements: vec![ZoneEntityPlacement {
                id: "stall".into(),
                definition: DefinitionId::new("entity.stall").unwrap(),
                position: AuthoredVec3::new(1.0, 0.0, 2.0),
                yaw_degrees: 15.0,
            }],
            entity_links: Vec::new(),
            link: None,
        };
        let directory = content_root.join("zones");
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("editor_test.ron"),
            ron::ser::to_string(&zone).unwrap(),
        )
        .unwrap();
    }

    fn write_editor_project(project: &Path) {
        let content_root = project.join("content");
        write_map(&content_root, 0);
        write_zone(&content_root);
    }

    #[test]
    fn edit_undo_redo_save_and_reopen_preserves_map() {
        let project = tempfile::tempdir().unwrap();
        let content_root = project.path().join("content");
        write_map(&content_root, 0);
        let mut session = EditorSession::open(project.path()).unwrap();
        let original = session.map().unwrap().clone();
        session.move_selected(3, -2).unwrap();
        let edited = session.map().unwrap().clone();
        assert!(session.undo());
        assert_eq!(session.map(), Some(&original));
        assert!(session.redo());
        assert_eq!(session.map(), Some(&edited));
        session.save().unwrap();
        drop(session);

        let reopened = EditorSession::open(project.path()).unwrap();
        assert_eq!(reopened.map(), Some(&edited));
        assert!(!reopened.is_dirty());
        assert!(!project.path().join(".lynx/editor-recovery.ron").exists());
    }

    #[test]
    fn selected_zone_entity_transform_undo_redo_save_and_reopen() {
        let project = tempfile::tempdir().unwrap();
        write_editor_project(project.path());
        let mut session = EditorSession::open(project.path()).unwrap();
        let original = session.selected_zone_definition().unwrap().clone();

        session
            .set_selected_entity_placement_transform(
                "stall",
                AuthoredVec3::new(4.5, 1.0, -3.0),
                270.0,
            )
            .unwrap();
        let edited = session.selected_zone_definition().unwrap().clone();
        assert_eq!(
            edited.entity_placements[0].position,
            AuthoredVec3::new(4.5, 1.0, -3.0)
        );
        assert_eq!(edited.entity_placements[0].yaw_degrees, 270.0);
        assert!(session.is_dirty());
        assert!(session.undo());
        assert_eq!(session.selected_zone_definition(), Some(&original));
        assert!(session.redo());
        assert_eq!(session.selected_zone_definition(), Some(&edited));

        session.save().unwrap();
        assert!(
            session
                .take_saved_paths()
                .iter()
                .any(|path| path.ends_with(Path::new("zones/editor_test.ron")))
        );
        drop(session);
        let reopened = EditorSession::open(project.path()).unwrap();
        assert_eq!(reopened.selected_zone_definition(), Some(&edited));
        assert!(!reopened.is_dirty());
        assert!(!project.path().join(".lynx/editor-recovery.ron").exists());
    }

    #[test]
    fn non_finite_zone_entity_transform_is_rejected_without_dirtying_session() {
        let project = tempfile::tempdir().unwrap();
        write_editor_project(project.path());
        let mut session = EditorSession::open(project.path()).unwrap();
        let original = session.selected_zone_definition().unwrap().clone();
        let revision = session.revision();

        assert!(
            session
                .set_selected_entity_placement_transform(
                    "stall",
                    AuthoredVec3::new(f32::NAN, 0.0, 0.0),
                    0.0,
                )
                .is_err()
        );
        assert_eq!(session.selected_zone_definition(), Some(&original));
        assert_eq!(session.revision(), revision);
        assert!(!session.is_dirty());
    }

    #[test]
    fn autosaved_zone_entity_transform_recovers_and_can_be_saved() {
        let project = tempfile::tempdir().unwrap();
        write_editor_project(project.path());
        let mut session = EditorSession::open(project.path()).unwrap();
        session.move_selected(2, -1).unwrap();
        let edited_map = session.map().unwrap().clone();
        session
            .set_selected_entity_placement_transform(
                "stall",
                AuthoredVec3::new(-2.0, 0.5, 8.0),
                90.0,
            )
            .unwrap();
        let edited = session.selected_zone_definition().unwrap().clone();
        let source = project.path().join("content/zones/editor_test.ron");
        let original_bytes = fs::read(&source).unwrap();
        session.autosave().unwrap();
        assert_eq!(fs::read(&source).unwrap(), original_bytes);
        drop(session);

        let mut recovered = EditorSession::open(project.path()).unwrap();
        assert_eq!(recovered.map(), Some(&edited_map));
        assert_eq!(recovered.selected_zone_definition(), Some(&edited));
        assert!(recovered.is_dirty());
        recovered.save().unwrap();
        drop(recovered);

        let reopened = EditorSession::open(project.path()).unwrap();
        assert_eq!(reopened.map(), Some(&edited_map));
        assert_eq!(reopened.selected_zone_definition(), Some(&edited));
        assert!(!reopened.is_dirty());
    }

    #[test]
    fn interrupted_replacement_recovers_autosaved_edits_without_losing_original() {
        let project = tempfile::tempdir().unwrap();
        let content_root = project.path().join("content");
        write_map(&content_root, 0);
        let source = content_root.join("maps/editor_test.ron");
        let original_bytes = fs::read(&source).unwrap();
        let mut session = EditorSession::open(project.path()).unwrap();
        session.move_selected(7, 2).unwrap();
        let edited = session.map().unwrap().clone();
        session.autosave().unwrap();
        assert_eq!(fs::read(&source).unwrap(), original_bytes);
        drop(session);

        // Simulate termination after moving the original aside but before
        // installing the replacement. Opening must restore it before loading.
        fs::rename(&source, backup_path(&source)).unwrap();
        let mut recovered = EditorSession::open(project.path()).unwrap();
        assert_eq!(fs::read(&source).unwrap(), original_bytes);
        assert_eq!(recovered.map(), Some(&edited));
        assert!(recovered.is_dirty());
        recovered.save().unwrap();
        drop(recovered);
        let reopened = EditorSession::open(project.path()).unwrap();
        assert_eq!(reopened.map(), Some(&edited));
        assert!(!reopened.is_dirty());
    }

    #[test]
    fn external_reload_refreshes_clean_session_and_preserves_unsaved_session() {
        let project = tempfile::tempdir().unwrap();
        let content_root = project.path().join("content");
        fs::create_dir_all(&content_root).unwrap();
        write_map(&content_root, 0);

        let mut clean = EditorSession::open(project.path()).unwrap();
        let loaded_content_revision = clean.content_revision();
        write_map(&content_root, 5);
        clean.reload_external_changes().unwrap();
        assert_eq!(clean.map().unwrap().zones[0].position.x, 5);
        assert_eq!(clean.content_revision(), loaded_content_revision + 1);

        let mut saved = EditorSession::open(project.path()).unwrap();
        saved.move_selected(1, 0).unwrap();
        assert_eq!(saved.content_revision(), loaded_content_revision);
        saved.save().unwrap();
        assert!(saved.take_saved_paths()[0].ends_with(Path::new("maps").join("editor_test.ron")));
        saved.reload_external_changes().unwrap();
        assert!(saved.undo());
        assert_eq!(saved.map().unwrap().zones[0].position.x, 5);

        let mut dirty = EditorSession::open(project.path()).unwrap();
        dirty
            .dirty_maps
            .insert(DefinitionId::new("map.editor_test").unwrap());
        write_map(&content_root, 9);
        assert!(matches!(
            dirty.reload_external_changes(),
            Err(SessionError::UnsavedExternalChanges)
        ));
        assert_eq!(dirty.map().unwrap().zones[0].position.x, 6);
        assert!(dirty.is_dirty());
    }
}
