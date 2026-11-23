//! Exercise the editor session against real CLI-generated projects and validation.
use std::path::{Path, PathBuf};
use std::process::Command;

use engine_content::WorldMapDefinition;
use engine_core::id::DefinitionId;
use lynx_editor::EditorSession;

fn build_cli() -> PathBuf {
    // Use this test's target directory, including when CARGO_TARGET_DIR is set.
    let executable = std::env::current_exe().unwrap();
    let target_profile = executable.parent().unwrap().parent().unwrap();
    let target = target_profile.parent().unwrap();
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new(env!("CARGO"))
        .current_dir(workspace)
        .args([
            "build",
            "-p",
            "project_cli",
            "--bin",
            "lynx",
            "--target-dir",
        ])
        .arg(target)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "CLI build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    target
        .join("debug")
        .join(format!("lynx{}", std::env::consts::EXE_SUFFIX))
}

#[test]
fn interrupted_multi_map_save_recovers_all_edits_and_passes_cli_validation() {
    let cli = build_cli();
    let temporary = tempfile::tempdir().unwrap();
    let project = temporary.path().join("multi-map");
    let output = Command::new(&cli)
        .args([
            "new",
            "recovery-workflow",
            "--template",
            "minimal",
            "--output",
        ])
        .arg(&project)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Split the generated atlas into two valid maps with distinct zones.
    let first_path = project.join("content/maps/overworld.ron");
    let second_directory = project.join("content/maps/secondary");
    let second_path = second_directory.join("market.ron");
    let mut first: WorldMapDefinition =
        ron::from_str(&std::fs::read_to_string(&first_path).unwrap()).unwrap();
    let mut second = first.clone();
    second.id = DefinitionId::new("map.secondary").unwrap();
    second.zones = vec![first.zones.pop().unwrap()];
    first.routes.clear();
    second.routes.clear();
    std::fs::create_dir_all(&second_directory).unwrap();
    std::fs::write(&first_path, ron::to_string(&first).unwrap()).unwrap();
    std::fs::write(&second_path, ron::to_string(&second).unwrap()).unwrap();
    validate_cli(&cli, &project);
    let original_second_bytes = std::fs::read(&second_path).unwrap();

    let mut session = EditorSession::open(&project).unwrap();
    let mut expected = Vec::new();
    for map in [&first, &second] {
        session.select_map(&map.id).unwrap();
        session.move_selected(4, 3).unwrap();
        expected.push(session.map().unwrap().clone());
    }
    assert!(!session.validate().has_errors());

    // The save orders dirty maps by ID. Block the second map's parent so
    // the first replacement succeeds and the second returns an I/O error.
    let parked = temporary.path().join("parked-secondary");
    std::fs::rename(&second_directory, &parked).unwrap();
    std::fs::write(&second_directory, b"save fault injection").unwrap();
    assert!(session.save().is_err());
    let recovery = project.join(".lynx/editor-recovery.ron");
    assert!(recovery.is_file());
    let first_on_disk: WorldMapDefinition =
        ron::from_str(&std::fs::read_to_string(&first_path).unwrap()).unwrap();
    assert_eq!(first_on_disk, expected[0]);
    assert_eq!(
        std::fs::read(parked.join("market.ron")).unwrap(),
        original_second_bytes
    );
    drop(session);
    std::fs::remove_file(&second_directory).unwrap();
    std::fs::rename(&parked, &second_directory).unwrap();

    let mut recovered = EditorSession::open(&project).unwrap();
    assert!(recovered.is_dirty());
    assert_eq!(recovered.maps().cloned().collect::<Vec<_>>(), expected);
    assert!(!recovered.validate().has_errors());
    recovered.save().unwrap();
    drop(recovered);
    assert!(!recovery.exists());
    let reopened = EditorSession::open(&project).unwrap();
    assert!(!reopened.is_dirty());
    assert_eq!(reopened.maps().cloned().collect::<Vec<_>>(), expected);
    validate_cli(&cli, &project);
    println!(
        "multi-map: partial save confirmed; all edits recovered; save/reopen and CLI validate passed"
    );
}

fn validate_cli(cli: &Path, project: &Path) {
    let output = Command::new(cli)
        .args(["validate", "--project"])
        .arg(project)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "CLI validation failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn invalid_map_reference_has_matching_editor_and_cli_field_diagnostics() {
    let cli = build_cli();
    let temporary = tempfile::tempdir().unwrap();
    let project = temporary.path().join("diagnostics");
    let generated = Command::new(&cli)
        .args([
            "new",
            "diagnostic-workflow",
            "--template",
            "mmorpg",
            "--output",
        ])
        .arg(&project)
        .output()
        .unwrap();
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    validate_cli(&cli, &project);
    let source = project.join("content/maps/overworld.ron");
    let original = std::fs::read(&source).unwrap();
    let mut map: WorldMapDefinition = ron::de::from_bytes(&original).unwrap();
    map.zones[0].zone = DefinitionId::new("zone.missing").unwrap();
    std::fs::write(&source, ron::to_string(&map).unwrap()).unwrap();

    let mut session = EditorSession::open(&project).unwrap();
    assert!(session.validation().has_errors());
    let diagnostic = session
        .validation()
        .errors()
        .find(|diagnostic| diagnostic.code == "world.unknown_zone")
        .expect("unknown zone must be reported as an error");
    assert_eq!(diagnostic.path, "maps/overworld.ron:zones[0].zone");
    assert_eq!(
        diagnostic.message,
        "zone zone.missing from map.overworld does not resolve to a loaded zone definition"
    );
    let editor_errors: Vec<_> = session
        .validation()
        .errors()
        .map(ToString::to_string)
        .collect();
    let output = Command::new(&cli)
        .args(["validate", "--json", "--project"])
        .arg(&project)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ok"], false);
    assert_eq!(report["errors"], serde_json::json!(editor_errors));
    println!("{diagnostic}; CLI errors match editor errors exactly (exit 2)");

    std::fs::write(&source, original).unwrap();
    session.reload_external_changes().unwrap();
    assert!(!session.validation().has_errors());
    validate_cli(&cli, &project);
    println!("repaired map: editor reload and CLI validation passed");
}

#[test]
fn generated_projects_round_trip_editor_edits_and_pass_cli_validation() {
    let cli = build_cli();
    let temporary = tempfile::tempdir().unwrap();
    for template in ["minimal", "mmorpg"] {
        let project = temporary.path().join(template);
        let output = Command::new(&cli)
            .args(["new", "editor-workflow", "--template", template, "--output"])
            .arg(&project)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "Generation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        validate_cli(&cli, &project);
        let mut session = EditorSession::open(&project).unwrap();
        assert!(!session.validation().has_errors());
        let original = session.map().unwrap().clone();
        assert_eq!(
            original.zones.len(),
            2,
            "starter atlas must contain both zones"
        );
        // Translate the entire atlas to preserve passage adjacency.
        for zone in &original.zones {
            session.select_zone(&zone.zone).unwrap();
            session.move_selected(3, -2).unwrap();
        }
        let edited = session.map().unwrap().clone();
        assert_ne!(original, edited);
        assert!(!session.validate().has_errors());
        for _ in &original.zones {
            assert!(session.undo());
        }
        assert_eq!(session.map(), Some(&original));
        for _ in &original.zones {
            assert!(session.redo());
        }
        assert_eq!(session.map(), Some(&edited));
        session.save().unwrap();
        drop(session);

        let reopened = EditorSession::open(&project).unwrap();
        assert_eq!(reopened.map(), Some(&edited));
        assert!(!reopened.is_dirty());
        assert!(!reopened.validation().has_errors());
        assert!(!project.join(".lynx/editor-recovery.ron").exists());
        validate_cli(&cli, &project);
        println!("{template}: generate, edit, undo/redo, save/reopen, CLI validate passed");
    }
}
