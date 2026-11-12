//! Immediate-mode window for content-defined entity panels.
//!
//! The server sends the panel title, read-only fields, and the actions the
//! client may offer. This module only presents that data; every action is a
//! correlated `ui.action` request and the server remains authoritative.

use bevy::input::keyboard::KeyCode;
use bevy::picking::Pickable;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_immediate::{
    BevyImmediatePlugin, Imm, ImmCtx,
    ui::{
        CapsUi, clicked::ImmUiClicked, disabled::ImmUiInteractionsDisabled,
        floating_window_plugin::FloatingWindowPlugin, look::ImmUiLook,
    },
};

use crate::interaction::{Panel, PanelAction};
use crate::online::{OnlineClient, OnlineStatus};
use crate::ui::chat::ChatComposer;
use crate::ui::window::{ImmediateWindowPlugin, WindowMemory, WindowSpec, label, show_window};

/// Stable window identity for remembered position and size.
pub const CONTENT_WINDOW_ID: u64 = 4;

const CONTENT_DEFAULT_SIZE: Vec2 = Vec2::new(380.0, 320.0);
const CONTENT_MIN_SIZE: Vec2 = Vec2::new(300.0, 200.0);
const CONTENT_EDGE_MARGIN: f32 = 16.0;

/// Adds the generic content-panel window to the online client.
#[derive(Debug)]
pub struct ContentPanelPlugin;

impl Plugin for ContentPanelPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(BevyImmediatePlugin::<CapsUi>::new());
        if !app.is_plugin_added::<ImmediateWindowPlugin>() {
            app.add_plugins(ImmediateWindowPlugin);
        }
        if !app.is_plugin_added::<FloatingWindowPlugin>() {
            app.add_plugins(FloatingWindowPlugin);
        }
        app.add_systems(
            Update,
            (keyboard_actions, draw)
                .chain()
                .after(crate::online::poll_online),
        );
    }
}

/// Send one of the first nine fixed server-advertised actions with its number
/// key. Dynamic parameters remain unavailable until a content input widget is
/// present; the server continues to authorize every submitted action.
#[allow(clippy::needless_pass_by_value)]
fn keyboard_actions(
    mut client: ResMut<OnlineClient>,
    keys: Res<ButtonInput<KeyCode>>,
    chat: Res<ChatComposer>,
    windows: Query<&Window, With<PrimaryWindow>>,
) {
    if chat.active
        || client.status() != OnlineStatus::Active
        || client.interaction().pending()
        || !windows.single().is_ok_and(|window| window.focused)
    {
        return;
    }
    let Some(panel) = client.interaction().panel().cloned() else {
        return;
    };
    if !is_content_panel(&panel) {
        return;
    }

    let shortcuts = [
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
        KeyCode::Digit8,
        KeyCode::Digit9,
    ];
    for (index, key) in shortcuts.into_iter().enumerate() {
        if !keys.just_pressed(key) {
            continue;
        }
        if let Some(action) = panel
            .actions
            .get(index)
            .filter(|action| action.allowed_params.is_empty())
        {
            let _ = client.submit_panel_action(&panel, action);
        }
        break;
    }
}

#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
fn draw(
    ctx: ImmCtx<CapsUi>,
    mut client: ResMut<OnlineClient>,
    chat: Res<ChatComposer>,
    memory: Res<WindowMemory>,
    windows: Query<&Window, With<PrimaryWindow>>,
) {
    ctx.build_immediate_root("online-content-panel-root")
        .ch_id("canvas")
        .on_spawn_insert(|| {
            (
                Node {
                    position_type: PositionType::Absolute,
                    width: percent(100),
                    height: percent(100),
                    ..default()
                },
                Pickable::IGNORE,
                GlobalZIndex(12),
            )
        })
        .add(|ui| {
            let Some(panel) = client.interaction().panel().cloned() else {
                return;
            };
            // The lockpick dialog has its own keypad presentation.
            if !is_content_panel(&panel) || client.status() != OnlineStatus::Active {
                return;
            }

            let stored = memory.get(CONTENT_WINDOW_ID);
            let size = stored.map_or(CONTENT_DEFAULT_SIZE, |state| state.size);
            let position = stored.map_or_else(
                || {
                    windows
                        .single()
                        .ok()
                        .map_or(Vec2::splat(CONTENT_EDGE_MARGIN), |window| {
                            Vec2::new(
                                (window.width() - size.x - CONTENT_EDGE_MARGIN)
                                    .max(CONTENT_EDGE_MARGIN),
                                CONTENT_EDGE_MARGIN,
                            )
                        })
                },
                |state| state.position,
            );
            let enabled = !chat.active
                && !client.interaction().pending()
                && windows.single().is_ok_and(|window| window.focused);
            let spec = WindowSpec {
                min_size: CONTENT_MIN_SIZE,
                position,
                size,
                subtitle: Some(panel.schema.to_string()),
                ..WindowSpec::new(CONTENT_WINDOW_ID, panel.title.clone(), CONTENT_DEFAULT_SIZE)
            };
            let fields = panel.fields.clone();
            let actions = panel.actions.clone();
            let status = client.interaction().status().to_owned();

            let outcome = show_window(ui.ch_id(("content-panel", panel.target)), spec, |ui| {
                if fields.is_empty() {
                    label(ui, "empty", 13.0, "Sem dados disponíveis.");
                }
                for (index, field) in fields.iter().enumerate() {
                    label(
                        ui,
                        &format!("content-field-{index}"),
                        14.0,
                        format!("{}: {}", field.label, field.value),
                    );
                }
                if !actions.is_empty() {
                    ui.ch_id("content-actions")
                        .on_spawn_insert(|| {
                            (Node {
                                display: Display::Flex,
                                flex_direction: FlexDirection::Row,
                                flex_wrap: FlexWrap::Wrap,
                                column_gap: px(6),
                                row_gap: px(6),
                                margin: UiRect::top(px(4)),
                                ..default()
                            },)
                        })
                        .add(|ui| {
                            for (index, action) in actions.iter().enumerate() {
                                let action_enabled = enabled && action.allowed_params.is_empty();
                                let shortcut = (index < 9 && action.allowed_params.is_empty())
                                    .then_some(index + 1);
                                if action_button(ui, index, action, action_enabled, shortcut)
                                    && client.submit_panel_action(&panel, action).is_ok()
                                {
                                    // The next poll consumes the correlated result.
                                }
                            }
                        });
                }
                label(ui, "content-status", 13.0, status);
            });
            if outcome.close_requested {
                let _ = client.close_panel();
            }
        });
}

fn action_button(
    ui: &mut Imm<CapsUi>,
    index: usize,
    action: &PanelAction,
    enabled: bool,
    shortcut: Option<usize>,
) -> bool {
    ui.ch_id(format!("content-action-{index}-{}", action.name))
        .on_spawn_insert(|| {
            (
                Name::new(format!("content-action-{index}-{}", action.name)),
                bevy::ui_widgets::Button,
                Node {
                    padding: UiRect::axes(px(10), px(6)),
                    min_height: px(32),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    border_radius: BorderRadius::all(px(4)),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.13, 0.24, 0.32)),
            )
        })
        .interactions_disabled(!enabled)
        .background_color(if enabled {
            Color::srgb(0.13, 0.24, 0.32)
        } else {
            Color::srgb(0.07, 0.09, 0.11)
        })
        .add(|ui| {
            let caption = shortcut.map_or_else(
                || action.label.clone(),
                |key| format!("{key} · {}", action.label),
            );
            label(ui, "caption", 13.0, caption);
        })
        .primary_clicked()
        && enabled
}

/// Whether a panel is rendered by this generic window rather than a native
/// schema-specific surface.
#[must_use]
pub fn is_content_panel(panel: &Panel) -> bool {
    panel.schema.as_str() != "ui.lockpick"
}

#[cfg_attr(coverage_nightly, coverage(off))]
#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::id::{DefinitionId, IdKind, StableId};

    fn panel(schema: &str) -> Panel {
        Panel {
            target: StableId::from_parts(IdKind::Entity, 1, 1),
            schema: DefinitionId::new(schema).unwrap(),
            title: "Display".to_string(),
            fields: Vec::new(),
            actions: Vec::new(),
        }
    }

    #[test]
    fn native_and_generic_schemas_are_routed_distinctly() {
        assert!(!is_content_panel(&panel("ui.lockpick")));
        assert!(is_content_panel(&panel("ui.display")));
        assert!(is_content_panel(&panel("ui.terminal")));
    }
}
