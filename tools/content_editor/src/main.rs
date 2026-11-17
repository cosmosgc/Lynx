use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::{EguiContexts, EguiPlugin, EguiPrimaryContextPass, egui};
use engine_content::MapPosition;
use engine_core::id::DefinitionId;
use lynx_editor::EditorSession;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

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

#[derive(Resource)]
struct ContentWatch {
    _watcher: Mutex<Option<RecommendedWatcher>>,
    events: Mutex<mpsc::Receiver<notify::Result<Event>>>,
    last_event: Option<Instant>,
    changed_on_disk: bool,
    /// Paths this editor just wrote, with the time the write was recorded.
    own_writes: Vec<(PathBuf, Instant)>,
}

impl ContentWatch {
    fn new(content_root: &Path) -> Result<Self, notify::Error> {
        let (sender, events) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = sender.send(event);
        })?;
        watcher.watch(content_root, RecursiveMode::Recursive)?;
        Ok(Self {
            _watcher: Mutex::new(Some(watcher)),
            events: Mutex::new(events),
            last_event: None,
            changed_on_disk: false,
            own_writes: Vec::new(),
        })
    }

    fn disabled() -> Self {
        let (_sender, events) = mpsc::channel();
        Self {
            _watcher: Mutex::new(None),
            events: Mutex::new(events),
            last_event: None,
            changed_on_disk: false,
            own_writes: Vec::new(),
        }
    }

    /// Clear the pending external-change state once the user resolved it.
    fn mark_resolved(&mut self) {
        self.changed_on_disk = false;
        self.last_event = None;
    }

    /// Remember files the editor wrote so their watcher events are ignored.
    fn expect_writes(&mut self, paths: Vec<PathBuf>) {
        self.prune_own_writes();
        let now = Instant::now();
        self.own_writes
            .extend(paths.into_iter().map(|path| (path, now)));
    }

    fn prune_own_writes(&mut self) {
        let now = Instant::now();
        self.own_writes
            .retain(|(_, at)| now.duration_since(*at) < SELF_WRITE_WINDOW);
    }

    /// True when every content path in the event came from this editor's own write.
    fn ignores_event(&mut self, event: &Event) -> bool {
        if self.own_writes.is_empty() {
            return false;
        }
        self.prune_own_writes();
        let changed: Vec<&Path> = event
            .paths
            .iter()
            .map(PathBuf::as_path)
            .filter(|path| is_content_file(path))
            .collect();
        !changed.is_empty()
            && changed.iter().all(|path| {
                self.own_writes
                    .iter()
                    .any(|(written, _)| same_file(written, path))
            })
    }
}

/// How long after an editor save its own filesystem events are ignored.
const SELF_WRITE_WINDOW: Duration = Duration::from_secs(3);

fn same_file(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

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
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    let session = EditorSession::open(&project).unwrap_or_else(|error| {
        eprintln!("Could not open Lynx project {}: {error}", project.display());
        std::process::exit(2);
    });
    let content_root = session.project_root().join("content");
    let (content_watch, watch_notice) = match ContentWatch::new(&content_root) {
        Ok(watch) => (watch, String::new()),
        Err(error) => (
            ContentWatch::disabled(),
            format!("Content file watching is unavailable: {error}"),
        ),
    };
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Lynx Content Editor".into(),
                resolution: (1180, 760).into(),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(EguiPlugin::default())
        .insert_resource(Session(session))
        .insert_resource(Notice(watch_notice))
        .insert_resource(content_watch)
        .init_resource::<PreviewMode>()
        .init_resource::<PlayPreview>()
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (
                poll_external_content,
                keyboard,
                select_with_mouse,
                sync_scene,
                autosave,
                monitor_preview,
            )
                .chain(),
        )
        .add_systems(EguiPrimaryContextPass, draw_editor_ui)
        .run();
}

fn setup(mut commands: Commands) {
    commands.spawn((Camera2d, EditorCamera));
}

#[derive(Component)]
struct EditorCamera;

#[allow(clippy::needless_pass_by_value)]
fn keyboard(
    keys: Res<ButtonInput<KeyCode>>,
    mut session: ResMut<Session>,
    mut notice: ResMut<Notice>,
    mut watch: ResMut<ContentWatch>,
    mut preview: ResMut<PreviewMode>,
    mut play: ResMut<PlayPreview>,
) {
    let control = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    if !(keys.just_pressed(KeyCode::Space)
        || keys.just_pressed(KeyCode::KeyP)
        || control && keys.just_pressed(KeyCode::KeyS)
        || !preview.0)
    {
        return;
    }
    if control && keys.just_pressed(KeyCode::KeyS) {
        notice.0 = if watch.changed_on_disk {
            "Files changed outside the editor. Resolve the conflict in the inspector before saving."
                .into()
        } else {
            save_session(&mut session.0, &mut watch, "Saved project content.")
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
        toggle_play_preview(&mut session.0, &mut play, &mut notice, &mut watch);
    }
}

#[allow(clippy::needless_pass_by_value)]
fn poll_external_content(
    mut watch: ResMut<ContentWatch>,
    mut session: ResMut<Session>,
    mut notice: ResMut<Notice>,
) {
    let mut changed = false;
    let received: Vec<notify::Result<Event>> = match watch.events.lock() {
        Ok(events) => {
            let mut received = Vec::new();
            while let Ok(event) = events.try_recv() {
                received.push(event);
            }
            received
        }
        Err(_) => return,
    };
    for event in received {
        match event {
            Ok(event) if watch.ignores_event(&event) => {}
            Ok(event) if is_content_change(&event) => changed = true,
            Ok(_) => {}
            Err(error) => notice.0 = format!("Content watcher error: {error}"),
        }
    }
    if changed {
        watch.changed_on_disk = true;
        watch.last_event = Some(Instant::now());
    }

    let Some(last_event) = watch.last_event else {
        return;
    };
    if last_event.elapsed() < Duration::from_millis(300) {
        return;
    }
    watch.last_event = None;

    if session.0.is_dirty() {
        notice.0 = "Project files changed externally. Unsaved editor changes were preserved; resolve the conflict in the inspector.".into();
        return;
    }

    match session.0.reload_external_changes() {
        Ok(()) => {
            watch.changed_on_disk = false;
            notice.0 = "Reloaded project content changed on disk.".into();
        }
        Err(error) => notice.0 = format!("Could not reload external content: {error}"),
    }
}

fn is_content_change(event: &Event) -> bool {
    !matches!(event.kind, EventKind::Access(_))
        && event.paths.iter().any(|path| is_content_file(path))
}

/// Staging files, crash-recovery backups, and directory-only events are
/// bookkeeping, not authored content.
fn is_content_file(path: &Path) -> bool {
    if path.is_dir() {
        return false;
    }
    let is_temporary = path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("tmp"));
    let is_editor_backup = path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("bak"))
        && path.file_stem().is_some_and(|stem| {
            stem.to_string_lossy()
                .to_ascii_lowercase()
                .ends_with(".lynx-editor")
        });
    !is_temporary && !is_editor_backup
}

fn save_session(session: &mut EditorSession, watch: &mut ContentWatch, success: &str) -> String {
    match session.save() {
        Ok(()) => {
            watch.expect_writes(session.take_saved_paths());
            success.to_owned()
        }
        Err(error) => format!("Save failed: {error}"),
    }
}

fn toggle_play_preview(
    session: &mut EditorSession,
    play: &mut PlayPreview,
    notice: &mut Notice,
    watch: &mut ContentWatch,
) {
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
    if watch.changed_on_disk && session.is_dirty() {
        notice.0 =
            "Resolve external content changes before saving and launching play preview.".into();
        return;
    }
    if let Err(error) = session.save() {
        notice.0 = format!("Save before play preview failed: {error}");
        return;
    }
    watch.expect_writes(session.take_saved_paths());
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
        let panel_width = window.width().min(350.0);
        let panel_left = (window.width() - panel_width - 12.0).max(0.0);
        if cursor.x >= panel_left {
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
        if let Some((_, zone)) = nearest
            && session.0.select_zone(zone).is_ok()
        {
            notice.0 = format!("Selected {zone}; drag to move it.");
            drag.zone = Some(zone.clone());
            drag.pointer_origin = world_position;
        }
    }
    if buttons.just_released(MouseButton::Left)
        && let (Some(zone), Some(cursor)) = (drag.zone.take(), window.cursor_position())
        && let Ok(world_position) = camera.viewport_to_world_2d(camera_transform, cursor)
    {
        let delta = world_position - drag.pointer_origin;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "pointer movement is rounded to integer authored map cells"
        )]
        let (dx, dy) = (
            (delta.x / 110.0).round() as i32,
            (-delta.y / 110.0).round() as i32,
        );
        if session.0.select_zone(&zone).is_ok() && (dx != 0 || dy != 0) {
            apply_edit(&mut session.0, &mut notice, |session| {
                session.move_selected(dx, dy)
            });
        }
    }
}

type RenderedState = (u64, Option<DefinitionId>, Option<DefinitionId>);

#[allow(clippy::needless_pass_by_value)]
fn sync_scene(
    mut commands: Commands,
    session: Res<Session>,
    markers: Query<Entity, With<ZoneMarker>>,
    labels: Query<Entity, With<ZoneLabel>>,
    gizmos: Query<Entity, With<TransformGizmo>>,
    mut rendered_state: Local<Option<RenderedState>>,
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
        f32::midpoint(min_x as f32, max_x as f32),
        f32::midpoint(min_y as f32, max_y as f32),
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
        |()| "Edit recorded. Ctrl+S saves; Ctrl+Z undoes.".into(),
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
    if timer.just_finished()
        && session.0.is_dirty()
        && let Err(error) = session.0.autosave()
    {
        notice.0 = format!("Recovery autosave failed: {error}");
    }
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    reason = "Keeps the compact editor inspector layout and its actions together."
)]
fn draw_editor_ui(
    mut contexts: EguiContexts,
    mut session: ResMut<Session>,
    mut notice: ResMut<Notice>,
    mut preview: ResMut<PreviewMode>,
    mut play: ResMut<PlayPreview>,
    mut watch: ResMut<ContentWatch>,
) -> Result {
    let context = contexts.ctx_mut()?;
    let viewport = context.viewport_rect();
    let panel_width = viewport.width().min(350.0);
    let panel_height = (viewport.height() - 24.0).max(120.0);
    let panel_position = egui::pos2(
        (viewport.right() - panel_width - 12.0).max(viewport.left()),
        viewport.top() + 12.0,
    );
    let project_root = session.0.project_root().display().to_string();
    let is_dirty = session.0.is_dirty();
    let map_ids: Vec<_> = session.0.maps().map(|map| map.id.clone()).collect();
    let placements: Vec<_> = session
        .0
        .map()
        .map(|map| {
            map.zones
                .iter()
                .map(|placement| (placement.zone.clone(), placement.position))
                .collect()
        })
        .unwrap_or_default();
    let diagnostics: Vec<_> = session
        .0
        .validation()
        .diagnostics
        .iter()
        .take(8)
        .map(|diagnostic| {
            format!(
                "{} · {} — {}",
                diagnostic.severity, diagnostic.path, diagnostic.message
            )
        })
        .collect();

    egui::Window::new("Lynx Content Editor")
        .id(egui::Id::new("lynx-editor-inspector"))
        .title_bar(false)
        .resizable(false)
        .movable(false)
        .fixed_pos(panel_position)
        .fixed_size(egui::vec2(panel_width, panel_height))
        .show(context, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.heading(if preview.0 {
                    "Lynx Content Editor · Preview"
                } else {
                    "Lynx Content Editor"
                });
                ui.label(&project_root);
                ui.label(if is_dirty {
                    "Unsaved map edits"
                } else {
                    "All map edits saved"
                });

                ui.horizontal(|ui| {
                    if ui.button("Undo").clicked() {
                        notice.0 = if session.0.undo() {
                            "Undo.".into()
                        } else {
                            "Nothing to undo.".into()
                        };
                    }
                    if ui.button("Redo").clicked() {
                        notice.0 = if session.0.redo() {
                            "Redo.".into()
                        } else {
                            "Nothing to redo.".into()
                        };
                    }
                    if ui.button("Validate").clicked() {
                        let count = session.0.validate().len();
                        notice.0 = format!("Validation finished: {count} diagnostic(s).");
                    }
                });

                ui.horizontal(|ui| {
                    if ui.button(if preview.0 { "Edit mode" } else { "Map preview" }).clicked() {
                        preview.0 = !preview.0;
                        notice.0 = if preview.0 {
                            "Preview mode: read-only map view.".into()
                        } else {
                            "Edit mode.".into()
                        };
                    }
                    if ui.button(if play.child.lock().is_ok_and(|child| child.is_some()) {
                        "Stop play"
                    } else {
                        "Play"
                    }).clicked() {
                        toggle_play_preview(&mut session.0, &mut play, &mut notice, &mut watch);
                    }
                });

                if watch.changed_on_disk {
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        "Project files changed outside the editor.",
                    );
                    if session.0.is_dirty() {
                        ui.label("Your in-memory edits were preserved. This overwrites the externally changed map files with your edits:");
                        if ui.button("Keep my edits").clicked() {
                            notice.0 = save_session(
                                &mut session.0,
                                &mut watch,
                                "Saved editor changes over the external content.",
                            );
                            watch.mark_resolved();
                        }
                    } else if ui.button("Reload changed files").clicked() {
                        notice.0 = match session.0.reload_external_changes() {
                            Ok(()) => {
                                watch.mark_resolved();
                                "Reloaded project content.".into()
                            }
                            Err(error) => format!("Reload failed: {error}"),
                        };
                    }
                } else if ui
                    .add_enabled(is_dirty, egui::Button::new("Save (Ctrl+S)"))
                    .clicked()
                {
                    notice.0 = save_session(&mut session.0, &mut watch, "Saved project content.");
                }

                if !map_ids.is_empty() {
                    let mut selected_map = session.0.selected_map().cloned();
                    let selected_text = selected_map
                        .as_ref()
                        .map_or_else(|| "Select a map".to_owned(), ToString::to_string);
                    egui::ComboBox::from_label("World map")
                        .selected_text(selected_text)
                        .show_ui(ui, |ui| {
                            for map_id in &map_ids {
                                if ui
                                    .selectable_label(
                                        selected_map.as_ref() == Some(map_id),
                                        map_id.as_str(),
                                    )
                                    .clicked()
                                {
                                    selected_map = Some(map_id.clone());
                                }
                            }
                        });
                    if selected_map != session.0.selected_map().cloned()
                        && let Some(map_id) = selected_map
                    {
                        let _ = session.0.select_map(&map_id);
                    }
                }

                ui.separator();
                ui.strong("Placed zones");
                if placements.is_empty() {
                    ui.label("No zones are placed on this map.");
                }
                for (zone, position) in &placements {
                    let selected = session.0.selected_zone() == Some(zone);
                    let label = format!("{}  ({}, {})", zone, position.x, position.y);
                    if ui.selectable_label(selected, label).clicked() {
                        let _ = session.0.select_zone(zone);
                    }
                }

                ui.horizontal(|ui| {
                    if ui.button("Place next zone (N)").clicked() {
                        place_next_zone(&mut session.0, &mut notice);
                    }
                    if ui.button("Delete selected").clicked() {
                        apply_edit(&mut session.0, &mut notice, EditorSession::delete_selected);
                    }
                });

                ui.separator();
                let report = session.0.validation();
                ui.strong(format!(
                    "Validation · {} error(s), {} warning(s)",
                    report.errors().count(),
                    report.warnings().count()
                ));
                for diagnostic in &diagnostics {
                    ui.label(diagnostic);
                }
                if let Ok(status) = play.status.lock() {
                    ui.label(status.as_str());
                }
                ui.separator();
                ui.label("Click or drag a zone · arrows move · M map · Tab zone");
                ui.label("N place · Delete remove · Ctrl+Z/Y undo/redo");
                ui.label("Ctrl+S save · V validate · Space preview · P play");
                if !notice.0.is_empty() {
                    ui.separator();
                    ui.label(&notice.0);
                }
            });
        });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(path: &str) -> Event {
        Event::new(EventKind::Modify(notify::event::ModifyKind::Any)).add_path(PathBuf::from(path))
    }

    #[test]
    fn editor_saves_are_not_external_content_changes() {
        let mut watch = ContentWatch::disabled();
        let saved = PathBuf::from("content/maps/overworld.ron");

        assert!(!watch.ignores_event(&event("content/maps/overworld.ron")));

        watch.expect_writes(vec![saved]);
        assert!(watch.ignores_event(&event("content/maps/overworld.ron")));
        assert!(!watch.ignores_event(&event("content/zones/starter.ron")));
    }

    #[test]
    fn staging_renames_of_editor_writes_are_ignored() {
        let mut watch = ContentWatch::disabled();
        watch.expect_writes(vec![PathBuf::from("content/maps/overworld.ron")]);

        let mut rename = event("content/maps/overworld.ron");
        rename
            .paths
            .insert(0, PathBuf::from("content/maps/overworld.ron.4242.99.tmp"));
        assert!(watch.ignores_event(&rename));

        let mut mixed = event("content/maps/overworld.ron");
        mixed.paths.push(PathBuf::from("content/zones/starter.ron"));
        assert!(!watch.ignores_event(&mixed));
    }

    #[test]
    fn directory_events_are_not_content_changes() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert!(!is_content_file(manifest));
        assert!(!is_content_file(&manifest.join("src")));
        assert!(is_content_file(&manifest.join("src").join("main.rs")));
    }

    #[test]
    fn staging_and_recovery_files_are_not_content_changes() {
        assert!(!is_content_change(&event(
            "content/maps/overworld.ron.4242.123.tmp"
        )));
        assert!(!is_content_change(&event(
            "content/maps/overworld.ron.lynx-editor.bak"
        )));
        assert!(is_content_change(&event("content/maps/overworld.ron")));
    }
}
