use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use engine_content::{
    ContentLoader, ContentSet, MapPosition, MapZone, ValidationReport, WorldMapDefinition,
    starter_component_registry,
};
use engine_core::id::DefinitionId;
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
struct MapEdit {
    map_id: DefinitionId,
    before: WorldMapDefinition,
    after: WorldMapDefinition,
}

#[derive(Debug, Serialize, Deserialize)]
struct RecoverySnapshot {
    version: u32,
    project_root: PathBuf,
    maps: Vec<RecoveryMap>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RecoveryMap {
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
    undo: Vec<MapEdit>,
    redo: Vec<MapEdit>,
    revision: u64,
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
            undo: Vec::new(),
            redo: Vec::new(),
            revision: 0,
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
        !self.dirty_maps.is_empty()
    }

    /// Monotonic edit revision for view synchronization.
    pub fn revision(&self) -> u64 {
        self.revision
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
        self.content
            .maps
            .insert(edit.map_id.clone(), edit.before.clone());
        self.dirty_maps.insert(edit.map_id.clone());
        self.redo.push(edit);
        self.revision = self.revision.saturating_add(1);
        self.refresh_validation();
        self.select_first_zone_if_missing();
        true
    }

    /// Redo the latest undone map edit.
    pub fn redo(&mut self) -> bool {
        let Some(edit) = self.redo.pop() else {
            return false;
        };
        self.content
            .maps
            .insert(edit.map_id.clone(), edit.after.clone());
        self.dirty_maps.insert(edit.map_id.clone());
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

    /// Save changed maps with sibling staging files, preserving the previous
    /// complete file if serialization or writing fails.
    pub fn save(&mut self) -> Result<(), SessionError> {
        // Persist the full edited state first. If the process stops while
        // several map files are being replaced, reopening replays this snapshot
        // over any partially written project files.
        self.autosave()?;
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
        }
        let recovery_backup = backup_path(&self.recovery_path);
        if self.recovery_path.exists() {
            fs::remove_file(&self.recovery_path)?;
        }
        if recovery_backup.exists() {
            fs::remove_file(recovery_backup)?;
        }
        self.dirty_maps.clear();
        self.refresh_validation();
        Ok(())
    }

    /// Persist a separate recovery snapshot. It never writes over project content.
    pub fn autosave(&self) -> Result<(), SessionError> {
        if self.dirty_maps.is_empty() {
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
        let snapshot = RecoverySnapshot {
            version: RECOVERY_VERSION,
            project_root: self.project_root.clone(),
            maps,
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
        self.undo.push(MapEdit {
            map_id: map_id.clone(),
            before,
            after,
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
        self.refresh_validation();
        Ok(())
    }
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
