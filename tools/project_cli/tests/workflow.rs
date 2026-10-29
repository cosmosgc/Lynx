//! End-to-end tests for the validated project execution workflow.
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

fn invoke(project: &Path, command: &str) -> (std::process::ExitStatus, Value) {
    let output = Command::new(env!("CARGO_BIN_EXE_lynx"))
        .args([command, "--project"])
        .arg(project)
        .arg("--json")
        .output()
        .unwrap();
    let value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("invalid JSON: {}", String::from_utf8_lossy(&output.stderr)));
    (output.status, value)
}

#[test]
fn run_and_test_validate_before_executing_cargo() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let project =
        std::env::temp_dir().join(format!("lynx-workflow-{}-{nonce}", std::process::id()));
    let generated = Command::new(env!("CARGO_BIN_EXE_lynx"))
        .args(["new", "workflow", "--output"])
        .arg(&project)
        .output()
        .unwrap();
    assert!(generated.status.success());

    // Keep this end-to-end test independent of the engine's build dependencies.
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"workflow\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::write(
        project.join("src/main.rs"),
        "fn main() { println!(\"workflow ran\"); }\n#[test] fn works() { assert_eq!(2 + 2, 4); }\n",
    )
    .unwrap();

    for command in ["build", "run", "test"] {
        let (status, result) = invoke(&project, command);
        assert!(status.success(), "{result}");
        assert_eq!(result["ok"], true);
        assert_eq!(result["command"], command);
        assert_eq!(result["validation"]["ok"], true);
        assert_eq!(result["cargo"]["exit_code"], 0);
        if command != "build" {
            assert!(
                result["cargo"]["stdout"]
                    .as_str()
                    .unwrap()
                    .contains(if command == "run" {
                        "workflow ran"
                    } else {
                        "test result: ok"
                    })
            );
        }
    }

    fs::write(project.join("content/zones/starter.ron"), "not valid ron").unwrap();
    for command in ["build", "run", "test"] {
        let (status, result) = invoke(&project, command);
        assert!(!status.success());
        assert_eq!(result["ok"], false);
        assert_eq!(result["validation"]["ok"], false);
        assert!(result["cargo"].is_null());
    }

    fs::remove_dir_all(project).unwrap();
}

#[test]
fn generated_templates_build_run_and_test_without_modification() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root =
        std::env::temp_dir().join(format!("lynx-real-workflow-{}-{nonce}", std::process::id()));
    for template in ["minimal", "mmorpg"] {
        let project = root.join(template);
        let generated = Command::new(env!("CARGO_BIN_EXE_lynx"))
            .args(["new", "workflow", "--template", template, "--output"])
            .arg(&project)
            .output()
            .unwrap();
        assert!(
            generated.status.success(),
            "{}",
            String::from_utf8_lossy(&generated.stderr)
        );
        for command in ["build", "run", "test"] {
            let output = Command::new(env!("CARGO_BIN_EXE_lynx"))
                .args([command, "--project"])
                .arg(&project)
                .arg("--json")
                .env("CARGO_TARGET_DIR", root.join("target"))
                .output()
                .unwrap();
            let result: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
                panic!("invalid JSON: {}", String::from_utf8_lossy(&output.stderr))
            });
            assert!(output.status.success(), "{template} {command}: {result}");
            assert_eq!(result["validation"]["ok"], true);
            assert_eq!(result["cargo"]["exit_code"], 0);
            if command == "run" {
                assert!(
                    result["cargo"]["stdout"]
                        .as_str()
                        .unwrap()
                        .contains("Welcome to workflow!")
                );
            }
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn cargo_failure_is_reported_and_returns_nonzero() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let project =
        std::env::temp_dir().join(format!("lynx-workflow-fail-{}-{nonce}", std::process::id()));
    let generated = Command::new(env!("CARGO_BIN_EXE_lynx"))
        .args(["new", "workflow", "--output"])
        .arg(&project)
        .output()
        .unwrap();
    assert!(generated.status.success());
    fs::write(project.join("Cargo.toml"), "not a valid manifest").unwrap();
    for command in ["build", "run", "test"] {
        let (status, result) = invoke(&project, command);
        assert!(!status.success());
        assert_eq!(result["ok"], false);
        assert_eq!(result["validation"]["ok"], true);
        assert_ne!(result["cargo"]["exit_code"], 0);
        assert_ne!(result["cargo"]["stderr"].as_str().unwrap(), "");
    }
    fs::remove_dir_all(project).unwrap();
}

#[test]
fn content_build_writes_a_validated_stable_manifest() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let project =
        std::env::temp_dir().join(format!("lynx-content-build-{}-{nonce}", std::process::id()));
    let generated = Command::new(env!("CARGO_BIN_EXE_lynx"))
        .args(["new", "content-build", "--template", "mmorpg", "--output"])
        .arg(&project)
        .output()
        .unwrap();
    assert!(generated.status.success());

    let output = Command::new(env!("CARGO_BIN_EXE_lynx"))
        .args([
            "content",
            "build",
            "--project",
            project.to_str().unwrap(),
            "--content-version",
            "1.2.3",
            "--engine",
            "^0.1",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ok"], true);
    assert_eq!(report["command"], "content build");
    assert_eq!(report["manifest"]["content_version"], "1.2.3");
    let manifest = fs::read_to_string(project.join("content/manifest.json")).unwrap();
    assert!(manifest.contains("entity.training_master"));
    assert!(manifest.contains("pt-BR"));

    fs::write(project.join("content/zones/starter.ron"), "not valid ron").unwrap();
    let failed = Command::new(env!("CARGO_BIN_EXE_lynx"))
        .args(["content", "build", "--project"])
        .arg(&project)
        .arg("--json")
        .output()
        .unwrap();
    assert!(failed.status.success(), "validation is a structured report");
    let report: Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(report["ok"], false);
    assert!(report["manifest"].is_null());
    assert_eq!(
        fs::read_to_string(project.join("content/manifest.json")).unwrap(),
        manifest
    );
    fs::remove_dir_all(project).unwrap();
}
