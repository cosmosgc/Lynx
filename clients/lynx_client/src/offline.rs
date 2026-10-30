//! Display-independent, local-only scene and control primitives.
//!
//! These local adapters consume a loaded presentation scene but never provide
//! prediction or server state.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// Actions are independent of physical controls and camera implementation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum Action {
    MoveForward,
    MoveBackward,
    MoveLeft,
    MoveRight,
    Interact,
    Chat,
    Inventory,
    Camera,
}

/// A platform-neutral control identifier; a platform adapter supplies pressed keys.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub enum Control {
    W,
    A,
    S,
    D,
    E,
    Enter,
    I,
    C,
}

/// Multiple controls may be bound to an action, and bindings may be replaced.
#[derive(Debug, Resource)]
pub struct InputBindings(pub HashMap<Control, Action>);

impl Default for InputBindings {
    fn default() -> Self {
        Self(HashMap::from([
            (Control::W, Action::MoveForward),
            (Control::S, Action::MoveBackward),
            (Control::A, Action::MoveLeft),
            (Control::D, Action::MoveRight),
            (Control::E, Action::Interact),
            (Control::Enter, Action::Chat),
            (Control::I, Action::Inventory),
            (Control::C, Action::Camera),
        ]))
    }
}

/// Optional binding-file shape. It is deliberately platform-neutral so the
/// same file can be reused by keyboard, gamepad, or replay adapters.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingFile {
    bindings: HashMap<Control, Action>,
}

impl InputBindings {
    /// Replace one control binding. Setting the same action on several controls
    /// is supported; the default resolver combines them.
    pub fn set_binding(&mut self, control: Control, action: Action) {
        self.0.insert(control, action);
    }

    /// Parse a rebinding-friendly JSON file: `{"bindings":{"W":"MoveForward"}}`.
    ///
    /// # Errors
    ///
    /// Returns a clear error for malformed JSON, unknown controls/actions, or
    /// an empty binding set.
    pub fn from_json(json: &str) -> Result<Self, InputBindingError> {
        let file: BindingFile =
            serde_json::from_str(json).map_err(|error| InputBindingError(error.to_string()))?;
        if file.bindings.is_empty() {
            return Err(InputBindingError(
                "binding file must declare at least one control".to_owned(),
            ));
        }
        Ok(Self(file.bindings))
    }
}

/// Why a binding file could not be used.
#[derive(Debug)]
pub struct InputBindingError(pub String);

impl std::fmt::Display for InputBindingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for InputBindingError {}

/// Controls supplied by an input adapter. Nothing reads a window directly.
#[derive(Debug, Default, Resource)]
pub struct PressedControls(pub HashSet<Control>);

/// Local presentation input gate, for example while a placeholder UI has focus.
/// Disabled input is discarded each update; adapters may supply it again on resume.
#[derive(Debug, Resource)]
pub struct OfflineInputEnabled(pub bool);

impl Default for OfflineInputEnabled {
    fn default() -> Self {
        Self(true)
    }
}

/// Resolved actions available to presentation or future gameplay adapters.
#[derive(Debug, Default, Resource)]
pub struct ActiveActions(pub HashSet<Action>);

/// Action edges for the current update, derived after bindings are resolved.
#[derive(Debug, Default, Resource)]
pub struct ActionTransitions {
    pub pressed: HashSet<Action>,
    pub released: HashSet<Action>,
}

/// Local avatar state layered over a loaded presentation zone, never an
/// authoritative world.
#[derive(Debug, Resource)]
pub struct OfflineScene {
    pub avatar_position: Vec3,
}

impl Default for OfflineScene {
    fn default() -> Self {
        Self {
            avatar_position: Vec3::ZERO,
        }
    }
}

/// Camera output is separate from the scene and can be supplied by another rig.
#[derive(Debug, Default, Resource)]
pub struct CameraPose {
    pub position: Vec3,
    pub target: Vec3,
    pub mode: CameraMode,
}

/// The slice's presentation camera. The 2D billboard path is the documented
/// fallback for renderers that cannot show the primary follow view.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraMode {
    /// Primary third-person follow pose.
    #[default]
    Follow3d,
    /// Flat, camera-facing fallback presentation.
    Billboard2d,
}

impl CameraMode {
    /// Short name used by CLI flags and window titles.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Follow3d => "follow3d",
            Self::Billboard2d => "billboard2d",
        }
    }

    /// Parse a CLI-friendly camera mode name.
    ///
    /// # Errors
    ///
    /// Returns the original value when it is not a supported mode.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "follow3d" | "follow" | "3d" => Ok(Self::Follow3d),
            "billboard2d" | "billboard" | "2d" => Ok(Self::Billboard2d),
            _ => Err(format!(
                "unknown camera mode '{value}' (expected follow3d or billboard2d)"
            )),
        }
    }
}

/// The selected camera mode, owned by the camera adapter rather than gameplay.
#[derive(Debug, Default, Resource)]
pub struct CameraController {
    pub mode: CameraMode,
}

/// Offset of the simple follow rig in world space.
#[derive(Debug, Resource)]
pub struct FollowCamera {
    pub offset: Vec3,
}

impl Default for FollowCamera {
    fn default() -> Self {
        Self {
            offset: Vec3::new(0.0, 4.0, 6.0),
        }
    }
}

/// Installs the existing offline core and its default placeholder adapters.
#[derive(Debug)]
pub struct OfflineFoundationPlugin;

/// Ordering points for platform input, action resolution, movement, camera and presentation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, SystemSet)]
pub enum OfflineSet {
    Input,
    InputFilter,
    Actions,
    Simulation,
    Camera,
    Presentation,
}

impl Plugin for OfflineFoundationPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((OfflineCorePlugin, DefaultActionPlugin, FollowCameraPlugin));
    }
}

/// Local placeholder state and movement; install adapters separately when needed.
#[derive(Debug)]
pub struct OfflineCorePlugin;

impl Plugin for OfflineCorePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<InputBindings>()
            .init_resource::<PressedControls>()
            .init_resource::<OfflineInputEnabled>()
            .init_resource::<ActiveActions>()
            .init_resource::<ActionTransitions>()
            .init_resource::<OfflineScene>()
            .init_resource::<crate::zone_scene::ZoneScene>()
            .init_resource::<CameraPose>()
            .configure_sets(
                Update,
                (
                    OfflineSet::Input,
                    OfflineSet::InputFilter,
                    OfflineSet::Actions,
                    OfflineSet::Simulation,
                    OfflineSet::Camera,
                    OfflineSet::Presentation,
                )
                    .chain(),
            )
            .add_systems(Update, suppress_input.in_set(OfflineSet::InputFilter))
            .add_systems(Update, move_placeholder.in_set(OfflineSet::Simulation));
    }
}

/// Resolves the existing control bindings into local placeholder actions.
#[derive(Debug)]
pub struct DefaultActionPlugin;

impl Plugin for DefaultActionPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, resolve_actions.in_set(OfflineSet::Actions));
    }
}

/// Supplies the existing local follow-camera pose.
#[derive(Debug)]
pub struct FollowCameraPlugin;

impl Plugin for FollowCameraPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FollowCamera>()
            .init_resource::<CameraController>()
            .add_systems(
                Update,
                (cycle_camera, follow_camera)
                    .chain()
                    .in_set(OfflineSet::Camera),
            );
    }
}

#[allow(clippy::needless_pass_by_value)] // Bevy system parameters.
fn suppress_input(enabled: Res<OfflineInputEnabled>, mut pressed: ResMut<PressedControls>) {
    if !enabled.0 {
        pressed.0.clear();
    }
}

#[allow(clippy::needless_pass_by_value)] // Bevy system parameters must be passed by value.
fn resolve_actions(
    bindings: Res<InputBindings>,
    pressed: Res<PressedControls>,
    mut actions: ResMut<ActiveActions>,
    mut transitions: ResMut<ActionTransitions>,
) {
    let next: HashSet<_> = pressed
        .0
        .iter()
        .filter_map(|key| bindings.0.get(key).copied())
        .collect();
    transitions.pressed = next.difference(&actions.0).copied().collect();
    transitions.released = actions.0.difference(&next).copied().collect();
    actions.0 = next;
}

#[allow(clippy::needless_pass_by_value)] // Bevy system parameter.
fn move_placeholder(
    actions: Res<ActiveActions>,
    zone: Res<crate::zone_scene::ZoneScene>,
    mut scene: ResMut<OfflineScene>,
) {
    let axis =
        |positive, negative| match (actions.0.contains(&positive), actions.0.contains(&negative)) {
            (true, false) => 1.0,
            (false, true) => -1.0,
            _ => 0.0,
        };
    let direction = Vec3::new(
        axis(Action::MoveRight, Action::MoveLeft),
        0.0,
        axis(Action::MoveBackward, Action::MoveForward),
    );
    // Fixed distance per update is deliberate for deterministic offline tests.
    scene.avatar_position = zone
        .bounds
        .clamp(scene.avatar_position + direction.normalize_or_zero());
}

#[allow(clippy::needless_pass_by_value)] // Bevy system parameter.
fn cycle_camera(transitions: Res<ActionTransitions>, mut controller: ResMut<CameraController>) {
    if transitions.pressed.contains(&Action::Camera) {
        controller.mode = match controller.mode {
            CameraMode::Follow3d => CameraMode::Billboard2d,
            CameraMode::Billboard2d => CameraMode::Follow3d,
        };
    }
}

#[allow(clippy::needless_pass_by_value)] // Bevy system parameters.
fn follow_camera(
    scene: Res<OfflineScene>,
    rig: Res<FollowCamera>,
    controller: Res<CameraController>,
    mut pose: ResMut<CameraPose>,
) {
    pose.target = scene.avatar_position;
    pose.mode = controller.mode;
    pose.position = pose.target
        + match controller.mode {
            CameraMode::Follow3d => rig.offset,
            // The fallback removes the 3D offset and looks straight down.
            CameraMode::Billboard2d => Vec3::new(0.0, 16.0, 0.0),
        };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headless_app;

    #[test]
    fn alternative_action_and_camera_adapters_run_in_order() {
        fn custom_actions(mut actions: ResMut<ActiveActions>) {
            actions.0 = HashSet::from([Action::MoveRight]);
        }
        #[allow(clippy::needless_pass_by_value)] // Bevy system parameters must be passed by value.
        fn custom_camera(scene: Res<OfflineScene>, mut pose: ResMut<CameraPose>) {
            pose.target = scene.avatar_position;
            pose.position = scene.avatar_position + Vec3::Y;
        }
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, OfflineCorePlugin))
            .add_systems(Update, custom_actions.in_set(OfflineSet::Actions))
            .add_systems(Update, custom_camera.in_set(OfflineSet::Camera));
        app.update();
        assert_eq!(
            app.world().resource::<OfflineScene>().avatar_position,
            Vec3::X
        );
        let pose = app.world().resource::<CameraPose>();
        assert_eq!(pose.target, Vec3::X);
        assert_eq!(pose.position, Vec3::X + Vec3::Y);
        assert!(!app.world().contains_resource::<FollowCamera>());
    }

    #[test]
    fn actions_move_placeholder_and_camera_follows() {
        let mut app = headless_app();
        app.world_mut()
            .resource_mut::<PressedControls>()
            .0
            .insert(Control::W);
        app.update();
        assert_eq!(
            app.world().resource::<OfflineScene>().avatar_position,
            Vec3::NEG_Z
        );
        let pose = app.world().resource::<CameraPose>();
        assert_eq!(pose.target, Vec3::NEG_Z);
        assert_eq!(pose.position, Vec3::NEG_Z + FollowCamera::default().offset);
        app.world_mut().resource_mut::<PressedControls>().0.clear();
        app.update();
        assert!(app.world().resource::<ActiveActions>().0.is_empty());
        assert_eq!(
            app.world().resource::<OfflineScene>().avatar_position,
            Vec3::NEG_Z
        );
    }

    #[test]
    fn bindings_can_be_changed_and_opposites_cancel() {
        let mut app = headless_app();
        app.world_mut()
            .resource_mut::<InputBindings>()
            .0
            .insert(Control::I, Action::MoveForward);
        app.world_mut()
            .resource_mut::<PressedControls>()
            .0
            .extend([Control::I, Control::S]);
        app.update();
        assert_eq!(
            app.world().resource::<OfflineScene>().avatar_position,
            Vec3::ZERO
        );
        let actions = &app.world().resource::<ActiveActions>().0;
        assert!(actions.contains(&Action::MoveForward));
        assert!(!actions.contains(&Action::Inventory));
    }

    #[test]
    fn diagonal_movement_is_normalized() {
        let mut app = headless_app();
        app.world_mut()
            .resource_mut::<PressedControls>()
            .0
            .extend([Control::W, Control::D]);
        app.update();
        let distance = app
            .world()
            .resource::<OfflineScene>()
            .avatar_position
            .length();
        assert!((distance - 1.0).abs() < 1e-6);
    }

    #[test]
    fn action_edges_follow_resolved_actions_not_individual_controls() {
        let mut app = headless_app();
        app.world_mut()
            .resource_mut::<InputBindings>()
            .0
            .insert(Control::I, Action::MoveForward);
        app.world_mut()
            .resource_mut::<PressedControls>()
            .0
            .insert(Control::W);
        app.update();
        let edges = app.world().resource::<ActionTransitions>();
        assert_eq!(edges.pressed, HashSet::from([Action::MoveForward]));
        assert!(edges.released.is_empty());

        app.world_mut()
            .resource_mut::<PressedControls>()
            .0
            .extend([Control::I]);
        app.update();
        let edges = app.world().resource::<ActionTransitions>();
        assert!(edges.pressed.is_empty());
        assert!(edges.released.is_empty());

        app.world_mut()
            .resource_mut::<PressedControls>()
            .0
            .remove(&Control::W);
        app.update();
        assert!(
            app.world()
                .resource::<ActionTransitions>()
                .released
                .is_empty()
        );

        app.world_mut().resource_mut::<PressedControls>().0.clear();
        app.update();
        assert_eq!(
            app.world().resource::<ActionTransitions>().released,
            HashSet::from([Action::MoveForward])
        );
        app.update();
        assert!(
            app.world()
                .resource::<ActionTransitions>()
                .released
                .is_empty()
        );
    }

    #[test]
    fn rebinding_held_control_produces_release_and_press() {
        let mut app = headless_app();
        app.world_mut()
            .resource_mut::<PressedControls>()
            .0
            .insert(Control::W);
        app.update();
        app.world_mut()
            .resource_mut::<InputBindings>()
            .0
            .insert(Control::W, Action::Chat);
        app.update();
        let edges = app.world().resource::<ActionTransitions>();
        assert_eq!(edges.released, HashSet::from([Action::MoveForward]));
        assert_eq!(edges.pressed, HashSet::from([Action::Chat]));
    }

    #[test]
    fn input_gate_releases_actions_and_discards_suppressed_input() {
        let mut app = headless_app();
        app.world_mut()
            .resource_mut::<PressedControls>()
            .0
            .insert(Control::W);
        app.update();
        app.world_mut().resource_mut::<OfflineInputEnabled>().0 = false;
        app.update();
        assert!(app.world().resource::<PressedControls>().0.is_empty());
        assert!(app.world().resource::<ActiveActions>().0.is_empty());
        assert_eq!(
            app.world().resource::<ActionTransitions>().released,
            HashSet::from([Action::MoveForward])
        );
        app.world_mut()
            .resource_mut::<PressedControls>()
            .0
            .insert(Control::W);
        app.update();
        assert!(
            app.world()
                .resource::<ActionTransitions>()
                .released
                .is_empty()
        );
        assert_eq!(
            app.world().resource::<OfflineScene>().avatar_position,
            Vec3::NEG_Z
        );
        app.world_mut().resource_mut::<OfflineInputEnabled>().0 = true;
        app.update();
        assert!(app.world().resource::<ActiveActions>().0.is_empty());
        app.world_mut()
            .resource_mut::<PressedControls>()
            .0
            .insert(Control::W);
        app.update();
        assert_eq!(
            app.world().resource::<ActionTransitions>().pressed,
            HashSet::from([Action::MoveForward])
        );
    }

    #[test]
    fn binding_files_replace_the_default_map() {
        let bindings = InputBindings::from_json(
            r#"{"bindings":{"W":"MoveBackward","I":"MoveForward","C":"Camera"}}"#,
        )
        .unwrap();
        assert_eq!(bindings.0.get(&Control::W), Some(&Action::MoveBackward));
        assert_eq!(bindings.0.get(&Control::I), Some(&Action::MoveForward));
        assert!(InputBindings::from_json(r#"{"bindings":{}}"#).is_err());
        assert!(InputBindings::from_json(r#"{"bindings":{"Q":"MoveForward"}}"#).is_err());
        assert!(InputBindings::from_json(r#"{"bindings":{"W":"Fly"}}"#).is_err());
    }

    #[test]
    fn camera_action_cycles_modes_and_changes_only_the_camera_pose() {
        let mut app = headless_app();
        app.world_mut()
            .resource_mut::<PressedControls>()
            .0
            .insert(Control::C);
        app.update();
        assert_eq!(
            app.world().resource::<CameraController>().mode,
            CameraMode::Billboard2d
        );
        assert_eq!(
            app.world().resource::<CameraPose>().mode,
            CameraMode::Billboard2d
        );
        assert_eq!(
            app.world().resource::<CameraPose>().position,
            Vec3::new(0.0, 16.0, 0.0)
        );
        assert_eq!(
            app.world().resource::<OfflineScene>().avatar_position,
            Vec3::ZERO
        );
        app.world_mut().resource_mut::<PressedControls>().0.clear();
        app.update();
        app.world_mut()
            .resource_mut::<PressedControls>()
            .0
            .insert(Control::C);
        app.update();
        assert_eq!(
            app.world().resource::<CameraController>().mode,
            CameraMode::Follow3d
        );
        assert_eq!(
            app.world().resource::<CameraPose>().position,
            FollowCamera::default().offset
        );
    }

    #[test]
    fn placeholder_movement_is_clamped_to_the_loaded_zone() {
        let mut app = headless_app();
        {
            let mut zone = app
                .world_mut()
                .resource_mut::<crate::zone_scene::ZoneScene>();
            zone.bounds.min = Vec3::new(-0.5, 0.0, -0.5);
            zone.bounds.max = Vec3::new(0.5, 8.0, 0.5);
        }
        app.world_mut()
            .resource_mut::<PressedControls>()
            .0
            .insert(Control::W);
        for _ in 0..10 {
            app.update();
        }
        assert_eq!(
            app.world().resource::<OfflineScene>().avatar_position,
            Vec3::new(0.0, 0.0, -0.5)
        );
    }
}
