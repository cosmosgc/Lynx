//! Camera-facing entity information: health bars and name labels above
//! replicated entities.
//!
//! The native backend here uses ordinary meshes for the health bar and
//! screen-space `bevy_ui` text for the name. Bevy 0.20 only builds a
//! `Transparent2d` render phase for [`Camera2d`], and this client has a single
//! [`Camera3d`], so `Text2d` and other sprites never reach the screen. The
//! label is therefore projected with [`Camera::world_to_viewport`], which also
//! keeps names readable at any distance and always facing the camera.
//!
//! Two external crates are under evaluation for later adoption, and each has
//! exactly one swap point here:
//!
//! - `bevy_health_bar3d` replaces [`spawn_health_bar`]. It owns bar meshes and
//!   sizing, so a future backend can ignore [`EntityInfoAssets`] and set the
//!   bar from the replicated health in [`present_health`].
//! - `bevy_slugtext` replaces [`spawn_label`]. It owns label layout, so a
//!   future backend can drop the projected `bevy_ui` text spawn entirely.
//!
//! To add a backend:
//!
//! 1. Add the crate to the client's `window` feature in `Cargo.toml`.
//! 2. Add a variant to [`EntityInfoBackend`]. [`spawn_entity_info`] already
//!    receives the selected resource, so `present_online` needs no change.
//! 3. Implement the variant's branch of [`spawn_health_bar`] and
//!    [`spawn_label`], and update [`present_health`] if the crate owns sizing.
//! 4. Remove the native branch once the external backend is validated.
//!
//! `bevy_health_bar3d` 3.11 requires Bevy `^0.19` and `bevy_slugtext` 0.1.1
//! requires Bevy 0.19, so neither can replace the native path while the
//! workspace pins `=0.20.0`. This seam keeps that replacement localized.

use bevy::picking::Pickable;
use bevy::prelude::*;
use bevy::text::FontSize;
use bevy::window::PrimaryWindow;

use crate::online::OnlineClient;
use crate::windowed::SceneCamera;
use engine_core::id::StableId;

/// Height above an entity's origin where the billboard bar sits.
const BAR_HEIGHT: f32 = 1.35;
/// Vertical gap between the bar and the bottom of the name label.
const LABEL_GAP: f32 = 0.45;
/// Name-label font size in logical pixels.
const LABEL_FONT_SIZE: f32 = 14.0;

/// Selects the implementation used to draw in-world entity information.
///
/// Only [`EntityInfoBackend::Native`] exists while the compatible external
/// crates are unavailable. The resource is read once per spawn so a new
/// backend can be selected without touching the entity synchronization code.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntityInfoBackend {
    /// Billboarded bar meshes plus projected `bevy_ui` name labels.
    #[default]
    Native,
}

/// Cached meshes and materials shared by every native health bar.
#[derive(Resource, Debug)]
pub(crate) struct EntityInfoAssets {
    background_mesh: Handle<Mesh>,
    fill_mesh: Handle<Mesh>,
    background_material: Handle<StandardMaterial>,
    fill_material: Handle<StandardMaterial>,
}

impl FromWorld for EntityInfoAssets {
    fn from_world(world: &mut World) -> Self {
        let (background_mesh, fill_mesh) = {
            let mut meshes = world.resource_mut::<Assets<Mesh>>();
            (
                meshes.add(Cuboid::new(0.9, 0.12, 0.04)),
                meshes.add(Cuboid::new(0.8, 0.07, 0.04)),
            )
        };
        let background_material = world
            .resource_mut::<Assets<StandardMaterial>>()
            .add(Color::srgb(0.08, 0.08, 0.1));
        let fill_material = world
            .resource_mut::<Assets<StandardMaterial>>()
            .add(Color::srgb(0.2, 0.85, 0.35));
        Self {
            background_mesh,
            fill_mesh,
            background_material,
            fill_material,
        }
    }
}

/// Marker for the camera-facing root that owns an entity's bar.
#[derive(Component)]
struct Billboard;

/// Links a bar piece to the replicated entity whose health it presents.
#[derive(Component)]
struct HealthBar {
    entity: StableId,
    fill: bool,
}

/// Links a screen-space name label to the replicated entity it follows.
#[derive(Component)]
struct EntityInfoLabel {
    entity: StableId,
}

/// Systems owned by this module, ordered after the online entity synchronizer.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct EntityInfoSet;

#[derive(Debug)]
pub(crate) struct EntityInfoPlugin;

impl Plugin for EntityInfoPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<StandardMaterial>>()
            .init_resource::<EntityInfoAssets>()
            .init_resource::<EntityInfoBackend>()
            .add_systems(
                Update,
                (present_health, billboard_info, present_labels).in_set(EntityInfoSet),
            );
    }
}

/// Spawn the health bar and name label for a replicated entity body.
///
/// The bar lives in one billboard root under the body, while the name label is
/// a top-level UI node that follows the body's screen projection. Keeping the
/// seam at this level means a future backend can spawn entirely different
/// components without changing `present_online`.
pub(crate) fn spawn_entity_info(
    parent: &mut ChildSpawnerCommands,
    entity: StableId,
    label: &str,
    assets: &EntityInfoAssets,
    backend: EntityInfoBackend,
) {
    match backend {
        EntityInfoBackend::Native => {
            parent
                .spawn((
                    Billboard,
                    Transform::from_xyz(0.0, BAR_HEIGHT, 0.0),
                    Visibility::Inherited,
                ))
                .with_children(|info| spawn_health_bar(info, entity, assets));
            spawn_label(parent, entity, label);
        }
    }
}

/// Native health bar swap point: replace with `bevy_health_bar3d` when it
/// supports the workspace's Bevy version.
fn spawn_health_bar(
    parent: &mut ChildSpawnerCommands,
    entity: StableId,
    assets: &EntityInfoAssets,
) {
    parent.spawn((
        HealthBar {
            entity,
            fill: false,
        },
        Mesh3d(assets.background_mesh.clone()),
        MeshMaterial3d(assets.background_material.clone()),
        Visibility::Hidden,
    ));
    parent.spawn((
        HealthBar { entity, fill: true },
        Mesh3d(assets.fill_mesh.clone()),
        MeshMaterial3d(assets.fill_material.clone()),
        // Keep the fill in front of the background while the root billboards.
        Transform::from_xyz(0.0, 0.0, 0.08),
        Visibility::Hidden,
    ));
}

/// Native name-label swap point: replace with `bevy_slugtext` when it supports
/// the workspace's Bevy version.
///
/// The label is a UI root rather than a child of the billboard because Bevy
/// 0.20 does not render sprites or `Text2d` in [`Camera3d`] views.
fn spawn_label(parent: &mut ChildSpawnerCommands, entity: StableId, label: &str) {
    parent.commands().spawn((
        EntityInfoLabel { entity },
        Text::new(label),
        TextFont {
            font_size: FontSize::Px(LABEL_FONT_SIZE),
            ..default()
        },
        TextColor(Color::srgb(0.95, 0.95, 0.9)),
        TextShadow {
            offset: Vec2::splat(1.0),
            color: Color::srgba(0.0, 0.0, 0.0, 0.85),
        },
        Node {
            position_type: PositionType::Absolute,
            ..default()
        },
        // Keep nameplates behind HUD panels, however late they spawn.
        GlobalZIndex(-1),
        // Nameplates should never swallow clicks meant for the scene.
        Pickable::IGNORE,
    ));
}

#[allow(clippy::cast_precision_loss)] // Bar scale only; authoritative integers stay intact.
fn health_fraction(health: Option<crate::world_state::PublicHealth>) -> Option<f32> {
    health
        .filter(|health| health.max > 0)
        .map(|health| (health.current as f32 / health.max as f32).clamp(0.0, 1.0))
}

#[allow(clippy::needless_pass_by_value)]
fn present_health(
    client: Res<OnlineClient>,
    mut bars: Query<(&HealthBar, &mut Transform, &mut Visibility)>,
) {
    let health: std::collections::BTreeMap<_, _> = client
        .world()
        .entities()
        .map(|entity| (entity.entity, health_fraction(entity.health)))
        .collect();
    for (bar, mut transform, mut visibility) in &mut bars {
        let fraction = health.get(&bar.entity).copied().flatten();
        *visibility = if fraction.is_some() {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if bar.fill {
            let fraction = fraction.unwrap_or(0.0);
            transform.scale.x = fraction;
            transform.translation.x = (fraction - 1.0) * 0.4;
        }
    }
}

/// Rotate each info root so its local +Z axis points back at the camera.
#[allow(clippy::type_complexity)] // Disjoint filters keep the camera read separate.
fn billboard_info(
    camera: Query<&Transform, With<SceneCamera>>,
    parents: Query<&GlobalTransform>,
    mut boards: Query<(&mut Transform, &ChildOf), (With<Billboard>, Without<SceneCamera>)>,
) {
    let Ok(camera) = camera.single() else {
        return;
    };
    for (mut transform, child_of) in &mut boards {
        let Ok(parent) = parents.get(child_of.parent()) else {
            continue;
        };
        transform.rotation = billboard_rotation(parent.rotation(), camera.rotation);
    }
}

/// Project every name label above its entity and drop labels for despawned
/// entities.
#[allow(clippy::type_complexity)] // One query keeps label layout state together.
fn present_labels(
    mut commands: Commands,
    camera: Query<(&Camera, &Transform), With<SceneCamera>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    entities: Query<(&crate::windowed::OnlineEntity, &Transform)>,
    mut labels: Query<(
        Entity,
        &EntityInfoLabel,
        &ComputedNode,
        &mut Node,
        &mut Visibility,
    )>,
) {
    let (Ok((camera, camera_transform)), Ok(window)) = (camera.single(), windows.single()) else {
        for (_, _, _, _, mut visibility) in &mut labels {
            *visibility = Visibility::Hidden;
        }
        return;
    };
    // The scene camera is unparented; use its Update transform rather than
    // waiting for GlobalTransform propagation in PostUpdate.
    let camera_transform = GlobalTransform::from(*camera_transform);
    let viewport = window.size();
    let positions: std::collections::BTreeMap<_, _> = entities
        .iter()
        .map(|(entity, transform)| {
            (
                entity.id(),
                transform.translation + Vec3::Y * (BAR_HEIGHT + LABEL_GAP),
            )
        })
        .collect();
    for (label_entity, label, computed, mut node, mut visibility) in &mut labels {
        let Some(world) = positions.get(&label.entity) else {
            commands.entity(label_entity).despawn();
            continue;
        };
        // Hide labels for entities outside the window instead of pinning them
        // to the screen edge.
        let Ok(point) = camera.world_to_viewport(&camera_transform, *world) else {
            *visibility = Visibility::Hidden;
            continue;
        };
        if point.x < 0.0 || point.y < 0.0 || point.x > viewport.x || point.y > viewport.y {
            *visibility = Visibility::Hidden;
            continue;
        }
        // UI layout uses logical pixels, matching `world_to_viewport`.
        let size = computed.size() * computed.inverse_scale_factor();
        node.left = Val::Px(point.x - size.x * 0.5);
        node.top = Val::Px(point.y - size.y);
        *visibility = Visibility::Inherited;
    }
}

/// Local rotation that makes a child of `parent` match the camera's world
/// orientation.
fn billboard_rotation(parent: Quat, camera: Quat) -> Quat {
    parent.inverse() * camera
}

#[cfg_attr(coverage_nightly, coverage(off))]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::windowed::prototype_name;

    #[test]
    fn public_health_is_optional_and_bar_fraction_is_bounded() {
        use crate::world_state::PublicHealth;
        assert_eq!(health_fraction(None), None);
        assert_eq!(
            health_fraction(Some(PublicHealth { current: 5, max: 0 })),
            None
        );
        assert_eq!(
            health_fraction(Some(PublicHealth {
                current: 5,
                max: 10
            })),
            Some(0.5)
        );
        assert_eq!(
            health_fraction(Some(PublicHealth {
                current: -1,
                max: 10
            })),
            Some(0.0)
        );
        assert_eq!(
            health_fraction(Some(PublicHealth {
                current: 20,
                max: 10
            })),
            Some(1.0)
        );
    }

    #[test]
    fn billboard_rotation_matches_the_camera_despite_parent_rotation() {
        let camera = Quat::from_euler(EulerRot::YXZ, 0.7, -0.4, 0.0);
        let parent = Quat::from_rotation_y(1.2);
        let world = parent * billboard_rotation(parent, camera);
        assert!(world.abs_diff_eq(camera, 1e-5));
    }

    #[test]
    fn prototype_nameplate_fallback_is_readable() {
        use std::str::FromStr;
        assert_eq!(
            prototype_name(
                &engine_core::id::DefinitionId::from_str("character.demo_guard").unwrap()
            ),
            "Demo Guard"
        );
        assert_eq!(
            prototype_name(&engine_core::id::DefinitionId::from_str("npc.wood-worker").unwrap()),
            "Wood Worker"
        );
    }
}
