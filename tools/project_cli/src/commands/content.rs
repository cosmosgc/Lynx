use super::{absolute_path, project};
use crate::{ContentArgs, ContentBuildArgs, ContentCommand, DiffArgs, PublishCheckArgs};
use anyhow::{Context, Result, bail};
use engine_content::{
    ContentLoader, ContentManifest, ContentSet, PublishingPolicy, check_publish, diff, sha256_hex,
    starter_component_registry,
};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;

pub(crate) fn content(args: &ContentArgs) -> Result<Value> {
    match &args.command {
        ContentCommand::Build(build_args) => build_content(build_args),
        ContentCommand::Hash(hash_args) => hash_file(hash_args.path.as_path()),
        ContentCommand::Diff(diff_args) => diff_manifests(diff_args),
        ContentCommand::PublishCheck(check_args) => check_publish_manifest(check_args),
    }
}

fn build_content(args: &ContentBuildArgs) -> Result<Value> {
    let project = absolute_path(&args.project)?;
    let validation = project::validate_dir(&project)?;
    let output = if args.output.is_absolute() {
        args.output.clone()
    } else {
        project.join(&args.output)
    };
    if validation["ok"] != true {
        return Ok(json!({
            "ok": false,
            "command": "content build",
            "project": project.to_string_lossy(),
            "output": output.to_string_lossy(),
            "validation": validation,
            "manifest": null,
        }));
    }
    let version = semver::Version::parse(&args.content_version)
        .context("--content-version must be a semantic version")?;
    let engine = semver::VersionReq::parse(&args.engine)
        .context("--engine must be a semantic-version requirement")?;
    let content_root = project.join("content");
    let (set, report) =
        ContentLoader::new(starter_component_registry()?).load_project(&content_root);
    if report.has_errors() {
        bail!("content validation changed while building manifest");
    }
    let manifest = manifest_for(&content_root, &set, version, engine)?;
    manifest
        .write(&output)
        .with_context(|| format!("failed to write {}", output.display()))?;
    Ok(json!({
        "ok": true,
        "command": "content build",
        "project": project.to_string_lossy(),
        "output": output.to_string_lossy(),
        "validation": validation,
        "manifest": manifest,
    }))
}

fn manifest_for(
    root: &Path,
    set: &ContentSet,
    content_version: semver::Version,
    engine: semver::VersionReq,
) -> Result<ContentManifest> {
    let mut manifest = ContentManifest::new(content_version, engine);
    for (id, definition) in &set.definitions {
        let path = &set.sources[id];
        manifest.add_definition(path, definition, &fs::read(root.join(path))?)?;
    }
    for (id, panel) in &set.panels {
        let path = &set.panel_sources[id];
        manifest.add_panel(path, panel, &fs::read(root.join(path))?)?;
    }
    for (id, minigame) in &set.minigames {
        let path = &set.minigame_sources[id];
        manifest.add_minigame(path, minigame, &fs::read(root.join(path))?)?;
    }
    for (id, map) in &set.maps {
        let path = &set.map_sources[id];
        manifest.add_map(path, map, &fs::read(root.join(path))?)?;
    }
    for (locale, path) in &set.catalog_paths {
        manifest.add_locale(path, locale.clone(), &fs::read(root.join(path))?)?;
    }
    Ok(manifest)
}

fn hash_file(path: &Path) -> Result<Value> {
    let path = absolute_path(path)?;
    let bytes = fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let digest = sha256_hex(&bytes);
    Ok(json!({
        "ok": true,
        "command": "content hash",
        "path": path.to_string_lossy(),
        "sha256": digest,
    }))
}

fn diff_manifests(args: &DiffArgs) -> Result<Value> {
    let base_path = absolute_path(args.base.as_path())?;
    let next_path = absolute_path(args.next.as_path())?;
    let base = ContentManifest::read(&base_path)
        .with_context(|| format!("failed to read {}", base_path.display()))?;
    let next = ContentManifest::read(&next_path)
        .with_context(|| format!("failed to read {}", next_path.display()))?;

    let result = diff(&base, &next);
    let mut output = serde_json::to_value(&result)?;
    output["ok"] = Value::Bool(result.is_compatible());
    output["command"] = Value::String("content diff".to_string());
    output["base"] = Value::String(base_path.to_string_lossy().into_owned());
    output["next"] = Value::String(next_path.to_string_lossy().into_owned());
    Ok(output)
}

fn check_publish_manifest(args: &PublishCheckArgs) -> Result<Value> {
    let manifest_path = absolute_path(args.manifest.as_path())?;
    let base_path = absolute_path(args.base.as_path())?;
    let candidate = ContentManifest::read(&manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let base = ContentManifest::read(&base_path)
        .with_context(|| format!("failed to read {}", base_path.display()))?;

    let Some(record) = &candidate.publishing else {
        bail!(
            "manifest has no publishing record: {}",
            manifest_path.display()
        );
    };
    let policy = PublishingPolicy {
        require_review: !args.skip_review,
        allow_self_review: args.allow_self_review,
        require_license: !args.license_optional,
    };
    let result = diff(&base, &candidate);
    let report = check_publish(record, &result, &policy);

    let mut output = serde_json::to_value(&report)?;
    output["ok"] = Value::Bool(report.is_ok());
    output["command"] = Value::String("content publish-check".to_string());
    output["manifest"] = Value::String(manifest_path.to_string_lossy().into_owned());
    output["base"] = Value::String(base_path.to_string_lossy().into_owned());
    output["author"] = Value::String(record.author.to_string());
    output["state"] = Value::String(record.state.as_str().to_string());
    output["revision"] = Value::from(record.revision);
    output["actual_bump"] = serde_json::to_value(result.actual_bump)?;
    output["required_bump"] = serde_json::to_value(result.required_bump)?;
    Ok(output)
}
