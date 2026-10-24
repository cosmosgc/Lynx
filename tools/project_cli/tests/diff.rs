//! End-to-end tests for `lynx content diff`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lynx-cli-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn manifest(version: &str, hash: &str) -> Value {
    json!({
        "content_version": version,
        "engine": ">=0.1, <0.2",
        "definitions": [{
            "path": "entities/door.ron",
            "kind": "entity",
            "id": "prop.door",
            "version": { "major": 1, "minor": 0 },
            "sha256": hash.repeat(64),
        }],
        "locales": [{
            "locale": "pt-BR",
            "path": "locales/pt-BR.ron",
            "sha256": "a".repeat(64),
        }],
    })
}

fn write_manifest(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

fn run_diff(base: &Path, next: &Path) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_lynx"))
        .args([
            "content",
            "diff",
            "--base",
            &base.to_string_lossy(),
            "--next",
            &next.to_string_lossy(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "diff command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn run_diff_human(base: &Path, next: &Path) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_lynx"))
        .args([
            "content",
            "diff",
            "--base",
            &base.to_string_lossy(),
            "--next",
            &next.to_string_lossy(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "diff command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn compatible_addition_reports_the_required_bump() {
    let dir = temp_dir("compatible");
    let base_path = dir.join("base.json");
    let next_path = dir.join("next.json");
    write_manifest(&base_path, &manifest("1.0.0", "b"));
    let mut next = manifest("1.1.0", "b");
    next["definitions"].as_array_mut().unwrap().push(json!({
        "path": "entities/barrel.ron",
        "kind": "entity",
        "id": "prop.barrel",
        "version": { "major": 1, "minor": 0 },
        "sha256": "c".repeat(64),
    }));
    write_manifest(&next_path, &next);

    let output = run_diff(&base_path, &next_path);
    assert_eq!(output["ok"], json!(true));
    assert_eq!(output["command"], json!("content diff"));
    assert_eq!(output["required_bump"], json!("minor"));
    assert_eq!(output["added"][0]["id"], json!("prop.barrel"));
    assert_eq!(output["report"]["diagnostics"], json!([]));
}

#[test]
fn changed_bytes_without_a_bump_are_rejected() {
    let dir = temp_dir("rejected");
    let base_path = dir.join("base.json");
    let next_path = dir.join("next.json");
    write_manifest(&base_path, &manifest("1.0.0", "b"));
    write_manifest(&next_path, &manifest("1.0.0", "d"));

    let output = run_diff(&base_path, &next_path);
    assert_eq!(output["ok"], json!(false));
    assert_eq!(output["required_bump"], json!("minor"));
    assert!(
        output["report"]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["code"] == json!("content.diff.version_bump_too_small"))
    );
}

#[test]
fn human_output_summarizes_the_change() {
    let dir = temp_dir("human");
    let base_path = dir.join("base.json");
    let next_path = dir.join("next.json");
    write_manifest(&base_path, &manifest("1.0.0", "b"));
    write_manifest(&next_path, &manifest("1.0.0", "d"));

    let text = run_diff_human(&base_path, &next_path);
    assert!(text.contains("Content diff 1.0.0 -> 1.0.0: incompatible"));
    assert!(text.contains("- changed: prop.door"));
    assert!(text.contains("content.diff.version_bump_too_small"));
}
