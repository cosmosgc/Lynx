//! Reusable immediate-mode floating window chrome.
//!
//! Call [`show_window`] from an immediate-mode draw system to get a draggable,
//! resizable, closable window. Position and size are remembered per
//! [`WindowSpec::id`] in [`WindowMemory`] for as long as the client runs.

use std::collections::BTreeMap;

use bevy::picking::Pickable;
use bevy::prelude::*;
use bevy::text::{FontSize, TextLayout};
use bevy::ui::{
    ComputedNode, ComputedUiRenderTargetInfo, UiGlobalTransform, UiSystems, px, vh, vw,
};
use bevy_immediate::{
    Imm, ImmEntity,
    ui::{
        CapsUi,
        clicked::ImmUiClicked,
        floating_window_plugin::{FloatingWindow, resizable_borders},
        interaction::ImmUiInteraction,
        look::ImmUiLook,
        text::ImmUiText,
    },
};

const WINDOW_BACKGROUND: Color = Color::srgba(0.035, 0.055, 0.08, 0.97);
const WINDOW_BORDER: Color = Color::srgb(0.20, 0.30, 0.38);
const TITLE_BACKGROUND: Color = Color::srgb(0.08, 0.13, 0.18);
const TITLE_TEXT: Color = Color::srgb(0.86, 0.92, 0.96);
const SUBTITLE_TEXT: Color = Color::srgb(0.55, 0.68, 0.76);
const CLOSE_BACKGROUND: Color = Color::srgb(0.10, 0.13, 0.17);
const CLOSE_HOVER: Color = Color::srgb(0.55, 0.16, 0.16);

/// Identifies a remembered window. Keep ids stable for the lifetime of a panel.
pub const CHAT_WINDOW_ID: u64 = 2;

/// Marker placed on the root entity of every window built by this module.
#[derive(Component, Debug, Clone, Copy)]
pub struct ManagedWindow(pub u64);

/// Remembered screen position and size for one [`ManagedWindow`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StoredWindow {
    pub position: Vec2,
    pub size: Vec2,
}

/// Per-window geometry remembered across close/reopen cycles.
#[derive(Resource, Debug, Default)]
pub struct WindowMemory {
    stored: BTreeMap<u64, StoredWindow>,
}

impl WindowMemory {
    pub fn get(&self, id: u64) -> Option<StoredWindow> {
        self.stored.get(&id).copied()
    }
}

/// Description of the chrome and initial geometry for [`show_window`].
#[derive(Debug, Clone)]
pub struct WindowSpec {
    /// Stable identity used for position/size memory.
    pub id: u64,
    pub title: String,
    pub subtitle: Option<String>,
    pub size: Vec2,
    pub min_size: Vec2,
    /// Screen position used the first time the window appears.
    pub position: Vec2,
    pub resizable: bool,
}

impl WindowSpec {
    pub fn new(id: u64, title: impl Into<String>, size: Vec2) -> Self {
        Self {
            id,
            title: title.into(),
            subtitle: None,
            size,
            min_size: Vec2::new(280.0, 180.0),
            position: Vec2::new(40.0, 40.0),
            resizable: true,
        }
    }
}

/// Result of drawing one window for a frame.
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowOutcome {
    pub close_requested: bool,
}

/// Draw a floating window and return the close-button outcome.
///
/// The body is called inside the window content area. Both the window and its
/// body are rebuilt each frame by immediate mode; entity identity keeps layout
/// state such as drag position and scrollback stable.
pub fn show_window(
    mut window: ImmEntity<CapsUi>,
    spec: WindowSpec,
    body: impl FnOnce(&mut Imm<CapsUi>),
) -> WindowOutcome {
    let mut outcome = WindowOutcome::default();
    let spawn = spec.clone();

    window = window.on_spawn_insert(move || {
        (
            Name::new(spawn.title.clone()),
            ManagedWindow(spawn.id),
            Node {
                position_type: PositionType::Absolute,
                width: px(spawn.size.x),
                height: px(spawn.size.y),
                padding: UiRect::all(px(10)),
                border: UiRect::all(px(1)),
                flex_direction: FlexDirection::Column,
                row_gap: px(8),
                border_radius: BorderRadius::all(px(8)),
                ..default()
            },
            FloatingWindow {
                // Auto initial sizing keeps the resize limits supplied by this
                // spec authoritative. The explicit Node width/height below is
                // the remembered size.
                initial_width: Val::Auto,
                initial_height: Val::Auto,
                min_width: px(spawn.min_size.x),
                min_height: px(spawn.min_size.y),
                max_width: vw(92.0),
                max_height: vh(92.0),
                initial_position: Some(spawn.position),
                overlap_ratio: Some(1.0),
            },
            BackgroundColor(WINDOW_BACKGROUND),
            BorderColor::all(WINDOW_BORDER),
        )
    });

    if spec.resizable {
        // The helper adds eight grab strips around the window edge. Dragging
        // one of them resizes the window; dragging the title bar moves it.
        window = window.on_spawn_insert(|| resizable_borders(6.0, ()));
    }

    let title = spec.title;
    let subtitle = spec.subtitle;
    window.add(|ui| {
        ui.ch_id("titlebar")
            .on_spawn_insert(|| {
                (
                    Node {
                        width: percent(100),
                        flex_direction: FlexDirection::Row,
                        align_items: AlignItems::Center,
                        column_gap: px(8),
                        padding: UiRect::axes(px(8), px(5)),
                        border_radius: BorderRadius::all(px(5)),
                        ..default()
                    },
                    BackgroundColor(TITLE_BACKGROUND),
                )
            })
            .add(|ui| {
                ui.ch_id("title")
                    .on_spawn_insert(|| {
                        (
                            TextFont {
                                font_size: FontSize::Px(16.0),
                                ..default()
                            },
                            Node {
                                flex_grow: 1.0,
                                ..default()
                            },
                            TextColor(TITLE_TEXT),
                        )
                    })
                    .text(title);
                if let Some(subtitle) = subtitle {
                    ui.ch_id("subtitle")
                        .on_spawn_insert(|| {
                            (
                                TextFont {
                                    font_size: FontSize::Px(13.0),
                                    ..default()
                                },
                                TextColor(SUBTITLE_TEXT),
                            )
                        })
                        .text(subtitle);
                }
                if close_button(ui) {
                    outcome.close_requested = true;
                }
            });

        body(ui);
    });

    outcome
}

fn close_button(ui: &mut Imm<CapsUi>) -> bool {
    let mut close = ui.ch_id("close").on_spawn_insert(|| {
        (
            Name::new("window-close"),
            bevy::ui_widgets::Button,
            Node {
                width: px(26),
                height: px(26),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                border_radius: BorderRadius::all(px(4)),
                ..default()
            },
            BackgroundColor(CLOSE_BACKGROUND),
            Pickable::default(),
        )
    });
    let hovered = close.hovered();
    close = close.background_color(if hovered {
        CLOSE_HOVER
    } else {
        CLOSE_BACKGROUND
    });
    close
        .add(|ui| {
            ui.ch_id("caption")
                .on_spawn_insert(|| {
                    (
                        TextFont {
                            font_size: FontSize::Px(14.0),
                            ..default()
                        },
                        TextColor(TITLE_TEXT),
                    )
                })
                .text("X");
        })
        .primary_clicked()
}

/// Keep every managed window fully inside the game viewport during drag/resize.
fn clamp_windows(
    mut windows: Query<
        (&mut Node, &ComputedNode, &ComputedUiRenderTargetInfo),
        With<ManagedWindow>,
    >,
) {
    for (mut node, computed, target) in &mut windows {
        let (Val::Px(left), Val::Px(top)) = (node.left, node.top) else {
            continue;
        };
        if computed.size.min_element() <= 1.0 {
            continue;
        }
        let viewport = target.logical_size();
        if viewport.x <= 0.0 || viewport.y <= 0.0 {
            continue;
        }
        let size = computed.size * computed.inverse_scale_factor;
        let clamped_left = px(clamped_offset(left, viewport.x, size.x));
        let clamped_top = px(clamped_offset(top, viewport.y, size.y));
        if node.left != clamped_left {
            node.left = clamped_left;
        }
        if node.top != clamped_top {
            node.top = clamped_top;
        }
    }
}

/// Capture the latest geometry before a closed window is despawned.
fn remember_windows(
    mut memory: ResMut<WindowMemory>,
    windows: Query<(&ManagedWindow, &ComputedNode, &UiGlobalTransform)>,
) {
    for (window, computed, transform) in &windows {
        if computed.size.min_element() <= 1.0 {
            continue;
        }
        let scale = computed.inverse_scale_factor;
        memory.stored.insert(
            window.0,
            StoredWindow {
                position: (transform.translation - computed.size * 0.5) * scale,
                size: computed.size * scale,
            },
        );
    }
}

fn clamped_offset(offset: f32, viewport: f32, size: f32) -> f32 {
    offset.clamp(0.0, (viewport - size).max(0.0))
}

/// Plugin that adds shared window geometry memory and clamping.
#[derive(Debug)]
pub struct ImmediateWindowPlugin;

impl Plugin for ImmediateWindowPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WindowMemory>();
        app.add_systems(
            PostUpdate,
            (clamp_windows, remember_windows).before_weak(UiSystems::Prepare),
        );
    }
}

/// A full-width line of text inside a window body.
pub fn label(ui: &mut Imm<CapsUi>, id: &str, size: f32, text: impl Into<String>) {
    ui.ch_id(id)
        .on_spawn_insert(move || {
            (
                TextFont {
                    font_size: FontSize::Px(size),
                    ..default()
                },
                TextLayout::default(),
                Node {
                    width: percent(100),
                    ..default()
                },
            )
        })
        .text(text.into());
}

#[cfg_attr(coverage_nightly, coverage(off))]
#[cfg(test)]
mod tests {
    use super::clamped_offset;

    #[test]
    fn window_offsets_stay_inside_the_viewport() {
        assert_eq!(clamped_offset(-120.0, 1280.0, 440.0), 0.0);
        assert_eq!(clamped_offset(300.0, 1280.0, 440.0), 300.0);
        assert_eq!(clamped_offset(2000.0, 1280.0, 440.0), 840.0);
        assert_eq!(clamped_offset(50.0, 300.0, 440.0), 0.0);
    }
}
