//! End-to-end tests for generated project templates.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

fn temp_dir() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("lynx-cli-new-{}-{nonce}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(args: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_lynx"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "lynx {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn has_file(report: &Value, path: &Path) -> bool {
    let expected = path.to_string_lossy().replace('\\', "/");
    report["files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|file| file.as_str() == Some(expected.as_str()))
}

#[test]
fn templates_generate_and_validate_their_starter_content() {
    let temp = temp_dir();
    for (template, definitions, panels, minigames) in [("minimal", 3, 1, 0), ("mmorpg", 4, 2, 1)] {
        let project = temp.join(template);
        let project_str = project.to_string_lossy().into_owned();
        let created = run(&[
            "--json",
            "new",
            "test-world",
            "--template",
            template,
            "--output",
            &project_str,
        ]);
        assert_eq!(created["ok"], json!(true));
        assert_eq!(created["template"], json!(template));
        for relative in [
            "content/zones/starter.ron",
            "content/entities/tavern_keeper.ron",
            "content/items/torch.ron",
            "content/ui/inventory.ron",
            "content/localization/pt-BR.ron",
        ] {
            assert!(has_file(&created, &project.join(relative)));
        }

        let doctor = run(&["--json", "doctor", "--project", &project_str]);
        assert_eq!(doctor["ok"], json!(true), "{doctor}");
        assert_eq!(doctor["project"]["definitions"], json!(definitions));
        assert_eq!(doctor["project"]["panels"], json!(panels));
        assert_eq!(doctor["project"]["minigames"], json!(minigames));
        assert_eq!(doctor["project"]["catalogs"], json!(1));

        let validation = run(&["--json", "validate", "--project", &project_str]);
        assert_eq!(validation["ok"], json!(true), "{validation}");

        if template == "mmorpg" {
            for relative in [
                "content/entities/training_master.ron",
                "content/ui/practice.ron",
                "content/minigames/practice.ron",
            ] {
                assert!(has_file(&created, &project.join(relative)));
            }
        }
    }
    fs::remove_dir_all(temp).unwrap();
}
