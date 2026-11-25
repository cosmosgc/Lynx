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
fn bulk_edits_are_atomic_and_restore_linked_placements_after_reopen() {
    use engine_core::math::Vec3;
    use std::collections::BTreeSet;

    let cli = build_cli();
    let temporary = tempfile::tempdir().unwrap();
    let project = temporary.path().join("bulk-workflow");
    let output = Command::new(&cli)
        .args(["new", "bulk-workflow", "--template", "mmorpg", "--output"])
        .arg(&project)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // An independent pair must survive the deletion, including its link.
    let source = project.join("content/zones/starter.ron");
    let mut fixture: engine_content::ContentDefinition =
        ron::from_str(&std::fs::read_to_string(&source).unwrap()).unwrap();
    for mut extra in fixture.entity_placements.clone() {
        extra.id.insert_str(0, "extra_");
        extra.position.z += 3.0;
        fixture.entity_placements.push(extra);
    }
    let mut extra_link = fixture.entity_links[0].clone();
    extra_link.source.insert_str(0, "extra_");
    extra_link.target.insert_str(0, "extra_");
    fixture.entity_links.push(extra_link);
    std::fs::write(source, ron::to_string(&fixture).unwrap()).unwrap();
    let mut session = EditorSession::open(&project).unwrap();
    let original = session.selected_zone_definition().unwrap().clone();
    let link = &original.entity_links[0];
    let ids = BTreeSet::from([link.source.clone(), link.target.clone()]);
    assert_eq!(ids.len(), 2);
    let mut stale = ids.clone();
    stale.insert("missing".into());
    assert!(session.delete_entity_placements(&stale).is_err());
    assert_eq!(session.selected_zone_definition(), Some(&original));
    let revision = session.revision();
    assert!(
        session
            .move_entity_placements(&ids, Vec3::new(1000.0, 0.0, 0.0))
            .is_err()
    );
    assert_eq!(session.revision(), revision);
    assert!(!session.undo());
    let mut moved = original.clone();
    for placement in &mut moved.entity_placements {
        if ids.contains(&placement.id) {
            placement.position.z += 1.0;
        }
    }
    session
        .move_entity_placements(&ids, Vec3::new(0.0, 0.0, 1.0))
        .unwrap();
    assert_eq!(session.selected_zone_definition(), Some(&moved));
    assert!(session.undo());
    assert_eq!(session.selected_zone_definition(), Some(&original));
    assert!(!session.undo());
    assert!(session.redo());
    session.save().unwrap();
    drop(session);
    let mut session = EditorSession::open(&project).unwrap();
    assert_eq!(session.selected_zone_definition(), Some(&moved));
    validate_cli(&cli, &project);
    session.delete_entity_placements(&ids).unwrap();
    let mut deleted = moved.clone();
    deleted.entity_placements.retain(|p| !ids.contains(&p.id));
    deleted
        .entity_links
        .retain(|l| !ids.contains(&l.source) && !ids.contains(&l.target));
    assert_eq!(session.selected_zone_definition(), Some(&deleted));
    assert!(session.undo());
    assert_eq!(session.selected_zone_definition(), Some(&moved));
    assert!(!session.undo());
    assert!(session.redo());
    session.save().unwrap();
    drop(session);
    assert_eq!(
        EditorSession::open(&project)
            .unwrap()
            .selected_zone_definition(),
        Some(&deleted)
    );
    validate_cli(&cli, &project);
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
fn placement_creation_duplication_and_linked_deletion_recover_and_validate() {
    let cli = build_cli();
    let temporary = tempfile::tempdir().unwrap();
    let project = temporary.path().join("placement-workflow");
    let output = Command::new(&cli)
        .args([
            "new",
            "placement-workflow",
            "--template",
            "mmorpg",
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
    let mut session = EditorSession::open(&project).unwrap();
    let original = session.selected_zone_definition().unwrap().clone();
    let prototype = original.entity_placements[0].clone();
    let revision = session.revision();
    assert!(
        session
            .add_entity_placement(
                &prototype.definition,
                engine_core::math::Vec3::new(1_000_000.0, 0.0, 1_000_000.0),
            )
            .is_err()
    );
    assert_eq!(session.selected_zone_definition(), Some(&original));
    assert_eq!(session.revision(), revision);
    assert!(!session.is_dirty());
    assert!(!session.undo());
    let created = session
        .add_entity_placement(&prototype.definition, prototype.position)
        .unwrap();
    let duplicated = session.duplicate_entity_placement(&created).unwrap();
    assert_ne!(created, duplicated);
    assert!(!session.validation().has_errors());
    assert!(session.undo());
    assert!(session.undo());
    assert_eq!(session.selected_zone_definition(), Some(&original));
    assert!(session.redo());
    assert!(session.redo());
    let before_delete = session.selected_zone_definition().unwrap().clone();
    let linked = original
        .entity_links
        .first()
        .expect("template has a lever/door link")
        .source
        .clone();
    session.delete_entity_placement(&linked).unwrap();
    let deleted = session.selected_zone_definition().unwrap().clone();
    assert!(
        !deleted
            .entity_links
            .iter()
            .any(|link| link.source == linked || link.target == linked)
    );
    assert!(session.undo());
    assert_eq!(session.selected_zone_definition(), Some(&before_delete));
    assert!(session.redo());
    assert_eq!(session.selected_zone_definition(), Some(&deleted));
    session.autosave().unwrap();
    drop(session);
    let mut recovered = EditorSession::open(&project).unwrap();
    assert_eq!(recovered.selected_zone_definition(), Some(&deleted));
    recovered.save().unwrap();
    let reopened = EditorSession::open(&project).unwrap();
    assert_eq!(reopened.selected_zone_definition(), Some(&deleted));
    assert!(!reopened.is_dirty());
    validate_cli(&cli, &project);
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
        let zone = session
            .selected_zone_definition()
            .expect("the generated map's selected zone definition is loaded");
        if template == "mmorpg" {
            assert!(
                !zone.entity_placements.is_empty(),
                "the MMORPG fixture should exercise authored scene placeholders"
            );
        }
        let zone_for_transform = (template == "mmorpg").then(|| zone.id.clone());
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
        let expected_zone_definition = if template == "mmorpg" {
            let zone_id = zone_for_transform.as_ref().unwrap();
            session.select_zone(zone_id).unwrap();
            let placement = session
                .selected_zone_definition()
                .unwrap()
                .entity_placements[0]
                .clone();
            session
                .set_selected_entity_placement_transform(
                    &placement.id,
                    placement.position,
                    placement.yaw_degrees + 12.0,
                )
                .unwrap();
            let transformed = session.selected_zone_definition().unwrap().clone();
            assert!(session.undo());
            assert_eq!(
                session
                    .selected_zone_definition()
                    .unwrap()
                    .entity_placements[0],
                placement
            );
            assert!(session.redo());
            assert_eq!(session.selected_zone_definition(), Some(&transformed));
            Some(transformed)
        } else {
            None
        };
        assert!(!session.validate().has_errors());
        session.save().unwrap();
        drop(session);

        let mut reopened = EditorSession::open(&project).unwrap();
        assert_eq!(reopened.map(), Some(&edited));
        if let Some(expected) = &expected_zone_definition {
            reopened
                .select_zone(zone_for_transform.as_ref().unwrap())
                .unwrap();
            assert_eq!(reopened.selected_zone_definition(), Some(expected));
        }
        assert!(!reopened.is_dirty());
        assert!(!reopened.validation().has_errors());
        assert!(!project.join(".lynx/editor-recovery.ron").exists());
        validate_cli(&cli, &project);
        println!("{template}: generate, edit, undo/redo, save/reopen, CLI validate passed");
    }
}
