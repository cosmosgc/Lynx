use super::absolute_path;
use crate::{ProjectCommandArgs, ValidateArgs};
use anyhow::{Context, Result};
use engine_content::{ContentLoader, starter_component_registry};
use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;

pub(crate) fn validate_project(args: &ValidateArgs) -> Result<Value> {
    let project_dir = absolute_path(args.project.as_path())?;
    let mut report = validate_dir(project_dir.as_path())?;
    report["command"] = json!("validate");
    report["project"] = json!(project_dir.to_string_lossy());
    Ok(report)
}

pub(super) fn validate_dir(project_dir: &Path) -> Result<Value> {
    if !project_dir.exists() {
        return Ok(json!({
            "ok": false,
            "path": project_dir.to_string_lossy(),
            "errors": [format!("project missing: {}", project_dir.display())],
            "warnings": [],
            "definitions": 0,
            "panels": 0,
            "minigames": 0,
            "maps": 0,
            "catalogs": 0,
        }));
    }

    if !project_dir.is_dir() {
        return Ok(json!({
            "ok": false,
            "path": project_dir.to_string_lossy(),
            "errors": [format!("project path is not a directory: {}", project_dir.display())],
            "warnings": [],
            "definitions": 0,
            "panels": 0,
            "minigames": 0,
            "maps": 0,
            "catalogs": 0,
        }));
    }

    let content_dir = project_dir.join("content");
    let content_root = if content_dir.is_dir() {
        content_dir.as_path()
    } else {
        project_dir
    };
    let registry = starter_component_registry()?;
    let loader = ContentLoader::new(registry);
    let (set, report) = loader.load_project(content_root);

    let errors: Vec<String> = report.errors().map(ToString::to_string).collect();
    let warnings: Vec<String> = report.warnings().map(ToString::to_string).collect();

    let ok = errors.is_empty();

    Ok(json!({
        "ok": ok,
        "path": project_dir.to_string_lossy(),
        "errors": errors,
        "warnings": warnings,
        "definitions": set.definitions.len(),
        "panels": set.panels.len(),
        "minigames": set.minigames.len(),
        "maps": set.maps.len(),
        "catalogs": set.catalogs.len(),
    }))
}

pub(crate) fn project_command(args: &ProjectCommandArgs, command: &str) -> Result<Value> {
    let project = absolute_path(&args.project)?;
    let validation = validate_dir(&project)?;
    if validation["ok"] != true {
        return Ok(json!({
            "ok": false, "command": command, "project": project.to_string_lossy(),
            "validation": validation, "cargo": null,
        }));
    }

    let manifest = project.join("Cargo.toml");
    let output = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .arg(command)
        .arg("--manifest-path")
        .arg(&manifest)
        .current_dir(&project)
        .output()
        .with_context(|| format!("failed to start cargo {command}"))?;
    Ok(json!({
        "ok": output.status.success(), "command": command,
        "project": project.to_string_lossy(), "validation": validation,
        "cargo": {
            "exit_code": output.status.code(),
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr),
        },
    }))
}
