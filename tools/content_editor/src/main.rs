use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use engine_content::MapPosition;
use engine_core::id::DefinitionId;
use lynx_editor::EditorSession;

#[derive(Resource)]
struct Session(EditorSession);

#[derive(Resource, Default)]
struct Notice(String);

#[derive(Resource, Default)]
struct PreviewMode(bool);

#[derive(Resource, Default)]
struct PlayPreview {
    child: Mutex<Option<Child>>,
    status: Mutex<String>,
}

impl Drop for PlayPreview {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock()
            && let Some(mut child) = child.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[derive(Component)]
struct EditorHud;

#[derive(Component)]
struct ZoneMarker(DefinitionId);

#[derive(Component)]
struct ZoneLabel;

#[derive(Component)]
struct TransformGizmo;

#[derive(Default)]
struct DragState {
    zone: Option<DefinitionId>,
    pointer_origin: Vec2,
}

fn main() {
    let project = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let session = EditorSession::open(&project).unwrap_or_else(|error| {
        eprintln!("Could not open Lynx project {}: {error}", project.display());
        std::process::exit(2);
    });
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Lynx Content Editor".into(),
                resolution: (1180, 760).into(),
                ..default()
            }),
            ..default()
        }))
        .insert_resource(Session(session))
        .init_resource::<Notice>()
        .init_resource::<PreviewMode>()
        .init_resource::<PlayPreview>()
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (
                keyboard,
                select_with_mouse,
                sync_scene,
                autosave,
                monitor_preview,
                draw_hud,
            )
                .chain(),
        )
        .run();
}

fn setup(mut commands: Commands) {
    commands.spawn((Camera2d, EditorCamera));
    commands.spawn((
        EditorHud,
        Text::new("Opening project…"),
        Node {
            position_type: PositionType::Absolute,
            right: px(18),
            top: px(18),
            width: px(345),
            height: px(710),
            padding: UiRect::all(px(18)),
            overflow: Overflow::clip(),
            ..default()
        },
        BackgroundColor(Color::srgba(0.025, 0.035, 0.05, 0.96)),
        TextColor(Color::srgb(0.86, 0.91, 0.94)),
        TextFont {
            font_size: bevy::text::FontSize::Px(15.0),
            ..default()
        },
    ));
}

#[derive(Component)]
struct EditorCamera;

#[allow(clippy::needless_pass_by_value)]
fn keyboard(
    keys: Res<ButtonInput<KeyCode>>,
    mut session: ResMut<Session>,
    mut notice: ResMut<Notice>,
    mut preview: ResMut<PreviewMode>,
    mut play: ResMut<PlayPreview>,
) {
    let control = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    if preview.0
        && !keys.just_pressed(KeyCode::Space)
        && !keys.just_pressed(KeyCode::KeyP)
        && !(control && keys.just_pressed(KeyCode::KeyS))
    {
        return;
    }
    if control && keys.just_pressed(KeyCode::KeyS) {
        notice.0 = match session.0.save() {
            Ok(()) => "Saved project content.".into(),
            Err(error) => format!("Save failed: {error}"),
        };
    } else if control && keys.just_pressed(KeyCode::KeyZ) {
        notice.0 = if session.0.undo() {
            "Undo.".into()
        } else {
            "Nothing to undo.".into()
        };
    } else if control && keys.just_pressed(KeyCode::KeyY) {
        notice.0 = if session.0.redo() {
            "Redo.".into()
        } else {
            "Nothing to redo.".into()
        };
    } else if keys.just_pressed(KeyCode::Tab) {
        cycle_selection(&mut session.0);
    } else if keys.just_pressed(KeyCode::KeyM) {
        cycle_map(&mut session.0);
    } else if keys.just_pressed(KeyCode::ArrowLeft) {
        apply_edit(&mut session.0, &mut notice, |session| {
            session.move_selected(-1, 0)
        });
    } else if keys.just_pressed(KeyCode::ArrowRight) {
        apply_edit(&mut session.0, &mut notice, |session| {
            session.move_selected(1, 0)
        });
    } else if keys.just_pressed(KeyCode::ArrowUp) {
        apply_edit(&mut session.0, &mut notice, |session| {
            session.move_selected(0, -1)
        });
    } else if keys.just_pressed(KeyCode::ArrowDown) {
        apply_edit(&mut session.0, &mut notice, |session| {
            session.move_selected(0, 1)
        });
    } else if keys.just_pressed(KeyCode::Delete) {
        apply_edit(&mut session.0, &mut notice, EditorSession::delete_selected);
    } else if keys.just_pressed(KeyCode::KeyN) {
        place_next_zone(&mut session.0, &mut notice);
    } else if keys.just_pressed(KeyCode::KeyV) {
        let report = session.0.validate();
        notice.0 = format!("Validation finished: {} diagnostic(s).", report.len());
    } else if keys.just_pressed(KeyCode::Space) {
        preview.0 = !preview.0;
        notice.0 = if preview.0 {
            "Preview mode: read-only map view.".into()
        } else {
            "Edit mode.".into()
        };
    } else if keys.just_pressed(KeyCode::KeyP) {
        toggle_play_preview(&mut session.0, &mut play, &mut notice);
    }
}

fn toggle_play_preview(session: &mut EditorSession, play: &mut PlayPreview, notice: &mut Notice) {
    let Ok(mut slot) = play.child.lock() else {
        notice.0 = "Play preview process state is unavailable.".into();
        return;
    };
    if let Some(mut child) = slot.take() {
        let _ = child.kill();
        let _ = child.wait();
        if let Ok(mut status) = play.status.lock() {
            *status = "Stopped local play preview.".into();
        }
        notice.0 = "Stopped local play preview.".into();
        return;
    }
    if let Err(error) = session.save() {
        notice.0 = format!("Save before play preview failed: {error}");
        return;
    }
    let Some(zone) = session.selected_zone() else {
        notice.0 = "Select a placed zone before starting play preview.".into();
        return;
    };
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .map(PathBuf::from);
    let Some(workspace) = workspace else {
        notice.0 = "Could not locate the Lynx workspace for the local preview client.".into();
        return;
    };
    let manifest = workspace
        .join("clients")
        .join("lynx_client")
        .join("Cargo.toml");
    let child = Command::new("cargo")
        .arg("run")
        .arg("--manifest-path")
        .arg(manifest)
        .arg("--")
        .arg("--content")
        .arg(session.project_root())
        .arg("--zone")
        .arg(zone.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match child {
        Ok(child) => {
            *slot = Some(child);
            if let Ok(mut status) = play.status.lock() {
                *status = format!("Local play preview: {zone}");
            }
            notice.0 = "Started local Lynx client preview.".into();
        }
        Err(error) => notice.0 = format!("Could not start local play preview: {error}"),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn monitor_preview(play: Res<PlayPreview>, mut notice: ResMut<Notice>) {
    let Ok(mut slot) = play.child.lock() else {
        return;
    };
    let Some(child) = slot.as_mut() else {
        return;
    };
    match child.try_wait() {
        Ok(Some(status)) => {
            let code = status
                .code()
                .map_or_else(|| "signal".to_owned(), |code| code.to_string());
            slot.take();
            if let Ok(mut text) = play.status.lock() {
                *text = format!("Local preview exited with {code}.");
            }
            notice.0 = format!("Local preview exited with status {code}.");
        }
        Ok(None) => {}
        Err(error) => {
            if let Ok(mut text) = play.status.lock() {
                *text = format!("Could not inspect preview process: {error}");
            }
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
fn select_with_mouse(
    buttons: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    markers: Query<(&Transform, &ZoneMarker)>,
    cameras: Query<(&Camera, &GlobalTransform), With<EditorCamera>>,
    mut session: ResMut<Session>,
    mut notice: ResMut<Notice>,
    mut drag: Local<DragState>,
) {
    let Ok(window) = windows.single() else {
        return;
    };
    let Ok((camera, camera_transform)) = cameras.single() else {
        return;
    };
    if buttons.just_pressed(MouseButton::Left) {
        let Some(cursor) = window.cursor_position() else {
            return;
        };
        if cursor.x > window.width() - 380.0 {
            return;
        }
        let Ok(world_position) = camera.viewport_to_world_2d(camera_transform, cursor) else {
            return;
        };
        let nearest = markers
            .iter()
            .map(|(transform, marker)| {
                (
                    transform.translation.truncate().distance(world_position),
                    &marker.0,
                )
            })
            .filter(|(distance, _)| *distance <= 48.0)
            .min_by(|left, right| left.0.total_cmp(&right.0));
        if let Some((_, zone)) = nearest {
            if session.0.select_zone(zone).is_ok() {
                notice.0 = format!("Selected {zone}; drag to move it.");
                drag.zone = Some(zone.clone());
                drag.pointer_origin = world_position;
            }
        }
    }
    if buttons.just_released(MouseButton::Left) {
        if let (Some(zone), Some(cursor)) = (drag.zone.take(), window.cursor_position())
            && let Ok(world_position) = camera.viewport_to_world_2d(camera_transform, cursor)
        {
            let delta = world_position - drag.pointer_origin;
            let dx = (delta.x / 110.0).round() as i32;
            let dy = (-delta.y / 110.0).round() as i32;
            if session.0.select_zone(&zone).is_ok() && (dx != 0 || dy != 0) {
                apply_edit(&mut session.0, &mut notice, |session| {
                    session.move_selected(dx, dy)
                });
            }
        }
    };
}

#[allow(clippy::needless_pass_by_value)]
fn sync_scene(
    mut commands: Commands,
    session: Res<Session>,
    markers: Query<Entity, With<ZoneMarker>>,
    labels: Query<Entity, With<ZoneLabel>>,
    gizmos: Query<Entity, With<TransformGizmo>>,
    mut rendered_state: Local<Option<(u64, Option<DefinitionId>, Option<DefinitionId>)>>,
) {
    let revision = session.0.revision();
    let selected = session.0.selected_zone().cloned();
    let selected_map = session.0.selected_map().cloned();
    let state = (revision, selected_map, selected.clone());
    if *rendered_state == Some(state.clone()) {
        return;
    }
    for entity in &markers {
        commands.entity(entity).despawn();
    }
    for entity in &labels {
        commands.entity(entity).despawn();
    }
    for entity in &gizmos {
        commands.entity(entity).despawn();
    }
    let Some(map) = session.0.map() else {
        *rendered_state = Some(state);
        return;
    };
    if map.zones.is_empty() {
        *rendered_state = Some(state);
        return;
    }
    let min_x = map
        .zones
        .iter()
        .map(|zone| zone.position.x)
        .min()
        .unwrap_or_default();
    let max_x = map
        .zones
        .iter()
        .map(|zone| zone.position.x)
        .max()
        .unwrap_or_default();
    let min_y = map
        .zones
        .iter()
        .map(|zone| zone.position.y)
        .min()
        .unwrap_or_default();
    let max_y = map
        .zones
        .iter()
        .map(|zone| zone.position.y)
        .max()
        .unwrap_or_default();
    #[allow(clippy::cast_precision_loss)]
    let (center_x, center_y) = (
        (min_x as f32 + max_x as f32) * 0.5,
        (min_y as f32 + max_y as f32) * 0.5,
    );
    #[allow(clippy::cast_precision_loss)]
    for zone in &map.zones {
        let position = Vec3::new(
            (zone.position.x as f32 - center_x) * 110.0,
            (center_y - zone.position.y as f32) * 110.0,
            0.0,
        );
        let is_selected = selected.as_ref() == Some(&zone.zone);
        commands.spawn((
            ZoneMarker(zone.zone.clone()),
            Sprite::from_color(
                if is_selected {
                    Color::srgb(0.18, 0.64, 0.82)
                } else {
                    Color::srgb(0.18, 0.27, 0.34)
                },
                Vec2::new(82.0, 58.0),
            ),
            Transform::from_translation(position),
        ));
        commands.spawn((
            ZoneLabel,
            Text2d::new(zone.zone.to_string()),
            TextFont {
                font_size: bevy::text::FontSize::Px(16.0),
                ..default()
            },
            TextColor(Color::srgb(0.95, 0.97, 0.99)),
            Transform::from_translation(position + Vec3::new(0.0, 42.0, 2.0)),
        ));
        if is_selected {
            let gizmo_color = Color::srgb(0.96, 0.72, 0.24);
            for (offset, size) in [
                (Vec2::new(52.0, 0.0), Vec2::new(30.0, 4.0)),
                (Vec2::new(-52.0, 0.0), Vec2::new(30.0, 4.0)),
                (Vec2::new(0.0, 42.0), Vec2::new(4.0, 26.0)),
                (Vec2::new(0.0, -42.0), Vec2::new(4.0, 26.0)),
            ] {
                commands.spawn((
                    TransformGizmo,
                    Sprite::from_color(gizmo_color, size),
                    Transform::from_translation(position + offset.extend(1.0)),
                ));
            }
        }
    }
    *rendered_state = Some(state);
}

fn cycle_selection(session: &mut EditorSession) {
    let Some(map) = session.map() else {
        return;
    };
    if map.zones.is_empty() {
        return;
    }
    let index = session
        .selected_zone()
        .and_then(|selected| {
            map.zones
                .iter()
                .position(|placement| &placement.zone == selected)
        })
        .unwrap_or(0);
    let next = map.zones[(index + 1) % map.zones.len()].zone.clone();
    let _ = session.select_zone(&next);
}

fn cycle_map(session: &mut EditorSession) {
    let maps: Vec<_> = session.maps().map(|map| map.id.clone()).collect();
    if maps.is_empty() {
        return;
    }
    let current = session
        .selected_map()
        .and_then(|selected| maps.iter().position(|map| map == selected))
        .unwrap_or(0);
    let next = &maps[(current + 1) % maps.len()];
    let _ = session.select_map(next);
}

fn place_next_zone(session: &mut EditorSession, notice: &mut Notice) {
    let Some(map) = session.map() else {
        notice.0 = "No map is available for placement.".into();
        return;
    };
    let placed: Vec<_> = map
        .zones
        .iter()
        .map(|placement| placement.zone.clone())
        .collect();
    let position = session
        .selected_zone()
        .and_then(|selected| {
            map.zones
                .iter()
                .find(|placement| &placement.zone == selected)
        })
        .map_or(MapPosition { x: 0, y: 0 }, |placement| MapPosition {
            x: placement.position.x.saturating_add(1),
            y: placement.position.y,
        });
    let next = session
        .placeable_zones()
        .find(|zone| !placed.contains(zone))
        .cloned();
    let Some(zone) = next else {
        notice.0 = "All zone definitions are already placed.".into();
        return;
    };
    apply_edit(session, notice, |session| {
        session.place_zone(&zone, position)
    });
}

fn apply_edit(
    session: &mut EditorSession,
    notice: &mut Notice,
    operation: impl FnOnce(&mut EditorSession) -> Result<(), lynx_editor::SessionError>,
) {
    notice.0 = operation(session).map_or_else(
        |error| error.to_string(),
        |_| "Edit recorded. Ctrl+S saves; Ctrl+Z undoes.".into(),
    );
}

#[allow(clippy::needless_pass_by_value)]
fn autosave(
    time: Res<Time>,
    mut timer: Local<Timer>,
    session: ResMut<Session>,
    mut notice: ResMut<Notice>,
) {
    if timer.duration().is_zero() {
        *timer = Timer::from_seconds(2.0, TimerMode::Repeating);
    }
    timer.tick(time.delta());
    if timer.just_finished() && session.0.is_dirty() {
        if let Err(error) = session.0.autosave() {
            notice.0 = format!("Recovery autosave failed: {error}");
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
fn draw_hud(
    session: Res<Session>,
    notice: Res<Notice>,
    preview: Res<PreviewMode>,
    play: Res<PlayPreview>,
    mut hud: Query<&mut Text, With<EditorHud>>,
) {
    let mut output = String::new();
    let _ = writeln!(
        output,
        "LYNX CONTENT EDITOR{}",
        if preview.0 { " · PREVIEW" } else { "" }
    );
    let _ = writeln!(
        output,
        "Project: {}{}",
        session.0.project_root().display(),
        if session.0.is_dirty() {
            "  [unsaved]"
        } else {
            ""
        }
    );
    if let Some(map) = session.0.map() {
        let _ = writeln!(
            output,
            "\nINSPECTOR\nMap: {}  ·  {} placement(s)",
            map.id,
            map.zones.len()
        );
        let selected = session.0.selected_zone();
        for placement in &map.zones {
            let marker = if selected == Some(&placement.zone) {
                ">"
            } else {
                " "
            };
            let _ = writeln!(
                output,
                " {marker} {:<30} ({:>4}, {:>4})",
                placement.zone, placement.position.x, placement.position.y
            );
        }
    } else {
        let _ = writeln!(output, "\nNo world map definitions found.");
    }
    let report = session.0.validation();
    let selected_position = session.0.map().and_then(|map| {
        session
            .0
            .selected_zone()
            .and_then(|id| map.zones.iter().find(|zone| &zone.zone == id))
    });
    if let Some(zone) = selected_position {
        let _ = writeln!(
            output,
            "\nTransform\nX: {}\nY: {}",
            zone.position.x, zone.position.y
        );
    }
    let _ = writeln!(
        output,
        "\nVALIDATION · {} error(s), {} warning(s)",
        report.errors().count(),
        report.warnings().count()
    );
    for diagnostic in report.diagnostics.iter().take(8) {
        let _ = writeln!(
            output,
            "  {} {} — {}",
            diagnostic.severity, diagnostic.path, diagnostic.message
        );
    }
    let _ = writeln!(
        output,
        "\nM map · Tab zone · drag/arrows transform\nN place · Delete remove\nCtrl+Z/Y undo/redo · Ctrl+S save\nV validate · Space map view · P play"
    );
    if let Ok(status) = play.status.lock() {
        let _ = writeln!(output, "{}", *status);
    }
    let _ = writeln!(output, "{}", notice.0);
    for mut text in &mut hud {
        text.0.clone_from(&output);
    }
}
