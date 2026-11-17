//! In-world pause/options menu and live window graphics settings.

use bevy::prelude::*;
use bevy::window::{MonitorSelection, PresentMode, PrimaryWindow, WindowMode};
use engine_core::i18n::Locale;

use crate::localization::{LocalePreference, text};
use crate::ui::chat::ChatComposer;

const PANEL_COLOR: Color = Color::srgb(0.055, 0.075, 0.11);
const BUTTON_COLOR: Color = Color::srgb(0.13, 0.19, 0.27);
const BUTTON_HOVER_COLOR: Color = Color::srgb(0.2, 0.31, 0.43);
const BUTTON_ACTIVE_COLOR: Color = Color::srgb(0.12, 0.34, 0.5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum OptionsTab {
    Controls,
    #[default]
    Graphics,
    Language,
}

/// Shared state used by input adapters to suspend gameplay while the menu is open.
#[derive(Debug, Resource)]
pub struct OptionsMenuState {
    open: bool,
    tab: OptionsTab,
    vsync: bool,
    fullscreen: bool,
}

impl Default for OptionsMenuState {
    fn default() -> Self {
        Self {
            open: false,
            tab: OptionsTab::default(),
            vsync: true,
            fullscreen: false,
        }
    }
}

impl OptionsMenuState {
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }
}

#[derive(Debug)]
pub struct OptionsMenuPlugin;

impl Plugin for OptionsMenuPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<OptionsMenuState>()
            .init_resource::<LocalePreference>()
            .add_systems(Startup, setup_menu)
            .add_systems(Update, toggle_menu)
            .add_systems(
                Update,
                (apply_menu_buttons, present_menu)
                    .chain()
                    .after(toggle_menu),
            );
    }
}

#[derive(Component)]
struct MenuOverlay;

#[derive(Component)]
struct ControlsPage;

#[derive(Component)]
struct GraphicsPage;

#[derive(Component)]
struct LanguagePage;

#[derive(Component)]
struct MenuTitle;

/// Index into the fixed controls-help list, so a locale change can retranslate
/// it without rebuilding the menu hierarchy.
#[derive(Component, Clone, Copy)]
struct ControlsHint(usize);

const CONTROLS_HINTS: [&str; 5] = [
    "ui.options.controls.move",
    "ui.options.controls.camera",
    "ui.options.controls.select",
    "ui.options.controls.chat",
    "ui.options.controls.escape",
];

#[derive(Component, Clone, Copy, PartialEq, Eq)]
enum MenuControl {
    Tab(OptionsTab),
    ToggleVsync,
    ToggleFullscreen,
    Locale(&'static str),
    Close,
}

#[derive(Component)]
struct MenuButton;

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn toggle_menu(
    keys: Res<ButtonInput<KeyCode>>,
    composer: Option<Res<ChatComposer>>,
    mut menu: ResMut<OptionsMenuState>,
    windows: Query<&Window, With<PrimaryWindow>>,
) {
    if !keys.just_pressed(KeyCode::Escape)
        || composer.is_some_and(|composer| composer.active)
        || !windows.single().is_ok_and(|window| window.focused)
    {
        return;
    }
    menu.open = !menu.open;
}

#[allow(deprecated)] // Bevy UI still uses FocusPolicy to route pointer input.
#[allow(clippy::too_many_lines)] // Keep the small static menu hierarchy together.
#[allow(clippy::needless_pass_by_value)] // Bevy system parameters are taken by value.
fn setup_menu(
    mut commands: Commands,
    windows: Query<&Window, With<PrimaryWindow>>,
    preference: Res<LocalePreference>,
) {
    let (vsync, fullscreen) = windows.single().map_or((true, false), |window| {
        (
            !matches!(
                window.present_mode,
                PresentMode::AutoNoVsync | PresentMode::Immediate
            ),
            window.mode != WindowMode::Windowed,
        )
    });
    let locale = preference.locale().clone();
    commands.insert_resource(OptionsMenuState {
        vsync,
        fullscreen,
        ..default()
    });

    commands
        .spawn((
            MenuOverlay,
            Interaction::default(),
            bevy::ui::FocusPolicy::Block,
            Node {
                position_type: PositionType::Absolute,
                width: percent(100),
                height: percent(100),
                display: Display::None,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                padding: UiRect::all(px(16)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.015, 0.025, 0.045, 0.76)),
            GlobalZIndex(100),
        ))
        .with_children(|overlay| {
            overlay
                .spawn((
                    Node {
                        width: px(560),
                        max_width: percent(100),
                        flex_direction: FlexDirection::Column,
                        row_gap: px(16),
                        padding: UiRect::all(px(22)),
                        border_radius: BorderRadius::all(px(12)),
                        ..default()
                    },
                    BackgroundColor(PANEL_COLOR),
                ))
                .with_children(|panel| {
                    panel.spawn((
                        MenuTitle,
                        Text::new(text(&locale, "ui.options.title")),
                        TextFont {
                            font_size: bevy::text::FontSize::Px(26.0),
                            ..default()
                        },
                    ));
                    panel
                        .spawn((Node {
                            flex_direction: FlexDirection::Row,
                            column_gap: px(8),
                            ..default()
                        },))
                        .with_children(|tabs| {
                            menu_button(tabs, "Controls", MenuControl::Tab(OptionsTab::Controls));
                            menu_button(tabs, "Graphics", MenuControl::Tab(OptionsTab::Graphics));
                            menu_button(tabs, "Language", MenuControl::Tab(OptionsTab::Language));
                        });
                    panel
                        .spawn((
                            ControlsPage,
                            Node {
                                display: Display::None,
                                flex_direction: FlexDirection::Column,
                                row_gap: px(8),
                                min_height: px(140),
                                ..default()
                            },
                        ))
                        .with_children(|page| {
                            for (index, key) in CONTROLS_HINTS.iter().enumerate() {
                                page.spawn((ControlsHint(index), Text::new(text(&locale, key))));
                            }
                        });
                    panel
                        .spawn((
                            GraphicsPage,
                            Node {
                                flex_direction: FlexDirection::Column,
                                row_gap: px(10),
                                min_height: px(140),
                                ..default()
                            },
                        ))
                        .with_children(|page| {
                            menu_button(
                                page,
                                &text(&locale, "ui.options.vsync_on"),
                                MenuControl::ToggleVsync,
                            );
                            menu_button(
                                page,
                                &text(&locale, "ui.options.windowed"),
                                MenuControl::ToggleFullscreen,
                            );
                            page.spawn((
                                Text::new(text(&locale, "ui.options.vsync_hint")),
                                TextFont {
                                    font_size: bevy::text::FontSize::Px(14.0),
                                    ..default()
                                },
                            ));
                        });
                    panel
                        .spawn((
                            LanguagePage,
                            Node {
                                display: Display::None,
                                flex_direction: FlexDirection::Column,
                                row_gap: px(10),
                                min_height: px(140),
                                ..default()
                            },
                        ))
                        .with_children(|page| {
                            menu_button(page, "Português (Brasil)", MenuControl::Locale("pt-BR"));
                            menu_button(page, "English", MenuControl::Locale("en"));
                            page.spawn((
                                Text::new(text(&locale, "ui.options.language_hint")),
                                TextFont {
                                    font_size: bevy::text::FontSize::Px(14.0),
                                    ..default()
                                },
                            ));
                        });
                    panel
                        .spawn((Node {
                            flex_direction: FlexDirection::Row,
                            justify_content: JustifyContent::FlexEnd,
                            ..default()
                        },))
                        .with_children(|footer| {
                            menu_button(
                                footer,
                                &text(&locale, "ui.options.resume"),
                                MenuControl::Close,
                            );
                        });
                });
        });
}

#[allow(deprecated)] // Bevy 0.20 UI interaction compatibility.
fn menu_button(parent: &mut ChildSpawnerCommands, label: &str, control: MenuControl) {
    parent.spawn((
        MenuButton,
        control,
        Interaction::default(),
        bevy::ui::FocusPolicy::Block,
        Text::new(label.to_owned()),
        TextFont {
            font_size: bevy::text::FontSize::Px(16.0),
            ..default()
        },
        Node {
            padding: UiRect::axes(px(14), px(10)),
            border_radius: BorderRadius::all(px(5)),
            ..default()
        },
        BackgroundColor(BUTTON_COLOR),
    ));
}

#[allow(deprecated)] // Bevy 0.20 UI interaction compatibility.
#[allow(clippy::needless_pass_by_value)]
fn apply_menu_buttons(
    mut menu: ResMut<OptionsMenuState>,
    mut preference: ResMut<LocalePreference>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
    buttons: Query<(&Interaction, &MenuControl), Changed<Interaction>>,
) {
    for (interaction, control) in &buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }
        match control {
            MenuControl::Tab(tab) => menu.tab = *tab,
            MenuControl::ToggleVsync => {
                menu.vsync = !menu.vsync;
                if let Ok(mut window) = windows.single_mut() {
                    window.present_mode = if menu.vsync {
                        PresentMode::AutoVsync
                    } else {
                        PresentMode::AutoNoVsync
                    };
                }
            }
            MenuControl::ToggleFullscreen => {
                menu.fullscreen = !menu.fullscreen;
                if let Ok(mut window) = windows.single_mut() {
                    window.mode = if menu.fullscreen {
                        WindowMode::BorderlessFullscreen(MonitorSelection::Current)
                    } else {
                        WindowMode::Windowed
                    };
                }
            }
            MenuControl::Locale(tag) => {
                if let Ok(locale) = Locale::parse(tag)
                    && let Err(error) = preference.set_locale(locale)
                {
                    eprintln!("Could not persist the locale preference: {error}");
                }
            }
            MenuControl::Close => menu.open = false,
        }
    }
}

#[allow(deprecated)] // Bevy 0.20 UI interaction compatibility.
#[allow(clippy::needless_pass_by_value)] // Bevy system parameters are taken by value.
#[allow(clippy::type_complexity)] // These filters make the three Node queries provably disjoint.
#[allow(clippy::too_many_arguments)]
fn present_menu(
    menu: Res<OptionsMenuState>,
    preference: Res<LocalePreference>,
    mut overlay: Query<
        &mut Node,
        (
            With<MenuOverlay>,
            Without<ControlsPage>,
            Without<GraphicsPage>,
            Without<LanguagePage>,
        ),
    >,
    mut controls: Query<
        &mut Node,
        (
            With<ControlsPage>,
            Without<GraphicsPage>,
            Without<LanguagePage>,
            Without<MenuOverlay>,
        ),
    >,
    mut graphics: Query<
        &mut Node,
        (
            With<GraphicsPage>,
            Without<ControlsPage>,
            Without<LanguagePage>,
            Without<MenuOverlay>,
        ),
    >,
    mut language: Query<
        &mut Node,
        (
            With<LanguagePage>,
            Without<ControlsPage>,
            Without<GraphicsPage>,
            Without<MenuOverlay>,
        ),
    >,
    mut buttons: Query<(&Interaction, &MenuControl, &mut BackgroundColor), With<MenuButton>>,
    mut labels: Query<(&MenuControl, &mut Text)>,
    mut titles: Query<&mut Text, (With<MenuTitle>, Without<MenuControl>)>,
    mut hints: Query<(&ControlsHint, &mut Text), (Without<MenuControl>, Without<MenuTitle>)>,
) {
    let locale = preference.locale();
    for mut node in &mut overlay {
        let display = if menu.open {
            Display::Flex
        } else {
            Display::None
        };
        if node.display != display {
            node.display = display;
        }
    }
    for mut node in &mut controls {
        let display = if menu.tab == OptionsTab::Controls {
            Display::Flex
        } else {
            Display::None
        };
        if node.display != display {
            node.display = display;
        }
    }
    for mut node in &mut graphics {
        let display = if menu.tab == OptionsTab::Graphics {
            Display::Flex
        } else {
            Display::None
        };
        if node.display != display {
            node.display = display;
        }
    }
    for mut node in &mut language {
        let display = if menu.tab == OptionsTab::Language {
            Display::Flex
        } else {
            Display::None
        };
        if node.display != display {
            node.display = display;
        }
    }

    for (interaction, control, mut background) in &mut buttons {
        let color = if matches!(control, MenuControl::Tab(tab) if *tab == menu.tab)
            || matches!(control, MenuControl::Locale(tag) if locale.as_str() == *tag)
        {
            BUTTON_ACTIVE_COLOR
        } else if *interaction == Interaction::Hovered {
            BUTTON_HOVER_COLOR
        } else {
            BUTTON_COLOR
        };
        background.set_if_neq(BackgroundColor(color));
    }
    for (control, mut label_text) in &mut labels {
        let label = match control {
            MenuControl::Tab(OptionsTab::Controls) => text(locale, "ui.options.controls"),
            MenuControl::Tab(OptionsTab::Graphics) => text(locale, "ui.options.graphics"),
            MenuControl::Tab(OptionsTab::Language) => text(locale, "ui.options.language"),
            MenuControl::ToggleVsync => text(
                locale,
                if menu.vsync {
                    "ui.options.vsync_on"
                } else {
                    "ui.options.vsync_off"
                },
            ),
            MenuControl::ToggleFullscreen => text(
                locale,
                if menu.fullscreen {
                    "ui.options.fullscreen"
                } else {
                    "ui.options.windowed"
                },
            ),
            MenuControl::Locale(_) => continue,
            MenuControl::Close => text(locale, "ui.options.resume"),
        };
        if label_text.0 != label {
            label_text.0 = label;
        }
    }
    for mut title_text in &mut titles {
        let label = text(locale, "ui.options.title");
        if title_text.0 != label {
            title_text.0 = label;
        }
    }
    for (hint, mut hint_text) in &mut hints {
        let label = text(locale, CONTROLS_HINTS[hint.0]);
        if hint_text.0 != label {
            hint_text.0 = label;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_opens_and_closes_the_menu_for_a_focused_window() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<ButtonInput<KeyCode>>()
            .add_plugins(OptionsMenuPlugin);
        app.world_mut().spawn((
            Window {
                focused: true,
                ..default()
            },
            PrimaryWindow,
        ));

        app.update();
        assert!(!app.world().resource::<OptionsMenuState>().is_open());

        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::Escape);
        app.update();
        assert!(app.world().resource::<OptionsMenuState>().is_open());

        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .reset(KeyCode::Escape);
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::Escape);
        app.update();
        assert!(!app.world().resource::<OptionsMenuState>().is_open());
    }

    #[allow(deprecated)] // Bevy UI still uses the legacy interaction component in tests.
    fn press(app: &mut App, control: MenuControl) {
        app.world_mut().spawn((
            MenuButton,
            control,
            Interaction::Pressed,
            BackgroundColor(BUTTON_COLOR),
        ));
        app.update();
    }

    fn menu_text(app: &mut App, control: MenuControl) -> String {
        let mut query = app.world_mut().query::<(&MenuControl, &Text)>();
        query
            .iter(app.world())
            .find(|(candidate, _)| **candidate == control)
            .map(|(_, text)| text.0.clone())
            .expect("menu label exists")
    }

    #[test]
    fn language_tab_lists_endonyms_and_switches_tabs() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<LocalePreference>()
            .add_plugins(OptionsMenuPlugin);
        app.update();

        press(&mut app, MenuControl::Tab(OptionsTab::Language));
        assert_eq!(
            app.world().resource::<OptionsMenuState>().tab,
            OptionsTab::Language
        );

        let mut query = app.world_mut().query::<(&MenuControl, &Text)>();
        let mut labels: Vec<String> = query
            .iter(app.world())
            .filter_map(|(control, text)| match control {
                MenuControl::Locale(_) => Some(text.0.clone()),
                _ => None,
            })
            .collect();
        labels.sort();
        assert_eq!(labels, vec!["English", "Português (Brasil)"]);
    }

    #[test]
    fn selecting_a_language_retranslates_the_menu_and_persists_it() {
        let directory =
            std::env::temp_dir().join(format!("lynx-options-locale-{}", std::process::id()));
        let path = directory.join("locale.json");
        let preference = LocalePreference::load(Some(path.clone()));

        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<ButtonInput<KeyCode>>()
            .insert_resource(preference)
            .add_plugins(OptionsMenuPlugin);
        app.update();
        assert_eq!(
            menu_text(&mut app, MenuControl::Tab(OptionsTab::Controls)),
            "Controles"
        );

        press(&mut app, MenuControl::Locale("en"));
        assert_eq!(
            app.world().resource::<LocalePreference>().locale().as_str(),
            "en"
        );
        assert_eq!(
            menu_text(&mut app, MenuControl::Tab(OptionsTab::Controls)),
            "Controls"
        );
        assert_eq!(
            LocalePreference::load(Some(path)).locale().as_str(),
            "en",
            "the selection survives a restart"
        );
        let _ = std::fs::remove_dir_all(directory);
    }
}
