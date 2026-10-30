use super::absolute_path;
use crate::{ProjectCommandArgs, ValidateArgs};
use anyhow::{Context, Result};
use engine_content::ContentLoader;
use engine_core::schema::{ComponentKey, ComponentRegistry, ComponentSchema, FieldSchema, FieldType, PersistencePolicy, SchemaVersion, Visibility};
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
    let registry = project_component_registry()?;
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

pub(super) fn project_component_registry() -> Result<ComponentRegistry> {
    let mut registry = ComponentRegistry::new();
    let component_schemas = [
        ComponentSchema::new(
            ComponentKey::new("core.zone")?,
            SchemaVersion::new(1, 0),
            vec![
                FieldSchema::required("max_players", FieldType::Int { min: 1, max: 1000 }),
                FieldSchema::required(
                    "cell_size_m",
                    FieldType::Float {
                        min: 0.25,
                        max: 64.0,
                    },
                ),
            ],
            Visibility::ServerOnly,
            PersistencePolicy::StaticDefinition,
        ),
        ComponentSchema::new(
            ComponentKey::new("core.interactable")?,
            SchemaVersion::new(1, 0),
            vec![
                FieldSchema::required(
                    "verbs",
                    FieldType::List {
                        item: Box::new(FieldType::Enum {
                            values: ["inspect", "talk", "practice"]
                                .into_iter()
                                .map(str::to_owned)
                                .collect(),
                        }),
                        max_len: 8,
                    },
                ),
                FieldSchema::required(
                    "range_m",
                    FieldType::Float {
                        min: 0.1,
                        max: 20.0,
                    },
                ),
                FieldSchema::required("requires_line_of_sight", FieldType::Bool),
            ],
            Visibility::Client,
            PersistencePolicy::StaticDefinition,
        ),
        ComponentSchema::new(
            ComponentKey::new("core.item")?,
            SchemaVersion::new(1, 0),
            vec![
                FieldSchema::required("stack_max", FieldType::Int { min: 1, max: 999 }),
                FieldSchema::required(
                    "weight_kg",
                    FieldType::Float {
                        min: 0.0,
                        max: 1000.0,
                    },
                ),
            ],
            Visibility::Client,
            PersistencePolicy::StaticDefinition,
        ),
    ];
    for schema in component_schemas {
        registry
            .register(schema)
            .context("failed to register starter component schema")?;
    }
    Ok(registry)
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
