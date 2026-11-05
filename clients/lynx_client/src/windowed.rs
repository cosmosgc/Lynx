//! Window-only keyboard and 2D presentation adapters for the offline foundation.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy::{input::ButtonState, input::keyboard::KeyboardInput};

use crate::offline::{CameraPose, Control, OfflineScene, OfflineSet, PressedControls};
use crate::online::{OnlineClient, OnlineStatus};
use crate::reconciliation::Position;
use crate::replay::ReplayInput;
use crate::zone_scene::{SceneObjectKind, ZoneScene};
use engine_core::id::StableId;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

const SCALE: f32 = 32.0;
const INTERPOLATION_RATE: f32 = 14.0;
const SNAP_DISTANCE: f32 = 8.0 * SCALE;

/// Read-only presentation of the current server-owned public interest set.
#[derive(Debug)]
pub struct OnlineWindowPlugin;

impl Plugin for OnlineWindowPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ChatComposer>()
            .init_resource::<SelectedTarget>()
            // InputPlugin provides this in the windowed app; headless tests add
            // the same resource so the shared keyboard system can run.
            .init_resource::<bevy::ecs::message::Messages<KeyboardInput>>()
            .add_systems(Startup, setup_online)
            .add_systems(Update, select_target.after(crate::online::poll_online).before(chat_keyboard))
            .add_systems(Update, chat_keyboard.after(crate::online::poll_online))
            .add_systems(
                Update,
                online_movement.after(chat_keyboard).after(crate::online::smooth_online).before(present_online),
            )
            .add_systems(Update, present_online.after(crate::online::poll_online))
            .add_systems(Update, interpolate_online.after(present_online))
            .add_systems(Update, present_health.after(present_online))
            .add_systems(Update, present_interactions.after(present_online))
            .add_systems(Update, present_status.after(present_online))
            .add_systems(Update, present_target.after(present_online).after(select_target))
            .add_systems(
                Update,
                refresh_chat_presence.after(crate::online::poll_online),
            )
            .add_systems(Update, present_chat.after(crate::online::poll_online))
            .add_systems(
                Update,
                present_telemetry.after(crate::online::poll_online),
            );
    }
}

#[derive(Debug, Resource)]
struct ChatComposer {
    active: bool,
    text: String,
    history_offset: usize,
    presence_refresh: Timer,
}

impl Default for ChatComposer {
    fn default() -> Self {
        Self {
            active: false,
            text: String::new(),
            history_offset: 0,
            presence_refresh: Timer::from_seconds(5.0, TimerMode::Repeating),
        }
    }
}

#[derive(Component)]
struct ChatHud;

#[derive(Component)]
struct TelemetryHud;

#[derive(Component)]
struct OnlineEntity(StableId);

#[derive(Component)]
struct FacingIndicator(StableId);

/// Local UI selection only; does not grant permission to act on an entity.
#[derive(Resource, Default)]
struct SelectedTarget(Option<StableId>);

#[derive(Component)]
struct TargetRing(StableId);

#[derive(Component)]
struct HealthBar {
    entity: StableId,
    fill: bool,
}

#[derive(Component)]
struct InteractionMarker(StableId);

#[derive(Component)]
struct StatusMarker(StableId);

#[allow(clippy::needless_pass_by_value)]
fn present_interactions(
    client: Res<OnlineClient>,
    mut markers: Query<(&InteractionMarker, &mut Visibility)>,
) {
    let available: BTreeSet<_> = client
        .world()
        .entities()
        .filter(|entity| {
            entity
                .interaction
                .as_ref()
                .is_some_and(crate::world_state::PublicInteraction::is_available)
        })
        .map(|entity| entity.entity)
        .collect();
    for (marker, mut visibility) in &mut markers {
        *visibility = if available.contains(&marker.0) {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
    }
}

#[allow(clippy::needless_pass_by_value)]
fn present_status(client: Res<OnlineClient>, mut markers: Query<(&StatusMarker, &mut Visibility)>) {
    let stunned: BTreeSet<_> = client
        .world()
        .entities()
        .filter(|entity| entity.stunned)
        .map(|entity| entity.entity)
        .collect();
    for (marker, mut visibility) in &mut markers {
        *visibility = if stunned.contains(&marker.0) {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
    }
}

#[allow(clippy::cast_precision_loss)] // Pixel-scale ratio only; authoritative integers stay intact.
fn health_fraction(health: Option<crate::world_state::PublicHealth>) -> Option<f32> {
    health
        .filter(|health| health.max > 0)
        .map(|health| (health.current as f32 / health.max as f32).clamp(0.0, 1.0))
}

#[allow(clippy::needless_pass_by_value)]
fn present_health(
    client: Res<OnlineClient>,
    mut bars: Query<(&HealthBar, &mut Sprite, &mut Transform, &mut Visibility)>,
) {
    let health: BTreeMap<_, _> = client
        .world()
        .entities()
        .map(|entity| (entity.entity, health_fraction(entity.health)))
        .collect();
    for (bar, mut sprite, mut transform, mut visibility) in &mut bars {
        let fraction = health.get(&bar.entity).copied().flatten();
        *visibility = if fraction.is_some() {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if bar.fill {
            let width = 40.0 * fraction.unwrap_or(0.0);
            sprite.custom_size = Some(Vec2::new(width, 4.0));
            transform.translation.x = (width - 40.0) / 2.0;
        }
    }
}

fn health_bar_bundle(entity: StableId, fill: bool) -> impl Bundle {
    (
        HealthBar { entity, fill },
        Sprite::from_color(
            if fill {
                Color::srgb(0.2, 0.85, 0.35)
            } else {
                Color::srgb(0.08, 0.08, 0.1)
            },
            if fill {
                Vec2::new(40.0, 4.0)
            } else {
                Vec2::new(44.0, 8.0)
            },
        ),
        Transform::from_xyz(0.0, 42.0, if fill { 0.4 } else { 0.3 }),
        Visibility::Hidden,
    )
}

fn prototype_name(prototype: &engine_core::id::DefinitionId) -> String {
    let leaf = prototype
        .as_str()
        .rsplit('.')
        .next()
        .unwrap_or(prototype.as_str());
    leaf.split(['_', '-'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn cycle_target(
    current: Option<StableId>,
    candidates: &[StableId],
    reverse: bool,
) -> Option<StableId> {
    if candidates.is_empty() {
        return None;
    }
    let index = current.and_then(|id| candidates.iter().position(|candidate| *candidate == id));
    let next = match (index, reverse) {
        (Some(index), false) => (index + 1) % candidates.len(),
        (Some(index), true) => (index + candidates.len() - 1) % candidates.len(),
        (None, false) => 0,
        (None, true) => candidates.len() - 1,
    };
    Some(candidates[next])
}

#[allow(clippy::needless_pass_by_value)]
fn select_target(
    client: Res<OnlineClient>,
    keys: Res<ButtonInput<KeyCode>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    composer: Res<ChatComposer>,
    mut selected: ResMut<SelectedTarget>,
) {
    let mut candidates: Vec<_> = client
        .world()
        .entities()
        .map(|entity| entity.entity)
        .filter(|id| Some(*id) != client.world().actor())
        .collect();
    candidates.sort_unstable();
    if client.status() != OnlineStatus::Active {
        selected.0 = None;
        return;
    }
    if selected.0.is_some_and(|id| !candidates.contains(&id)) {
        selected.0 = None;
    }
    if composer.active || !windows.single().is_ok_and(|window| window.focused) {
        return;
    }
    if keys.just_pressed(KeyCode::Escape) {
        selected.0 = None;
    } else if keys.just_pressed(KeyCode::Tab) {
        selected.0 = cycle_target(
            selected.0,
            &candidates,
            keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight),
        );
    }
}

#[allow(clippy::needless_pass_by_value)]
fn present_target(selected: Res<SelectedTarget>, mut rings: Query<(&TargetRing, &mut Visibility)>) {
    for (ring, mut visibility) in &mut rings {
        *visibility = if selected.0 == Some(ring.0) {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
    }
}

/// Presentation-only interpolation between the previous and current
/// authoritative screen positions. This never writes back into world state.
#[derive(Component)]
struct OnlineMotion {
    to: Vec3,
}

fn setup_online(mut commands: Commands) {
    commands.spawn((Camera2d, SceneCamera));
    commands.spawn((
        ChatHud,
        Text::new("Chat da zona · T para escrever · Enter para enviar\nCarregando chat…"),
        Node {
            position_type: PositionType::Absolute,
            left: px(16),
            bottom: px(16),
            max_width: percent(60),
            padding: UiRect::all(px(10)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.03, 0.04, 0.06, 0.82)),
    ));
    commands.spawn((
        TelemetryHud,
        Text::new("online: connecting"),
        Node {
            position_type: PositionType::Absolute,
            left: px(16),
            top: px(16),
            padding: UiRect::all(px(10)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.03, 0.04, 0.06, 0.82)),
    ));
}

fn chat_keyboard(
    mut events: MessageReader<KeyboardInput>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut composer: ResMut<ChatComposer>,
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
            let before = client.chat().next_before();
            if let Some(before) = before {
                let _ = client.request_chat_history(50, Some(before));
                composer.history_offset = composer.history_offset.saturating_add(50);
            }
            continue;
        }
        if !composer.active && event.key_code == KeyCode::PageDown {
            composer.history_offset = composer.history_offset.saturating_sub(50);
            continue;
        }
        if !composer.active && event.key_code == KeyCode::KeyT {
            composer.active = true;
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
                let _ = client.send_chat(&composer.text);
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

/// Read held movement keys and issue one bounded intent at a time. The server
/// remains authoritative: every endpoint is a local prediction until the
/// correlation result arrives. Movement is suppressed while composing chat.
#[allow(clippy::needless_pass_by_value)] // Bevy systems take resources by value.
fn online_movement(
    keys: Res<ButtonInput<KeyCode>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    composer: Res<ChatComposer>,
    mut client: ResMut<OnlineClient>,
    time: Res<Time>,
    mut cooldown: Local<f32>,
) {
    *cooldown = (*cooldown - time.delta_secs()).max(0.0);
    if !composer.active
        && windows.single().is_ok_and(|window| window.focused)
        && keys.just_pressed(KeyCode::KeyR)
        && matches!(client.status(), OnlineStatus::Failed(_))
    {
        let _ = client.reconnect();
        return;
    }
    if composer.active
        || !windows.single().is_ok_and(|window| window.focused)
        || client.status() != OnlineStatus::Active
        || client.pending_movement()
        || *cooldown > 0.0
    {
        return;
    }
    let Some(authority) = client.world().reconciliation().authoritative_position() else {
        return;
    };
    let Some(destination) = movement_destination(authority, &keys, client.movement_step()) else {
        return;
    };
    if client.move_to(destination).is_ok() {
        *cooldown = 0.05;
    }
}

/// Pure direction sampler, kept separate so it can be tested without a client.
fn movement_destination(
    position: Position,
    keys: &ButtonInput<KeyCode>,
    step: f32,
) -> Option<Position> {
    let mut x = 0.0_f32;
    let mut z = 0.0_f32;
    if keys.pressed(KeyCode::KeyW) {
        z -= 1.0;
    }
    if keys.pressed(KeyCode::KeyS) {
        z += 1.0;
    }
    if keys.pressed(KeyCode::KeyA) {
        x -= 1.0;
    }
    if keys.pressed(KeyCode::KeyD) {
        x += 1.0;
    }
    if x == 0.0 && z == 0.0 {
        return None;
    }
    let length = x.hypot(z);
    Some(Position {
        x: position.x + x / length * step,
        y: position.y,
        z: position.z + z / length * step,
    })
}

#[allow(clippy::needless_pass_by_value)] // Bevy systems take resources by value.
fn refresh_chat_presence(
    time: Res<Time>,
    mut composer: ResMut<ChatComposer>,
    mut client: ResMut<OnlineClient>,
) {
    composer.presence_refresh.tick(time.delta());
    if composer.presence_refresh.just_finished() {
        let _ = client.request_chat_presence();
    }
}

#[allow(clippy::needless_pass_by_value)] // Bevy systems take resources by value.
fn present_chat(
    client: Res<OnlineClient>,
    composer: Res<ChatComposer>,
    mut hud: Query<&mut Text, With<ChatHud>>,
) {
    let client = client.into_inner();
    let messages: Vec<_> = client.chat().messages().collect();
    let end = messages.len().saturating_sub(composer.history_offset);
    let start = end.saturating_sub(5);
    let lines: Vec<String> = messages[start..end]
        .iter()
        .map(|message| format!("{}: {}", message.sender_name, message.content))
        .collect();
    let mut display = if lines.is_empty() {
        "Sem mensagens recentes".to_owned()
    } else {
        lines.join("\n")
    };
    let members = client.chat().members();
    let roster = members
        .iter()
        .take(6)
        .map(|member| member.character_id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    if client.chat().presence_truncated() || members.len() > 6 {
        let _ = write!(display, "\nOnline: {roster}, …");
    } else {
        let _ = write!(display, "\nOnline: {roster}");
    }
    if let Some(notice) = client.chat().notice() {
        let _ = write!(display, "\n{notice}");
    }
    if composer.active {
        let _ = write!(display, "\n> {}▏", composer.text);
    } else {
        display.push_str("\nT para conversar · PageUp para mensagens anteriores");
    }
    for mut text in &mut hud {
        text.0.clone_from(&display);
    }
}

// Clamp before scaling so even a finite but extreme server coordinate cannot
// produce an infinite Bevy transform.
fn screen_position(x: f32, z: f32) -> Vec3 {
    Vec3::new(
        x.clamp(-1_000_000.0, 1_000_000.0) * SCALE,
        -z.clamp(-1_000_000.0, 1_000_000.0) * SCALE,
        1.0,
    )
}

fn status_label(status: OnlineStatus) -> String {
    match status {
        OnlineStatus::Disconnected => "disconnected".to_owned(),
        OnlineStatus::Connecting => "connecting".to_owned(),
        OnlineStatus::AwaitingSnapshot => "awaiting snapshot".to_owned(),
        OnlineStatus::Active => "active".to_owned(),
        OnlineStatus::Reconnecting { attempt } => format!("reconnecting (attempt {attempt})"),
        OnlineStatus::Failed(error) => format!("connection failed: {}", failure_label(error)),
    }
}

fn failure_label(error: crate::online::OnlineError) -> &'static str {
    use crate::online::OnlineError;
    match error {
        OnlineError::SessionExpired
        | OnlineError::Transport(lynx_client_net::Failure::Authentication) => {
            "credentials expired or revoked"
        }
        OnlineError::Conflict => "another session is still attached",
        OnlineError::Transport(lynx_client_net::Failure::CharacterForbidden) => {
            "selected character is unavailable"
        }
        OnlineError::Transport(lynx_client_net::Failure::Unavailable) => {
            "world service unavailable"
        }
        OnlineError::Transport(lynx_client_net::Failure::Timeout) => "connection timed out",
        OnlineError::Protocol | OnlineError::State(_) => "server/client protocol state error",
        OnlineError::Transport(_) | OnlineError::Closed | OnlineError::Send(_) => {
            "connection could not be established"
        }
    }
}

fn recovery_hint(status: OnlineStatus) -> Option<&'static str> {
    match status {
        OnlineStatus::Failed(
            crate::online::OnlineError::SessionExpired
            | crate::online::OnlineError::Transport(lynx_client_net::Failure::Authentication),
        ) => Some("Restart Lynx and sign in again. Press R to retry current credentials."),
        OnlineStatus::Failed(crate::online::OnlineError::Conflict) => {
            Some("Wait for the other session to close, then press R to retry.")
        }
        OnlineStatus::Failed(crate::online::OnlineError::Transport(
            lynx_client_net::Failure::CharacterForbidden,
        )) => Some("Return to character selection, choose an available character, and reconnect."),
        OnlineStatus::Failed(_) => Some("Check the server connection, then press R to retry."),
        _ => None,
    }
}

fn position_label(position: Position) -> String {
    format!("{:.2}, {:.2}, {:.2}", position.x, position.y, position.z)
}

#[allow(clippy::needless_pass_by_value)] // Bevy systems take resources by value.
fn present_telemetry(client: Res<OnlineClient>, mut hud: Query<&mut Text, With<TelemetryHud>>) {
    let state = client.world();
    let reconciliation = state.reconciliation();
    let tick = state
        .tick()
        .map_or_else(|| "-".to_owned(), |tick| tick.to_string());
    let rtt = client.last_round_trip().map_or_else(
        || "-".to_owned(),
        |duration| format!("{} ms", duration.as_millis()),
    );
    let movement = match client.last_movement_applied() {
        Some(true) => "accepted",
        Some(false) => "corrected",
        None => "-",
    };
    let authority = reconciliation
        .authoritative_position()
        .map_or_else(|| "-".to_owned(), position_label);
    let rendered = reconciliation
        .presentation_position()
        .map_or_else(|| "-".to_owned(), position_label);
    let status = client.status();
    let recovery =
        recovery_hint(status).map_or_else(String::new, |hint| format!("\nRecovery: {hint}"));
    let retry_control = if matches!(status, OnlineStatus::Failed(_)) {
        " · R: retry"
    } else {
        ""
    };
    let display = format!(
        "WASD: move · T: chat{retry_control}\nonline: {}{recovery}\ntick {tick} · entities {} · move pending {}\nmove round trip {rtt} · step {:.3} m · last move {movement}\nresume {} · reconnects {} · resumed {}\nauthority {authority}\nrendered {rendered}\nqueues (in, out): {:?} · packet loss: unavailable (WebSocket)",
        status_label(status),
        state.entities().count(),
        if client.pending_movement() {
            "yes"
        } else {
            "no"
        },
        client.movement_step(),
        if client.resume_available() {
            "ready"
        } else {
            "none"
        },
        client.reconnect_attempts(),
        if client.resumed() { "yes" } else { "no" },
        client.queue_depths(),
    );
    for mut text in &mut hud {
        text.0.clone_from(&display);
    }
}

#[allow(clippy::needless_pass_by_value, clippy::type_complexity)] // Bevy system parameters must be passed by value.
#[allow(clippy::too_many_lines)] // Spawn bundles stay beside entity synchronization logic.
fn present_online(
    mut commands: Commands,
    client: Res<OnlineClient>,
    mut entities: Query<
        (Entity, &OnlineEntity, &mut Transform, &mut OnlineMotion),
        Without<SceneCamera>,
    >,
    mut cameras: Query<&mut Transform, (With<SceneCamera>, Without<OnlineEntity>)>,
    mut windows: Query<&mut Window>,
    mut indicators: Query<
        (&FacingIndicator, &mut Transform),
        (Without<SceneCamera>, Without<OnlineEntity>),
    >,
) {
    let client = client.into_inner();
    let state = client.world();
    let public: BTreeMap<_, _> = state
        .entities()
        .map(|entity| (entity.entity, entity))
        .collect();
    let actor = state.actor();
    let predicted = state.reconciliation().presentation_position();
    for (indicator, mut transform) in &mut indicators {
        if let Some(public) = public.get(&indicator.0) {
            *transform = facing_transform(public.yaw_degrees);
        }
    }
    let mut missing: BTreeSet<_> = public.keys().copied().collect();
    for (entity, id, mut transform, mut motion) in &mut entities {
        if !public.contains_key(&id.0) {
            commands.entity(entity).despawn();
            continue;
        }
        missing.remove(&id.0);
        let target = if Some(id.0) == actor {
            // Local prediction is already smoothed by reconciliation; keep the
            // avatar and camera locked instead of interpolating it twice.
            predicted.map(|position| screen_position(position.x, position.z))
        } else {
            public
                .get(&id.0)
                .map(|public| screen_position(public.position.x, public.position.z))
        };
        let Some(target) = target else { continue };
        if Some(id.0) == actor {
            transform.translation = target;
            motion.to = target;
        } else if motion.to.distance_squared(target) > 1e-4 {
            // Interpolate from what is currently rendered, never from an
            // extrapolated future position.
            motion.to = target;
        }
    }
    for public in missing.iter().filter_map(|id| public.get(id)) {
        let target = if Some(public.entity) == actor {
            predicted.map_or_else(
                || screen_position(public.position.x, public.position.z),
                |position| screen_position(position.x, position.z),
            )
        } else {
            screen_position(public.position.x, public.position.z)
        };
        let local = Some(public.entity) == actor;
        let colour = if local {
            Color::srgb(0.3, 0.85, 1.0)
        } else {
            Color::srgb(0.95, 0.75, 0.3)
        };
        commands
            .spawn((
                Sprite::from_color(colour, Vec2::splat(16.0)),
                Transform::from_translation(target),
                OnlineEntity(public.entity),
                OnlineMotion { to: target },
            ))
            .with_children(|parent| {
                parent.spawn(health_bar_bundle(public.entity, false));
                parent.spawn(health_bar_bundle(public.entity, true));
                parent.spawn((
                    Text2d::new(prototype_name(&public.prototype)),
                    TextFont {
                        font_size: bevy::text::FontSize::Px(14.0),
                        ..default()
                    },
                    TextColor(Color::srgb(0.95, 0.95, 0.9)),
                    Transform::from_xyz(0.0, 64.0, 0.6),
                ));
                parent.spawn((
                    InteractionMarker(public.entity),
                    Sprite::from_color(Color::srgb(1.0, 0.82, 0.25), Vec2::splat(8.0)),
                    Transform::from_xyz(0.0, 54.0, 0.5),
                    Visibility::Hidden,
                ));
                parent.spawn((
                    StatusMarker(public.entity),
                    Text2d::new("STUN"),
                    TextFont {
                        font_size: bevy::text::FontSize::Px(10.0),
                        ..default()
                    },
                    TextColor(Color::srgb(1.0, 0.35, 0.2)),
                    Transform::from_xyz(30.0, 52.0, 0.6),
                    Visibility::Hidden,
                ));
                // Segmented ground ring works with the existing sprite renderer.
                // It is presentation only, not a collision/range boundary.
                for segment in 0..32_u16 {
                    let angle = f32::from(segment) * std::f32::consts::TAU / 32.0;
                    parent.spawn((
                        TargetRing(public.entity),
                        Sprite::from_color(Color::srgb(1.0, 0.35, 0.2), Vec2::new(6.0, 3.0)),
                        ground_ring_transform(angle, 33.0, 0.2),
                        Visibility::Hidden,
                    ));
                    parent.spawn((
                        Sprite::from_color(colour, Vec2::new(4.0, 2.0)),
                        ground_ring_transform(angle, 21.0, -0.1),
                    ));
                }
                parent.spawn((
                    FacingIndicator(public.entity),
                    Sprite::from_color(colour, Vec2::new(5.0, 12.0)),
                    facing_transform(public.yaw_degrees),
                ));
            });
    }
    if let Some(position) = predicted {
        for mut camera in &mut cameras {
            let position = screen_position(position.x, position.z);
            camera.translation.x = position.x;
            camera.translation.y = position.y;
        }
    }
    let title = format!(
        "Lynx demo online - {} - tick {}",
        status_label(client.status()),
        state
            .tick()
            .map_or_else(|| "-".to_owned(), |tick| tick.to_string())
    );
    for mut window in &mut windows {
        window.title.clone_from(&title);
    }
}

/// World yaw is clockwise from +Z toward +X; screen Y is negative world Z.
fn facing_transform(yaw_degrees: f32) -> Transform {
    let angle = yaw_degrees.rem_euclid(360.0).to_radians();
    Transform::from_xyz(angle.sin() * 26.0, -angle.cos() * 26.0, 0.1)
        .with_rotation(Quat::from_rotation_z(angle))
}

fn ground_ring_transform(angle: f32, radius: f32, depth: f32) -> Transform {
    Transform::from_xyz(angle.cos() * radius, angle.sin() * radius, depth)
        .with_rotation(Quat::from_rotation_z(angle + std::f32::consts::FRAC_PI_2))
}

/// Advance remote visuals toward the latest authoritative target. Interpolation
/// never extrapolates past that target, and large discontinuities snap.
#[allow(clippy::needless_pass_by_value)] // Bevy systems take resources by value.
fn interpolate_online(
    time: Res<Time>,
    mut entities: Query<(&mut Transform, &OnlineMotion), Without<SceneCamera>>,
) {
    let fraction = (time.delta_secs() * INTERPOLATION_RATE).clamp(0.0, 1.0);
    for (mut transform, motion) in &mut entities {
        if fraction >= 1.0
            || transform.translation.distance_squared(motion.to) > SNAP_DISTANCE * SNAP_DISTANCE
        {
            transform.translation = motion.to;
        } else {
            transform.translation = transform.translation.lerp(motion.to, fraction);
        }
    }
}

#[derive(Debug)]
pub struct OfflineWindowPlugin;

impl Plugin for OfflineWindowPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_scene)
            .add_systems(Update, keyboard_input.in_set(OfflineSet::Input))
            .add_systems(Update, present_scene.in_set(OfflineSet::Presentation));
    }
}

#[derive(Component)]
struct Avatar;

#[derive(Component)]
struct SceneCamera;

/// Marks a sprite whose transform is derived from an authored scene position.
#[derive(Component)]
struct WorldPosition(Vec3);

/// Sprite colour and size for a scene object category.
fn object_style(kind: SceneObjectKind) -> (Color, f32, f32) {
    match kind {
        SceneObjectKind::Prop => (Color::srgb(0.58, 0.40, 0.22), 0.9, 0.6),
        SceneObjectKind::Character => (Color::srgb(0.95, 0.78, 0.30), 0.8, 0.8),
        SceneObjectKind::Interactable => (Color::srgb(0.90, 0.62, 0.18), 0.7, 0.6),
    }
}

#[allow(clippy::needless_pass_by_value)] // Bevy system parameters must be passed by value.
fn setup_scene(mut commands: Commands, zone: Res<ZoneScene>, mut scene: ResMut<OfflineScene>) {
    commands.spawn((Camera2d, SceneCamera));
    scene.avatar_position = zone.bounds.clamp(zone.spawn);

    // Terrain and authored objects are placeholder sprites for the current 2D
    // renderer. Assets verified by the content policy are not decoded yet.
    for (index, tile) in zone.terrain.iter().enumerate() {
        let colour = if index % 2 == 0 {
            Color::srgb(0.13, 0.22, 0.16)
        } else {
            Color::srgb(0.15, 0.25, 0.18)
        };
        commands.spawn((
            Sprite::from_color(colour, Vec2::splat(tile.size * SCALE)),
            Transform::from_translation(Vec3::new(0.0, 0.0, -1.0)),
            WorldPosition(tile.position),
        ));
    }
    for object in &zone.objects {
        let (colour, size, z) = object_style(object.kind);
        commands.spawn((
            Sprite::from_color(colour, Vec2::splat(size * SCALE)),
            Transform::from_translation(Vec3::new(0.0, 0.0, z)),
            WorldPosition(object.position),
        ));
    }
    commands.spawn((
        Sprite::from_color(Color::srgb(0.3, 0.7, 1.0), Vec2::splat(24.0)),
        Transform::from_xyz(0.0, 0.0, 1.0),
        Avatar,
    ));
}

#[allow(clippy::needless_pass_by_value)] // Bevy system parameters must be passed by value.
fn keyboard_input(
    keys: Res<ButtonInput<KeyCode>>,
    replay: Option<Res<ReplayInput>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut pressed: ResMut<PressedControls>,
) {
    const KEYS: [(KeyCode, Control); 8] = [
        (KeyCode::KeyW, Control::W),
        (KeyCode::KeyA, Control::A),
        (KeyCode::KeyS, Control::S),
        (KeyCode::KeyD, Control::D),
        (KeyCode::KeyE, Control::E),
        (KeyCode::Enter, Control::Enter),
        (KeyCode::KeyI, Control::I),
        (KeyCode::KeyC, Control::C),
    ];
    if replay.is_some() {
        return;
    }
    // Keyboard state is global; do not move the offline avatar while another
    // window has focus. Replay is deliberately independent of window focus.
    if !windows.single().is_ok_and(|window| window.focused) {
        pressed.0.clear();
        return;
    }
    let keys = keys.into_inner();
    pressed.0 = KEYS
        .iter()
        .filter_map(|(key, control)| keys.pressed(*key).then_some(*control))
        .collect();
}

/// Project a world position into camera-relative 2D pixels. The primary follow
/// mode uses the slice's closer scale; the billboard fallback is slightly wider.
fn project(world: Vec3, pose: &CameraPose) -> Vec3 {
    let scale = match pose.mode {
        crate::offline::CameraMode::Follow3d => SCALE,
        crate::offline::CameraMode::Billboard2d => SCALE * 0.75,
    };
    Vec3::new(
        (world.x - pose.target.x) * scale,
        -(world.z - pose.target.z) * scale,
        0.0,
    )
}

#[allow(clippy::needless_pass_by_value, clippy::type_complexity)] // Bevy system parameters must be passed by value.
fn present_scene(
    zone: Res<ZoneScene>,
    scene: Res<OfflineScene>,
    pose: Res<CameraPose>,
    mut avatars: Query<&mut Transform, (With<Avatar>, Without<SceneCamera>)>,
    mut objects: Query<(&WorldPosition, &mut Transform), (Without<Avatar>, Without<SceneCamera>)>,
    mut cameras: Query<
        &mut Transform,
        (With<SceneCamera>, Without<Avatar>, Without<WorldPosition>),
    >,
    mut windows: Query<&mut Window>,
) {
    let scene = scene.into_inner();
    let pose = pose.into_inner();
    for mut transform in &mut avatars {
        let projected = project(scene.avatar_position, pose);
        transform.translation.x = projected.x;
        transform.translation.y = projected.y;
    }
    for (world, mut transform) in &mut objects {
        let projected = project(world.0, pose);
        transform.translation.x = projected.x;
        transform.translation.y = projected.y;
    }
    for mut transform in &mut cameras {
        // Objects are already camera-relative, so the camera stays at the origin.
        transform.translation.x = 0.0;
        transform.translation.y = 0.0;
    }
    if let Some(mut window) = windows.iter_mut().next() {
        let version = zone
            .content_version
            .as_deref()
            .map_or("source".to_owned(), |version| format!("v{version}"));
        let suffix = if zone.issue_count() == 0 {
            String::new()
        } else {
            format!(" - {} warning(s)", zone.issue_count())
        };
        window.title = format!(
            "Lynx offline - {} [{}] {} - camera {}{suffix}",
            zone.name,
            zone.zone_id,
            version,
            pose.mode.as_str()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_failure_text_points_to_specific_recovery_action() {
        use crate::online::OnlineError;
        assert!(
            recovery_hint(OnlineStatus::Failed(OnlineError::SessionExpired))
                .unwrap()
                .contains("sign in again")
        );
        assert!(
            recovery_hint(OnlineStatus::Failed(OnlineError::Conflict))
                .unwrap()
                .contains("other session")
        );
        assert!(
            recovery_hint(OnlineStatus::Failed(OnlineError::Transport(
                lynx_client_net::Failure::CharacterForbidden,
            )))
            .unwrap()
            .contains("character selection")
        );
        assert!(recovery_hint(OnlineStatus::Active).is_none());
    }

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

    #[test]
    fn interaction_marker_visibility_tracks_advertised_availability() {
        use crate::world_state::PublicInteraction;
        assert!(
            PublicInteraction {
                verbs: vec![serde_json::json!("talk")],
                enabled: true
            }
            .is_available()
        );
        assert!(
            !PublicInteraction {
                verbs: vec![serde_json::json!("talk")],
                enabled: false
            }
            .is_available()
        );
        assert!(
            !PublicInteraction {
                verbs: vec![],
                enabled: true
            }
            .is_available()
        );
    }

    #[test]
    fn only_selected_ring_is_visible_and_clearing_hides_it() {
        use engine_core::id::IdKind;
        let id = StableId::from_parts(IdKind::Entity, 1, 1);
        let other = StableId::from_parts(IdKind::Entity, 1, 2);
        let mut app = App::new();
        app.insert_resource(SelectedTarget(Some(id)))
            .add_systems(Update, present_target);
        let selected = app
            .world_mut()
            .spawn((TargetRing(id), Visibility::Hidden))
            .id();
        let unselected = app
            .world_mut()
            .spawn((TargetRing(other), Visibility::Inherited))
            .id();
        app.update();
        assert_eq!(
            app.world().get::<Visibility>(selected),
            Some(&Visibility::Inherited)
        );
        assert_eq!(
            app.world().get::<Visibility>(unselected),
            Some(&Visibility::Hidden)
        );
        app.world_mut().resource_mut::<SelectedTarget>().0 = None;
        app.update();
        assert_eq!(
            app.world().get::<Visibility>(selected),
            Some(&Visibility::Hidden)
        );
    }

    #[test]
    fn target_cycle_wraps_in_both_directions_and_handles_stale_selection() {
        use engine_core::id::IdKind;
        let ids = [1, 2, 3].map(|id| StableId::from_parts(IdKind::Entity, 1, id));
        assert_eq!(cycle_target(None, &ids, false), Some(ids[0]));
        assert_eq!(cycle_target(None, &ids, true), Some(ids[2]));
        assert_eq!(cycle_target(Some(ids[2]), &ids, false), Some(ids[0]));
        assert_eq!(cycle_target(Some(ids[0]), &ids, true), Some(ids[2]));
        assert_eq!(cycle_target(Some(ids[0]), &ids[1..], false), Some(ids[1]));
        assert_eq!(cycle_target(Some(ids[0]), &[], false), None);
        assert_eq!(cycle_target(Some(ids[0]), &ids[..1], false), Some(ids[0]));
    }

    #[test]
    fn facing_projects_server_yaw_into_ground_plane() {
        for (yaw, expected) in [
            (0.0, Vec2::new(0.0, -26.0)),
            (90.0, Vec2::new(26.0, 0.0)),
            (180.0, Vec2::new(0.0, 26.0)),
            (270.0, Vec2::new(-26.0, 0.0)),
            (450.0, Vec2::new(26.0, 0.0)),
        ] {
            assert!(
                facing_transform(yaw)
                    .translation
                    .truncate()
                    .distance(expected)
                    < 0.001
            );
        }
    }
    use crate::{
        headless_app,
        offline::{Action, ActiveActions, OfflineInputEnabled},
    };

    #[test]
    fn keyboard_drives_scene_and_camera_in_same_update_and_releases() {
        let mut app = headless_app();
        app.init_resource::<ButtonInput<KeyCode>>()
            .add_plugins(OfflineWindowPlugin);
        app.world_mut().spawn((Window::default(), PrimaryWindow));
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyW);
        app.update();
        assert!(
            app.world()
                .resource::<ActiveActions>()
                .0
                .contains(&Action::MoveForward)
        );
        assert_eq!(
            app.world().resource::<OfflineScene>().avatar_position,
            Vec3::NEG_Z
        );
        let world = app.world_mut();
        let avatar = world
            .query_filtered::<&Transform, With<Avatar>>()
            .single(world)
            .unwrap();
        assert_eq!(avatar.translation.y, 0.0);
        let camera = world
            .query_filtered::<&Transform, With<SceneCamera>>()
            .single(world)
            .unwrap();
        assert_eq!(camera.translation.y, 0.0);
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .release(KeyCode::KeyW);
        app.update();
        assert!(app.world().resource::<ActiveActions>().0.is_empty());
        assert_eq!(
            app.world().resource::<OfflineScene>().avatar_position,
            Vec3::NEG_Z
        );
    }

    #[test]
    fn held_keyboard_input_resumes_after_gate_reopens() {
        let mut app = headless_app();
        app.init_resource::<ButtonInput<KeyCode>>()
            .add_plugins(OfflineWindowPlugin);
        app.world_mut().spawn((Window::default(), PrimaryWindow));
        app.world_mut().resource_mut::<OfflineInputEnabled>().0 = false;
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyW);
        app.update();
        assert_eq!(
            app.world().resource::<OfflineScene>().avatar_position,
            Vec3::ZERO
        );
        app.world_mut().resource_mut::<OfflineInputEnabled>().0 = true;
        app.update();
        assert_eq!(
            app.world().resource::<OfflineScene>().avatar_position,
            Vec3::NEG_Z
        );
    }

    #[test]
    fn losing_focus_releases_keyboard_actions_before_movement() {
        use crate::offline::ActionTransitions;

        let mut app = headless_app();
        app.init_resource::<ButtonInput<KeyCode>>()
            .add_plugins(OfflineWindowPlugin);
        let window = app
            .world_mut()
            .spawn((Window::default(), PrimaryWindow))
            .id();
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyW);
        app.update();
        app.world_mut().get_mut::<Window>(window).unwrap().focused = false;
        app.update();
        assert!(app.world().resource::<ActiveActions>().0.is_empty());
        assert!(
            app.world()
                .resource::<ActionTransitions>()
                .released
                .contains(&Action::MoveForward)
        );
        assert_eq!(
            app.world().resource::<OfflineScene>().avatar_position,
            Vec3::NEG_Z
        );
        app.update();
        assert!(
            app.world()
                .resource::<ActionTransitions>()
                .released
                .is_empty()
        );
        app.world_mut().get_mut::<Window>(window).unwrap().focused = true;
        app.update();
        assert_eq!(
            app.world().resource::<OfflineScene>().avatar_position,
            Vec3::NEG_Z * 2.0
        );
    }

    #[test]
    fn replay_is_not_suppressed_by_missing_or_unfocused_window() {
        use crate::replay::ReplayFixture;

        let mut app = headless_app();
        app.init_resource::<ButtonInput<KeyCode>>()
            .add_plugins(OfflineWindowPlugin)
            .insert_resource(ReplayInput::new(
                ReplayFixture::from_json(r#"{"frames":[{"pressed":["W"]},{"pressed":["D"]}]}"#)
                    .unwrap(),
            ));
        app.update();
        app.world_mut().spawn((
            Window {
                focused: false,
                ..default()
            },
            PrimaryWindow,
        ));
        app.update();
        assert_eq!(
            app.world().resource::<OfflineScene>().avatar_position,
            Vec3::new(1.0, 0.0, -1.0)
        );
    }

    #[test]
    fn startup_spawns_terrain_props_characters_and_interactables() {
        let mut app = headless_app();
        app.init_resource::<ButtonInput<KeyCode>>()
            .add_plugins(OfflineWindowPlugin);
        app.update();
        let world = app.world_mut();
        let terrain = world
            .query_filtered::<&WorldPosition, Without<Avatar>>()
            .iter(world)
            .count();
        assert!(terrain > 6, "terrain and objects should be spawned");
        let avatar = world
            .query_filtered::<&Transform, With<Avatar>>()
            .single(world)
            .unwrap();
        assert_eq!(avatar.translation, Vec3::new(0.0, 0.0, 1.0));
    }

    #[test]
    fn billboard_mode_uses_the_documented_fallback_scale() {
        let follow = CameraPose {
            target: Vec3::ZERO,
            ..default()
        };
        let billboard = CameraPose {
            mode: crate::offline::CameraMode::Billboard2d,
            target: Vec3::ZERO,
            ..default()
        };
        assert_eq!(project(Vec3::X, &follow), Vec3::new(SCALE, 0.0, 0.0));
        assert_eq!(
            project(Vec3::X, &billboard),
            Vec3::new(SCALE * 0.75, 0.0, 0.0)
        );
    }

    #[test]
    fn movement_destination_is_normalized_bounded_and_height_preserving() {
        let origin = Position {
            x: 1.0,
            y: 2.0,
            z: 3.0,
        };
        let mut keys = ButtonInput::<KeyCode>::default();
        assert_eq!(movement_destination(origin, &keys, 0.09), None);
        keys.press(KeyCode::KeyW);
        let up = movement_destination(origin, &keys, 0.09).unwrap();
        assert_eq!(up.y, origin.y);
        assert!((up.z - (origin.z - 0.09)).abs() < 1e-6);
        keys.press(KeyCode::KeyD);
        let diagonal = movement_destination(origin, &keys, 0.09).unwrap();
        let dx = diagonal.x - origin.x;
        let dz = diagonal.z - origin.z;
        assert!((dx.hypot(dz) - 0.09).abs() < 1e-6);
        assert!((dx - 0.09 / 2.0_f32.sqrt()).abs() < 1e-6);
        assert!((dz + 0.09 / 2.0_f32.sqrt()).abs() < 1e-6);
    }

    #[test]
    fn interpolation_never_extrapolates_past_the_authoritative_target() {
        let mut app = headless_app();
        app.insert_resource(OnlineClient::new(1.0).unwrap())
            .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
                std::time::Duration::from_millis(20),
            ))
            .init_resource::<ButtonInput<KeyCode>>()
            .add_plugins(OnlineWindowPlugin);
        let entity = app
            .world_mut()
            .spawn((
                Transform::from_translation(Vec3::ZERO),
                OnlineMotion {
                    to: Vec3::new(10.0, 0.0, 0.0),
                },
            ))
            .id();
        app.update();
        let transform = app.world().get::<Transform>(entity).unwrap();
        assert!(transform.translation.x >= 0.0);
        assert!(transform.translation.x <= 10.0);
        app.update();
        let transform = app.world().get::<Transform>(entity).unwrap();
        assert!(transform.translation.x > 0.0 && transform.translation.x < 10.0);
        app.world_mut().get_mut::<OnlineMotion>(entity).unwrap().to = Vec3::X * 1000.0;
        app.update();
        assert_eq!(
            app.world().get::<Transform>(entity).unwrap().translation,
            Vec3::X * 1000.0
        );
    }
}
