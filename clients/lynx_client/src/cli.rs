//! Offline command-line options shared by the windowed and headless binaries.
//!
//! These flags only configure local presentation. They never enable gameplay
//! authority or bypass the server's admission and validation rules.

use std::path::PathBuf;

use crate::offline::CameraMode;

/// Local scene, input, and replay options.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OfflineOptions {
    /// JSON replay fixture. Exactly one update is consumed per frame.
    pub replay: Option<PathBuf>,
    /// Project or content directory to load instead of the built-in demo scene.
    pub content_root: Option<PathBuf>,
    /// Authored zone definition ID.
    pub zone: Option<String>,
    /// Camera mode for this run.
    pub camera: CameraMode,
    /// Optional rebinding JSON file.
    pub bindings: Option<PathBuf>,
    /// Print a scene summary before running.
    pub describe: bool,
}

/// The offline usage line shown for unknown options.
#[must_use]
pub const fn offline_usage() -> &'static str {
    "Usage: lynx_client [--content <project-or-content-dir>] [--zone <zone-id>] \
     [--camera follow3d|billboard2d] [--bindings <bindings.json>] \
     [--replay <fixture.json>] [--describe]"
}

/// Parse only the offline flags. Online modes are handled by the windowed entry
/// point before this parser is called.
///
/// # Errors
///
/// Returns a message for a missing value, an unknown flag, or an invalid camera
/// mode.
pub fn parse_offline_args(
    args: impl IntoIterator<Item = String>,
) -> Result<OfflineOptions, String> {
    let mut args = args.into_iter();
    let mut options = OfflineOptions::default();
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--replay" => options.replay = Some(PathBuf::from(next_value(&mut args, "--replay")?)),
            "--content" => {
                options.content_root = Some(PathBuf::from(next_value(&mut args, "--content")?));
            }
            "--zone" => options.zone = Some(next_value(&mut args, "--zone")?),
            "--camera" => {
                let value = next_value(&mut args, "--camera")?;
                options.camera = CameraMode::parse(&value)?;
            }
            "--bindings" => {
                options.bindings = Some(PathBuf::from(next_value(&mut args, "--bindings")?));
            }
            "--describe" => options.describe = true,
            other => {
                return Err(format!(
                    "unknown offline option '{other}'\n{}",
                    offline_usage()
                ));
            }
        }
    }
    Ok(options)
}

fn next_value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{flag} requires a value\n{}", offline_usage()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<OfflineOptions, String> {
        parse_offline_args(args.iter().map(|value| (*value).to_owned()))
    }

    #[test]
    fn parses_scene_camera_and_input_options() {
        let options = parse(&[
            "--content",
            "../my-world",
            "--zone",
            "zone.market",
            "--camera",
            "billboard2d",
            "--bindings",
            "bindings.json",
            "--describe",
        ])
        .unwrap();
        assert_eq!(options.content_root, Some(PathBuf::from("../my-world")));
        assert_eq!(options.zone.as_deref(), Some("zone.market"));
        assert_eq!(options.camera, CameraMode::Billboard2d);
        assert_eq!(options.bindings, Some(PathBuf::from("bindings.json")));
        assert!(options.describe);
    }

    #[test]
    fn unknown_or_incomplete_options_are_clear_errors() {
        assert!(
            parse(&["--camera"])
                .unwrap_err()
                .contains("requires a value")
        );
        assert!(
            parse(&["--camera", "sideways"])
                .unwrap_err()
                .contains("unknown camera")
        );
        assert!(
            parse(&["--server", "now"])
                .unwrap_err()
                .contains("unknown offline")
        );
    }

    #[test]
    fn camera_aliases_are_accepted() {
        assert_eq!(
            parse(&["--camera", "3d"]).unwrap().camera,
            CameraMode::Follow3d
        );
        assert_eq!(
            parse(&["--camera", "2d"]).unwrap().camera,
            CameraMode::Billboard2d
        );
    }
}
