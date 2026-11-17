//! Window-only keyboard and presentation adapters for the client.

use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::input::keyboard::KeyboardInput;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::entity_info::{
    EntityInfoAssets, EntityInfoBackend, EntityInfoPlugin, EntityInfoSet, spawn_entity_info,
};
use crate::offline::{CameraPose, Control, OfflineScene, OfflineSet, PressedControls};
use crate::online::{OnlineClient, OnlineStatus};
use crate::reconciliation::Position;
use crate::replay::ReplayInput;
use crate::ui::chat::{ChatComposer, chat_keyboard};
use crate::zone_scene::{SceneObjectKind, ZoneScene};
use engine_core::id::StableId;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

const INTERPOLATION_RATE: f32 = 14.0;
const SNAP_DISTANCE: f32 = 8.0;

/// Read-only presentation of the current server-owned public interest set.
#[derive(Debug)]
pub struct OnlineWindowPlugin;

impl Plugin for OnlineWindowPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(EntityInfoPlugin)
            .init_resource::<MouseCamera>()
            .init_resource::<bevy::input::mouse::AccumulatedMouseMotion>()
            .init_resource::<bevy::input::mouse::AccumulatedMouseScroll>()
            .init_resource::<ButtonInput<MouseButton>>()
            .add_systems(
                Update,
                mouse_controls
                    .after(crate::options::toggle_menu)
                    .before(select_target),
            )
            .init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<StandardMaterial>>()
            .init_resource::<ChatComposer>()
            .init_resource::<LockComposer>()
            .init_resource::<SelectedTarget>()
            // InputPlugin provides this in the windowed app; headless tests add
            // the same resource so the shared keyboard system can run.
            .init_resource::<bevy::ecs::message::Messages<KeyboardInput>>()
            .add_systems(Startup, setup_online)
            .add_systems(
                Update,
                select_target
                    .after(crate::options::toggle_menu)
                    .after(crate::online::poll_online)
                    .before(chat_keyboard),
            )
            .add_systems(
                Update,
                interaction_keyboard
                    .after(crate::options::toggle_menu)
                    .after(select_target)
                    .before(chat_keyboard),
            )
            .add_systems(Update, present_interaction_hud.after(interaction_keyboard))
            .add_systems(
                Update,
                online_movement
                    .after(crate::options::toggle_menu)
                    .after(chat_keyboard)
                    .after(crate::online::smooth_online)
                    .before(present_online),
            )
            .add_systems(Update, present_online.after(crate::online::poll_online))
            .add_systems(Update, interpolate_online.after(present_online))
            // Info follows the interpolated body so bars and labels never lag.
            .configure_sets(
                Update,
                EntityInfoSet.after(present_online).after(interpolate_online),
            )
            .add_systems(Update, present_interactions.after(present_online))
            .add_systems(Update, present_status.after(present_online))
            .add_systems(Update, present_slow.after(present_online))
            .add_systems(Update, present_target.after(present_online).after(select_target))
            .add_systems(
                Update,
                refresh_chat_presence.after(crate::online::poll_online),
            )
            .add_systems(
                Update,
                present_telemetry.after(crate::online::poll_online),
            );
    }
}

#[derive(Component)]
struct TelemetryHud;

#[derive(Component)]
struct InteractionHud;

#[derive(Resource, Default)]
pub(crate) struct LockComposer {
    pub(crate) panel: Option<crate::interaction::Panel>,
    pub(crate) guess: String,
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn interaction_keyboard(
    mut client: ResMut<OnlineClient>,
    selected: Res<SelectedTarget>,
    keys: Res<ButtonInput<KeyCode>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    composer: Res<ChatComposer>,
    mut lock_input: ResMut<LockComposer>,
    menu: Option<Res<crate::options::OptionsMenuState>>,
) {
    if lock_input.panel.as_ref() != client.interaction().panel() {
        lock_input.panel = client.interaction().panel().cloned();
        lock_input.guess.clear();
    }
    if composer.active
        || menu.is_some_and(|menu| menu.is_open())
        || !windows.single().is_ok_and(|window| window.focused)
        || client.status() != OnlineStatus::Active
        || client.interaction().pending()
    {
        return;
    }
    if keys.just_pressed(KeyCode::KeyX) && client.interaction().panel().is_some() {
        let _ = client.close_panel();
        return;
    }
    if let Some(lock) = client.interaction().lock() {
        let length = usize::from(lock.length);
        if keys.just_pressed(KeyCode::Backspace) {
            lock_input.guess.pop();
        }
        let digits = [
            KeyCode::Digit0,
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
        for (digit, key) in "0123456789".chars().zip(digits) {
            if keys.just_pressed(key) && lock_input.guess.len() < length {
                lock_input.guess.push(digit);
            }
        }
        if keys.just_pressed(KeyCode::Enter)
            && let Some(panel) = lock_input.panel.clone()
            && client.submit_lock_guess(&panel, &lock_input.guess).is_ok()
        {
            lock_input.guess.clear();
        }
        return;
    }
    if client
        .interaction()
        .panel()
        .is_some_and(crate::content_panel::is_content_panel)
    {
        return;
    }
    let Some(target) = selected.0 else {
        return;
    };
    let verbs = client.target_verbs(target);
    let controls = [
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
    ];
    for (verb, key) in verbs.iter().zip(controls) {
        if keys.just_pressed(key) {
            let _ = client.interact(target, verb);
            break;
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
fn present_interaction_hud(
    client: Res<OnlineClient>,
    selected: Res<SelectedTarget>,
    lock_input: Res<LockComposer>,
    dialog: Option<Res<crate::lock_panel::LiveLockDialog>>,
    mut hud: Query<&mut Text, With<InteractionHud>>,
) {
    let mut text = String::from("Interação · Tab: selecionar alvo\n");
    if client.interaction().lock().is_none()
        && let Some(target) = selected.0
    {
        if let Some(entity) = client
            .world()
            .entities()
            .find(|entity| entity.entity == target)
        {
            let _ = writeln!(text, "Alvo: {}", prototype_name(&entity.prototype));
        }
        for (index, verb) in client.target_verbs(target).iter().enumerate() {
            let _ = writeln!(text, "{}: {verb}", index + 1);
        }
    }
    if let Some(lock) = client.interaction().lock().filter(|_| dialog.is_none()) {
        let _ = writeln!(
            text,
            "Código ({} dígitos): {}\nEnter: tentar · Backspace: apagar\nTentativas: {}",
            lock.length, lock_input.guess, lock.attempts_left
        );
        for hint in &lock.hints {
            text.push_str(match hint {
                crate::interaction::DigitHint::Exact => "= ",
                crate::interaction::DigitHint::Higher => "↑ ",
                crate::interaction::DigitHint::Lower => "↓ ",
            });
        }
        if let Some(tick) = lock.retry_after_tick {
            let _ = write!(text, "\nBloqueado até tick {tick}");
        }
        text.push('\n');
    }
    if let Some(panel) = client.interaction().panel()
        && (dialog.is_none() || client.interaction().lock().is_none())
    {
        let _ = writeln!(
            text,
            "\n{}\n{}\nX: fechar interface",
            panel.title, panel.schema
        );
    }
    text.push_str(client.interaction().status());
    for mut label in &mut hud {
        label.0.clone_from(&text);
    }
}

/// Marks a 3D presentation of a server-replicated entity.
#[derive(Component, Debug)]
pub struct OnlineEntity(StableId);

impl OnlineEntity {
    /// Stable id this presentation tracks.
    pub(crate) fn id(&self) -> StableId {
        self.0
    }
}

#[derive(Component)]
struct FacingIndicator(StableId);

/// Local UI selection only; does not grant permission to act on an entity.
#[derive(Resource, Default)]
pub(crate) struct SelectedTarget(Option<StableId>);

#[derive(Component)]
struct TargetRing(StableId);

#[derive(Component)]
struct InteractionMarker(StableId);

#[derive(Component)]
struct StatusMarker(StableId);

#[derive(Component)]
struct SlowMarker(StableId);

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
        *visibility = status_visibility(stunned.contains(&marker.0));
    }
}

#[allow(clippy::needless_pass_by_value)]
fn present_slow(client: Res<OnlineClient>, mut markers: Query<(&SlowMarker, &mut Visibility)>) {
    let slowed: BTreeSet<_> = client
        .world()
        .entities()
        .filter(|entity| entity.slowed)
        .map(|entity| entity.entity)
        .collect();
    for (marker, mut visibility) in &mut markers {
        *visibility = status_visibility(slowed.contains(&marker.0));
    }
}

fn status_visibility(active: bool) -> Visibility {
    if active {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    }
}

pub(crate) fn prototype_name(prototype: &engine_core::id::DefinitionId) -> String {
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
    menu: Option<Res<crate::options::OptionsMenuState>>,
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
    if composer.active
        || menu.is_some_and(|menu| menu.is_open())
        || !windows.single().is_ok_and(|window| window.focused)
    {
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

fn setup_online(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn((
        scene_camera(),
        Transform::from_xyz(0.0, 9.0, 13.0).looking_at(Vec3::ZERO, Vec3::Y),
        SceneCamera,
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 15_000.0,
            shadow_maps_enabled: true,
            ..default()
        },
        Transform::from_xyz(4.0, 12.0, 6.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(2000.0, 0.1, 2000.0))),
        MeshMaterial3d(materials.add(Color::srgb(0.16, 0.27, 0.18))),
        Transform::from_xyz(0.0, -0.1, 0.0),
    ));
    let grid_line = meshes.add(Cuboid::new(200.0, 0.015, 0.035));
    let grid_material = materials.add(Color::srgb(0.24, 0.37, 0.25));
    for index in -20_i16..=20 {
        let offset = f32::from(index) * 5.0;
        commands.spawn((
            Mesh3d(grid_line.clone()),
            MeshMaterial3d(grid_material.clone()),
            Transform::from_xyz(0.0, -0.04, offset),
        ));
        commands.spawn((
            Mesh3d(grid_line.clone()),
            MeshMaterial3d(grid_material.clone()),
            Transform::from_xyz(offset, -0.04, 0.0)
                .with_rotation(Quat::from_rotation_y(std::f32::consts::FRAC_PI_2)),
        ));
    }
    commands.spawn((
        InteractionHud,
        Text::new("Interação"),
        Node {
            position_type: PositionType::Absolute,
            right: px(16),
            top: px(16),
            width: px(360),
            max_width: percent(45),
            padding: UiRect::all(px(10)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.03, 0.04, 0.06, 0.9)),
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

/// Read held movement keys and issue one bounded intent at a time. The server
/// remains authoritative: every endpoint is a local prediction until the
/// correlation result arrives. Movement is suppressed while composing chat.
#[allow(clippy::needless_pass_by_value, clippy::too_many_arguments)] // Bevy systems take resources by value.
fn online_movement(
    orbit: Res<MouseCamera>,
    keys: Res<ButtonInput<KeyCode>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    composer: Res<ChatComposer>,
    mut client: ResMut<OnlineClient>,
    time: Res<Time>,
    mut cooldown: Local<f32>,
    menu: Option<Res<crate::options::OptionsMenuState>>,
) {
    *cooldown = (*cooldown - time.delta_secs()).max(0.0);
    if menu.as_ref().is_none_or(|menu| !menu.is_open())
        && !composer.active
        && windows.single().is_ok_and(|window| window.focused)
        && keys.just_pressed(KeyCode::KeyR)
        && matches!(client.status(), OnlineStatus::Failed(_))
    {
        let _ = client.reconnect();
        return;
    }
    if composer.active
        || menu.is_some_and(|menu| menu.is_open())
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
    let Some(destination) =
        movement_destination(authority, &keys, client.movement_step(), orbit.yaw)
    else {
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
    camera_yaw: f32,
) -> Option<Position> {
    if !step.is_finite() || step <= 0.0 || !camera_yaw.is_finite() {
        return None;
    }
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
    let (sin, cos) = camera_yaw.sin_cos();
    let world_x = cos * x + sin * z;
    let world_z = -sin * x + cos * z;
    Some(Position {
        x: position.x + world_x / length * step,
        y: position.y,
        z: position.z + world_z / length * step,
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

// Keep extreme server coordinates within a bounded presentation space.
fn screen_position(x: f32, y: f32, z: f32) -> Vec3 {
    Vec3::new(
        x.clamp(-1_000_000.0, 1_000_000.0),
        y.clamp(-1_000_000.0, 1_000_000.0) + 0.8,
        z.clamp(-1_000_000.0, 1_000_000.0),
    )
}

fn status_label(status: OnlineStatus) -> String {
    match status {
        OnlineStatus::Disconnected => "disconnected".to_owned(),
        OnlineStatus::Connecting => "connecting".to_owned(),
        OnlineStatus::AwaitingSnapshot => "awaiting snapshot".to_owned(),
        OnlineStatus::Active => "active".to_owned(),
        OnlineStatus::RefreshingCredentials => "refreshing account session".to_owned(),
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
        OnlineError::CredentialRefresh(
            crate::nakama_credentials::NakamaLoginError::Authentication(
                lynx_client_net::NakamaAuthFailure::Authentication,
            ),
        ) => "account credentials expired or revoked",
        OnlineError::CredentialRefresh(
            crate::nakama_credentials::NakamaLoginError::CredentialStore(_),
        ) => "secure credential store unavailable",
        OnlineError::CredentialRefresh(_) => "account session refresh failed",
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
        OnlineStatus::Failed(crate::online::OnlineError::CredentialRefresh(
            crate::nakama_credentials::NakamaLoginError::Authentication(
                lynx_client_net::NakamaAuthFailure::Authentication,
            ),
        )) => Some("Your saved account session expired. Restart Lynx and sign in again."),
        OnlineStatus::Failed(crate::online::OnlineError::CredentialRefresh(_)) => Some(
            "Check Nakama connectivity and the secure credential store, then press R to retry.",
        ),
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

#[allow(
    clippy::needless_pass_by_value,
    clippy::type_complexity,
    clippy::too_many_arguments
)] // Bevy supplies system parameters independently.
#[allow(clippy::too_many_lines)] // Spawn bundles stay beside entity synchronization logic.
fn present_online(
    mut commands: Commands,
    client: Res<OnlineClient>,
    orbit: Res<MouseCamera>,
    info_assets: Res<EntityInfoAssets>,
    backend: Res<EntityInfoBackend>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
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
            predicted.map(|position| screen_position(position.x, position.y, position.z))
        } else {
            public.get(&id.0).map(|public| {
                screen_position(public.position.x, public.position.y, public.position.z)
            })
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
                || screen_position(public.position.x, public.position.y, public.position.z),
                |position| screen_position(position.x, position.y, position.z),
            )
        } else {
            screen_position(public.position.x, public.position.y, public.position.z)
        };
        let local = Some(public.entity) == actor;
        let prototype = public.prototype.as_str();
        let colour = if local {
            Color::srgb(0.3, 0.85, 1.0)
        } else if prototype.starts_with("prop.") {
            Color::srgb(0.55, 0.35, 0.18)
        } else {
            Color::srgb(0.95, 0.75, 0.3)
        };
        let body = if prototype == "prop.door" {
            meshes.add(Cuboid::new(1.1, 1.6, 0.18))
        } else if prototype == "prop.barrel" {
            meshes.add(Cylinder::new(0.45, 1.4))
        } else if prototype.starts_with("prop.") {
            meshes.add(Cuboid::new(0.9, 1.4, 0.9))
        } else {
            meshes.add(Capsule3d::new(0.35, 0.9))
        };
        let body_material = materials.add(colour);
        let ring_mesh = meshes.add(Cuboid::new(0.15, 0.04, 0.05));
        let ring_material = materials.add(Color::srgb(1.0, 0.35, 0.2));
        let marker_mesh = meshes.add(Cuboid::new(0.18, 0.18, 0.18));
        let marker_material = materials.add(Color::srgb(1.0, 0.82, 0.25));
        let stun_material = materials.add(Color::srgb(1.0, 0.35, 0.2));
        let slow_material = materials.add(Color::srgb(0.45, 0.75, 1.0));
        commands
            .spawn((
                Mesh3d(body),
                MeshMaterial3d(body_material.clone()),
                Transform::from_translation(target),
                OnlineEntity(public.entity),
                OnlineMotion { to: target },
            ))
            .with_children(|parent| {
                spawn_entity_info(
                    parent,
                    public.entity,
                    &prototype_name(&public.prototype),
                    &info_assets,
                    *backend,
                );
                parent.spawn((
                    InteractionMarker(public.entity),
                    Mesh3d(marker_mesh.clone()),
                    MeshMaterial3d(marker_material),
                    Transform::from_xyz(0.0, 1.6, 0.0),
                    Visibility::Hidden,
                ));
                parent.spawn((
                    StatusMarker(public.entity),
                    Mesh3d(marker_mesh.clone()),
                    MeshMaterial3d(stun_material),
                    Transform::from_xyz(0.5, 1.6, 0.0),
                    Visibility::Hidden,
                ));
                parent.spawn((
                    SlowMarker(public.entity),
                    Mesh3d(marker_mesh.clone()),
                    MeshMaterial3d(slow_material),
                    Transform::from_xyz(0.5, 1.35, 0.0),
                    Visibility::Hidden,
                ));
                for segment in 0..32_u16 {
                    let angle = f32::from(segment) * std::f32::consts::TAU / 32.0;
                    parent.spawn((
                        TargetRing(public.entity),
                        Mesh3d(ring_mesh.clone()),
                        MeshMaterial3d(ring_material.clone()),
                        ground_ring_transform(angle, 0.95, -0.75),
                        Visibility::Hidden,
                    ));
                }
                parent.spawn((
                    FacingIndicator(public.entity),
                    Mesh3d(marker_mesh),
                    MeshMaterial3d(body_material),
                    facing_transform(public.yaw_degrees),
                ));
            });
    }
    if let Some(position) = predicted {
        for mut camera in &mut cameras {
            let target = screen_position(position.x, position.y, position.z);
            *camera =
                Transform::from_translation(target + orbit.offset()).looking_at(target, Vec3::Y);
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

/// World yaw is clockwise from +Z toward +X.
fn facing_transform(yaw_degrees: f32) -> Transform {
    let angle = yaw_degrees.rem_euclid(360.0).to_radians();
    Transform::from_xyz(angle.sin() * 0.45, 0.15, angle.cos() * 0.45)
        .with_rotation(Quat::from_rotation_y(angle))
}

fn ground_ring_transform(angle: f32, radius: f32, depth: f32) -> Transform {
    Transform::from_xyz(angle.cos() * radius, depth, angle.sin() * radius)
        .with_rotation(Quat::from_rotation_y(-angle - std::f32::consts::FRAC_PI_2))
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
        app.init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<StandardMaterial>>()
            .add_systems(Startup, setup_scene)
            .add_systems(
                Update,
                keyboard_input
                    .after(crate::options::toggle_menu)
                    .in_set(OfflineSet::Input),
            )
            .add_systems(Update, present_scene.in_set(OfflineSet::Presentation));
    }
}

#[derive(Component)]
struct Avatar;

#[derive(Component)]
pub(crate) struct SceneCamera;

#[derive(Resource)]
struct MouseCamera {
    yaw: f32,
    pitch: f32,
    distance: f32,
}

impl Default for MouseCamera {
    fn default() -> Self {
        Self {
            yaw: 0.0,
            pitch: 0.6,
            distance: 16.0,
        }
    }
}

impl MouseCamera {
    fn offset(&self) -> Vec3 {
        Vec3::new(
            self.yaw.sin() * self.pitch.cos(),
            self.pitch.sin(),
            self.yaw.cos() * self.pitch.cos(),
        ) * self.distance
    }
}

#[allow(clippy::needless_pass_by_value, clippy::too_many_arguments)]
fn mouse_controls(
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<bevy::input::mouse::AccumulatedMouseMotion>,
    scroll: Res<bevy::input::mouse::AccumulatedMouseScroll>,
    windows: Query<&Window, With<PrimaryWindow>>,
    ui: Query<&bevy::picking::hover::Hovered, With<Node>>,
    cameras: Query<(&Camera, &GlobalTransform), With<SceneCamera>>,
    entities: Query<(&OnlineEntity, &GlobalTransform)>,
    mut orbit: ResMut<MouseCamera>,
    mut selected: ResMut<SelectedTarget>,
    client: Res<OnlineClient>,
    menu: Option<Res<crate::options::OptionsMenuState>>,
) {
    let Ok(window) = windows.single() else {
        return;
    };
    if !window.focused
        || menu.is_some_and(|menu| menu.is_open())
        || ui.iter().any(|hovered| hovered.0)
    {
        return;
    }
    if buttons.pressed(MouseButton::Right) {
        orbit.yaw -= motion.delta.x * 0.005;
        orbit.pitch = (orbit.pitch + motion.delta.y * 0.005).clamp(0.15, 1.4);
    }
    orbit.distance = (orbit.distance * (-scroll.delta.y * 0.1).exp()).clamp(3.0, 45.0);
    if !buttons.just_pressed(MouseButton::Left) {
        return;
    }
    let (Some(cursor), Ok((camera, transform))) = (window.cursor_position(), cameras.single())
    else {
        return;
    };
    selected.0 = entities
        .iter()
        .filter(|(entity, _)| Some(entity.0) != client.world().actor())
        .filter_map(|(entity, position)| {
            camera
                .world_to_viewport(transform, position.translation())
                .ok()
                .map(|point| (entity.0, point.distance_squared(cursor)))
        })
        .filter(|(_, distance)| *distance <= 32.0 * 32.0)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(entity, _)| entity);
}

/// Both scene paths use a tone mapper that needs no optional LUT textures.
/// Bevy's default uses a magenta placeholder when `tonemapping_luts` is absent.
fn scene_camera() -> impl Bundle {
    (Camera3d::default(), Tonemapping::Reinhard)
}

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
fn setup_scene(
    mut commands: Commands,
    zone: Res<ZoneScene>,
    mut scene: ResMut<OfflineScene>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn((
        scene_camera(),
        Transform::from_xyz(0.0, 12.0, 16.0).looking_at(Vec3::ZERO, Vec3::Y),
        SceneCamera,
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 12_000.0,
            ..default()
        },
        Transform::from_xyz(4.0, 12.0, 6.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    scene.avatar_position = zone.bounds.clamp(zone.spawn);

    // Use placeholder geometry until verified authored assets can be loaded.
    for (index, tile) in zone.terrain.iter().enumerate() {
        let colour = if index % 2 == 0 {
            Color::srgb(0.13, 0.22, 0.16)
        } else {
            Color::srgb(0.15, 0.25, 0.18)
        };
        commands.spawn((
            Mesh3d(meshes.add(Cuboid::new(tile.size, 0.08, tile.size))),
            MeshMaterial3d(materials.add(colour)),
            Transform::from_translation(tile.position + Vec3::new(0.0, -0.04, 0.0)),
            WorldPosition(tile.position),
        ));
    }
    for object in &zone.objects {
        let (colour, size, _z) = object_style(object.kind);
        let mesh = match object.kind {
            SceneObjectKind::Character => meshes.add(Capsule3d::new(size * 0.45, size)),
            SceneObjectKind::Interactable => meshes.add(Cuboid::new(size, size, size)),
            SceneObjectKind::Prop => meshes.add(Cuboid::new(size, size * 2.0, size)),
        };
        commands.spawn((
            Mesh3d(mesh),
            MeshMaterial3d(materials.add(colour)),
            Transform::from_translation(
                object.position
                    + Vec3::Y
                        * if object.kind == SceneObjectKind::Interactable {
                            size * 0.5
                        } else {
                            size
                        },
            ),
        ));
    }
    commands.spawn((
        Mesh3d(meshes.add(Capsule3d::new(0.35, 0.8))),
        MeshMaterial3d(materials.add(Color::srgb(0.3, 0.7, 1.0))),
        Transform::from_translation(scene.avatar_position + Vec3::Y * 0.75),
        Avatar,
    ));
}

#[allow(clippy::needless_pass_by_value)] // Bevy system parameters must be passed by value.
fn keyboard_input(
    keys: Res<ButtonInput<KeyCode>>,
    replay: Option<Res<ReplayInput>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut pressed: ResMut<PressedControls>,
    menu: Option<Res<crate::options::OptionsMenuState>>,
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
    if menu.is_some_and(|menu| menu.is_open()) {
        pressed.0.clear();
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

fn offline_camera_offset(mode: crate::offline::CameraMode) -> Vec3 {
    match mode {
        crate::offline::CameraMode::Follow3d => Vec3::new(0.0, 12.0, 16.0),
        crate::offline::CameraMode::Billboard2d => Vec3::new(0.0, 22.0, 0.01),
    }
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
        transform.translation = scene.avatar_position + Vec3::Y * 0.75;
    }
    for (world, mut transform) in &mut objects {
        transform.translation.x = world.0.x;
        transform.translation.z = world.0.z;
    }
    for mut transform in &mut cameras {
        let offset = offline_camera_offset(pose.mode);
        *transform =
            Transform::from_translation(pose.target + offset).looking_at(pose.target, Vec3::Y);
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

#[cfg_attr(coverage_nightly, coverage(off))]
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
    fn status_indicators_follow_authoritative_presence_and_clearing() {
        assert_eq!(status_visibility(true), Visibility::Inherited);
        assert_eq!(status_visibility(false), Visibility::Hidden);
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
            (0.0, Vec2::new(0.0, 0.45)),
            (90.0, Vec2::new(0.45, 0.0)),
            (180.0, Vec2::new(0.0, -0.45)),
            (270.0, Vec2::new(-0.45, 0.0)),
            (450.0, Vec2::new(0.45, 0.0)),
        ] {
            assert!(facing_transform(yaw).translation.xz().distance(expected) < 0.001);
        }
    }

    #[test]
    fn online_presentation_preserves_world_height_and_bounds_coordinates() {
        assert_eq!(screen_position(2.0, 3.0, -4.0), Vec3::new(2.0, 3.8, -4.0));
        assert_eq!(
            screen_position(f32::MAX, 0.0, -f32::MAX),
            Vec3::new(1_000_000.0, 0.8, -1_000_000.0)
        );
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
        assert_eq!(avatar.translation, Vec3::new(0.0, 0.75, -1.0));
        let camera = world
            .query_filtered::<&Transform, With<SceneCamera>>()
            .single(world)
            .unwrap();
        assert_eq!(camera.translation, Vec3::new(0.0, 12.0, 15.0));
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
        assert_eq!(avatar.translation, Vec3::new(0.0, 0.75, 0.0));
    }

    #[test]
    fn camera_modes_offer_distinct_3d_views() {
        let follow = offline_camera_offset(crate::offline::CameraMode::Follow3d);
        let overhead = offline_camera_offset(crate::offline::CameraMode::Billboard2d);
        assert!(follow.z > follow.y);
        assert!(overhead.y > follow.y && overhead.z < 0.1);
    }

    #[test]
    fn scene_camera_uses_tonemapping_without_optional_lookup_textures() {
        for online in [false, true] {
            let mut app = headless_app();
            app.init_resource::<ButtonInput<KeyCode>>();
            if online {
                app.insert_resource(OnlineClient::new(1.0).unwrap())
                    .add_plugins(OnlineWindowPlugin);
            } else {
                app.add_plugins(OfflineWindowPlugin);
            }
            app.update();
            let world = app.world_mut();
            let tonemapping = world
                .query_filtered::<&Tonemapping, With<Camera3d>>()
                .single(world)
                .unwrap();
            assert_eq!(*tonemapping, Tonemapping::Reinhard);
        }
    }

    #[test]
    fn movement_destination_is_normalized_bounded_and_height_preserving() {
        let origin = Position {
            x: 1.0,
            y: 2.0,
            z: 3.0,
        };
        let mut keys = ButtonInput::<KeyCode>::default();
        assert_eq!(movement_destination(origin, &keys, 0.09, 0.0), None);
        keys.press(KeyCode::KeyW);
        assert_eq!(movement_destination(origin, &keys, 0.0, 0.0), None);
        let up = movement_destination(origin, &keys, 0.09, 0.0).unwrap();
        assert_eq!(up.y, origin.y);
        assert!((up.z - (origin.z - 0.09)).abs() < 1e-6);
        keys.press(KeyCode::KeyD);
        let diagonal = movement_destination(origin, &keys, 0.09, 0.0).unwrap();
        let dx = diagonal.x - origin.x;
        let dz = diagonal.z - origin.z;
        assert!((dx.hypot(dz) - 0.09).abs() < 1e-6);
        assert!((dx - 0.09 / 2.0_f32.sqrt()).abs() < 1e-6);
        assert!((dz + 0.09 / 2.0_f32.sqrt()).abs() < 1e-6);
    }

    #[test]
    fn movement_follows_camera_yaw_without_changing_speed_or_height() {
        let origin = Position {
            x: 0.0,
            y: 2.0,
            z: 0.0,
        };
        let mut keys = ButtonInput::default();
        keys.press(KeyCode::KeyW);
        let forward =
            movement_destination(origin, &keys, 1.0, std::f32::consts::FRAC_PI_2).unwrap();
        assert!((forward.x + 1.0).abs() < 1e-6);
        assert!(forward.z.abs() < 1e-6);
        assert_eq!(forward.y, origin.y);
        keys.press(KeyCode::KeyD);
        let diagonal =
            movement_destination(origin, &keys, 1.0, std::f32::consts::FRAC_PI_2).unwrap();
        assert!((diagonal.x.hypot(diagonal.z) - 1.0).abs() < 1e-6);
        assert!(diagonal.x < 0.0 && diagonal.z < 0.0);
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
                    to: Vec3::new(4.0, 0.0, 0.0),
                },
            ))
            .id();
        app.update();
        let transform = app.world().get::<Transform>(entity).unwrap();
        assert!(transform.translation.x >= 0.0);
        assert!(transform.translation.x <= 4.0);
        app.update();
        let transform = app.world().get::<Transform>(entity).unwrap();
        assert!(transform.translation.x > 0.0 && transform.translation.x < 4.0);
        app.world_mut().get_mut::<OnlineMotion>(entity).unwrap().to = Vec3::X * 1000.0;
        app.update();
        assert_eq!(
            app.world().get::<Transform>(entity).unwrap().translation,
            Vec3::X * 1000.0
        );
    }
}
