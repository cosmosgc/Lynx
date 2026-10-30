//! Account management screen. All network and credential-store work runs on workers.
use crate::nakama_credentials::{OsNakamaCredentialStore, authenticate_email_or_resume};
use bevy::{
    app::AppExit,
    input::{ButtonState, keyboard::KeyboardInput},
    prelude::*,
};
use game_api::{Character, CreateCharacter, SessionToken};
use lynx_client_net::{CharacterApiClient, NakamaAuthClient, NakamaAuthConfig};
use std::sync::{Mutex, mpsc};

/// In-client account and character management, independent of the offline demo.
#[derive(Debug)]
pub struct AccountUiPlugin;

/// Secret-bearing handoff from the account screen to the online world window.
#[derive(Resource, Default)]
pub struct AccountWorldLaunch(Option<(String, String)>);

impl std::fmt::Debug for AccountWorldLaunch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AccountWorldLaunch")
            .field("pending", &self.0.is_some())
            .field("credentials", &"[REDACTED]")
            .finish()
    }
}

impl AccountWorldLaunch {
    /// Take the authenticated access token and verified character selected by
    /// the account screen. The token should be kept only in memory.
    pub fn take(&mut self) -> Option<(String, String)> {
        self.0.take()
    }
}

impl Plugin for AccountUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Screen>()
            .add_systems(Startup, setup)
            .add_systems(Update, (buttons, keyboard, poll, render).chain());
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Email,
    Password,
    Name,
}

#[derive(Resource)]
struct Screen {
    email: String,
    password: String,
    name: String,
    focus: Field,
    token: Option<String>,
    characters: Vec<Character>,
    selected: Option<(String, String)>,
    intent: Option<(String, String)>,
    status: String,
    worker: Option<Mutex<mpsc::Receiver<Result<Outcome, String>>>>,
    dirty: bool,
}

impl Default for Screen {
    fn default() -> Self {
        Self {
            email: String::new(),
            password: String::new(),
            name: String::new(),
            focus: Field::Email,
            token: None,
            characters: Vec::new(),
            selected: None,
            intent: None,
            status: "Sign in to an existing account. New accounts: use account_portal first."
                .into(),
            worker: None,
            dirty: true,
        }
    }
}

enum Outcome {
    SignedIn(String),
    Listed(Vec<Character>),
    Created,
    Selected { id: String, name: String },
}
#[derive(Component)]
struct Root;
#[derive(Component, Clone)]
enum Action {
    Focus(Field),
    Login,
    List,
    Create,
    Select(String),
    EnterWorld,
}

fn setup(mut commands: Commands) {
    commands.spawn(Camera2d);
}

fn start(screen: &mut Screen, task: impl FnOnce() -> Result<Outcome, String> + Send + 'static) {
    let (tx, rx) = mpsc::channel();
    screen.worker = Some(Mutex::new(rx));
    screen.status = "Working…".into();
    screen.dirty = true;
    std::thread::spawn(move || {
        let _ = tx.send(task());
    });
}

fn api(token: &str) -> Result<(CharacterApiClient, SessionToken), String> {
    let url =
        std::env::var("LYNX_WORLD_API_URL").unwrap_or_else(|_| "http://127.0.0.1:8090".into());
    let api =
        CharacterApiClient::new(url).map_err(|e| format!("Character API configuration: {e:?}"))?;
    let token = SessionToken::new(token).map_err(|_| "Invalid session.".to_owned())?;
    Ok((api, token))
}

#[allow(deprecated)] // Bevy 0.20 still supports the legacy interaction component.
#[allow(clippy::too_many_lines)] // Keep the small screen's action routing together.
fn buttons(
    mut screen: ResMut<Screen>,
    mut launch: ResMut<AccountWorldLaunch>,
    mut exit: MessageWriter<AppExit>,
    query: Query<(&Interaction, &Action), Changed<Interaction>>,
) {
    for (interaction, action) in &query {
        if *interaction != Interaction::Pressed || screen.worker.is_some() {
            continue;
        }
        match action {
            Action::Focus(field) => {
                screen.focus = *field;
                screen.dirty = true;
            }
            Action::Login => {
                if screen.email.is_empty() {
                    screen.status = "Enter your email address.".into();
                    screen.dirty = true;
                    continue;
                }
                let email = screen.email.clone();
                let password = std::mem::take(&mut screen.password);
                start(&mut screen, move || {
                    let url = std::env::var("LYNX_NAKAMA_URL")
                        .map_err(|_| "Set LYNX_NAKAMA_URL.".to_owned())?;
                    let key = std::env::var("LYNX_NAKAMA_SERVER_KEY")
                        .map_err(|_| "Set LYNX_NAKAMA_SERVER_KEY.".to_owned())?;
                    let auth = NakamaAuthClient::new(NakamaAuthConfig::new(&url, key))
                        .map_err(|e| format!("Authentication configuration: {e:?}"))?;
                    let session = authenticate_email_or_resume(
                        &auth,
                        &url,
                        &OsNakamaCredentialStore,
                        &email,
                        || Ok(password),
                    )
                    .map_err(|e| {
                        format!(
                            "Sign in failed: {e:?}. Check credentials and secure credential store."
                        )
                    })?;
                    Ok(Outcome::SignedIn(session.access_token().to_owned()))
                });
            }
            Action::List => {
                if let Some(token) = screen.token.clone() {
                    start(&mut screen, move || {
                        let (api, token) = api(&token)?;
                        api.characters(&token)
                            .map(Outcome::Listed)
                            .map_err(|e| format!("List failed: {e:?}"))
                    });
                }
            }
            Action::Create => {
                let Some(token) = screen.token.clone() else {
                    continue;
                };
                let name = screen.name.clone();
                if (CreateCharacter { name: name.clone() }).validate().is_err() {
                    screen.status = "Invalid character name (1–32 characters, no surrounding whitespace or controls).".into();
                    screen.dirty = true;
                    continue;
                }
                if screen.intent.as_ref().is_some_and(|(old, _)| old != &name) {
                    screen.status = "A creation outcome is unresolved. Retry the original name before creating another.".into();
                    screen.dirty = true;
                    continue;
                }
                if screen.intent.is_none() {
                    let mut bytes = [0_u8; 16];
                    if getrandom::fill(&mut bytes).is_err() {
                        screen.status = "Could not generate creation key.".into();
                        screen.dirty = true;
                        continue;
                    }
                    let mut key = String::with_capacity(32);
                    for byte in bytes {
                        use std::fmt::Write as _;
                        write!(key, "{byte:02x}").expect("writing to a string cannot fail");
                    }
                    screen.intent = Some((name.clone(), key));
                }
                let key = screen.intent.as_ref().unwrap().1.clone();
                start(&mut screen, move || {
                    let (api, token) = api(&token)?;
                    api.create_character(&token, &key, &CreateCharacter { name }).map(|_| Outcome::Created).map_err(|e| format!("Create failed: {e:?}. Retry the same name; the creation key is retained for this session."))
                });
            }
            Action::Select(id) => {
                if let Some(token) = screen.token.clone() {
                    let id = id.clone();
                    start(&mut screen, move || {
                        let (api, token) = api(&token)?;
                        api.select_character(&token, &id)
                            .map(|c| Outcome::Selected {
                                id: c.id.to_string(),
                                name: c.name,
                            })
                            .map_err(|e| format!("Selection failed: {e:?}"))
                    });
                }
            }
            Action::EnterWorld => {
                if let Some((id, _)) = screen.selected.clone()
                    && let Some(token) = screen.token.take()
                {
                    launch.0 = Some((token, id));
                    exit.write(AppExit::Success);
                }
            }
        }
    }
}

fn keyboard(mut events: MessageReader<KeyboardInput>, mut screen: ResMut<Screen>) {
    if screen.worker.is_some() {
        events.clear();
        return;
    }
    for event in events.read() {
        if event.state != ButtonState::Pressed {
            continue;
        }
        if event.key_code == KeyCode::Tab {
            screen.focus = match screen.focus {
                Field::Email => Field::Password,
                Field::Password => Field::Name,
                Field::Name => Field::Email,
            };
            screen.dirty = true;
            continue;
        }
        let focus = screen.focus;
        let value = match focus {
            Field::Email => &mut screen.email,
            Field::Password => &mut screen.password,
            Field::Name => &mut screen.name,
        };
        if event.key_code == KeyCode::Backspace {
            value.pop();
        } else if let Some(text) = &event.text {
            for c in text.chars().filter(|c| !c.is_control()) {
                if value.len() + c.len_utf8() <= 320 {
                    value.push(c);
                }
            }
        }
        screen.dirty = true;
    }
}

fn poll(mut screen: ResMut<Screen>) {
    let result = screen
        .worker
        .as_ref()
        .and_then(|rx| match rx.lock().unwrap().try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err("Worker failed. Retry the operation.".into()))
            }
            Err(mpsc::TryRecvError::Empty) => None,
        });
    let Some(result) = result else {
        return;
    };
    screen.worker = None;
    screen.dirty = true;
    match result {
        Err(error) => screen.status = error,
        Ok(Outcome::SignedIn(token)) => {
            screen.token = Some(token.clone());
            screen.status = "Signed in; loading characters…".into();
            start(&mut screen, move || {
                let (api, token) = api(&token)?;
                api.characters(&token)
                    .map(Outcome::Listed)
                    .map_err(|e| format!("List failed: {e:?}"))
            });
        }
        Ok(Outcome::Listed(characters)) => {
            screen.characters = characters;
            screen.selected = None;
            screen.status = "Select a character or create one.".into();
        }
        Ok(Outcome::Created) => {
            screen.intent = None;
            screen.name.clear();
            screen.status = "Created. Refresh the list to see your character.".into();
        }
        Ok(Outcome::Selected { id, name }) => {
            screen.selected = Some((id, name.clone()));
            screen.status = format!("{name} verified. Enter the world when ready.");
        }
    }
}

fn render(mut commands: Commands, mut screen: ResMut<Screen>, roots: Query<Entity, With<Root>>) {
    if !screen.dirty {
        return;
    }
    screen.dirty = false;
    for entity in &roots {
        commands.entity(entity).despawn();
    }
    commands.spawn((Root, Node { width: percent(100), height: percent(100), flex_direction: FlexDirection::Column, padding: UiRect::all(px(24)), row_gap: px(8), overflow: Overflow::scroll_y(), ..default() }, BackgroundColor(Color::srgb(0.06, 0.08, 0.12)))).with_children(|parent| {
        parent.spawn((Text::new("Lynx — Account & Characters"), TextFont { font_size: bevy::text::FontSize::Px(28.0), ..default() }));
        parent.spawn(Text::new(screen.status.clone()));
        if screen.token.is_none() {
            button(parent, format!("{}Email: {}", marker(screen.focus == Field::Email), screen.email), Action::Focus(Field::Email));
            button(parent, format!("{}Password: {}", marker(screen.focus == Field::Password), "•".repeat(screen.password.chars().count())), Action::Focus(Field::Password));
            button(parent, "Sign in / resume saved session".into(), Action::Login);
            parent.spawn(Text::new("Enter email; password may be blank when resuming. Click a field or use Tab."));
        } else {
            button(parent, "Refresh character list".into(), Action::List);
            for c in &screen.characters { button(parent, format!("Select {} ({:?})", c.name, c.status), Action::Select(c.id.to_string())); }
            if let Some((_, name)) = &screen.selected {
                button(parent, format!("Enter world with {name}"), Action::EnterWorld);
            }
            button(parent, format!("{}New character name: {}", marker(screen.focus == Field::Name), screen.name), Action::Focus(Field::Name));
            button(parent, "Create / retry creation".into(), Action::Create);
            parent.spawn(Text::new("Creation retries reuse the same key during this session. Do not close the client while an outcome is uncertain."));
        }
    });
}
fn marker(focused: bool) -> &'static str {
    if focused { "> " } else { "" }
}
#[allow(deprecated)]
fn button(parent: &mut ChildSpawnerCommands, label: String, action: Action) {
    parent
        .spawn((
            Interaction::default(),
            bevy::ui::FocusPolicy::Block,
            action,
            Node {
                padding: UiRect::all(px(8)),
                ..default()
            },
            BackgroundColor(Color::srgb(0.16, 0.22, 0.32)),
        ))
        .with_children(|p| {
            p.spawn(Text::new(label));
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_launch_is_single_use_and_redacts_the_access_token() {
        let mut launch =
            AccountWorldLaunch(Some(("secret-access-token".into(), "chr-test".into())));
        assert!(!format!("{launch:?}").contains("secret-access-token"));
        assert_eq!(
            launch.take(),
            Some(("secret-access-token".into(), "chr-test".into()))
        );
        assert!(launch.take().is_none());
    }

    #[test]
    fn password_is_not_part_of_display_or_default_status() {
        let screen = Screen {
            password: "secret-password".into(),
            ..default()
        };
        let masked = "•".repeat(screen.password.chars().count());
        assert!(!masked.contains(&screen.password));
        assert!(!screen.status.contains(&screen.password));
    }

    #[test]
    fn failed_creation_keeps_retry_intent_until_success() {
        let mut app = App::new();
        let mut screen = Screen {
            intent: Some(("Hero".into(), "stable-key".into())),
            ..default()
        };
        let (tx, rx) = mpsc::channel();
        screen.worker = Some(Mutex::new(rx));
        app.insert_resource(screen).add_systems(Update, poll);
        assert!(tx.send(Err("uncertain".into())).is_ok());
        app.update();
        assert_eq!(
            app.world().resource::<Screen>().intent.as_ref().unwrap().1,
            "stable-key"
        );
        let (tx, rx) = mpsc::channel();
        app.world_mut().resource_mut::<Screen>().worker = Some(Mutex::new(rx));
        assert!(tx.send(Ok(Outcome::Created)).is_ok());
        app.update();
        assert!(app.world().resource::<Screen>().intent.is_none());
    }
}
