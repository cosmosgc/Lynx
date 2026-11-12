use super::absolute_path;
use crate::{MigrateArgs, MigrateCommand, MigrationApplyArgs, MigrationRollbackArgs};
use anyhow::{Context, Result, bail, ensure};
use engine_content::{
    ContentLoader, ContentManifest, ContentMigrationPlan, ContentSet, DefinitionMigration,
    apply_operations, diff, sha256_hex, starter_component_registry,
};
use engine_core::i18n::MessageCatalog;
use engine_core::id::DefinitionId;
use engine_core::schema::SchemaVersion;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn migrate(args: &MigrateArgs) -> Result<Value> {
    match &args.command {
        MigrateCommand::Apply(args) => apply(args),
        MigrateCommand::Rollback(args) => rollback(args),
    }
}

fn apply(args: &MigrationApplyArgs) -> Result<Value> {
    let project = absolute_path(&args.project)?;
    let content = project.join("content");
    require_directory(&content)?;
    let plan_path = absolute_path(&args.plan)?;
    let plan_source = fs::read_to_string(&plan_path)
        .with_context(|| format!("failed to read migration plan {}", plan_path.display()))?;
    let plan: ContentMigrationPlan = ron::from_str(&plan_source)
        .with_context(|| format!("failed to parse migration plan {}", plan_path.display()))?;
    plan.validate().context("invalid migration plan")?;
    let (base_manifest, set, manifest_hash) = load_source(&content, &plan)?;
    let prepared = prepare_candidate(&project, &content, &plan, &base_manifest, &set, args.apply)?;
    let affected_definitions: Vec<_> = plan
        .definitions
        .iter()
        .map(|step| {
            json!({
                "id": step.definition,
                "from_version": step.from_version,
                "to_version": step.to_version,
            })
        })
        .collect();
    let affected_catalogs: Vec<_> = plan
        .catalogs
        .iter()
        .map(|step| json!(step.locale))
        .collect();

    if !args.apply {
        return dry_run_report(
            &project,
            &plan,
            &manifest_hash,
            &affected_definitions,
            &affected_catalogs,
            prepared,
        );
    }

    commit_candidate(
        &project,
        &plan,
        &manifest_hash,
        &affected_definitions,
        &affected_catalogs,
        prepared,
    )
}

fn load_source(
    content: &Path,
    plan: &ContentMigrationPlan,
) -> Result<(ContentManifest, ContentSet, String)> {
    let manifest_path = content.join("manifest.json");
    let manifest_bytes = fs::read(&manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let manifest_hash = sha256_hex(&manifest_bytes);
    ensure!(
        manifest_hash == plan.source_manifest_sha256,
        "source manifest hash does not match migration plan"
    );
    let manifest: ContentManifest = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("invalid manifest {}", manifest_path.display()))?;
    ensure!(
        manifest.content_version == plan.from_content_version,
        "source content version does not match migration plan"
    );
    let (set, report) = load_content(content)?;
    ensure!(
        !report.has_errors(),
        "source content is invalid; refusing to migrate"
    );
    validate_manifest_files(content, &set, &manifest)?;
    Ok((manifest, set, manifest_hash))
}

struct PreparedMigration {
    stage_path: PathBuf,
    stage: StageDirectory,
    candidate_hash: String,
    changed_files: Vec<String>,
}

fn prepare_candidate(
    project: &Path,
    content: &Path,
    plan: &ContentMigrationPlan,
    base_manifest: &ContentManifest,
    set: &ContentSet,
    apply: bool,
) -> Result<PreparedMigration> {
    let stage_path = temporary_stage_path(project, &plan.id, apply)?;
    let stage = StageDirectory::new(stage_path.clone());
    copy_tree(content, &stage_path)?;
    let mut candidate_manifest = base_manifest.clone();
    let changed_files =
        transform_content(set, plan, content, &stage_path, &mut candidate_manifest)?;
    candidate_manifest.content_version = plan.to_content_version.clone();
    let compatibility = diff(base_manifest, &candidate_manifest);
    ensure!(
        compatibility.is_compatible(),
        "migration target content version is incompatible with the change set: {}",
        serde_json::to_string(&compatibility)?
    );
    candidate_manifest
        .write(stage_path.join("manifest.json"))
        .context("failed to write candidate manifest")?;
    let (_, report) = load_content(&stage_path)?;
    ensure!(
        !report.has_errors(),
        "migrated content is invalid: {}",
        report
            .errors()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    );
    let candidate_hash = sha256_hex(&fs::read(stage_path.join("manifest.json"))?);
    Ok(PreparedMigration {
        stage_path,
        stage,
        candidate_hash,
        changed_files,
    })
}

fn dry_run_report(
    project: &Path,
    plan: &ContentMigrationPlan,
    manifest_hash: &str,
    affected_definitions: &[Value],
    affected_catalogs: &[Value],
    mut prepared: PreparedMigration,
) -> Result<Value> {
    prepared.stage.remove_now()?;
    Ok(json!({
        "ok": true,
        "command": "migrate apply",
        "mode": "dry_run",
        "project": project,
        "migration": plan.id,
        "from_content_version": plan.from_content_version,
        "to_content_version": plan.to_content_version,
        "affected_definitions": affected_definitions,
        "affected_catalogs": affected_catalogs,
        "changed_files": prepared.changed_files,
        "source_manifest_sha256": manifest_hash,
        "candidate_manifest_sha256": prepared.candidate_hash,
        "content_valid": true,
        "compatible": true,
    }))
}

fn commit_candidate(
    project: &Path,
    plan: &ContentMigrationPlan,
    manifest_hash: &str,
    affected_definitions: &[Value],
    affected_catalogs: &[Value],
    mut prepared: PreparedMigration,
) -> Result<Value> {
    let content = project.join("content");
    let rollback = rollback_directory(project, &plan.id)?;
    ensure!(
        !rollback.exists(),
        "migration {} already has a rollback record",
        plan.id
    );
    create_internal_directory(&project.join(".lynx"))?;
    create_internal_directory(&project.join(".lynx/migrations"))?;
    fs::create_dir(&rollback)
        .with_context(|| format!("failed to create rollback directory {}", rollback.display()))?;
    let metadata = RollbackMetadata {
        migration: &plan.id,
        source_manifest_sha256: manifest_hash,
        candidate_manifest_sha256: &prepared.candidate_hash,
        from_content_version: &plan.from_content_version,
        to_content_version: &plan.to_content_version,
        changed_files: &prepared.changed_files,
    };
    if let Err(error) = write_json(&rollback.join("record.json"), &metadata) {
        let _ = fs::remove_dir_all(&rollback);
        return Err(error).context("failed to record rollback metadata");
    }

    let backup = rollback.join("backup");
    if let Err(error) = fs::rename(&content, &backup) {
        let _ = fs::remove_dir_all(&rollback);
        return Err(error).context("failed to preserve source content for rollback");
    }
    if let Err(error) = fs::rename(&prepared.stage_path, &content) {
        let restore = fs::rename(&backup, &content);
        return match restore {
            Ok(()) => {
                let _ = fs::remove_dir_all(&rollback);
                Err(error).context("failed to commit candidate content; source restored")
            }
            Err(restore_error) => Err(error).context(format!(
                "failed to commit candidate content, and source restore also failed: {restore_error}; source snapshot retained at {}",
                backup.display()
            )),
        };
    }
    prepared.stage.disarm();
    Ok(json!({
        "ok": true,
        "command": "migrate apply",
        "mode": "applied",
        "project": project,
        "migration": plan.id,
        "from_content_version": plan.from_content_version,
        "to_content_version": plan.to_content_version,
        "affected_definitions": affected_definitions,
        "affected_catalogs": affected_catalogs,
        "changed_files": prepared.changed_files,
        "source_manifest_sha256": manifest_hash,
        "candidate_manifest_sha256": prepared.candidate_hash,
        "rollback_record": rollback,
        "content_valid": true,
        "compatible": true,
    }))
}

fn rollback(args: &MigrationRollbackArgs) -> Result<Value> {
    let project = absolute_path(&args.project)?;
    let content = project.join("content");
    require_directory(&content)?;
    ensure!(valid_migration_id(&args.id), "invalid migration ID");
    require_directory(&project.join(".lynx"))?;
    require_directory(&project.join(".lynx/migrations"))?;
    let record = rollback_directory(&project, &args.id)?;
    require_directory(&record)?;
    let backup = record.join("backup");
    require_directory(&backup)?;
    validate_tree(&content)?;
    validate_tree(&backup)?;
    let applied = record.join("applied");
    ensure!(
        !applied.exists(),
        "migration {} has already been rolled back",
        args.id
    );
    let metadata_path = record.join("record.json");
    let metadata_file = fs::symlink_metadata(&metadata_path)
        .with_context(|| format!("failed to inspect {}", metadata_path.display()))?;
    ensure!(
        metadata_file.is_file() && !metadata_file.file_type().is_symlink(),
        "rollback record must be a regular file"
    );
    let metadata: Value = serde_json::from_slice(
        &fs::read(&metadata_path)
            .with_context(|| format!("failed to read {}", metadata_path.display()))?,
    )
    .with_context(|| format!("invalid rollback record {}", metadata_path.display()))?;
    let current_manifest = fs::read(content.join("manifest.json"))?;
    ensure!(
        sha256_hex(&current_manifest)
            == metadata["candidate_manifest_sha256"]
                .as_str()
                .unwrap_or_default(),
        "content changed after migration; refusing to overwrite it during rollback"
    );
    let manifest: ContentManifest = serde_json::from_slice(&current_manifest)?;
    let (current_set, current_report) = load_content(&content)?;
    ensure!(
        !current_report.has_errors(),
        "applied content is invalid; refusing to overwrite it during rollback"
    );
    validate_manifest_files(&content, &current_set, &manifest)?;
    let source_manifest = fs::read(backup.join("manifest.json"))?;
    ensure!(
        sha256_hex(&source_manifest)
            == metadata["source_manifest_sha256"]
                .as_str()
                .unwrap_or_default(),
        "retained source snapshot does not match its rollback record"
    );

    fs::rename(&content, &applied).context("failed to retain migrated content before rollback")?;
    if let Err(error) = fs::rename(&backup, &content) {
        let restore = fs::rename(&applied, &content);
        return match restore {
            Ok(()) => Err(error).context("rollback failed; migrated content restored"),
            Err(restore_error) => Err(error).context(format!(
                "rollback failed, and migrated content restore also failed: {restore_error}"
            )),
        };
    }
    Ok(json!({
        "ok": true,
        "command": "migrate rollback",
        "project": project,
        "migration": args.id,
        "from_content_version": metadata["from_content_version"],
        "restored_content_version": metadata["from_content_version"],
        "retained_applied_content": applied,
        "rollback_record": record,
    }))
}

fn transform_content(
    set: &ContentSet,
    plan: &ContentMigrationPlan,
    source_root: &Path,
    stage_root: &Path,
    manifest: &mut ContentManifest,
) -> Result<Vec<String>> {
    let mut changed = Vec::new();
    for migration in &plan.definitions {
        let path = source_path(set, &migration.definition)?;
        let stage = stage_root.join(&path);
        let current_version = manifest
            .definition(&migration.definition)
            .with_context(|| format!("manifest has no definition {}", migration.definition))?
            .version;
        ensure!(
            current_version == migration.from_version,
            "definition {} schema version does not match migration plan",
            migration.definition
        );
        let migrated = migrate_definition(set, migration)?;
        fs::write(&stage, &migrated)
            .with_context(|| format!("failed to stage definition {}", stage.display()))?;
        let entry = manifest
            .definitions
            .iter_mut()
            .find(|entry| entry.id == migration.definition)
            .with_context(|| format!("manifest has no definition {}", migration.definition))?;
        entry.version = migration.to_version;
        entry.sha256 = sha256_hex(&migrated);
        changed.push(path);
    }
    for migration in &plan.catalogs {
        let path = set
            .catalog_paths
            .get(&migration.locale)
            .with_context(|| format!("source content has no {} catalog", migration.locale))?;
        let source = source_root.join(path);
        let stage = stage_root.join(path);
        let source_bytes = fs::read(&source)
            .with_context(|| format!("failed to read catalog {}", source.display()))?;
        let catalog = set
            .catalogs
            .catalog(&migration.locale)
            .with_context(|| format!("source content has no {} catalog", migration.locale))?;
        let mut value = serde_json::to_value(catalog)?;
        apply_operations(&mut value, &migration.operations)?;
        let migrated: MessageCatalog = serde_json::from_value(value).with_context(|| {
            format!("migration produced an invalid {} catalog", migration.locale)
        })?;
        ensure!(
            migrated.locale() == &migration.locale,
            "catalog migration cannot change locale identity"
        );
        let migrated = serialize_ron(&migrated)?;
        fs::write(&stage, &migrated)
            .with_context(|| format!("failed to stage catalog {}", stage.display()))?;
        let entry = manifest
            .locales
            .iter_mut()
            .find(|entry| entry.locale == migration.locale)
            .with_context(|| format!("manifest has no {} catalog", migration.locale))?;
        ensure!(
            entry.sha256 == sha256_hex(&source_bytes),
            "catalog {} does not match its manifest hash",
            migration.locale
        );
        entry.sha256 = sha256_hex(&migrated);
        changed.push(path.clone());
    }
    changed.sort();
    Ok(changed)
}

fn migrate_definition(set: &ContentSet, migration: &DefinitionMigration) -> Result<Vec<u8>> {
    let mut value = if let Some(definition) = set.definitions.get(&migration.definition) {
        serde_json::to_value(definition)?
    } else if let Some(panel) = set.panels.get(&migration.definition) {
        serde_json::to_value(panel)?
    } else if let Some(minigame) = set.minigames.get(&migration.definition) {
        serde_json::to_value(minigame)?
    } else if let Some(map) = set.maps.get(&migration.definition) {
        serde_json::to_value(map)?
    } else {
        bail!("source content has no definition {}", migration.definition);
    };
    let serialized_version: SchemaVersion = serde_json::from_value(value["version"].clone())?;
    ensure!(
        serialized_version == migration.from_version,
        "definition {} data version does not match migration plan",
        migration.definition
    );
    apply_operations(&mut value, &migration.operations)?;
    value["version"] = json!({
        "major": migration.to_version.major,
        "minor": migration.to_version.minor,
    });
    ensure!(
        value["id"] == migration.definition.to_string(),
        "definition migration cannot change stable identity"
    );
    if set.definitions.contains_key(&migration.definition) {
        let definition: engine_content::ContentDefinition = serde_json::from_value(value)?;
        return serialize_ron(&definition);
    }
    if set.panels.contains_key(&migration.definition) {
        let panel: engine_content::UiPanelDefinition = serde_json::from_value(value)?;
        return serialize_ron(&panel);
    }
    if set.minigames.contains_key(&migration.definition) {
        let minigame: engine_content::MinigameDefinition = serde_json::from_value(value)?;
        return serialize_ron(&minigame);
    }
    let map: engine_content::WorldMapDefinition = serde_json::from_value(value)?;
    serialize_ron(&map)
}

fn serialize_ron<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut output = ron::ser::to_string_pretty(value, ron::ser::PrettyConfig::default())?;
    output.push('\n');
    Ok(output.into_bytes())
}

fn source_path(set: &ContentSet, id: &DefinitionId) -> Result<String> {
    set.sources
        .get(id)
        .or_else(|| set.panel_sources.get(id))
        .or_else(|| set.minigame_sources.get(id))
        .or_else(|| set.map_sources.get(id))
        .cloned()
        .with_context(|| format!("source content has no definition {id}"))
}

fn validate_manifest_files(
    root: &Path,
    set: &ContentSet,
    manifest: &ContentManifest,
) -> Result<()> {
    let manifest_report = manifest.validate();
    ensure!(!manifest_report.has_errors(), "source manifest is invalid");
    let manifest_definitions: BTreeSet<_> = manifest
        .definitions
        .iter()
        .map(|entry| entry.id.clone())
        .collect();
    let loaded_definitions: BTreeSet<_> = set
        .sources
        .keys()
        .chain(set.panel_sources.keys())
        .chain(set.minigame_sources.keys())
        .chain(set.map_sources.keys())
        .cloned()
        .collect();
    ensure!(
        manifest_definitions == loaded_definitions,
        "source manifest and loaded definitions differ"
    );
    for entry in &manifest.definitions {
        ensure!(
            source_path(set, &entry.id)? == entry.path,
            "manifest path differs from loaded path for definition {}",
            entry.id
        );
        let path = root.join(&entry.path);
        let bytes = fs::read(&path)
            .with_context(|| format!("failed to read definition {}", path.display()))?;
        ensure!(
            sha256_hex(&bytes) == entry.sha256,
            "definition {} does not match its manifest hash",
            entry.id
        );
    }
    let manifest_locales: BTreeSet<_> = manifest
        .locales
        .iter()
        .map(|entry| entry.locale.clone())
        .collect();
    let loaded_locales: BTreeSet<_> = set.catalog_paths.keys().cloned().collect();
    ensure!(
        manifest_locales == loaded_locales,
        "source manifest and loaded locale catalogs differ"
    );
    for entry in &manifest.locales {
        ensure!(
            set.catalog_paths.get(&entry.locale) == Some(&entry.path),
            "manifest path differs from loaded path for locale {}",
            entry.locale
        );
        let path = root.join(&entry.path);
        let bytes = fs::read(&path)
            .with_context(|| format!("failed to read catalog {}", path.display()))?;
        ensure!(
            sha256_hex(&bytes) == entry.sha256,
            "locale {} does not match its manifest hash",
            entry.locale
        );
    }
    Ok(())
}

fn load_content(root: &Path) -> Result<(ContentSet, engine_content::ValidationReport)> {
    let loader = ContentLoader::new(starter_component_registry()?);
    Ok(loader.load_project(root))
}

fn rollback_directory(project: &Path, migration_id: &str) -> Result<PathBuf> {
    ensure!(valid_migration_id(migration_id), "invalid migration ID");
    Ok(project.join(".lynx").join("migrations").join(migration_id))
}

fn temporary_stage_path(project: &Path, migration_id: &str, apply: bool) -> Result<PathBuf> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_nanos();
    let parent = if apply {
        project.to_path_buf()
    } else {
        std::env::temp_dir()
    };
    Ok(parent.join(format!(".lynx-stage-{migration_id}-{timestamp}")))
}

fn require_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("directory does not exist: {}", path.display()))?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "path must be a real directory: {}",
        path.display()
    );
    Ok(())
}

fn create_internal_directory(path: &Path) -> Result<()> {
    if path.exists() {
        require_directory(path)?;
    } else {
        fs::create_dir(path)
            .with_context(|| format!("failed to create directory {}", path.display()))?;
    }
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("failed to inspect {}", source.display()))?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "migration source must be a real directory: {}",
        source.display()
    );
    fs::create_dir(destination).with_context(|| {
        format!(
            "failed to create staging directory {}",
            destination.display()
        )
    })?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source_path)?;
        if metadata.file_type().is_symlink() {
            bail!("symbolic links are not allowed in content migrations");
        }
        if metadata.is_dir() {
            copy_tree(&source_path, &destination_path)?;
        } else if metadata.is_file() {
            fs::copy(&source_path, &destination_path)?;
        } else {
            bail!("unsupported content entry: {}", source_path.display());
        }
    }
    Ok(())
}

fn validate_tree(root: &Path) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            bail!("symbolic links are not allowed in content migrations");
        }
        if metadata.is_dir() {
            validate_tree(&path)?;
        } else if !metadata.is_file() {
            bail!("unsupported content entry: {}", path.display());
        }
    }
    Ok(())
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::write(path, bytes).with_context(|| format!("failed to write {}", path.display()))
}

fn valid_migration_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 96
        && id.as_bytes()[0].is_ascii_lowercase()
        && !id.ends_with('.')
        && !id.contains("..")
        && id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

#[derive(Serialize)]
struct RollbackMetadata<'a> {
    migration: &'a str,
    source_manifest_sha256: &'a str,
    candidate_manifest_sha256: &'a str,
    from_content_version: &'a semver::Version,
    to_content_version: &'a semver::Version,
    changed_files: &'a [String],
}

struct StageDirectory {
    path: PathBuf,
    armed: bool,
}

impl StageDirectory {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn remove_now(&mut self) -> Result<()> {
        if self.path.exists() {
            fs::remove_dir_all(&self.path).with_context(|| {
                format!("failed to remove staging directory {}", self.path.display())
            })?;
        }
        self.armed = false;
        Ok(())
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StageDirectory {
    fn drop(&mut self) {
        if self.armed && self.path.exists() {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
#[cfg(test)]
mod tests {
    use super::valid_migration_id;

    #[test]
    fn migration_ids_cannot_escape_rollback_directory() {
        for invalid in ["", "../other", "..", "nested/name", "Upper", "a..b"] {
            assert!(!valid_migration_id(invalid), "{invalid}");
        }
        assert!(valid_migration_id("migration.v1"));
    }
}
