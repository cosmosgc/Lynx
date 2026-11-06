//! The selected keypad layout, connected to the real online session.
//! Only rendering and input composition live here; outcomes come from the server.
use bevy::prelude::*;
use bevy::ui::{ComputedNode, ComputedUiRenderTargetInfo, UiSystems};
use bevy::window::PrimaryWindow;
use bevy_immediate::{
    BevyImmediatePlugin, Imm, ImmCtx,
    ui::{
        CapsUi,
        clicked::ImmUiClicked,
        disabled::ImmUiInteractionsDisabled,
        floating_window_plugin::{
            FloatingWindow, FloatingWindowPlugin, FloatingWindowStoreLocationId,
        },
        interaction::ImmUiInteraction,
        look::ImmUiLook,
        text::ImmUiText,
    },
};

/// Fixed window chrome; the height estimate only seeds the initial placement.
const DIALOG_WIDTH: f32 = 440.0;
const DIALOG_ESTIMATED_HEIGHT: f32 = 420.0;
/// One dialog exists at a time, so a fixed id remembers its screen position.
const DIALOG_LOCATION_ID: u64 = 1;

#[derive(Component)]
struct LockDialogWindow;

use crate::{
    interaction::DigitHint,
    online::{OnlineClient, OnlineStatus},
    windowed::{ChatComposer, LockComposer, chat_keyboard, interaction_keyboard},
};

#[derive(Debug)]
pub struct LockPanelPlugin;

#[derive(Resource, Default)]
pub(crate) struct LiveLockDialog;

impl Plugin for LockPanelPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LiveLockDialog>()
            .add_plugins(BevyImmediatePlugin::<CapsUi>::new());
        if !app.is_plugin_added::<FloatingWindowPlugin>() {
            app.add_plugins(FloatingWindowPlugin);
        }
        app.add_systems(
            Update,
            draw.after(interaction_keyboard).before(chat_keyboard),
        );
        // The plugin only re-clamps after a drag ends, so clamp every frame to
        // keep the dialog visible while the pointer is still held down.
        app.add_systems(
            PostUpdate,
            clamp_dialog_window.before_weak(UiSystems::Prepare),
        );
    }
}

/// Keep the dialog fully inside the game viewport, including during a drag.
fn clamp_dialog_window(
    mut dialogs: Query<
        (&mut Node, &ComputedNode, &ComputedUiRenderTargetInfo),
        With<LockDialogWindow>,
    >,
) {
    for (mut node, computed, target) in &mut dialogs {
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

/// Clamp a top-left offset so `size` stays within `viewport`.
fn clamped_offset(offset: f32, viewport: f32, size: f32) -> f32 {
    offset.clamp(0.0, (viewport - size).max(0.0))
}

fn label(ui: &mut Imm<CapsUi>, id: &str, text: impl Into<String>) {
    ui.ch_id(id)
        .on_spawn_insert(|| TextFont {
            font_size: bevy::text::FontSize::Px(18.0),
            ..default()
        })
        .text(text.into());
}

fn button(ui: &mut Imm<CapsUi>, id: &str, text: &str, enabled: bool) -> bool {
    // Pointer clicks only: Enter is handled once by the shared keyboard adapter,
    // rather than also activating a previously clicked digit button.
    ui.ch_id(id)
        .on_spawn_insert(|| {
            (
                Name::new(format!("lock-button-{id}")),
                bevy::ui_widgets::Button,
                Node {
                    padding: UiRect::axes(px(12), px(8)),
                    min_height: px(40),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    border_radius: BorderRadius::all(px(5)),
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
            label(ui, "caption", text);
        })
        .primary_clicked()
        && enabled
}

fn close_button(ui: &mut Imm<CapsUi>, enabled: bool) -> bool {
    let mut close = ui
        .ch_id("close")
        .on_spawn_insert(|| {
            (
                Name::new("lock-button-close"),
                bevy::ui_widgets::Button,
                Node {
                    width: px(30),
                    height: px(30),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    border_radius: BorderRadius::all(px(4)),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.10, 0.13, 0.17)),
            )
        })
        .interactions_disabled(!enabled);
    let hovered = close.hovered();
    close = close.background_color(match (enabled, hovered) {
        (false, _) => Color::srgb(0.06, 0.08, 0.10),
        (true, true) => Color::srgb(0.55, 0.16, 0.16),
        (true, false) => Color::srgb(0.10, 0.13, 0.17),
    });
    let clicked = close
        .add(|ui| {
            label(ui, "caption", "X");
        })
        .primary_clicked();
    clicked && enabled
}

#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
fn draw(
    ctx: ImmCtx<CapsUi>,
    mut client: ResMut<OnlineClient>,
    mut input: ResMut<LockComposer>,
    chat: Res<ChatComposer>,
    windows: Query<&Window, With<PrimaryWindow>>,
) {
    let initial_position = windows.single().map_or(Vec2::splat(80.0), |window| {
        ((Vec2::new(window.width(), window.height())
            - Vec2::new(DIALOG_WIDTH, DIALOG_ESTIMATED_HEIGHT))
            * 0.5)
            .max(Vec2::splat(8.0))
    });
    // Build the root even when closed so immediate mode removes old controls.
    ctx.build_immediate_root("online-lock-dialog")
        .ch_id("canvas")
        .on_spawn_insert(|| {
            (
                Node {
                    position_type: PositionType::Absolute,
                    width: percent(100),
                    height: percent(100),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    ..default()
                },
                bevy::picking::Pickable::IGNORE,
                GlobalZIndex(10),
            )
        })
        .add(|ui| {
            let Some(panel) = client.interaction().panel().cloned() else {
                return;
            };
            let Some(lock) = client.interaction().lock().cloned() else {
                return;
            };
            if client.status() != OnlineStatus::Active {
                return;
            }
            let enabled = !chat.active
                && !client.interaction().pending()
                && windows.single().is_ok_and(|window| window.focused)
                && input.panel.as_ref() == Some(&panel);
            let length = usize::from(lock.length);
            // Identity scopes prevent a queued click from becoming a different
            // provider's action when its panel replaces this one.
            ui.ch_id((panel.target, panel.schema.as_str()))
                .on_spawn_insert(move || {
                    (
                        Name::new("online-lock-dialog"),
                        LockDialogWindow,
                        Node {
                            position_type: PositionType::Absolute,
                            padding: UiRect::all(px(14)),
                            border: UiRect::all(px(1)),
                            row_gap: px(8),
                            flex_direction: FlexDirection::Column,
                            border_radius: BorderRadius::all(px(8)),
                            ..default()
                        },
                        FloatingWindow {
                            initial_width: px(DIALOG_WIDTH),
                            initial_height: Val::Auto,
                            initial_position: Some(initial_position),
                            // Require the whole dialog to stay inside the game
                            // viewport instead of the plugin's 7.5% overlap.
                            overlap_ratio: Some(1.0),
                            ..default()
                        },
                        FloatingWindowStoreLocationId(DIALOG_LOCATION_ID),
                        BackgroundColor(Color::srgba(0.035, 0.055, 0.08, 0.97)),
                        BorderColor::all(Color::srgb(0.20, 0.30, 0.38)),
                    )
                })
                .add(|ui| {
                    ui.ch_id("titlebar")
                        .on_spawn_insert(|| {
                            (
                                Name::new("lock-dialog-titlebar"),
                                Node {
                                    width: percent(100),
                                    flex_direction: FlexDirection::Row,
                                    justify_content: JustifyContent::SpaceBetween,
                                    align_items: AlignItems::Center,
                                    column_gap: px(8),
                                    padding: UiRect::bottom(px(6)),
                                    ..default()
                                },
                            )
                        })
                        .add(|ui| {
                            label(ui, "title", &panel.title);
                            if close_button(ui, enabled) {
                                let _ = client.close_panel();
                            }
                        });
                    label(
                        ui,
                        "instruction",
                        format!("Código de {} dígitos · Enter: tentar", lock.length),
                    );
                    label(
                        ui,
                        "input",
                        format!(
                            "[ {}{} ]",
                            input.guess,
                            "_".repeat(length.saturating_sub(input.guess.len()))
                        ),
                    );
                    label(
                        ui,
                        "attempts",
                        format!("Tentativas restantes: {}", lock.attempts_left),
                    );
                    ui.ch_id("keypad")
                        .on_spawn_insert(|| Node {
                            display: Display::Grid,
                            grid_template_columns: RepeatedGridTrack::flex(3, 1.0),
                            row_gap: px(6),
                            column_gap: px(6),
                            ..default()
                        })
                        .add(|ui| {
                            for digit in "123456789".chars() {
                                let text = digit.to_string();
                                if button(ui, &text, &text, enabled && input.guess.len() < length) {
                                    input.guess.push(digit);
                                }
                            }
                            if button(ui, "erase", "Apagar", enabled && !input.guess.is_empty()) {
                                input.guess.pop();
                            }
                            if button(ui, "zero", "0", enabled && input.guess.len() < length) {
                                input.guess.push('0');
                            }
                            if button(
                                ui,
                                "submit",
                                "Tentar",
                                enabled && input.guess.len() == length && !lock.solved,
                            ) && client.submit_lock_guess(&panel, &input.guess).is_ok()
                            {
                                input.guess.clear();
                            }
                        });
                    if !lock.hints.is_empty() {
                        let hints = lock
                            .hints
                            .iter()
                            .map(|hint| match hint {
                                DigitHint::Exact => "correto",
                                DigitHint::Higher => "maior",
                                DigitHint::Lower => "menor",
                            })
                            .collect::<Vec<_>>()
                            .join(" | ");
                        label(ui, "hints", hints);
                    }
                    if let Some(tick) = lock.retry_after_tick {
                        label(ui, "cooldown", format!("Bloqueio até tick {tick}"));
                    }
                    label(ui, "status", client.interaction().status());
                });
        });
}

#[cfg(test)]
mod tests {
    use super::clamped_offset;

    #[test]
    fn dialog_offsets_stay_inside_the_viewport() {
        assert_eq!(clamped_offset(-120.0, 1280.0, 440.0), 0.0);
        assert_eq!(clamped_offset(300.0, 1280.0, 440.0), 300.0);
        assert_eq!(clamped_offset(2000.0, 1280.0, 440.0), 840.0);
        // A viewport smaller than the dialog keeps the top-left pinned.
        assert_eq!(clamped_offset(50.0, 300.0, 440.0), 0.0);
    }
}
