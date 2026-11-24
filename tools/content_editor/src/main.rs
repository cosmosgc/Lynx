use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

use bevy::camera::Viewport;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::{EguiContexts, EguiPlugin, EguiPrimaryContextPass, egui};
use engine_content::definition::ZoneEntityPlacement;
use engine_content::{ContentDefinition, MapPosition};
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
struct ProjectDialog {
    open: bool,
    path: String,
}

#[derive(Resource, Default, Clone, Copy, PartialEq, Eq)]
enum EditorSurface {
    #[default]
    WorldAtlas,
    ZoneScene,
}

fn surface_is_active(current: EditorSurface, camera_surface: EditorSurface) -> bool {
    current == camera_surface
}

#[derive(Resource)]
struct EditorPanels {
    hierarchy: bool,
    inspector: bool,
}

impl Default for EditorPanels {
    fn default() -> Self {
        Self {
            hierarchy: true,
            inspector: true,
        }
    }
}

#[derive(Resource, Default)]
struct SceneViewport(Option<egui::Rect>);

impl SceneViewport {
    fn contains(&self, cursor: Vec2) -> bool {
        self.0
            .is_some_and(|rect| rect.contains(egui::pos2(cursor.x, cursor.y)))
    }
}

#[derive(Resource)]
struct ZoneSceneView {
    yaw: f32,
    pitch: f32,
    distance: f32,
    target: Vec3,
    selected_object: SceneObjectId,
    focus_selected: bool,
}

#[derive(Default)]
struct PlacementTransformDraft {
    selection: Option<(DefinitionId, String)>,
    session_revision: u64,
    position: [f32; 3],
    yaw_degrees: f32,
}

impl Default for ZoneSceneView {
    fn default() -> Self {
        Self {
            yaw: 0.65,
            pitch: 0.55,
            distance: 18.0,
            target: Vec3::ZERO,
            selected_object: SceneObjectId::ZoneFloor,
            focus_selected: false,
        }
    }
}

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

#[derive(Component)]
struct ZoneSceneCamera;

#[derive(Debug, Clone, PartialEq, Eq)]
enum SceneObjectId {
    ZoneFloor,
    EntityPlacement(String),
}

impl SceneObjectId {
    fn is_placement(&self, id: &str) -> bool {
        matches!(self, Self::EntityPlacement(placement_id) if placement_id == id)
    }

    fn label(&self) -> &str {
        match self {
            Self::ZoneFloor => "Zone floor",
            Self::EntityPlacement(id) => id,
        }
    }
}

#[derive(Component)]
struct ZoneSceneObject(SceneObjectId);

#[derive(Component)]
struct ZoneSceneGeometry;

#[derive(Resource, Default)]
struct ZoneScenePalette {
    floor: Option<Handle<StandardMaterial>>,
    selected_floor: Option<Handle<StandardMaterial>>,
    grid: Option<Handle<StandardMaterial>>,
    object: Option<Handle<StandardMaterial>>,
    selected_object: Option<Handle<StandardMaterial>>,
}

impl ZoneScenePalette {
    fn initialize(&mut self, materials: &mut Assets<StandardMaterial>) {
        self.floor
            .get_or_insert_with(|| materials.add(Color::srgb(0.19, 0.27, 0.31)));
        self.selected_floor
            .get_or_insert_with(|| materials.add(Color::srgb(0.31, 0.47, 0.58)));
        self.grid
            .get_or_insert_with(|| materials.add(Color::srgb(0.42, 0.51, 0.54)));
        self.object
            .get_or_insert_with(|| materials.add(Color::srgb(0.24, 0.68, 0.76)));
        self.selected_object
            .get_or_insert_with(|| materials.add(Color::srgb(0.98, 0.69, 0.22)));
    }
}

const MIN_ZONE_CAMERA_DISTANCE: f32 = 3.0;
const MAX_ZONE_CAMERA_DISTANCE: f32 = 120.0;

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
        .init_resource::<ProjectDialog>()
        .init_resource::<EditorSurface>()
        .init_resource::<EditorPanels>()
        .init_resource::<SceneViewport>()
        .init_resource::<ZoneSceneView>()
        .init_resource::<ZoneScenePalette>()
        .init_resource::<PlayPreview>()
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (
                poll_external_content,
                keyboard,
                sync_camera_surfaces,
                select_with_mouse,
                zone_scene_input,
                sync_scene,
                sync_zone_scene,
                sync_zone_scene_selection,
                update_zone_scene_camera,
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
    commands.spawn((
        Camera3d::default(),
        Tonemapping::Reinhard,
        Transform::from_xyz(12.0, 10.0, 16.0).looking_at(Vec3::ZERO, Vec3::Y),
        ZoneSceneCamera,
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 12_000.0,
            ..default()
        },
        Transform::from_xyz(6.0, 12.0, 8.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
}

#[derive(Component)]
struct EditorCamera;

#[allow(clippy::needless_pass_by_value)]
fn sync_camera_surfaces(
    surface: Res<EditorSurface>,
    cameras: Query<(&mut Camera, Option<&EditorCamera>, Option<&ZoneSceneCamera>)>,
) {
    for (mut camera, atlas, scene) in cameras {
        if atlas.is_some() {
            camera.is_active = surface_is_active(*surface, EditorSurface::WorldAtlas);
        } else if scene.is_some() {
            camera.is_active = surface_is_active(*surface, EditorSurface::ZoneScene);
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
#[allow(
    clippy::too_many_arguments,
    reason = "Bevy system inputs are kept explicit for the editor actions."
)]
fn keyboard(
    mut contexts: EguiContexts,
    keys: Res<ButtonInput<KeyCode>>,
    mut session: ResMut<Session>,
    mut project_dialog: ResMut<ProjectDialog>,
    mut notice: ResMut<Notice>,
    mut watch: ResMut<ContentWatch>,
    mut preview: ResMut<PreviewMode>,
    surface: Res<EditorSurface>,
    mut zone_view: ResMut<ZoneSceneView>,
    mut play: ResMut<PlayPreview>,
) {
    if contexts
        .ctx_mut()
        .is_ok_and(|context| context.egui_wants_keyboard_input())
    {
        return;
    }
    let control = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let open_project = control && keys.just_pressed(KeyCode::KeyO);
    if !(keys.just_pressed(KeyCode::Space)
        || keys.just_pressed(KeyCode::KeyP)
        || control && keys.just_pressed(KeyCode::KeyS)
        || open_project
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
    } else if open_project {
        if session.0.is_dirty() {
            notice.0 = "Save your changes before opening another project.".into();
        } else {
            project_dialog.path = session.0.project_root().display().to_string();
            project_dialog.open = true;
        }
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
    } else if keys.just_pressed(KeyCode::KeyF) && *surface == EditorSurface::ZoneScene {
        zone_view.focus_selected = true;
        notice.0 = "Focused the selected scene object.".into();
    } else if keys.just_pressed(KeyCode::Space) {
        preview.0 = !preview.0;
        notice.0 = if preview.0 {
            "Preview mode: content editing is disabled.".into()
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

#[allow(clippy::needless_pass_by_value, clippy::too_many_arguments)]
fn select_with_mouse(
    surface: Res<EditorSurface>,
    viewport: Res<SceneViewport>,
    buttons: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    markers: Query<(&Transform, &ZoneMarker)>,
    cameras: Query<(&Camera, &GlobalTransform), With<EditorCamera>>,
    mut session: ResMut<Session>,
    mut notice: ResMut<Notice>,
    mut drag: Local<DragState>,
) {
    if *surface != EditorSurface::WorldAtlas {
        drag.zone = None;
        return;
    }
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
        if !viewport.contains(cursor) {
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

#[allow(clippy::needless_pass_by_value, clippy::too_many_arguments)]
fn zone_scene_input(
    surface: Res<EditorSurface>,
    viewport: Res<SceneViewport>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<bevy::input::mouse::AccumulatedMouseMotion>,
    scroll: Res<bevy::input::mouse::AccumulatedMouseScroll>,
    windows: Query<&Window, With<PrimaryWindow>>,
    cameras: Query<(&Camera, &GlobalTransform), With<ZoneSceneCamera>>,
    objects: Query<(&GlobalTransform, &ZoneSceneObject)>,
    mut view: ResMut<ZoneSceneView>,
    mut notice: ResMut<Notice>,
) {
    if *surface != EditorSurface::ZoneScene {
        return;
    }
    let (Ok(window), Ok((camera, camera_transform))) = (windows.single(), cameras.single()) else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };
    if !viewport.contains(cursor) {
        return;
    }

    if buttons.pressed(MouseButton::Right) {
        orbit_zone_camera(&mut view, motion.delta);
    }
    if buttons.pressed(MouseButton::Middle) {
        pan_zone_camera(
            &mut view,
            motion.delta,
            viewport.0.map_or(window.height(), |rect| rect.height()),
        );
    }
    zoom_zone_camera(&mut view, scroll.delta.y);

    if buttons.just_pressed(MouseButton::Left) {
        let selected = objects
            .iter()
            .filter_map(|(transform, object)| {
                camera
                    .world_to_viewport(camera_transform, transform.translation())
                    .ok()
                    .map(|screen| (screen.distance_squared(cursor), object.0.clone()))
            })
            .filter(|(distance, _)| *distance <= 44.0 * 44.0)
            .min_by(|left, right| left.0.total_cmp(&right.0))
            .map(|(_, id)| id);
        if let Some(selected) = selected {
            view.selected_object = selected;
            notice.0 = format!("Selected scene object {}.", view.selected_object.label());
        }
    }
}

fn orbit_zone_camera(view: &mut ZoneSceneView, pointer_delta: Vec2) {
    view.yaw -= pointer_delta.x * 0.005;
    view.pitch = (view.pitch + pointer_delta.y * 0.005).clamp(0.15, 1.4);
}

fn pan_zone_camera(view: &mut ZoneSceneView, pointer_delta: Vec2, viewport_height: f32) {
    let scale = view.distance / viewport_height.max(1.0);
    view.target.x -= pointer_delta.x * scale;
    view.target.z += pointer_delta.y * scale;
}

fn zoom_zone_camera(view: &mut ZoneSceneView, scroll_delta: f32) {
    if scroll_delta != 0.0 {
        view.distance = (view.distance * (-scroll_delta * 0.1).exp())
            .clamp(MIN_ZONE_CAMERA_DISTANCE, MAX_ZONE_CAMERA_DISTANCE);
    }
}

fn zone_camera_offset(yaw: f32, pitch: f32, distance: f32) -> Vec3 {
    Vec3::new(
        yaw.sin() * pitch.cos(),
        pitch.sin(),
        yaw.cos() * pitch.cos(),
    ) * distance
}

fn zone_scene_layout(definition: Option<&ContentDefinition>) -> (Vec3, Vec2) {
    let placements = definition.map_or(&[][..], |definition| {
        definition.entity_placements.as_slice()
    });
    zone_scene_layout_from_placements(placements)
}

fn zone_scene_layout_from_placements(placements: &[ZoneEntityPlacement]) -> (Vec3, Vec2) {
    if placements.is_empty() {
        return (Vec3::ZERO, Vec2::splat(12.0));
    }
    let mut min_x = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut min_z = f32::INFINITY;
    let mut max_z = f32::NEG_INFINITY;
    for placement in placements {
        min_x = min_x.min(placement.position.x);
        max_x = max_x.max(placement.position.x);
        min_z = min_z.min(placement.position.z);
        max_z = max_z.max(placement.position.z);
    }
    let center = Vec3::new(
        f32::midpoint(min_x, max_x),
        0.0,
        f32::midpoint(min_z, max_z),
    );
    let size = Vec2::new(
        (max_x - min_x + 12.0).clamp(12.0, 64.0),
        (max_z - min_z + 12.0).clamp(12.0, 64.0),
    );
    (center, size)
}

#[allow(clippy::needless_pass_by_value, clippy::too_many_arguments)]
fn sync_zone_scene(
    mut commands: Commands,
    session: Res<Session>,
    mut view: ResMut<ZoneSceneView>,
    geometry: Query<Entity, With<ZoneSceneGeometry>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut palette: ResMut<ZoneScenePalette>,
    mut rendered_state: Local<Option<(u64, Option<DefinitionId>)>>,
) {
    let selected_zone = session.0.selected_zone().cloned();
    let state = (session.0.content_revision(), selected_zone.clone());
    if *rendered_state == Some(state.clone()) {
        return;
    }

    let definition = session.0.selected_zone_definition();
    let (center, size) = zone_scene_layout(definition);
    if rendered_state
        .as_ref()
        .is_none_or(|(_, old_zone)| *old_zone != selected_zone)
    {
        view.target = center;
        view.selected_object = SceneObjectId::ZoneFloor;
    }
    let Some(definition) = definition else {
        for entity in &geometry {
            commands.entity(entity).despawn();
        }
        *rendered_state = Some(state);
        return;
    };

    let selection_exists = match &view.selected_object {
        SceneObjectId::ZoneFloor => true,
        SceneObjectId::EntityPlacement(id) => definition
            .entity_placements
            .iter()
            .any(|placement| &placement.id == id),
    };
    if !selection_exists {
        view.selected_object = SceneObjectId::ZoneFloor;
    }
    for entity in &geometry {
        commands.entity(entity).despawn();
    }

    palette.initialize(&mut materials);
    let floor_material = if view.selected_object == SceneObjectId::ZoneFloor {
        palette.selected_floor.as_ref()
    } else {
        palette.floor.as_ref()
    }
    .expect("zone scene palette is initialized")
    .clone();
    commands.spawn((
        ZoneSceneGeometry,
        ZoneSceneObject(SceneObjectId::ZoneFloor),
        Mesh3d(meshes.add(Cuboid::new(size.x, 0.12, size.y))),
        MeshMaterial3d(floor_material),
        Transform::from_translation(center - Vec3::Y * 0.06),
    ));
    let grid_material = palette
        .grid
        .as_ref()
        .expect("zone scene palette is initialized")
        .clone();
    for offset in [-5.0_f32, -3.0, -1.0, 1.0, 3.0, 5.0] {
        if offset.abs() < size.x * 0.5 {
            commands.spawn((
                ZoneSceneGeometry,
                Mesh3d(meshes.add(Cuboid::new(size.x, 0.012, 0.025))),
                MeshMaterial3d(grid_material.clone()),
                Transform::from_xyz(center.x, 0.012, center.z + offset),
            ));
        }
        if offset.abs() < size.y * 0.5 {
            commands.spawn((
                ZoneSceneGeometry,
                Mesh3d(meshes.add(Cuboid::new(0.025, 0.012, size.y))),
                MeshMaterial3d(grid_material.clone()),
                Transform::from_xyz(center.x + offset, 0.012, center.z),
            ));
        }
    }
    for placement in &definition.entity_placements {
        let material = if view.selected_object.is_placement(&placement.id) {
            palette.selected_object.as_ref()
        } else {
            palette.object.as_ref()
        }
        .expect("zone scene palette is initialized")
        .clone();
        commands.spawn((
            ZoneSceneGeometry,
            ZoneSceneObject(SceneObjectId::EntityPlacement(placement.id.clone())),
            Mesh3d(meshes.add(Cuboid::new(0.8, 1.0, 0.8))),
            MeshMaterial3d(material),
            Transform {
                translation: Vec3::new(
                    placement.position.x,
                    placement.position.y + 0.5,
                    placement.position.z,
                ),
                rotation: Quat::from_rotation_y(-placement.yaw_degrees.to_radians()),
                ..default()
            },
        ));
    }
    *rendered_state = Some(state);
}

#[allow(clippy::needless_pass_by_value)]
fn sync_zone_scene_selection(
    view: Res<ZoneSceneView>,
    palette: Res<ZoneScenePalette>,
    mut objects: Query<(&ZoneSceneObject, &mut MeshMaterial3d<StandardMaterial>)>,
    mut rendered_selection: Local<Option<SceneObjectId>>,
) {
    if rendered_selection.as_ref() == Some(&view.selected_object) {
        return;
    }
    let floor_material = palette.floor.clone();
    let selected_floor_material = palette.selected_floor.clone();
    let selected_material = palette.selected_object.clone();
    let normal_material = palette.object.clone();
    for (object, mut material) in &mut objects {
        let selected = object.0 == view.selected_object;
        let next = if object.0 == SceneObjectId::ZoneFloor {
            if selected {
                selected_floor_material.clone()
            } else {
                floor_material.clone()
            }
        } else if selected {
            selected_material.clone()
        } else {
            normal_material.clone()
        };
        if let Some(next) = next {
            material.0 = next;
        }
    }
    *rendered_selection = Some(view.selected_object.clone());
}

#[allow(clippy::needless_pass_by_value)]
fn update_zone_scene_camera(
    mut view: ResMut<ZoneSceneView>,
    mut scene_entities: Query<(
        &mut Transform,
        Option<&ZoneSceneObject>,
        Option<&ZoneSceneCamera>,
    )>,
) {
    if view.focus_selected {
        if let Some(target) = scene_entities.iter().find_map(|(transform, object, _)| {
            object
                .is_some_and(|object| object.0 == view.selected_object)
                .then_some(transform.translation)
        }) {
            view.target = target;
        }
        view.focus_selected = false;
    }
    let offset = zone_camera_offset(view.yaw, view.pitch, view.distance);
    for (mut transform, _, camera) in &mut scene_entities {
        if camera.is_some() {
            transform.translation = view.target + offset;
            transform.look_at(view.target, Vec3::Y);
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

#[allow(clippy::too_many_arguments)]
fn draw_menu_bar(
    ui: &mut egui::Ui,
    session: &mut EditorSession,
    project_dialog: &mut ProjectDialog,
    notice: &mut Notice,
    preview: &mut PreviewMode,
    surface: &mut EditorSurface,
    panels: &mut EditorPanels,
    zone_view: &mut ZoneSceneView,
    watch: &mut ContentWatch,
) {
    egui::Panel::top("editor-menu-bar").show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Open Project…  Ctrl+O").clicked() {
                    if session.is_dirty() {
                        *notice =
                            Notice("Save your changes before opening another project.".into());
                    } else {
                        project_dialog.path = session.project_root().display().to_string();
                        project_dialog.open = true;
                    }
                }
                if ui
                    .add_enabled(
                        session.is_dirty() && !watch.changed_on_disk,
                        egui::Button::new("Save Project  Ctrl+S"),
                    )
                    .clicked()
                {
                    *notice = Notice(save_session(session, watch, "Saved project content."));
                }
                if ui.button("Show Project Folder").clicked() {
                    *notice = Notice(match open_project_folder(session.project_root()) {
                        Ok(()) => "Opened the project folder.".into(),
                        Err(error) => format!("Could not open project folder: {error}"),
                    });
                }
                if ui
                    .add_enabled(
                        watch.changed_on_disk && !session.is_dirty(),
                        egui::Button::new("Reload External Changes"),
                    )
                    .clicked()
                {
                    *notice = Notice(match session.reload_external_changes() {
                        Ok(()) => {
                            watch.mark_resolved();
                            "Reloaded project content.".into()
                        }
                        Err(error) => format!("Reload failed: {error}"),
                    });
                }
                if ui.button("Validate Content  V").clicked() {
                    let count = session.validate().len();
                    *notice = Notice(format!("Validation finished: {count} diagnostic(s)."));
                }
                ui.separator();
                if ui.button("Exit").clicked() {
                    if session.is_dirty() {
                        *notice = Notice(
                            "Save your changes before exiting; recovery autosave is also active."
                                .into(),
                        );
                    } else {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            });
            ui.menu_button("Edit", |ui| {
                if ui
                    .add_enabled(!preview.0, egui::Button::new("Undo  Ctrl+Z"))
                    .clicked()
                {
                    *notice = Notice(if session.undo() {
                        "Undo.".into()
                    } else {
                        "Nothing to undo.".into()
                    });
                }
                if ui
                    .add_enabled(!preview.0, egui::Button::new("Redo  Ctrl+Y"))
                    .clicked()
                {
                    *notice = Notice(if session.redo() {
                        "Redo.".into()
                    } else {
                        "Nothing to redo.".into()
                    });
                }
            });
            ui.menu_button("View", |ui| {
                ui.label("Authoring Surface");
                ui.selectable_value(surface, EditorSurface::WorldAtlas, "World Atlas · 2D");
                ui.selectable_value(surface, EditorSurface::ZoneScene, "Zone Scene · 3D");
                ui.separator();
                ui.checkbox(&mut panels.hierarchy, "Hierarchy Panel");
                ui.checkbox(&mut panels.inspector, "Inspector Panel");
                ui.checkbox(&mut preview.0, "Read-only Preview");
                if ui.button("Focus Selected Object  F").clicked() {
                    zone_view.focus_selected = true;
                }
            });
        });
    });
}

#[allow(clippy::too_many_arguments)]
fn draw_toolbar(
    ui: &mut egui::Ui,
    session: &mut EditorSession,
    notice: &mut Notice,
    preview: &mut PreviewMode,
    surface: &mut EditorSurface,
    play: &mut PlayPreview,
    watch: &mut ContentWatch,
) {
    egui::Panel::top("editor-toolbar").show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.selectable_value(surface, EditorSurface::WorldAtlas, "Atlas");
            ui.selectable_value(surface, EditorSurface::ZoneScene, "Zone Scene");
            ui.separator();
            if ui
                .add_enabled(
                    session.is_dirty() && !watch.changed_on_disk,
                    egui::Button::new("Save"),
                )
                .clicked()
            {
                *notice = Notice(save_session(session, watch, "Saved project content."));
            }
            if ui
                .add_enabled(!preview.0, egui::Button::new("Undo"))
                .clicked()
            {
                *notice = Notice(if session.undo() {
                    "Undo.".into()
                } else {
                    "Nothing to undo.".into()
                });
            }
            if ui
                .add_enabled(!preview.0, egui::Button::new("Redo"))
                .clicked()
            {
                *notice = Notice(if session.redo() {
                    "Redo.".into()
                } else {
                    "Nothing to redo.".into()
                });
            }
            if ui.button("Validate").clicked() {
                let count = session.validate().len();
                *notice = Notice(format!("Validation finished: {count} diagnostic(s)."));
            }
            ui.separator();
            if ui
                .selectable_label(preview.0, if preview.0 { "Preview" } else { "Edit" })
                .clicked()
            {
                preview.0 = !preview.0;
            }
            let play_label = if play.child.lock().is_ok_and(|child| child.is_some()) {
                "Stop Play"
            } else {
                "Play"
            };
            if ui.button(play_label).clicked() {
                toggle_play_preview(session, play, notice, watch);
            }
        });
    });
}

fn draw_hierarchy_panel(
    ui: &mut egui::Ui,
    session: &mut EditorSession,
    surface: &mut EditorSurface,
    zone_view: &mut ZoneSceneView,
    map_ids: &[DefinitionId],
    placements: &[(DefinitionId, MapPosition)],
    scene_objects: &[(String, DefinitionId)],
) {
    egui::Panel::left("editor-hierarchy")
        .default_size(250.0)
        .size_range(190.0..=420.0)
        .resizable(true)
        .show(ui, |ui| {
            ui.heading("Hierarchy");
            let mut selected_map = session.selected_map().cloned();
            egui::ComboBox::from_id_salt("hierarchy-map-picker")
                .selected_text(
                    selected_map
                        .as_ref()
                        .map_or_else(|| "Select a map".into(), ToString::to_string),
                )
                .show_ui(ui, |ui| {
                    for map_id in map_ids {
                        ui.selectable_value(
                            &mut selected_map,
                            Some(map_id.clone()),
                            map_id.as_str(),
                        );
                    }
                });
            if selected_map != session.selected_map().cloned()
                && let Some(map_id) = selected_map
            {
                let _ = session.select_map(&map_id);
                zone_view.selected_object = SceneObjectId::ZoneFloor;
            }

            ui.separator();
            ui.label("Zones");
            if placements.is_empty() {
                ui.label("No zones are placed on this map.");
            }
            for (zone, position) in placements {
                let selected = session.selected_zone() == Some(zone);
                if ui
                    .selectable_label(
                        selected,
                        format!("▸ {zone}  ·  ({}, {})", position.x, position.y),
                    )
                    .clicked()
                {
                    let _ = session.select_zone(zone);
                    zone_view.selected_object = SceneObjectId::ZoneFloor;
                }
            }

            if *surface == EditorSurface::ZoneScene {
                ui.separator();
                if let Some(zone_id) = session.selected_zone().cloned() {
                    egui::CollapsingHeader::new(zone_id.to_string())
                        .default_open(true)
                        .show(ui, |ui| {
                            if ui
                                .selectable_label(
                                    zone_view.selected_object == SceneObjectId::ZoneFloor,
                                    "Floor · placeholder",
                                )
                                .clicked()
                            {
                                zone_view.selected_object = SceneObjectId::ZoneFloor;
                            }
                            for (id, definition) in scene_objects {
                                if ui
                                    .selectable_label(
                                        zone_view.selected_object.is_placement(id),
                                        format!("{id}  ·  {definition}"),
                                    )
                                    .clicked()
                                {
                                    zone_view.selected_object =
                                        SceneObjectId::EntityPlacement(id.clone());
                                }
                            }
                        });
                } else {
                    ui.label("Select a zone to view its scene objects.");
                }
            }
        });
}

fn open_project_folder(path: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    let mut command = Command::new("explorer");
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = Command::new("xdg-open");

    command.arg(path).spawn().map(|_| ())
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::type_complexity,
    reason = "Coordinates the editor's dockable panels and shared Bevy session resources."
)]
fn draw_editor_ui(
    mut contexts: EguiContexts,
    mut session: ResMut<Session>,
    mut notice: ResMut<Notice>,
    mut preview: ResMut<PreviewMode>,
    mut project_dialog: ResMut<ProjectDialog>,
    mut surface: ResMut<EditorSurface>,
    mut panels: ResMut<EditorPanels>,
    mut zone_view: ResMut<ZoneSceneView>,
    mut scene_viewport: ResMut<SceneViewport>,
    mut transform_draft: Local<PlacementTransformDraft>,
    mut play: ResMut<PlayPreview>,
    mut watch: ResMut<ContentWatch>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut editor_cameras: Query<&mut Camera, Or<(With<EditorCamera>, With<ZoneSceneCamera>)>>,
) -> Result {
    let context = contexts.ctx_mut()?;
    let mut viewport_ui = egui::Ui::new(
        context.clone(),
        "lynx-editor-root".into(),
        egui::UiBuilder::new()
            .layer_id(egui::LayerId::background())
            .max_rect(context.viewport_rect()),
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
    let scene_objects: Vec<_> = session
        .0
        .selected_zone_definition()
        .map(|definition| {
            definition
                .entity_placements
                .iter()
                .map(|placement| (placement.id.clone(), placement.definition.clone()))
                .collect()
        })
        .unwrap_or_default();
    let selected_scene_placement = match &zone_view.selected_object {
        SceneObjectId::ZoneFloor => None,
        SceneObjectId::EntityPlacement(id) => session
            .0
            .selected_zone_definition()
            .and_then(|definition| {
                definition
                    .entity_placements
                    .iter()
                    .find(|placement| &placement.id == id)
            })
            .map(|placement| {
                (
                    placement.id.clone(),
                    placement.definition.clone(),
                    placement.position,
                    placement.yaw_degrees,
                )
            }),
    };
    let draft_selection = selected_scene_placement.as_ref().and_then(|(id, _, _, _)| {
        session
            .0
            .selected_zone()
            .cloned()
            .map(|zone_id| (zone_id, id.clone()))
    });
    if transform_draft.selection != draft_selection
        || transform_draft.session_revision != session.0.revision()
    {
        transform_draft.selection.clone_from(&draft_selection);
        if let Some((_, _, position, yaw_degrees)) = &selected_scene_placement {
            transform_draft.position = [position.x, position.y, position.z];
            transform_draft.yaw_degrees = *yaw_degrees;
        }
        transform_draft.session_revision = session.0.revision();
    }
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

    draw_menu_bar(
        &mut viewport_ui,
        &mut session.0,
        &mut project_dialog,
        &mut notice,
        &mut preview,
        &mut surface,
        &mut panels,
        &mut zone_view,
        &mut watch,
    );
    draw_toolbar(
        &mut viewport_ui,
        &mut session.0,
        &mut notice,
        &mut preview,
        &mut surface,
        &mut play,
        &mut watch,
    );
    egui::Panel::bottom("editor-status-bar").show(&mut viewport_ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(if is_dirty {
                "● Unsaved changes"
            } else {
                "✓ All changes saved"
            });
            ui.separator();
            ui.label(if *surface == EditorSurface::WorldAtlas {
                "World Atlas · 2D"
            } else {
                "Zone Scene · 3D"
            });
            ui.separator();
            if watch.changed_on_disk {
                ui.colored_label(egui::Color32::YELLOW, "External changes detected");
            } else {
                ui.label("Ready");
            }
            if !notice.0.is_empty() {
                ui.separator();
                ui.label(&notice.0);
            }
        });
    });

    if panels.hierarchy {
        draw_hierarchy_panel(
            &mut viewport_ui,
            &mut session.0,
            &mut surface,
            &mut zone_view,
            &map_ids,
            &placements,
            &scene_objects,
        );
    }

    if panels.inspector {
        egui::Panel::right("editor-inspector")
            .default_size(330.0)
            .size_range(270.0..=520.0)
            .resizable(true)
            .show(&mut viewport_ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.heading("Inspector");
                ui.label(&project_root);

                if watch.changed_on_disk {
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        "Project files changed outside the editor.",
                    );
                    if session.0.is_dirty() {
                        ui.label("Your in-memory edits were preserved. Saving overwrites the externally changed authored files with your edits:");
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

                ui.separator();
                ui.heading("Selection");
                if let Some(zone) = session.0.selected_zone() {
                    ui.label(format!("Zone: {zone}"));
                } else {
                    ui.label("No zone selected.");
                }
                if *surface == EditorSurface::WorldAtlas {
                    if let Some(position) = session
                        .0
                        .map()
                        .and_then(|map| {
                            map.zones
                                .iter()
                                .find(|zone| Some(&zone.zone) == session.0.selected_zone())
                        })
                        .map(|zone| zone.position)
                    {
                        ui.label(format!(
                            "Atlas cell: ({}, {})",
                            position.x, position.y
                        ));
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(!preview.0, egui::Button::new("←"))
                            .clicked()
                        {
                            apply_edit(&mut session.0, &mut notice, |session| {
                                session.move_selected(-1, 0)
                            });
                        }
                        if ui
                            .add_enabled(!preview.0, egui::Button::new("→"))
                            .clicked()
                        {
                            apply_edit(&mut session.0, &mut notice, |session| {
                                session.move_selected(1, 0)
                            });
                        }
                        if ui
                            .add_enabled(!preview.0, egui::Button::new("↑"))
                            .clicked()
                        {
                            apply_edit(&mut session.0, &mut notice, |session| {
                                session.move_selected(0, -1)
                            });
                        }
                        if ui
                            .add_enabled(!preview.0, egui::Button::new("↓"))
                            .clicked()
                        {
                            apply_edit(&mut session.0, &mut notice, |session| {
                                session.move_selected(0, 1)
                            });
                        }
                    });
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                !preview.0,
                                egui::Button::new("Place next zone (N)"),
                            )
                            .clicked()
                        {
                            place_next_zone(&mut session.0, &mut notice);
                        }
                        if ui
                            .add_enabled(
                                !preview.0,
                                egui::Button::new("Delete selected"),
                            )
                            .clicked()
                        {
                            apply_edit(&mut session.0, &mut notice, EditorSession::delete_selected);
                        }
                    });
                } else {
                    ui.horizontal(|ui| {
                        if ui.button("Focus (F)").clicked() {
                            zone_view.focus_selected = true;
                        }
                    });
                    ui.strong(format!(
                        "Scene object · {}",
                        zone_view.selected_object.label()
                    ));
                    if let Some((id, definition, _, _)) = &selected_scene_placement {
                        ui.label(format!("Placement: {id}"));
                        ui.label(format!("Definition: {definition}"));
                        ui.horizontal(|ui| {
                            ui.label("Position");
                            for axis in &mut transform_draft.position {
                                ui.add(egui::DragValue::new(axis).speed(0.1).range(-1000.0..=1000.0));
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Yaw");
                            ui.add(
                                egui::DragValue::new(&mut transform_draft.yaw_degrees)
                                    .speed(1.0)
                                    .suffix("°"),
                            );
                        });
                        if ui
                            .add_enabled(
                                !preview.0,
                                egui::Button::new("Apply transform"),
                            )
                            .clicked()
                        {
                            let result = session.0.set_selected_entity_placement_transform(
                                id,
                                engine_core::math::Vec3::new(
                                    transform_draft.position[0],
                                    transform_draft.position[1],
                                    transform_draft.position[2],
                                ),
                                transform_draft.yaw_degrees,
                            );
                            match result {
                                Ok(()) => {
                                    transform_draft.session_revision = session.0.revision();
                                    notice.0 = format!("Updated placement {id}.");
                                }
                                Err(error) => notice.0 = format!("Transform was not applied: {error}"),
                            }
                        }
                    } else {
                        ui.label("Zone floor placeholder has no authored transform.");
                    }
                }

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
            });
            });
    }
    let scene_rect = egui::CentralPanel::no_frame()
        .frame(egui::Frame::NONE)
        .show(&mut viewport_ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(if *surface == EditorSurface::WorldAtlas {
                    "Scene · World Atlas"
                } else {
                    "Scene · Zone View"
                });
                ui.separator();
                ui.label(if *surface == EditorSurface::WorldAtlas {
                    "2D topology"
                } else {
                    "3D authored placements"
                });
            });
            ui.add_space(4.0);
            ui.available_rect_before_wrap()
        })
        .inner;
    scene_viewport.0 = Some(scene_rect);
    if let Ok(window) = windows.single() {
        let scale = window.scale_factor();
        let physical_position = (Vec2::new(scene_rect.min.x, scene_rect.min.y) * scale)
            .round()
            .as_uvec2();
        let physical_size = (Vec2::new(scene_rect.width(), scene_rect.height()) * scale)
            .round()
            .as_uvec2();
        if physical_size.x > 0 && physical_size.y > 0 {
            for mut camera in &mut editor_cameras {
                camera.viewport = Some(Viewport {
                    physical_position,
                    physical_size,
                    ..default()
                });
            }
        }
    }

    if project_dialog.open {
        let mut open_clicked = false;
        let mut cancel_clicked = false;
        egui::Window::new("Open Project")
            .id(egui::Id::new("lynx-open-project"))
            .collapsible(false)
            .resizable(false)
            .default_width(480.0)
            .show(context, |ui| {
                ui.label("Enter the project folder containing a content/ directory:");
                ui.text_edit_singleline(&mut project_dialog.path);
                ui.horizontal(|ui| {
                    open_clicked = ui
                        .add_enabled(
                            !project_dialog.path.trim().is_empty(),
                            egui::Button::new("Open"),
                        )
                        .clicked();
                    cancel_clicked = ui.button("Cancel").clicked();
                });
            });
        if open_clicked && session.0.is_dirty() {
            notice.0 = "Save your changes before opening another project.".into();
            project_dialog.open = false;
        } else if open_clicked {
            match EditorSession::open(project_dialog.path.trim()) {
                Ok(opened) => {
                    let project_root = opened.project_root().to_path_buf();
                    let content_root = project_root.join("content");
                    session.0 = opened;
                    let watcher_error = match ContentWatch::new(&content_root) {
                        Ok(next_watch) => {
                            *watch = next_watch;
                            None
                        }
                        Err(error) => {
                            *watch = ContentWatch::disabled();
                            Some(error)
                        }
                    };
                    *surface = EditorSurface::WorldAtlas;
                    *preview = PreviewMode::default();
                    *zone_view = ZoneSceneView::default();
                    transform_draft.selection = None;
                    transform_draft.session_revision = u64::MAX;
                    project_dialog.open = false;
                    notice.0 = watcher_error.map_or_else(
                        || format!("Opened project {}.", project_root.display()),
                        |error| format!("Project opened, but file watching failed: {error}"),
                    );
                }
                Err(error) => {
                    notice.0 = format!("Could not open project: {error}");
                }
            }
        } else if cancel_clicked {
            project_dialog.open = false;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::math::Vec3 as AuthoredVec3;

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

    #[test]
    fn empty_zone_scene_uses_a_visible_placeholder_footprint() {
        let (center, size) = zone_scene_layout_from_placements(&[]);
        assert_eq!(center, Vec3::ZERO);
        assert_eq!(size, Vec2::splat(12.0));
    }

    #[test]
    fn zone_scene_centers_authored_placements_and_pads_the_floor() {
        let placements = [
            ZoneEntityPlacement {
                id: "west".into(),
                definition: DefinitionId::new("entity.west").unwrap(),
                position: AuthoredVec3::new(-2.0, 0.0, 1.0),
                yaw_degrees: 0.0,
            },
            ZoneEntityPlacement {
                id: "east".into(),
                definition: DefinitionId::new("entity.east").unwrap(),
                position: AuthoredVec3::new(4.0, 0.0, 7.0),
                yaw_degrees: 0.0,
            },
        ];
        let (center, size) = zone_scene_layout_from_placements(&placements);
        assert_eq!(center, Vec3::new(1.0, 0.0, 4.0));
        assert_eq!(size, Vec2::new(18.0, 18.0));
    }

    #[test]
    fn atlas_and_zone_scene_surfaces_are_mutually_exclusive() {
        assert!(surface_is_active(
            EditorSurface::WorldAtlas,
            EditorSurface::WorldAtlas
        ));
        assert!(!surface_is_active(
            EditorSurface::WorldAtlas,
            EditorSurface::ZoneScene
        ));
        assert!(surface_is_active(
            EditorSurface::ZoneScene,
            EditorSurface::ZoneScene
        ));
        assert!(!surface_is_active(
            EditorSurface::ZoneScene,
            EditorSurface::WorldAtlas
        ));
    }

    #[test]
    fn scene_viewport_bounds_exclude_menu_and_docked_panels() {
        let viewport = SceneViewport(Some(egui::Rect::from_min_max(
            egui::pos2(250.0, 50.0),
            egui::pos2(850.0, 700.0),
        )));

        assert!(viewport.contains(Vec2::new(500.0, 300.0)));
        assert!(!viewport.contains(Vec2::new(120.0, 300.0)));
        assert!(!viewport.contains(Vec2::new(900.0, 300.0)));
        assert!(!viewport.contains(Vec2::new(500.0, 20.0)));
    }

    #[test]
    fn zone_floor_selection_cannot_collide_with_an_authored_placement_id() {
        assert_ne!(
            SceneObjectId::ZoneFloor,
            SceneObjectId::EntityPlacement("__lynx_zone_floor__".into())
        );
    }

    #[test]
    fn zone_scene_camera_orbit_and_zoom_are_bounded() {
        let mut view = ZoneSceneView::default();
        orbit_zone_camera(&mut view, Vec2::new(20.0, 1000.0));
        assert_eq!(view.pitch, 1.4);
        orbit_zone_camera(&mut view, Vec2::new(0.0, -1000.0));
        assert_eq!(view.pitch, 0.15);

        zoom_zone_camera(&mut view, 100.0);
        assert_eq!(view.distance, MIN_ZONE_CAMERA_DISTANCE);
        zoom_zone_camera(&mut view, -100.0);
        assert_eq!(view.distance, MAX_ZONE_CAMERA_DISTANCE);
        assert!(
            (zone_camera_offset(view.yaw, view.pitch, view.distance).length() - view.distance)
                .abs()
                < 1e-4
        );
    }

    #[test]
    fn zone_scene_pan_scales_with_camera_distance() {
        let mut view = ZoneSceneView {
            distance: 20.0,
            ..default()
        };
        pan_zone_camera(&mut view, Vec2::new(50.0, -25.0), 100.0);
        assert_eq!(view.target, Vec3::new(-10.0, 0.0, -5.0));
    }
}
