//! Window-only keyboard and 2D presentation adapters for the offline foundation.

use bevy::prelude::*;

use crate::offline::{CameraPose, Control, OfflineScene, OfflineSet, PressedControls};
use crate::online::{OnlineClient, OnlineStatus};
use engine_core::id::StableId;
use std::collections::{BTreeMap, BTreeSet};

const SCALE: f32 = 32.0;

/// Read-only presentation of the current server-owned public interest set.
#[derive(Debug)]
pub struct OnlineWindowPlugin;

impl Plugin for OnlineWindowPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_online)
            .add_systems(Update, present_online);
    }
}

#[derive(Component)]
struct OnlineEntity(StableId);

fn setup_online(mut commands: Commands) {
    commands.spawn((Camera2d, SceneCamera));
}

// Clamp before scaling so even a finite but extreme server coordinate cannot
// produce an infinite Bevy transform.
fn screen_position(x: f32, z: f32) -> Vec3 {
    Vec3::new(
        x.clamp(-1_000_000.0, 1_000_000.0) * SCALE,
        -z.clamp(-1_000_000.0, 1_000_000.0) * SCALE,
        1.0,
    )
}

fn present_online(
    mut commands: Commands,
    client: Res<OnlineClient>,
    mut entities: Query<(Entity, &OnlineEntity, &mut Transform), Without<SceneCamera>>,
    mut cameras: Query<&mut Transform, (With<SceneCamera>, Without<OnlineEntity>)>,
    mut windows: Query<&mut Window>,
) {
    let state = client.world();
    let public: BTreeMap<_, _> = state
        .entities()
        .map(|entity| (entity.entity, entity))
        .collect();
    let mut missing: BTreeSet<_> = public.keys().copied().collect();
    for (entity, id, mut transform) in &mut entities {
        if let Some(public) = public.get(&id.0) {
            transform.translation = screen_position(public.position.x, public.position.z);
            missing.remove(&id.0);
        } else {
            commands.entity(entity).despawn();
        }
    }
    for public in missing.iter().filter_map(|id| public.get(id)) {
        commands.spawn((
            Sprite::from_color(Color::srgb(0.3, 0.7, 1.0), Vec2::splat(24.0)),
            Transform::from_translation(screen_position(public.position.x, public.position.z)),
            OnlineEntity(public.entity),
        ));
    }
    if let Some(position) = state.reconciliation().authoritative_position() {
        for mut camera in &mut cameras {
            let position = screen_position(position.x, position.z);
            camera.translation.x = position.x;
            camera.translation.y = position.y;
        }
    }
    let status = match client.status() {
        OnlineStatus::Disconnected => "disconnected",
        OnlineStatus::Connecting => "connecting",
        OnlineStatus::AwaitingSnapshot => "awaiting snapshot",
        OnlineStatus::Active => "active (read-only)",
        OnlineStatus::Failed(_) => "connection failed",
    };
    for mut window in &mut windows {
        window.title = format!("Lynx demo online - {status}");
    }
}

#[derive(Debug)]
pub struct OfflineWindowPlugin;

impl Plugin for OfflineWindowPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_scene)
            .add_systems(Update, keyboard_input.in_set(OfflineSet::Input))
            .add_systems(Update, present_scene.in_set(OfflineSet::Presentation));
    }
}

#[derive(Component)]
struct Avatar;

#[derive(Component)]
struct SceneCamera;

fn setup_scene(mut commands: Commands) {
    commands.spawn((Camera2d, SceneCamera));
    // A finite ground and landmarks make movement visible without external assets.
    commands.spawn((
        Sprite::from_color(Color::srgb(0.15, 0.25, 0.18), Vec2::splat(2048.0)),
        Transform::from_xyz(0.0, 0.0, -1.0),
    ));
    for x in -4..=4 {
        for y in -4..=4 {
            commands.spawn((
                Sprite::from_color(Color::srgb(0.3, 0.45, 0.3), Vec2::splat(8.0)),
                Transform::from_xyz(x as f32 * 128.0, y as f32 * 128.0, 0.0),
            ));
        }
    }
    commands.spawn((
        Sprite::from_color(Color::srgb(0.3, 0.7, 1.0), Vec2::splat(24.0)),
        Transform::from_xyz(0.0, 0.0, 1.0),
        Avatar,
    ));
}

fn keyboard_input(keys: Res<ButtonInput<KeyCode>>, mut pressed: ResMut<PressedControls>) {
    const KEYS: [(KeyCode, Control); 8] = [
        (KeyCode::KeyW, Control::W),
        (KeyCode::KeyA, Control::A),
        (KeyCode::KeyS, Control::S),
        (KeyCode::KeyD, Control::D),
        (KeyCode::KeyE, Control::E),
        (KeyCode::Enter, Control::Enter),
        (KeyCode::KeyI, Control::I),
        (KeyCode::KeyC, Control::C),
    ];
    pressed.0 = KEYS
        .iter()
        .filter_map(|(key, control)| keys.pressed(*key).then_some(*control))
        .collect();
}

fn present_scene(
    scene: Res<OfflineScene>,
    pose: Res<CameraPose>,
    mut avatars: Query<&mut Transform, (With<Avatar>, Without<SceneCamera>)>,
    mut cameras: Query<&mut Transform, (With<SceneCamera>, Without<Avatar>)>,
) {
    for mut transform in &mut avatars {
        transform.translation.x = scene.avatar_position.x * SCALE;
        transform.translation.y = -scene.avatar_position.z * SCALE;
    }
    // The top-down view follows the rig's target; its 3D offset is not used in 2D.
    for mut transform in &mut cameras {
        transform.translation.x = pose.target.x * SCALE;
        transform.translation.y = -pose.target.z * SCALE;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        headless_app,
        offline::{Action, ActiveActions},
    };

    #[test]
    fn keyboard_drives_scene_and_camera_in_same_update_and_releases() {
        let mut app = headless_app();
        app.init_resource::<ButtonInput<KeyCode>>()
            .add_plugins(OfflineWindowPlugin);
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyW);
        app.update();
        assert!(
            app.world()
                .resource::<ActiveActions>()
                .0
                .contains(&Action::MoveForward)
        );
        assert_eq!(
            app.world().resource::<OfflineScene>().avatar_position,
            Vec3::NEG_Z
        );
        let world = app.world_mut();
        let avatar = world
            .query_filtered::<&Transform, With<Avatar>>()
            .single(world)
            .unwrap();
        assert_eq!(avatar.translation.y, SCALE);
        let camera = world
            .query_filtered::<&Transform, With<SceneCamera>>()
            .single(world)
            .unwrap();
        assert_eq!(camera.translation.y, SCALE);
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .release(KeyCode::KeyW);
        app.update();
        assert!(app.world().resource::<ActiveActions>().0.is_empty());
        assert_eq!(
            app.world().resource::<OfflineScene>().avatar_position,
            Vec3::NEG_Z
        );
    }
}
