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

    for command in ["run", "test"] {
        let (status, result) = invoke(&project, command);
        assert!(status.success(), "{result}");
        assert_eq!(result["ok"], true);
        assert_eq!(result["command"], command);
        assert_eq!(result["validation"]["ok"], true);
        assert_eq!(result["cargo"]["exit_code"], 0);
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

    fs::write(project.join("content/zones/starter.ron"), "not valid ron").unwrap();
    let (status, result) = invoke(&project, "run");
    assert!(!status.success());
    assert_eq!(result["ok"], false);
    assert_eq!(result["validation"]["ok"], false);
    assert!(result["cargo"].is_null());

    fs::remove_dir_all(project).unwrap();
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
    let (status, result) = invoke(&project, "test");
    assert!(!status.success());
    assert_eq!(result["ok"], false);
    assert_eq!(result["validation"]["ok"], true);
    assert_ne!(result["cargo"]["exit_code"], 0);
    assert_ne!(result["cargo"]["stderr"].as_str().unwrap(), "");
    fs::remove_dir_all(project).unwrap();
}
