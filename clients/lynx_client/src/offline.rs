//! Display-independent, local-only scene and control primitives.
//!
//! These placeholders are not content loading, prediction, or server state.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;

/// Actions are independent of physical controls and camera implementation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
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
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
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

/// Controls supplied by an input adapter. Nothing reads a window directly.
#[derive(Debug, Default, Resource)]
pub struct PressedControls(pub HashSet<Control>);

/// Resolved actions available to presentation or future gameplay adapters.
#[derive(Debug, Default, Resource)]
pub struct ActiveActions(pub HashSet<Action>);

/// A small in-memory scene, not a loaded zone or authoritative world.
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

/// Installs a deterministic placeholder scene and follow camera.
/// Replacing this plugin allows another input or camera implementation.
#[derive(Debug)]
pub struct OfflineFoundationPlugin;

/// Ordering points for platform input and presentation adapters.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, SystemSet)]
pub enum OfflineSet {
    Input,
    Simulation,
    Presentation,
}

impl Plugin for OfflineFoundationPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<InputBindings>()
            .init_resource::<PressedControls>()
            .init_resource::<ActiveActions>()
            .init_resource::<OfflineScene>()
            .init_resource::<CameraPose>()
            .init_resource::<FollowCamera>()
            .configure_sets(
                Update,
                (
                    OfflineSet::Input,
                    OfflineSet::Simulation,
                    OfflineSet::Presentation,
                )
                    .chain(),
            )
            .add_systems(
                Update,
                (resolve_actions, move_placeholder, follow_camera)
                    .chain()
                    .in_set(OfflineSet::Simulation),
            );
    }
}

#[allow(clippy::needless_pass_by_value)] // Bevy system parameters must be passed by value.
fn resolve_actions(
    bindings: Res<InputBindings>,
    pressed: Res<PressedControls>,
    mut actions: ResMut<ActiveActions>,
) {
    actions.0 = pressed
        .0
        .iter()
        .filter_map(|key| bindings.0.get(key).copied())
        .collect();
}

#[allow(clippy::needless_pass_by_value)] // Bevy system parameter.
fn move_placeholder(actions: Res<ActiveActions>, mut scene: ResMut<OfflineScene>) {
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
    scene.avatar_position += direction.normalize_or_zero();
}

#[allow(clippy::needless_pass_by_value)] // Bevy system parameters.
fn follow_camera(scene: Res<OfflineScene>, rig: Res<FollowCamera>, mut pose: ResMut<CameraPose>) {
    pose.target = scene.avatar_position;
    pose.position = pose.target + rig.offset;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headless_app;

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
}
