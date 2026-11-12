//! The online chat window: channels, tabs, presence, and composer.
//!
//! Channel tabs are derived from the channels the client has actually seen, so
//! a zone-only server shows only zone/whisper navigation, while group and party
//! tabs appear as soon as those channels become active. Unread counts are kept
//! locally until the tab is opened.

use std::collections::BTreeMap;

use bevy::input::ButtonState;
use bevy::input::keyboard::KeyboardInput;
use bevy::picking::Pickable;
use bevy::prelude::*;
use bevy::text::{FontSize, LineBreak, TextLayout};
use bevy::ui::{Overflow, px};
use bevy::window::PrimaryWindow;
use bevy_immediate::{
    BevyImmediatePlugin, Imm, ImmCtx,
    ui::{
        CapsUi, clicked::ImmUiClicked, floating_window_plugin::FloatingWindowPlugin,
        interaction::ImmUiInteraction, look::ImmUiLook, text::ImmUiText,
    },
};

use crate::chat::MAX_CHAT_SCROLLBACK_MESSAGES;
use crate::online::OnlineClient;
use crate::ui::window::{
    CHAT_WINDOW_ID, ImmediateWindowPlugin, WindowMemory, WindowSpec, show_window,
};
use game_api::ChatChannelTarget;

const CHAT_DEFAULT_SIZE: Vec2 = Vec2::new(470.0, 380.0);
const CHAT_MIN_SIZE: Vec2 = Vec2::new(310.0, 220.0);
const TAB_IDLE: Color = Color::srgb(0.10, 0.15, 0.20);
const TAB_HOVER: Color = Color::srgb(0.16, 0.23, 0.29);
const TAB_ACTIVE: Color = Color::srgb(0.16, 0.38, 0.46);
const MESSAGE_BACKGROUND: Color = Color::srgba(0.02, 0.03, 0.05, 0.45);
const PROMPT_TEXT: Color = Color::srgb(0.72, 0.83, 0.89);
const NOTICE_TEXT: Color = Color::srgb(0.95, 0.72, 0.42);

/// One navigable chat channel.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ChatTab {
    /// The current zone room.
    Zone,
    /// The inbox for one-to-one direct messages.
    Whisper,
    /// One-to-one channel with an authorized friend.
    Direct(String),
    /// A group-scoped channel; the id is empty for a generic group feed.
    Group(String),
    /// A party channel; the id is empty for the local party feed.
    Party(String),
    /// Any other channel advertised by the server.
    Other(String),
}

impl ChatTab {
    /// Classify a server-provided channel id into a stable tab.
    pub fn from_channel_id(channel_id: &str) -> Self {
        let channel = channel_id.trim();
        match channel {
            "zone" | "global" => Self::Zone,
            "whisper" | "direct" => Self::Whisper,
            _ if is_whisper_id(channel) => Self::Whisper,
            "group" => Self::Group(String::new()),
            _ if channel.starts_with("group:") || channel.starts_with("group/") => {
                Self::Group(scoped_id(channel, "group"))
            }
            "party" => Self::Party(String::new()),
            _ if channel.starts_with("party:") || channel.starts_with("party/") => {
                Self::Party(scoped_id(channel, "party"))
            }
            _ => Self::Other(channel.to_owned()),
        }
    }

    /// The channel id used when sending from this tab.
    pub fn channel_id(&self) -> String {
        match self {
            Self::Zone => "zone".to_owned(),
            Self::Whisper => "whisper".to_owned(),
            Self::Direct(id) => format!("direct:{id}"),
            Self::Group(id) if id.is_empty() => "group".to_owned(),
            Self::Group(id) => format!("group:{id}"),
            Self::Party(id) if id.is_empty() => "party".to_owned(),
            Self::Party(id) => format!("party:{id}"),
            Self::Other(id) => id.clone(),
        }
    }

    /// Human-readable tab label (kept short so several tabs fit).
    pub fn label(&self) -> String {
        match self {
            Self::Zone => "Zona".to_owned(),
            Self::Whisper => "Sussurros".to_owned(),
            Self::Direct(id) => format!("Direto · {id}"),
            Self::Group(id) if id.is_empty() => "Grupo".to_owned(),
            Self::Group(id) => format!("Grupo · {id}"),
            Self::Party(id) if id.is_empty() => "Party".to_owned(),
            Self::Party(id) => format!("Party · {id}"),
            Self::Other(id) => id.clone(),
        }
    }

    /// Whether a message belongs to this tab.
    pub fn matches(&self, channel_id: &str) -> bool {
        match self {
            Self::Zone => channel_id == "zone" || channel_id == "global",
            Self::Whisper => is_whisper_id(channel_id),
            Self::Direct(id) => channel_id == format!("direct:{id}") || channel_id == id,
            Self::Group(id) => matches_scope(channel_id, "group", id),
            Self::Party(id) => matches_scope(channel_id, "party", id),
            Self::Other(id) => channel_id == id,
        }
    }
}

fn is_whisper_id(channel: &str) -> bool {
    channel == "whisper"
        || channel == "direct"
        || channel.starts_with("whisper:")
        || channel.starts_with("whisper/")
        || channel.starts_with("direct:")
        || channel.starts_with("direct/")
}

fn scoped_id(channel: &str, prefix: &str) -> String {
    channel
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix(':').or_else(|| rest.strip_prefix('/')))
        .unwrap_or_default()
        .to_owned()
}

fn matches_scope(channel: &str, prefix: &str, id: &str) -> bool {
    if id.is_empty() {
        channel == prefix
            || channel.starts_with(&format!("{prefix}:"))
            || channel.starts_with(&format!("{prefix}/"))
    } else {
        channel == id || channel == format!("{prefix}:{id}") || channel == format!("{prefix}/{id}")
    }
}

/// Local presentation state for the chat window.
#[derive(Resource, Debug)]
pub struct ChatPanel {
    /// Window visibility. Closing the window does not clear the feed.
    pub open: bool,
    /// Currently displayed channel.
    pub active: ChatTab,
    /// Group id when the client is in a group.
    pub group_id: Option<String>,
    /// Party id when the client is in a party.
    pub party_id: Option<String>,
    last_read: BTreeMap<ChatTab, String>,
}

impl Default for ChatPanel {
    fn default() -> Self {
        Self {
            open: true,
            active: ChatTab::Zone,
            group_id: None,
            party_id: None,
            last_read: BTreeMap::new(),
        }
    }
}

impl ChatPanel {
    /// Rebuild the tab list from explicit membership and observed channels.
    pub fn tabs(&self, client: &OnlineClient) -> Vec<ChatTab> {
        let mut tabs = tabs_from_channels(
            client
                .chat()
                .messages()
                .map(|message| message.channel_id.as_str()),
            self,
        );
        for target in client.chat_targets() {
            let tab = match target {
                ChatChannelTarget::Direct(id) => ChatTab::Direct(id.to_string()),
                ChatChannelTarget::Group(id) => ChatTab::Group(id.to_string()),
                ChatChannelTarget::Room(_) => continue,
            };
            if !tabs.contains(&tab) {
                tabs.push(tab);
            }
        }
        tabs
    }

    fn mark_read(&mut self, client: &OnlineClient, tab: &ChatTab) {
        if let Some(message) = client
            .chat()
            .messages()
            .rev()
            .find(|message| tab.matches(&message.channel_id))
        {
            self.last_read.insert(tab.clone(), message.id.clone());
        }
    }

    fn unread(&self, client: &OnlineClient, tab: &ChatTab) -> usize {
        let messages = || {
            client
                .chat()
                .messages()
                .filter(|message| tab.matches(&message.channel_id))
        };
        let Some(last_read) = self.last_read.get(tab) else {
            return messages().count();
        };
        let mut total = 0;
        let mut after = 0;
        let mut found = false;
        for message in messages() {
            total += 1;
            if !found {
                if &message.id == last_read {
                    found = true;
                }
                continue;
            }
            after += 1;
        }
        if found { after } else { total }
    }
}

fn tabs_from_channels<'a>(
    channels: impl IntoIterator<Item = &'a str>,
    panel: &ChatPanel,
) -> Vec<ChatTab> {
    let mut tabs = vec![ChatTab::Zone, ChatTab::Whisper];
    if let Some(group) = &panel.group_id {
        let tab = ChatTab::Group(group.clone());
        if !tabs.contains(&tab) {
            tabs.push(tab);
        }
    }
    if let Some(party) = &panel.party_id {
        let tab = ChatTab::Party(party.clone());
        if !tabs.contains(&tab) {
            tabs.push(tab);
        }
    }
    for channel in channels {
        let tab = ChatTab::from_channel_id(channel);
        if !tabs.contains(&tab) {
            tabs.push(tab);
        }
    }
    tabs
}

fn tab_caption(tab: &ChatTab, unread: usize) -> String {
    if unread == 0 {
        tab.label()
    } else {
        format!("{} ({unread})", tab.label())
    }
}

/// Text entry state shared with movement and interaction gates.
#[derive(Debug, Resource)]
pub struct ChatComposer {
    pub(crate) active: bool,
    pub(crate) text: String,
    pub(crate) channel: String,
    history_offset: usize,
    pub(crate) presence_refresh: Timer,
}

impl Default for ChatComposer {
    fn default() -> Self {
        Self {
            active: false,
            text: String::new(),
            channel: "zone".to_owned(),
            history_offset: 0,
            presence_refresh: Timer::from_seconds(5.0, TimerMode::Repeating),
        }
    }
}

/// Adds the chat window and its keyboard adapter to the online client.
#[derive(Debug)]
pub struct ChatPanelPlugin;

impl Plugin for ChatPanelPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ChatPanel>()
            .init_resource::<ChatComposer>()
            .add_plugins(BevyImmediatePlugin::<CapsUi>::new());
        if !app.is_plugin_added::<ImmediateWindowPlugin>() {
            app.add_plugins(ImmediateWindowPlugin);
        }
        if !app.is_plugin_added::<FloatingWindowPlugin>() {
            app.add_plugins(FloatingWindowPlugin);
        }
        app.add_systems(
            Update,
            (chat_keyboard, draw)
                .chain()
                .after(crate::online::poll_online),
        );
    }
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn chat_keyboard(
    mut events: MessageReader<KeyboardInput>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut composer: ResMut<ChatComposer>,
    mut panel: ResMut<ChatPanel>,
    mut client: ResMut<OnlineClient>,
) {
    if !windows.single().is_ok_and(|window| window.focused) {
        composer.active = false;
        return;
    }
    for event in events.read() {
        if event.state != ButtonState::Pressed {
            continue;
        }
        if !composer.active && event.key_code == KeyCode::PageUp {
            composer.history_offset = composer
                .history_offset
                .saturating_add(50)
                .min(MAX_CHAT_SCROLLBACK_MESSAGES);
            if panel.active == ChatTab::Zone
                && let Some(before) = client.chat().next_before()
            {
                let _ = client.request_chat_history(50, Some(before));
            }
            continue;
        }
        if !composer.active && event.key_code == KeyCode::PageDown {
            composer.history_offset = composer.history_offset.saturating_sub(50);
            continue;
        }
        if !composer.active && event.key_code == KeyCode::KeyT {
            composer.active = true;
            composer.channel = panel.active.channel_id();
            composer.history_offset = 0;
            panel.open = true;
            continue;
        }
        if !composer.active && event.key_code == KeyCode::KeyX {
            panel.open = false;
            composer.text.clear();
            continue;
        }
        if !composer.active {
            continue;
        }
        if event.key_code == KeyCode::Escape {
            composer.active = false;
            composer.text.clear();
        } else if event.key_code == KeyCode::Enter {
            if !composer.text.trim().is_empty() {
                let _ = client.send_chat_to(&composer.channel, &composer.text);
                composer.text.clear();
            }
            composer.active = false;
        } else if event.key_code == KeyCode::Backspace {
            composer.text.pop();
        } else if let Some(text) = &event.text {
            for character in text.chars().filter(|character| !character.is_control()) {
                if composer.text.chars().count() < game_api::MAX_MESSAGE_CHARS {
                    composer.text.push(character);
                }
            }
        }
    }
}

#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
fn draw(
    ctx: ImmCtx<CapsUi>,
    mut client: ResMut<OnlineClient>,
    mut composer: ResMut<ChatComposer>,
    mut panel: ResMut<ChatPanel>,
    memory: Res<WindowMemory>,
    windows: Query<&Window, With<PrimaryWindow>>,
) {
    ctx.build_immediate_root("online-chat-root")
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
                GlobalZIndex(20),
            )
        })
        .add(|ui| {
            if !panel.open {
                return;
            }
            let tabs = panel.tabs(&client);
            if !tabs.contains(&panel.active) {
                panel.active = ChatTab::Zone;
            }
            let stored = memory.get(CHAT_WINDOW_ID);
            let size = stored.map_or(CHAT_DEFAULT_SIZE, |state| state.size);
            let position = stored.map_or_else(
                || {
                    windows
                        .single()
                        .ok()
                        .map_or(Vec2::new(16.0, 120.0), |window| {
                            Vec2::new(16.0, (window.height() - size.y - 16.0).max(16.0))
                        })
                },
                |state| state.position,
            );
            let spec = WindowSpec {
                min_size: CHAT_MIN_SIZE,
                position,
                size,
                ..WindowSpec::new(CHAT_WINDOW_ID, "Chat", CHAT_DEFAULT_SIZE)
            };
            let max_lines = max_visible_lines(size.y);
            let outcome = show_window(ui.ch_id("chat-window"), spec, |ui| {
                chat_body(ui, &mut client, &mut panel, &mut composer, &tabs, max_lines);
            });
            if outcome.close_requested {
                panel.open = false;
                composer.active = false;
                composer.text.clear();
            }
        });
}

#[allow(clippy::too_many_lines)]
fn chat_body(
    ui: &mut Imm<CapsUi>,
    client: &mut OnlineClient,
    panel: &mut ChatPanel,
    composer: &mut ChatComposer,
    tabs: &[ChatTab],
    max_lines: usize,
) {
    let initial = panel.active.clone();
    panel.mark_read(client, &initial);

    ui.ch_id("tabs")
        .on_spawn_insert(|| Node {
            width: percent(100),
            flex_direction: FlexDirection::Row,
            flex_wrap: FlexWrap::Wrap,
            column_gap: px(4),
            row_gap: px(4),
            ..default()
        })
        .add(|ui| {
            for tab in tabs {
                let selected = tab == &initial;
                let unread = if selected {
                    0
                } else {
                    panel.unread(client, tab)
                };
                let mut button = ui
                    .ch_id(("chat-tab", tab.clone()))
                    .on_spawn_insert(|| {
                        (
                            bevy::ui_widgets::Button,
                            Node {
                                padding: UiRect::axes(px(8), px(4)),
                                border_radius: BorderRadius::all(px(4)),
                                ..default()
                            },
                            BackgroundColor(TAB_IDLE),
                        )
                    })
                    .background_color(if selected { TAB_ACTIVE } else { TAB_IDLE });
                let hovered = button.hovered();
                if hovered && !selected {
                    button = button.background_color(TAB_HOVER);
                }
                let clicked = button
                    .add(|ui| {
                        caption(ui, "caption", 13.0, tab_caption(tab, unread));
                    })
                    .primary_clicked();
                if clicked {
                    panel.active = tab.clone();
                    composer.history_offset = 0;
                    if let Some(target) = client
                        .chat_targets()
                        .iter()
                        .find(|target| crate::online::chat_target_alias(target) == tab.channel_id())
                        .cloned()
                    {
                        let _ = client.join_chat_target(&target);
                    }
                    if composer.active {
                        composer.channel = tab.channel_id();
                    }
                }
            }
        });

    let active = panel.active.clone();
    let messages: Vec<(String, String)> = client
        .chat()
        .messages()
        .filter(|message| active.matches(&message.channel_id))
        .map(|message| {
            (
                message.id.clone(),
                format!("{}: {}", message.sender_name, message.content),
            )
        })
        .collect();
    let history_offset = composer.history_offset.min(messages.len());
    let end = messages.len() - history_offset;
    let start = end.saturating_sub(max_lines);
    let visible = &messages[start..end];

    ui.ch_id("messages")
        .on_spawn_insert(|| {
            (
                Node {
                    width: percent(100),
                    flex_grow: 1.0,
                    min_height: px(0),
                    flex_direction: FlexDirection::Column,
                    row_gap: px(2),
                    padding: UiRect::all(px(6)),
                    border_radius: BorderRadius::all(px(4)),
                    overflow: Overflow::clip(),
                    ..default()
                },
                BackgroundColor(MESSAGE_BACKGROUND),
            )
        })
        .add(|ui| {
            if visible.is_empty() {
                label(ui, "empty", 13.0, "Sem mensagens neste canal.");
                return;
            }
            for (id, text) in visible {
                ui.ch_id(("chat-message", id.clone()))
                    .on_spawn_insert(|| {
                        (
                            TextFont {
                                font_size: FontSize::Px(14.0),
                                ..default()
                            },
                            TextLayout {
                                linebreak: LineBreak::WordBoundary,
                                ..default()
                            },
                            Node {
                                width: percent(100),
                                ..default()
                            },
                        )
                    })
                    .text(text.clone());
            }
        });

    let members = client.chat().members();
    let roster = members
        .iter()
        .take(6)
        .map(|member| member.character_id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let more = if client.chat().presence_truncated() || members.len() > 6 {
        ", …"
    } else {
        ""
    };
    let presence = if members.is_empty() {
        "Online: ninguém".to_owned()
    } else {
        format!("Online ({}): {roster}{more}", members.len())
    };
    ui.ch_id("presence")
        .on_spawn_insert(|| {
            (
                TextFont {
                    font_size: FontSize::Px(12.0),
                    ..default()
                },
                TextColor(PROMPT_TEXT),
                Node {
                    width: percent(100),
                    ..default()
                },
            )
        })
        .text(presence);

    if let Some(notice) = client.chat().notice() {
        ui.ch_id("notice")
            .on_spawn_insert(|| {
                (
                    TextFont {
                        font_size: FontSize::Px(12.0),
                        ..default()
                    },
                    TextColor(NOTICE_TEXT),
                    Node {
                        width: percent(100),
                        ..default()
                    },
                )
            })
            .text(notice);
    }

    ui.ch_id("composer")
        .on_spawn_insert(|| Node {
            width: percent(100),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: px(6),
            ..default()
        })
        .add(|ui| {
            let prompt = if composer.active {
                format!("> {}|", composer.text)
            } else {
                format!("T para conversar em {}", active.label())
            };
            ui.ch_id("prompt")
                .on_spawn_insert(|| {
                    (
                        TextFont {
                            font_size: FontSize::Px(14.0),
                            ..default()
                        },
                        TextColor(PROMPT_TEXT),
                        Node {
                            flex_grow: 1.0,
                            ..default()
                        },
                    )
                })
                .text(prompt);
            if composer.active {
                let send = button(ui, "send", "Enviar");
                let cancel = button(ui, "cancel", "Cancelar");
                if cancel {
                    composer.active = false;
                    composer.text.clear();
                }
                if send && !composer.text.trim().is_empty() {
                    let _ = client.send_chat_to(&composer.channel, &composer.text);
                    composer.text.clear();
                    composer.active = false;
                }
            }
        });
}

fn button(ui: &mut Imm<CapsUi>, id: &str, text: &str) -> bool {
    let mut button = ui.ch_id(id).on_spawn_insert(|| {
        (
            bevy::ui_widgets::Button,
            Node {
                padding: UiRect::axes(px(8), px(4)),
                border_radius: BorderRadius::all(px(4)),
                ..default()
            },
            BackgroundColor(TAB_IDLE),
        )
    });
    let hovered = button.hovered();
    button = button.background_color(if hovered { TAB_HOVER } else { TAB_IDLE });
    button
        .add(|ui| {
            caption(ui, "caption", 13.0, text);
        })
        .primary_clicked()
}

fn caption(ui: &mut Imm<CapsUi>, id: &str, size: f32, text: impl Into<String>) {
    ui.ch_id(id)
        .on_spawn_insert(move || {
            (
                TextFont {
                    font_size: FontSize::Px(size),
                    ..default()
                },
                TextLayout::default(),
            )
        })
        .text(text.into());
}

fn label(ui: &mut Imm<CapsUi>, id: &str, size: f32, text: impl Into<String>) {
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

fn max_visible_lines(window_height: f32) -> usize {
    // Title bar, tabs, presence, composer, padding, and gaps are roughly fixed.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let lines = ((window_height - 190.0) / 20.0).floor() as usize;
    lines.clamp(3, 30)
}

#[cfg_attr(coverage_nightly, coverage(off))]
#[cfg(test)]
mod tests {
    use super::{ChatPanel, ChatTab, max_visible_lines, tabs_from_channels};

    #[test]
    fn channel_ids_map_to_stable_tabs() {
        assert_eq!(ChatTab::from_channel_id("zone"), ChatTab::Zone);
        assert_eq!(ChatTab::from_channel_id("whisper"), ChatTab::Whisper);
        assert_eq!(ChatTab::from_channel_id("whisper:ana"), ChatTab::Whisper);
        assert_eq!(
            ChatTab::from_channel_id("group:core"),
            ChatTab::Group("core".to_owned())
        );
        assert_eq!(
            ChatTab::from_channel_id("party:raid"),
            ChatTab::Party("raid".to_owned())
        );
        assert_eq!(
            ChatTab::from_channel_id("custom-room"),
            ChatTab::Other("custom-room".to_owned())
        );
    }

    #[test]
    fn tabs_appear_as_channels_become_active() {
        let panel = ChatPanel::default();
        let base = tabs_from_channels(std::iter::empty(), &panel);
        assert_eq!(base, vec![ChatTab::Zone, ChatTab::Whisper]);

        let with_social =
            tabs_from_channels(["zone", "whisper:ana", "group:core", "party:raid"], &panel);
        assert!(with_social.contains(&ChatTab::Zone));
        assert!(with_social.contains(&ChatTab::Whisper));
        assert!(with_social.contains(&ChatTab::Group("core".to_owned())));
        assert!(with_social.contains(&ChatTab::Party("raid".to_owned())));
    }

    #[test]
    fn tab_matching_covers_scoped_channels() {
        assert!(ChatTab::Zone.matches("zone"));
        assert!(ChatTab::Whisper.matches("whisper:ana"));
        assert!(ChatTab::Whisper.matches("direct:ana"));
        assert!(ChatTab::Group("core".to_owned()).matches("group:core"));
        assert!(ChatTab::Group(String::new()).matches("group:core"));
        assert!(!ChatTab::Group("core".to_owned()).matches("group:other"));
        assert!(ChatTab::Party("raid".to_owned()).matches("party/raid"));
    }

    #[test]
    fn visible_line_count_tracks_window_height() {
        assert_eq!(max_visible_lines(120.0), 3);
        assert!(max_visible_lines(380.0) > max_visible_lines(260.0));
        assert_eq!(max_visible_lines(10_000.0), 30);
    }
}
