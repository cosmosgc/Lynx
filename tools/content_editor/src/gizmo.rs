//! Viewport transforms are previews until release: one drag is one session edit.
use std::path::PathBuf;

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::{EguiContexts, egui};
use engine_core::id::DefinitionId;
use lynx_editor::EditorSession;

use super::{
    EditorSurface, Notice, PreviewMode, ProjectDialog, SceneObjectId, SceneViewport, Session,
    TransformSnap, ZoneSceneCamera, ZoneSceneObject, ZoneSceneView,
};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Tool {
    #[default]
    Translate,
    Rotate,
}

#[derive(Clone)]
struct Drag {
    zone: DefinitionId,
    id: String,
    revision: u64,
    project: PathBuf,
    start: egui::Pos2,
    origin: egui::Pos2,
    axis: usize,
    screen_axis: egui::Vec2,
    position: Vec3,
    yaw: f32,
    candidate_position: Vec3,
    candidate_yaw: f32,
}

#[derive(Resource, Default)]
pub(super) struct GizmoState {
    tool: Tool,
    drag: Option<Drag>,
    handles: Vec<egui::Rect>,
    snap: bool,
    logical_to_points: f32,
}

impl GizmoState {
    pub(super) fn captures(&self, cursor: Vec2) -> bool {
        self.drag.is_some()
            || self
                .handles
                .iter()
                .any(|rect| rect.contains(egui::pos2(cursor.x, cursor.y) * self.logical_to_points))
    }
}

fn snapped(value: f32, step: f32, enabled: bool) -> f32 {
    if enabled {
        (value / step).round() * step
    } else {
        value
    }
}

impl Drag {
    fn update(&mut self, pointer: egui::Pos2, tool: Tool, snap: bool, steps: &TransformSnap) {
        self.candidate_position = self.position;
        self.candidate_yaw = self.yaw;
        if pointer.distance(self.start) < 2.0 {
            return;
        }
        match tool {
            Tool::Translate => {
                let delta = (pointer - self.start).dot(self.screen_axis)
                    / self.screen_axis.length_sq().max(0.001);
                self.candidate_position[self.axis] =
                    snapped(self.position[self.axis] + delta, steps.position_step, snap);
            }
            Tool::Rotate => {
                let start = self.start - self.origin;
                let current = pointer - self.origin;
                let angle = (current.angle() - start.angle() + std::f32::consts::PI)
                    .rem_euclid(std::f32::consts::TAU)
                    - std::f32::consts::PI;
                self.candidate_yaw = snapped(self.yaw + angle.to_degrees(), steps.yaw_step, snap);
            }
        }
    }

    fn valid(&self, session: &EditorSession, view: &ZoneSceneView) -> bool {
        session.revision() == self.revision
            && session.project_root() == self.project
            && session.selected_zone() == Some(&self.zone)
            && view.selected_object.is_placement(&self.id)
    }

    fn commit(&self, session: &mut EditorSession) -> Result<(), lynx_editor::SessionError> {
        session.set_selected_entity_placement_transform(
            &self.id,
            engine_core::math::Vec3::new(
                self.candidate_position.x,
                self.candidate_position.y,
                self.candidate_position.z,
            ),
            self.candidate_yaw,
        )
    }
}

#[allow(clippy::needless_pass_by_value)]
pub(super) fn preview(
    session: Res<Session>,
    state: Res<GizmoState>,
    view: Res<ZoneSceneView>,
    mut objects: Query<(&ZoneSceneObject, &mut Transform)>,
    mut last: Local<Option<String>>,
) {
    // Restore only the previous preview on cancellation/selection change.
    let active = state
        .drag
        .as_ref()
        .filter(|drag| drag.valid(&session.0, &view));
    if active.is_none() && last.is_none() {
        return;
    }
    let Some(zone) = session.0.selected_zone_definition() else {
        return;
    };
    for (object, mut transform) in &mut objects {
        let SceneObjectId::EntityPlacement(id) = &object.0 else {
            continue;
        };
        if active.is_none_or(|drag| &drag.id != id) && last.as_ref() != Some(id) {
            continue;
        }
        let Some(placement) = zone
            .entity_placements
            .iter()
            .find(|placement| &placement.id == id)
        else {
            continue;
        };
        let (position, yaw) = state
            .drag
            .as_ref()
            .filter(|drag| &drag.id == id && drag.valid(&session.0, &view))
            .map_or(
                (
                    Vec3::new(
                        placement.position.x,
                        placement.position.y,
                        placement.position.z,
                    ),
                    placement.yaw_degrees,
                ),
                |drag| (drag.candidate_position, drag.candidate_yaw),
            );
        transform.translation = position + Vec3::Y * 0.5;
        transform.rotation = Quat::from_rotation_y(-yaw.to_radians());
    }
    *last = active.map(|drag| drag.id.clone());
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::needless_pass_by_value
)]
pub(super) fn draw(
    mut contexts: EguiContexts,
    mut state: ResMut<GizmoState>,
    mut session: ResMut<Session>,
    view: Res<ZoneSceneView>,
    surface: Res<EditorSurface>,
    viewport: Res<SceneViewport>,
    preview_mode: Res<PreviewMode>,
    dialog: Res<ProjectDialog>,
    steps: Res<TransformSnap>,
    cameras: Query<(&Camera, &GlobalTransform), With<ZoneSceneCamera>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut notice: ResMut<Notice>,
) -> Result {
    state.handles.clear();
    if *surface != EditorSurface::ZoneScene
        || preview_mode.0
        || dialog.open
        || !view.additional.is_empty()
    {
        state.drag = None;
        return Ok(());
    }
    let (Some(rect), Ok((camera, camera_transform)), Ok(window)) =
        (viewport.0, cameras.single(), windows.single())
    else {
        return Ok(());
    };
    let ctx = contexts.ctx_mut()?;
    let scale = window.scale_factor() / ctx.pixels_per_point();
    state.logical_to_points = scale;
    if state
        .drag
        .as_ref()
        .is_some_and(|drag| !drag.valid(&session.0, &view))
        || ctx.input(|input| input.key_pressed(egui::Key::Escape))
        || !window.focused
    {
        state.drag = None;
    }
    let SceneObjectId::EntityPlacement(id) = &view.selected_object else {
        return Ok(());
    };
    let Some(zone) = session.0.selected_zone_definition() else {
        return Ok(());
    };
    let Some(placement) = zone
        .entity_placements
        .iter()
        .find(|placement| &placement.id == id)
    else {
        return Ok(());
    };
    let position = Vec3::new(
        placement.position.x,
        placement.position.y,
        placement.position.z,
    );
    let yaw = placement.yaw_degrees;
    let zone_id = zone.id.clone();
    let project = session.0.project_root().to_path_buf();
    let project_point = |point| {
        camera
            .world_to_viewport(camera_transform, point)
            .ok()
            .map(|point| egui::pos2(point.x * scale, point.y * scale))
    };
    let Some(origin) = project_point(position + Vec3::Y * 0.5) else {
        return Ok(());
    };
    let toolbar = egui::Area::new("transform-tools".into())
        .fixed_pos(rect.min + egui::vec2(8.0, 8.0))
        .show(ctx, |ui| {
            ui.add_enabled_ui(state.drag.is_none(), |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut state.tool, Tool::Translate, "Move");
                    ui.selectable_value(&mut state.tool, Tool::Rotate, "Rotate");
                    ui.checkbox(&mut state.snap, "Snap");
                    ui.label("Esc: cancel");
                });
            });
        });
    state.handles.push(toolbar.response.rect);
    let painter = ctx
        .layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            "transform-handles".into(),
        ))
        .with_clip_rect(rect);
    let tool = state.tool;
    let mut handles = Vec::new();
    if tool == Tool::Translate {
        for (axis, direction, color, label) in [
            (0, Vec3::X, egui::Color32::RED, "X"),
            (1, Vec3::Y, egui::Color32::GREEN, "Y"),
            (2, Vec3::Z, egui::Color32::LIGHT_BLUE, "Z"),
        ] {
            let Some(end) = project_point(position + Vec3::Y * 0.5 + direction) else {
                continue;
            };
            let screen_axis = end - origin;
            if screen_axis.length() < 2.0 {
                continue;
            }
            let handle = origin + screen_axis.normalized() * 70.0;
            painter.arrow(origin, handle - origin, egui::Stroke::new(3.0, color));
            painter.circle_filled(handle, 7.0, color);
            painter.text(
                handle + egui::vec2(10.0, 0.0),
                egui::Align2::LEFT_CENTER,
                label,
                egui::FontId::proportional(14.0),
                color,
            );
            handles.push((
                egui::Rect::from_center_size(handle, egui::vec2(22.0, 22.0)),
                axis,
                screen_axis,
            ));
        }
    } else {
        painter.circle_stroke(origin, 65.0, egui::Stroke::new(3.0, egui::Color32::YELLOW));
        // Discrete hit regions follow the ring, leaving its interior selectable.
        for index in 0_u16..48 {
            let angle = f32::from(index) * std::f32::consts::TAU / 48.0;
            let handle = origin + egui::vec2(angle.cos(), angle.sin()) * 65.0;
            handles.push((
                egui::Rect::from_center_size(handle, egui::vec2(14.0, 14.0)),
                0,
                egui::Vec2::ZERO,
            ));
        }
    }
    state
        .handles
        .extend(handles.iter().map(|(hit, _, _)| hit.intersect(rect)));
    let cursor = ctx.input(|input| input.pointer.interact_pos());
    if let Some(pointer) = cursor {
        if state.drag.is_none()
            && rect.contains(pointer)
            && ctx.input(|input| input.pointer.primary_pressed())
            && let Some((_, axis, screen_axis)) =
                handles.iter().find(|(hit, _, _)| hit.contains(pointer))
        {
            state.drag = Some(Drag {
                zone: zone_id,
                id: id.clone(),
                revision: session.0.revision(),
                project,
                start: pointer,
                origin,
                axis: *axis,
                screen_axis: *screen_axis,
                position,
                yaw,
                candidate_position: position,
                candidate_yaw: yaw,
            });
        }
        let snap = state.snap;
        if let Some(drag) = state.drag.as_mut() {
            drag.update(pointer, tool, snap, &steps);
            painter.text(
                rect.left_bottom() + egui::vec2(8.0, -8.0),
                egui::Align2::LEFT_BOTTOM,
                format!(
                    "Position {:?} · yaw {:.1}°",
                    drag.candidate_position, drag.candidate_yaw
                ),
                egui::FontId::proportional(14.0),
                egui::Color32::WHITE,
            );
        }
    }
    if ctx.input(|input| input.pointer.primary_released())
        && let Some(drag) = state.drag.take()
    {
        notice.0 = match drag.commit(&mut session.0) {
            Ok(()) => "Transform applied. Ctrl+Z to undo.".into(),
            Err(error) => format!("Transform rejected: {error}"),
        };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn copy_content(source: &Path, destination: &Path) {
        std::fs::create_dir_all(destination).unwrap();
        for entry in std::fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let target = destination.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_content(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    #[test]
    fn drag_previews_commit_once_and_round_trip_with_snapping() {
        let temporary = tempfile::tempdir().unwrap();
        copy_content(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content-editor-demo/content"),
            &temporary.path().join("content"),
        );
        let mut session = EditorSession::open(temporary.path()).unwrap();
        let zone = session.selected_zone_definition().unwrap();
        let placement = zone.entity_placements[0].clone();
        let original = zone.clone();
        let mut drag = Drag {
            zone: zone.id.clone(),
            id: placement.id.clone(),
            revision: session.revision(),
            project: session.project_root().to_path_buf(),
            start: egui::pos2(100.0, 100.0),
            origin: egui::pos2(50.0, 100.0),
            axis: 0,
            screen_axis: egui::vec2(50.0, 0.0),
            position: Vec3::new(
                placement.position.x,
                placement.position.y,
                placement.position.z,
            ),
            yaw: placement.yaw_degrees,
            candidate_position: Vec3::ZERO,
            candidate_yaw: 0.0,
        };
        let steps = TransformSnap {
            position_step: 0.5,
            yaw_step: 15.0,
        };
        for pointer in [110.0, 125.0, 150.0, 163.0] {
            drag.update(egui::pos2(pointer, 100.0), Tool::Translate, true, &steps);
        }
        // Preview and cancellation do not touch canonical data or history.
        assert_eq!(session.selected_zone_definition(), Some(&original));
        assert!(!session.is_dirty());
        assert!(!session.undo());
        assert_eq!(drag.candidate_position.x, placement.position.x + 1.5);
        drag.commit(&mut session).unwrap();
        let translated = session.selected_zone_definition().unwrap().clone();
        assert_eq!(session.revision(), drag.revision + 1);
        assert!(session.undo());
        assert_eq!(session.selected_zone_definition(), Some(&original));
        assert!(
            !session.undo(),
            "a drag must produce exactly one history entry"
        );
        assert!(session.redo());
        assert_eq!(session.selected_zone_definition(), Some(&translated));

        drag.position = drag.candidate_position;
        drag.update(egui::pos2(50.0, 150.0), Tool::Rotate, true, &steps);
        assert_eq!(drag.candidate_yaw, placement.yaw_degrees + 90.0);
        drag.commit(&mut session).unwrap();
        let rotated = session.selected_zone_definition().unwrap().clone();
        assert!(session.undo());
        assert_eq!(session.selected_zone_definition(), Some(&translated));
        assert!(session.redo());
        session.save().unwrap();
        let reopened = EditorSession::open(temporary.path()).unwrap();
        assert_eq!(reopened.selected_zone_definition(), Some(&rotated));
        assert!(!reopened.validation().has_errors());
        assert!(!reopened.is_dirty());
    }

    #[test]
    fn snapping_handles_negative_values_and_can_be_disabled() {
        assert_eq!(snapped(-1.3, 0.5, true), -1.5);
        assert_eq!(snapped(-1.3, 0.5, false), -1.3);
    }
}
