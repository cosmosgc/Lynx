use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use engine_content::{
    CatalogMigration, ContentManifest, ContentMigrationPlan, DefinitionMigration,
    MigrationOperation, sha256_hex,
};
use engine_core::i18n::Locale;
use engine_core::id::DefinitionId;
use engine_core::schema::SchemaVersion;
use serde_json::{Value, json};

fn temp_dir() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("lynx-cli-migrate-{}-{nonce}", std::process::id()));
    fs::create_dir_all(&path).unwrap();
    path
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lynx"))
        .args(args)
        .output()
        .unwrap()
}

fn report(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "expected JSON report, got stdout {:?} and stderr {:?}: {error}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn path_arg(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn fixture_project(root: &Path) -> PathBuf {
    let project = root.join("fixture");
    let output = run(&[
        "--json",
        "new",
        "migration-fixture",
        "--template",
        "minimal",
        "--output",
        &path_arg(&project),
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = run(&[
        "--json",
        "content",
        "build",
        "--project",
        &path_arg(&project),
        "--content-version",
        "1.0.0",
        "--engine",
        "^0.1",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    project
}

fn migration_plan(project: &Path, expected_name: &str, file_name: &str) -> PathBuf {
    let manifest_path = project.join("content/manifest.json");
    let manifest_bytes = fs::read(&manifest_path).unwrap();
    let manifest: ContentManifest = serde_json::from_slice(&manifest_bytes).unwrap();
    let plan = ContentMigrationPlan {
        id: "rename-torch-name".into(),
        source_manifest_sha256: sha256_hex(&manifest_bytes),
        from_content_version: semver::Version::new(1, 0, 0),
        to_content_version: semver::Version::new(1, 1, 0),
        definitions: vec![DefinitionMigration {
            definition: DefinitionId::new("item.torch").unwrap(),
            from_version: SchemaVersion::new(1, 0),
            to_version: SchemaVersion::new(1, 1),
            operations: vec![MigrationOperation::Replace {
                path: "/display/name".into(),
                expected: json!(expected_name),
                value: json!("item.torch.title"),
            }],
        }],
        catalogs: vec![CatalogMigration {
            locale: Locale::parse("pt-BR").unwrap(),
            operations: vec![MigrationOperation::Rename {
                from: "/messages/item.torch.name".into(),
                to: "/messages/item.torch.title".into(),
                expected: json!("Tocha"),
            }],
        }],
    };
    assert_eq!(manifest.content_version, plan.from_content_version);
    let plan_path = project.join(file_name);
    fs::write(
        &plan_path,
        ron::ser::to_string_pretty(&plan, ron::ser::PrettyConfig::default()).unwrap(),
    )
    .unwrap();
    plan_path
}

fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, directory: &Path, output: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, output);
            } else {
                output.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut output = BTreeMap::new();
    visit(root, root, &mut output);
    output
}

#[test]
#[allow(clippy::too_many_lines)] // One scenario proves preview, apply, failure, and rollback together.
fn migration_dry_run_failure_apply_and_rollback_preserve_project_boundaries() {
    let temp = temp_dir();
    let project = fixture_project(&temp);
    let plan = migration_plan(&project, "item.torch.name", "migration.ron");
    let project_path = path_arg(&project);
    let plan_path = path_arg(&plan);
    let before = snapshot_tree(&project.join("content"));

    let dry_run = run(&[
        "--json",
        "migrate",
        "apply",
        "--project",
        &project_path,
        "--plan",
        &plan_path,
    ]);
    assert!(
        dry_run.status.success(),
        "{}",
        String::from_utf8_lossy(&dry_run.stderr)
    );
    let dry_report = report(&dry_run);
    assert_eq!(dry_report["mode"], json!("dry_run"));
    assert_eq!(
        dry_report["affected_definitions"].as_array().unwrap().len(),
        1
    );
    assert_eq!(dry_report["affected_catalogs"].as_array().unwrap().len(), 1);
    assert_eq!(snapshot_tree(&project.join("content")), before);
    assert!(!project.join(".lynx").exists());

    let bad_plan = migration_plan(&project, "stale.expected.value", "stale-migration.ron");
    let bad_path = path_arg(&bad_plan);
    let rejected = run(&[
        "--json",
        "migrate",
        "apply",
        "--project",
        &project_path,
        "--plan",
        &bad_path,
        "--apply",
    ]);
    assert_eq!(rejected.status.code(), Some(1));
    assert_eq!(report(&rejected)["error"]["code"], json!("command_failed"));
    assert_eq!(snapshot_tree(&project.join("content")), before);
    assert!(!project.join(".lynx").exists());

    let applied = run(&[
        "--json",
        "migrate",
        "apply",
        "--project",
        &project_path,
        "--plan",
        &plan_path,
        "--apply",
    ]);
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let applied_report = report(&applied);
    assert_eq!(applied_report["mode"], json!("applied"));
    let manifest: ContentManifest =
        serde_json::from_slice(&fs::read(project.join("content/manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest.content_version, semver::Version::new(1, 1, 0));
    let migrated_definition = fs::read_to_string(project.join("content/items/torch.ron")).unwrap();
    assert!(migrated_definition.contains("item.torch.title"));
    let migrated_catalog =
        fs::read_to_string(project.join("content/localization/pt-BR.ron")).unwrap();
    assert!(migrated_catalog.contains("item.torch.title"));

    let catalog_path = project.join("content/localization/pt-BR.ron");
    fs::write(&catalog_path, migrated_catalog.replace("Tocha", "Lanterna")).unwrap();
    let refused_rollback = run(&[
        "--json",
        "migrate",
        "rollback",
        "--project",
        &project_path,
        "--id",
        "rename-torch-name",
    ]);
    assert_eq!(refused_rollback.status.code(), Some(1));
    assert!(
        fs::read_to_string(&catalog_path)
            .unwrap()
            .contains("Lanterna")
    );
    fs::write(&catalog_path, migrated_catalog.as_bytes()).unwrap();

    let rollback = run(&[
        "--json",
        "migrate",
        "rollback",
        "--project",
        &project_path,
        "--id",
        "rename-torch-name",
    ]);
    assert!(
        rollback.status.success(),
        "{}",
        String::from_utf8_lossy(&rollback.stderr)
    );
    assert_eq!(
        report(&rollback)["restored_content_version"],
        json!("1.0.0")
    );
    assert_eq!(snapshot_tree(&project.join("content")), before);
    fs::remove_dir_all(temp).unwrap();
}
