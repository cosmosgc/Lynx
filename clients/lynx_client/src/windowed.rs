//! Window-only keyboard and 2D presentation adapters for the offline foundation.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::offline::{CameraPose, Control, OfflineScene, OfflineSet, PressedControls};
use crate::online::{OnlineClient, OnlineStatus};
use crate::replay::ReplayInput;
use crate::zone_scene::{SceneObjectKind, ZoneScene};
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
    let client = client.into_inner();
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
    if let Some(position) = state.reconciliation().presentation_position() {
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

/// Marks a sprite whose transform is derived from an authored scene position.
#[derive(Component)]
struct WorldPosition(Vec3);

/// Sprite colour and size for a scene object category.
fn object_style(kind: SceneObjectKind) -> (Color, f32, f32) {
    match kind {
        SceneObjectKind::Prop => (Color::srgb(0.58, 0.40, 0.22), 0.9, 0.6),
        SceneObjectKind::Character => (Color::srgb(0.95, 0.78, 0.30), 0.8, 0.8),
        SceneObjectKind::Interactable => (Color::srgb(0.90, 0.62, 0.18), 0.7, 0.6),
    }
}

#[allow(clippy::needless_pass_by_value)] // Bevy system parameters must be passed by value.
fn setup_scene(mut commands: Commands, zone: Res<ZoneScene>, mut scene: ResMut<OfflineScene>) {
    commands.spawn((Camera2d, SceneCamera));
    scene.avatar_position = zone.bounds.clamp(zone.spawn);

    // Terrain and authored objects are placeholder sprites for the current 2D
    // renderer. Assets verified by the content policy are not decoded yet.
    for (index, tile) in zone.terrain.iter().enumerate() {
        let colour = if index % 2 == 0 {
            Color::srgb(0.13, 0.22, 0.16)
        } else {
            Color::srgb(0.15, 0.25, 0.18)
        };
        commands.spawn((
            Sprite::from_color(colour, Vec2::splat(tile.size * SCALE)),
            Transform::from_translation(Vec3::new(0.0, 0.0, -1.0)),
            WorldPosition(tile.position),
        ));
    }
    for object in &zone.objects {
        let (colour, size, z) = object_style(object.kind);
        commands.spawn((
            Sprite::from_color(colour, Vec2::splat(size * SCALE)),
            Transform::from_translation(Vec3::new(0.0, 0.0, z)),
            WorldPosition(object.position),
        ));
    }
    commands.spawn((
        Sprite::from_color(Color::srgb(0.3, 0.7, 1.0), Vec2::splat(24.0)),
        Transform::from_xyz(0.0, 0.0, 1.0),
        Avatar,
    ));
}

#[allow(clippy::needless_pass_by_value)] // Bevy system parameters must be passed by value.
fn keyboard_input(
    keys: Res<ButtonInput<KeyCode>>,
    replay: Option<Res<ReplayInput>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut pressed: ResMut<PressedControls>,
) {
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
    if replay.is_some() {
        return;
    }
    // Keyboard state is global; do not move the offline avatar while another
    // window has focus. Replay is deliberately independent of window focus.
    if !windows.single().is_ok_and(|window| window.focused) {
        pressed.0.clear();
        return;
    }
    let keys = keys.into_inner();
    pressed.0 = KEYS
        .iter()
        .filter_map(|(key, control)| keys.pressed(*key).then_some(*control))
        .collect();
}

/// Project a world position into camera-relative 2D pixels. The primary follow
/// mode uses the slice's closer scale; the billboard fallback is slightly wider.
fn project(world: Vec3, pose: &CameraPose) -> Vec3 {
    let scale = match pose.mode {
        crate::offline::CameraMode::Follow3d => SCALE,
        crate::offline::CameraMode::Billboard2d => SCALE * 0.75,
    };
    Vec3::new(
        (world.x - pose.target.x) * scale,
        -(world.z - pose.target.z) * scale,
        0.0,
    )
}

#[allow(clippy::needless_pass_by_value, clippy::type_complexity)] // Bevy system parameters must be passed by value.
fn present_scene(
    zone: Res<ZoneScene>,
    scene: Res<OfflineScene>,
    pose: Res<CameraPose>,
    mut avatars: Query<&mut Transform, (With<Avatar>, Without<SceneCamera>)>,
    mut objects: Query<(&WorldPosition, &mut Transform), (Without<Avatar>, Without<SceneCamera>)>,
    mut cameras: Query<
        &mut Transform,
        (With<SceneCamera>, Without<Avatar>, Without<WorldPosition>),
    >,
    mut windows: Query<&mut Window>,
) {
    let scene = scene.into_inner();
    let pose = pose.into_inner();
    for mut transform in &mut avatars {
        let projected = project(scene.avatar_position, pose);
        transform.translation.x = projected.x;
        transform.translation.y = projected.y;
    }
    for (world, mut transform) in &mut objects {
        let projected = project(world.0, pose);
        transform.translation.x = projected.x;
        transform.translation.y = projected.y;
    }
    for mut transform in &mut cameras {
        // Objects are already camera-relative, so the camera stays at the origin.
        transform.translation.x = 0.0;
        transform.translation.y = 0.0;
    }
    if let Some(mut window) = windows.iter_mut().next() {
        let version = zone
            .content_version
            .as_deref()
            .map_or("source".to_owned(), |version| format!("v{version}"));
        let suffix = if zone.issue_count() == 0 {
            String::new()
        } else {
            format!(" - {} warning(s)", zone.issue_count())
        };
        window.title = format!(
            "Lynx offline - {} [{}] {} - camera {}{suffix}",
            zone.name,
            zone.zone_id,
            version,
            pose.mode.as_str()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        headless_app,
        offline::{Action, ActiveActions, OfflineInputEnabled},
    };

    #[test]
    fn keyboard_drives_scene_and_camera_in_same_update_and_releases() {
        let mut app = headless_app();
        app.init_resource::<ButtonInput<KeyCode>>()
            .add_plugins(OfflineWindowPlugin);
        app.world_mut().spawn((Window::default(), PrimaryWindow));
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
        assert_eq!(avatar.translation.y, 0.0);
        let camera = world
            .query_filtered::<&Transform, With<SceneCamera>>()
            .single(world)
            .unwrap();
        assert_eq!(camera.translation.y, 0.0);
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

    #[test]
    fn held_keyboard_input_resumes_after_gate_reopens() {
        let mut app = headless_app();
        app.init_resource::<ButtonInput<KeyCode>>()
            .add_plugins(OfflineWindowPlugin);
        app.world_mut().spawn((Window::default(), PrimaryWindow));
        app.world_mut().resource_mut::<OfflineInputEnabled>().0 = false;
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyW);
        app.update();
        assert_eq!(
            app.world().resource::<OfflineScene>().avatar_position,
            Vec3::ZERO
        );
        app.world_mut().resource_mut::<OfflineInputEnabled>().0 = true;
        app.update();
        assert_eq!(
            app.world().resource::<OfflineScene>().avatar_position,
            Vec3::NEG_Z
        );
    }

    #[test]
    fn losing_focus_releases_keyboard_actions_before_movement() {
        use crate::offline::ActionTransitions;

        let mut app = headless_app();
        app.init_resource::<ButtonInput<KeyCode>>()
            .add_plugins(OfflineWindowPlugin);
        let window = app
            .world_mut()
            .spawn((Window::default(), PrimaryWindow))
            .id();
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyW);
        app.update();
        app.world_mut().get_mut::<Window>(window).unwrap().focused = false;
        app.update();
        assert!(app.world().resource::<ActiveActions>().0.is_empty());
        assert!(
            app.world()
                .resource::<ActionTransitions>()
                .released
                .contains(&Action::MoveForward)
        );
        assert_eq!(
            app.world().resource::<OfflineScene>().avatar_position,
            Vec3::NEG_Z
        );
        app.update();
        assert!(
            app.world()
                .resource::<ActionTransitions>()
                .released
                .is_empty()
        );
        app.world_mut().get_mut::<Window>(window).unwrap().focused = true;
        app.update();
        assert_eq!(
            app.world().resource::<OfflineScene>().avatar_position,
            Vec3::NEG_Z * 2.0
        );
    }

    #[test]
    fn replay_is_not_suppressed_by_missing_or_unfocused_window() {
        use crate::replay::ReplayFixture;

        let mut app = headless_app();
        app.init_resource::<ButtonInput<KeyCode>>()
            .add_plugins(OfflineWindowPlugin)
            .insert_resource(ReplayInput::new(
                ReplayFixture::from_json(r#"{"frames":[{"pressed":["W"]},{"pressed":["D"]}]}"#)
                    .unwrap(),
            ));
        app.update();
        app.world_mut().spawn((
            Window {
                focused: false,
                ..default()
            },
            PrimaryWindow,
        ));
        app.update();
        assert_eq!(
            app.world().resource::<OfflineScene>().avatar_position,
            Vec3::new(1.0, 0.0, -1.0)
        );
    }

    #[test]
    fn startup_spawns_terrain_props_characters_and_interactables() {
        let mut app = headless_app();
        app.init_resource::<ButtonInput<KeyCode>>()
            .add_plugins(OfflineWindowPlugin);
        app.update();
        let world = app.world_mut();
        let terrain = world
            .query_filtered::<&WorldPosition, Without<Avatar>>()
            .iter(world)
            .count();
        assert!(terrain > 6, "terrain and objects should be spawned");
        let avatar = world
            .query_filtered::<&Transform, With<Avatar>>()
            .single(world)
            .unwrap();
        assert_eq!(avatar.translation, Vec3::new(0.0, 0.0, 1.0));
    }

    #[test]
    fn billboard_mode_uses_the_documented_fallback_scale() {
        let follow = CameraPose {
            target: Vec3::ZERO,
            ..default()
        };
        let billboard = CameraPose {
            mode: crate::offline::CameraMode::Billboard2d,
            target: Vec3::ZERO,
            ..default()
        };
        assert_eq!(project(Vec3::X, &follow), Vec3::new(SCALE, 0.0, 0.0));
        assert_eq!(
            project(Vec3::X, &billboard),
            Vec3::new(SCALE * 0.75, 0.0, 0.0)
        );
    }
}
