//! End-to-end tests for `lynx content publish-check`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lynx-publish-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn manifest(version: &str, publishing: &Value) -> Value {
    json!({
        "content_version": version,
        "engine": ">=0.1, <0.2",
        "definitions": [{
            "path": "entities/door.ron",
            "kind": "entity",
            "id": "prop.door",
            "version": { "major": 1, "minor": 0 },
            "sha256": "b".repeat(64),
        }],
        "locales": [{
            "locale": "pt-BR",
            "path": "locales/pt-BR.ron",
            "sha256": "a".repeat(64),
        }],
        "publishing": publishing,
    })
}

fn publishing(author: &str, reviewer: &str) -> Value {
    json!({
        "author": author,
        "license": "CC-BY-4.0",
        "state": "approved",
        "revision": 1,
        "reviewer": reviewer,
    })
}

fn write_manifest(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

fn run(base: &Path, candidate: &Path, extra: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_lynx"))
        .args([
            "content",
            "publish-check",
            "--base",
            &base.to_string_lossy(),
            "--manifest",
            &candidate.to_string_lossy(),
            "--json",
        ])
        .args(extra)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "publish-check failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn candidate_manifest(publishing: &Value) -> Value {
    let mut candidate = manifest("1.1.0", publishing);
    candidate["definitions"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "path": "entities/barrel.ron",
            "kind": "entity",
            "id": "prop.barrel",
            "version": { "major": 1, "minor": 0 },
            "sha256": "c".repeat(64),
        }));
    candidate
}

#[test]
fn an_approved_candidate_is_allowed() {
    let dir = temp_dir("approved");
    let base_path = dir.join("base.json");
    let candidate_path = dir.join("candidate.json");
    write_manifest(
        &base_path,
        &manifest("1.0.0", &publishing("alice", "moderator")),
    );
    write_manifest(
        &candidate_path,
        &candidate_manifest(&publishing("alice", "moderator")),
    );

    let output = run(&base_path, &candidate_path, &[]);
    assert_eq!(output["ok"], json!(true));
    assert_eq!(output["command"], json!("content publish-check"));
    assert_eq!(output["author"], json!("alice"));
    assert_eq!(output["state"], json!("approved"));
    assert_eq!(output["required_bump"], json!("minor"));
    assert_eq!(output["diagnostics"], json!([]));
}

#[test]
fn self_approval_is_blocked_by_default() {
    let dir = temp_dir("self-approved");
    let base_path = dir.join("base.json");
    let candidate_path = dir.join("candidate.json");
    write_manifest(
        &base_path,
        &manifest("1.0.0", &publishing("alice", "moderator")),
    );
    write_manifest(
        &candidate_path,
        &candidate_manifest(&publishing("alice", "alice")),
    );

    let output = run(&base_path, &candidate_path, &[]);
    assert_eq!(output["ok"], json!(false));
    assert!(
        output["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["code"] == json!("content.publish.self_approved"))
    );

    let allowed = run(&base_path, &candidate_path, &["--allow-self-review"]);
    assert_eq!(allowed["ok"], json!(true));
}

#[test]
fn a_draft_needs_approval_unless_review_is_skipped() {
    let dir = temp_dir("draft");
    let base_path = dir.join("base.json");
    let candidate_path = dir.join("candidate.json");
    let draft = json!({
        "author": "alice",
        "license": "CC-BY-4.0",
        "state": "draft",
        "revision": 0,
    });
    write_manifest(
        &base_path,
        &manifest("1.0.0", &publishing("alice", "moderator")),
    );
    write_manifest(&candidate_path, &candidate_manifest(&draft));

    let blocked = run(&base_path, &candidate_path, &[]);
    assert_eq!(blocked["ok"], json!(false));
    assert!(
        blocked["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["code"] == json!("content.publish.not_approved"))
    );

    let allowed = run(&base_path, &candidate_path, &["--skip-review"]);
    assert_eq!(allowed["ok"], json!(true));
}

#[test]
fn a_manifest_without_publishing_metadata_is_rejected() {
    let dir = temp_dir("no-metadata");
    let base_path = dir.join("base.json");
    let candidate_path = dir.join("candidate.json");
    let mut candidate = candidate_manifest(&publishing("alice", "moderator"));
    candidate.as_object_mut().unwrap().remove("publishing");
    write_manifest(
        &base_path,
        &manifest("1.0.0", &publishing("alice", "moderator")),
    );
    write_manifest(&candidate_path, &candidate);

    let output = Command::new(env!("CARGO_BIN_EXE_lynx"))
        .args([
            "content",
            "publish-check",
            "--base",
            &base_path.to_string_lossy(),
            "--manifest",
            &candidate_path.to_string_lossy(),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no publishing record"),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_missing_license_blocks_publishing_unless_optional() {
    let dir = temp_dir("license");
    let base_path = dir.join("base.json");
    let candidate_path = dir.join("candidate.json");
    let mut record = publishing("alice", "moderator");
    record.as_object_mut().unwrap().remove("license");
    write_manifest(
        &base_path,
        &manifest("1.0.0", &publishing("alice", "moderator")),
    );
    write_manifest(&candidate_path, &candidate_manifest(&record));

    let blocked = run(&base_path, &candidate_path, &[]);
    assert_eq!(blocked["ok"], json!(false));
    assert!(
        blocked["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["code"] == json!("content.publish.missing_license"))
    );

    let allowed = run(&base_path, &candidate_path, &["--license-optional"]);
    assert_eq!(allowed["ok"], json!(true));
}
