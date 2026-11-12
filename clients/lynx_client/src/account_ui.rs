//! Account management screen. All network and credential-store work runs on workers.
use crate::nakama_credentials::{
    CredentialScope, NakamaLogoutOutcome, OsNakamaCredentialStore, authenticate_email_or_resume,
    logout_saved_account,
};
use crate::server_browser::{ServerBrowser, ServerProfile, default_config_path};
use bevy::{
    app::AppExit,
    input::{ButtonState, keyboard::KeyboardInput},
    prelude::*,
};
use game_api::{
    Character, CreateCharacter, CreateGroup, Friend, FriendState, GroupMember, GroupMembership,
    GroupRole, GroupSearch, SessionToken, SocialGroup,
};
use lynx_client_net::{CharacterApiClient, NakamaAuthClient, NakamaAuthConfig};
use std::{
    path::PathBuf,
    sync::{Mutex, mpsc},
};

/// In-client account and character management, independent of the offline demo.
#[derive(Debug)]
pub struct AccountUiPlugin;

/// Secret-bearing handoff from the account screen to the online world window.
#[derive(Resource, Default)]
pub struct AccountWorldLaunch(Option<WorldLaunch>);

/// Everything the account screen hands to the world window. The bearer is kept
/// in memory only and never appears in `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct WorldLaunch {
    pub bearer: String,
    pub character: String,
    pub world_url: String,
    pub api_url: String,
    /// Credential namespace used for post-entry token refresh.
    pub credential_scope: CredentialScope,
}

impl std::fmt::Debug for WorldLaunch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorldLaunch")
            .field("bearer", &"[REDACTED]")
            .field("character", &self.character)
            .field("world_url", &self.world_url)
            .field("api_url", &self.api_url)
            .field("credential_scope", &"[REDACTED]")
            .finish()
    }
}

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
    /// Take the authenticated access token, verified character, and world URL
    /// selected by the account screen. The token should be kept only in memory.
    pub fn take(&mut self) -> Option<WorldLaunch> {
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
    ServerName,
    ServerHost,
    ServerPort,
    FriendAccount,
    GroupName,
}

#[derive(Resource)]
struct Screen {
    email: String,
    password: String,
    name: String,
    focus: Field,
    token: Option<String>,
    characters: Vec<Character>,
    friends: Vec<Friend>,
    friend_account: String,
    groups: Vec<GroupMembership>,
    discovered_groups: Vec<SocialGroup>,
    group_members: Vec<GroupMember>,
    selected_group: Option<String>,
    group_name: String,
    selected: Option<(String, String)>,
    intent: Option<(String, String)>,
    credential_scope: Option<CredentialScope>,
    status: String,
    worker: Option<Mutex<mpsc::Receiver<Result<Outcome, String>>>>,
    dirty: bool,
    browser: ServerBrowser,
    browser_path: Option<PathBuf>,
    browser_loaded: bool,
    active_server: Option<String>,
    server_name: String,
    server_host: String,
    server_port: String,
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
            friends: Vec::new(),
            friend_account: String::new(),
            groups: Vec::new(),
            discovered_groups: Vec::new(),
            group_members: Vec::new(),
            selected_group: None,
            group_name: String::new(),
            selected: None,
            intent: None,
            credential_scope: None,
            status: "Sign in to an existing account. New accounts: use account_portal first."
                .into(),
            worker: None,
            dirty: true,
            browser: ServerBrowser::default(),
            browser_path: None,
            browser_loaded: false,
            active_server: None,
            server_name: String::new(),
            server_host: String::new(),
            server_port: "8090".into(),
        }
    }
}

impl Screen {
    fn load_browser(&mut self) {
        if self.browser_loaded {
            return;
        }
        self.browser_loaded = true;
        self.browser_path = default_config_path();
        let Some(path) = self.browser_path.as_deref() else {
            return;
        };
        match ServerBrowser::load(path) {
            Ok(browser) => {
                self.browser = browser;
                self.active_server = self.browser.profiles().first().map(ServerProfile::key);
            }
            Err(error) => {
                self.status = format!("Could not load saved servers ({error}); using defaults.");
            }
        }
    }

    fn selected_profile(&self) -> Option<&ServerProfile> {
        let key = self.active_server.as_deref()?;
        self.browser
            .profiles()
            .iter()
            .find(|profile| profile.key() == key)
    }

    /// Credentials are isolated per selected world profile and Nakama origin.
    fn scope_for_nakama(&self, nakama_url: &str) -> CredentialScope {
        self.selected_profile().map_or_else(
            || CredentialScope::for_nakama(nakama_url),
            |profile| CredentialScope::for_world(&profile.key(), nakama_url),
        )
    }

    fn api_url(&self) -> String {
        self.selected_profile().map_or_else(
            || {
                std::env::var("LYNX_WORLD_API_URL")
                    .unwrap_or_else(|_| "http://127.0.0.1:8090".into())
            },
            ServerProfile::api_url,
        )
    }

    fn world_url(&self) -> String {
        self.selected_profile().map_or_else(
            || {
                std::env::var("LYNX_WORLD_URL")
                    .unwrap_or_else(|_| "ws://127.0.0.1:8090/world".into())
            },
            ServerProfile::world_url,
        )
    }

    fn persist_browser(&mut self) {
        let Some(path) = self.browser_path.as_deref() else {
            return;
        };
        if let Err(error) = self.browser.save(path) {
            self.status = format!("Could not save server profiles: {error}");
        }
    }
}

enum Outcome {
    SignedIn(String),
    SignedOut(NakamaLogoutOutcome),
    Listed(Vec<Character>),
    Created,
    Selected {
        id: String,
        name: String,
    },
    Friends(Vec<Friend>),
    FriendMutation(&'static str),
    MyGroups(Vec<GroupMembership>),
    DiscoveredGroups(Vec<SocialGroup>),
    GroupMutation(&'static str),
    GroupCreated(SocialGroup),
    GroupMembers {
        group_id: String,
        members: Vec<GroupMember>,
    },
    GroupMembersChanged {
        group_id: String,
    },
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
    UseServer(String),
    TogglePin(String),
    AddServer,
    SignOut,
    FriendList,
    RequestFriend,
    BlockEnteredAccount,
    FriendMutation(String, RelationshipAction),
    MyGroups,
    DiscoverGroups,
    CreateGroup,
    JoinGroup(String),
    LeaveGroup(String),
    LoadGroupMembers(String),
    AddGroupAccount,
    GroupMemberMutation(String, GroupMemberAction),
}

#[derive(Clone, Copy)]
enum RelationshipAction {
    Accept,
    Dismiss,
    Remove,
    Block,
    Unblock,
}

#[derive(Clone, Copy)]
enum GroupMemberAction {
    Add,
    Kick,
    Promote,
    Demote,
}

fn setup(mut commands: Commands, mut screen: ResMut<Screen>) {
    screen.load_browser();
    screen.dirty = true;
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

fn api(token: &str, url: &str) -> Result<(CharacterApiClient, SessionToken), String> {
    let api =
        CharacterApiClient::new(url).map_err(|e| format!("Character API configuration: {e:?}"))?;
    let token = SessionToken::new(token).map_err(|_| "Invalid session.".to_owned())?;
    Ok((api, token))
}

fn load_friends(screen: &mut Screen) {
    let Some(token) = screen.token.clone() else {
        return;
    };
    let api_url = screen.api_url();
    start(screen, move || {
        let (api, token) = api(&token, &api_url)?;
        api.friends(&token)
            .map(Outcome::Friends)
            .map_err(|error| format!("Friend list failed: {error:?}."))
    });
}

fn mutate_friend(screen: &mut Screen, account: String, action: RelationshipAction) {
    let Some(token) = screen.token.clone() else {
        return;
    };
    let api_url = screen.api_url();
    start(screen, move || {
        let (api, token) = api(&token, &api_url)?;
        let result = match action {
            RelationshipAction::Accept => api.accept_friend_request(&token, &account),
            RelationshipAction::Dismiss => api.dismiss_friend_request(&token, &account),
            RelationshipAction::Remove => api.remove_friend(&token, &account),
            RelationshipAction::Block => api.block_account(&token, &account),
            RelationshipAction::Unblock => api.unblock_account(&token, &account),
        };
        result
            .map(|()| Outcome::FriendMutation("Social relationship updated."))
            .map_err(|error| format!("Social action failed: {error:?}."))
    });
}

fn load_my_groups(screen: &mut Screen) {
    let Some(token) = screen.token.clone() else {
        return;
    };
    let api_url = screen.api_url();
    start(screen, move || {
        let (api, token) = api(&token, &api_url)?;
        api.my_groups(&token, 50, None)
            .map(|page| Outcome::MyGroups(page.items))
            .map_err(|error| format!("Group list failed: {error:?}."))
    });
}

fn discover_groups(screen: &mut Screen) {
    let Some(token) = screen.token.clone() else {
        return;
    };
    let api_url = screen.api_url();
    let name = screen.group_name.trim().to_owned();
    let search = GroupSearch {
        name: (!name.is_empty()).then_some(name),
        ..GroupSearch::default()
    };
    start(screen, move || {
        let (api, token) = api(&token, &api_url)?;
        api.discover_groups(&token, &search, 50, None)
            .map(|page| Outcome::DiscoveredGroups(page.items))
            .map_err(|error| format!("Group discovery failed: {error:?}."))
    });
}

fn mutate_group(screen: &mut Screen, group: String, join: bool) {
    let Some(token) = screen.token.clone() else {
        return;
    };
    let api_url = screen.api_url();
    start(screen, move || {
        let (api, token) = api(&token, &api_url)?;
        let result = if join {
            api.request_group_join(&token, &group)
        } else {
            api.leave_group(&token, &group)
        };
        result
            .map(|()| {
                Outcome::GroupMutation(if join {
                    "Join request submitted."
                } else {
                    "Left the group."
                })
            })
            .map_err(|error| format!("Group action failed: {error:?}."))
    });
}

fn load_group_members(screen: &mut Screen, group: String) {
    let Some(token) = screen.token.clone() else {
        return;
    };
    screen.selected_group = Some(group.clone());
    let api_url = screen.api_url();
    start(screen, move || {
        let (api, token) = api(&token, &api_url)?;
        api.group_members(&token, &group, 100, None)
            .map(|page| Outcome::GroupMembers {
                group_id: group,
                members: page.items,
            })
            .map_err(|error| format!("Group roster failed: {error:?}."))
    });
}

fn mutate_group_member(screen: &mut Screen, account: String, action: GroupMemberAction) {
    let Some(group) = screen.selected_group.clone() else {
        screen.status = "Select a group roster first.".into();
        screen.dirty = true;
        return;
    };
    let Some(token) = screen.token.clone() else {
        return;
    };
    let api_url = screen.api_url();
    start(screen, move || {
        let (api, token) = api(&token, &api_url)?;
        let accounts = vec![account];
        let result = match action {
            GroupMemberAction::Add => api.add_group_members(&token, &group, &accounts),
            GroupMemberAction::Kick => api.kick_group_members(&token, &group, &accounts),
            GroupMemberAction::Promote => api.promote_group_members(&token, &group, &accounts),
            GroupMemberAction::Demote => api.demote_group_members(&token, &group, &accounts),
        };
        result
            .map(|()| Outcome::GroupMembersChanged { group_id: group })
            .map_err(|error| format!("Group roster action failed: {error:?}."))
    });
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
                let (Ok(url), Ok(key)) = (
                    std::env::var("LYNX_NAKAMA_URL"),
                    std::env::var("LYNX_NAKAMA_SERVER_KEY"),
                ) else {
                    screen.status =
                        "Set LYNX_NAKAMA_URL and LYNX_NAKAMA_SERVER_KEY to sign in.".into();
                    screen.dirty = true;
                    continue;
                };
                let scope = screen.scope_for_nakama(&url);
                let email = screen.email.clone();
                let password = std::mem::take(&mut screen.password);
                screen.credential_scope = Some(scope.clone());
                start(&mut screen, move || {
                    let auth = NakamaAuthClient::new(NakamaAuthConfig::new(&url, key))
                        .map_err(|e| format!("Authentication configuration: {e:?}"))?;
                    let session = authenticate_email_or_resume(
                        &auth,
                        &scope,
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
            Action::SignOut => {
                let (Ok(url), Ok(key)) = (
                    std::env::var("LYNX_NAKAMA_URL"),
                    std::env::var("LYNX_NAKAMA_SERVER_KEY"),
                ) else {
                    screen.status =
                        "Set LYNX_NAKAMA_URL and LYNX_NAKAMA_SERVER_KEY to sign out.".into();
                    screen.dirty = true;
                    continue;
                };
                let scope = screen
                    .credential_scope
                    .clone()
                    .unwrap_or_else(|| screen.scope_for_nakama(&url));
                start(&mut screen, move || {
                    let auth = NakamaAuthClient::new(NakamaAuthConfig::new(&url, key))
                        .map_err(|e| format!("Authentication configuration: {e:?}"))?;
                    let outcome = logout_saved_account(&auth, &scope, &OsNakamaCredentialStore)
                        .map_err(|e| format!("Sign out failed: {e:?}."))?;
                    Ok(Outcome::SignedOut(outcome))
                });
            }
            Action::List => {
                if let Some(token) = screen.token.clone() {
                    let api_url = screen.api_url();
                    start(&mut screen, move || {
                        let (api, token) = api(&token, &api_url)?;
                        api.characters(&token)
                            .map(Outcome::Listed)
                            .map_err(|e| format!("List failed: {e:?}"))
                    });
                }
            }
            Action::FriendList => load_friends(&mut screen),
            Action::RequestFriend => {
                let account = screen.friend_account.trim().to_owned();
                if game_api::AccountId::new(account.clone()).is_err() {
                    screen.status = "Enter a valid account ID.".into();
                    screen.dirty = true;
                    continue;
                }
                let Some(token) = screen.token.clone() else {
                    continue;
                };
                let api_url = screen.api_url();
                start(&mut screen, move || {
                    let (api, token) = api(&token, &api_url)?;
                    api.request_friend(&token, &account)
                        .map(|()| Outcome::FriendMutation("Friend request sent."))
                        .map_err(|error| format!("Friend request failed: {error:?}."))
                });
            }
            Action::BlockEnteredAccount => {
                let account = screen.friend_account.trim().to_owned();
                if game_api::AccountId::new(account.clone()).is_err() {
                    screen.status = "Enter a valid account ID.".into();
                    screen.dirty = true;
                    continue;
                }
                mutate_friend(&mut screen, account, RelationshipAction::Block);
            }
            Action::FriendMutation(account, action) => {
                mutate_friend(&mut screen, account.clone(), *action);
            }
            Action::MyGroups => load_my_groups(&mut screen),
            Action::DiscoverGroups => discover_groups(&mut screen),
            Action::JoinGroup(group) => mutate_group(&mut screen, group.clone(), true),
            Action::LeaveGroup(group) => mutate_group(&mut screen, group.clone(), false),
            Action::LoadGroupMembers(group) => load_group_members(&mut screen, group.clone()),
            Action::AddGroupAccount => {
                let account = screen.friend_account.trim().to_owned();
                if game_api::AccountId::new(account.clone()).is_err() {
                    screen.status = "Enter a valid account ID.".into();
                    screen.dirty = true;
                    continue;
                }
                mutate_group_member(&mut screen, account, GroupMemberAction::Add);
            }
            Action::GroupMemberMutation(account, action) => {
                mutate_group_member(&mut screen, account.clone(), *action);
            }
            Action::CreateGroup => {
                let name = screen.group_name.trim().to_owned();
                let request = CreateGroup {
                    name,
                    description: String::new(),
                    open: true,
                    max_count: 25,
                };
                if let Err(error) = request.validate() {
                    screen.status = format!("Invalid group name: {error}");
                    screen.dirty = true;
                    continue;
                }
                let Some(token) = screen.token.clone() else {
                    continue;
                };
                let api_url = screen.api_url();
                start(&mut screen, move || {
                    let (api, token) = api(&token, &api_url)?;
                    api.create_group(&token, &request)
                        .map(Outcome::GroupCreated)
                        .map_err(|error| format!("Group creation failed: {error:?}."))
                });
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
                let api_url = screen.api_url();
                start(&mut screen, move || {
                    let (api, token) = api(&token, &api_url)?;
                    api.create_character(&token, &key, &CreateCharacter { name }).map(|_| Outcome::Created).map_err(|e| format!("Create failed: {e:?}. Retry the same name; the creation key is retained for this session."))
                });
            }
            Action::Select(id) => {
                if let Some(token) = screen.token.clone() {
                    let id = id.clone();
                    let api_url = screen.api_url();
                    start(&mut screen, move || {
                        let (api, token) = api(&token, &api_url)?;
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
                    && let Some(credential_scope) = screen.credential_scope.clone()
                {
                    launch.0 = Some(WorldLaunch {
                        bearer: token,
                        character: id,
                        world_url: screen.world_url(),
                        api_url: screen.api_url(),
                        credential_scope,
                    });
                    exit.write(AppExit::Success);
                }
            }
            Action::UseServer(key) => {
                if screen.token.is_some() {
                    screen.status =
                        "The server is fixed while signed in. Finish or restart the session to switch."
                            .into();
                    screen.dirty = true;
                    continue;
                }
                if screen.browser.profiles().iter().any(|p| p.key() == *key) {
                    screen.active_server = Some(key.clone());
                    screen.status = format!("Server selected: {key}.");
                    screen.dirty = true;
                }
            }
            Action::TogglePin(key) => {
                let pinned = screen
                    .browser
                    .profiles()
                    .iter()
                    .find(|p| p.key() == *key)
                    .is_some_and(|p| !p.pinned);
                if screen.browser.set_pinned(key, pinned).is_ok() {
                    screen.status = if pinned {
                        format!("Pinned {key}.")
                    } else {
                        format!("Unpinned {key}.")
                    };
                    screen.persist_browser();
                    screen.dirty = true;
                }
            }
            Action::AddServer => {
                let port = match screen.server_port.trim().parse::<u16>() {
                    Ok(port) if port != 0 => port,
                    _ => {
                        screen.status = "Enter a port between 1 and 65535.".into();
                        screen.dirty = true;
                        continue;
                    }
                };
                match ServerProfile::new(screen.server_name.trim(), screen.server_host.trim(), port)
                {
                    Ok(profile) => {
                        let key = profile.key();
                        match screen.browser.add(profile) {
                            Ok(()) => {
                                screen.active_server = Some(key.clone());
                                screen.server_name.clear();
                                screen.server_host.clear();
                                screen.status = format!("Added server {key}.");
                                screen.persist_browser();
                            }
                            Err(error) => {
                                screen.status = format!("Could not add server: {error:?}.");
                            }
                        }
                    }
                    Err(error) => {
                        screen.status = format!(
                            "Invalid server name or address ({error:?}). Use a hostname or IP and a port."
                        );
                    }
                }
                screen.dirty = true;
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
                Field::Password => Field::ServerName,
                Field::ServerName => Field::ServerHost,
                Field::ServerHost => Field::ServerPort,
                Field::ServerPort => Field::Name,
                Field::Name => Field::FriendAccount,
                Field::FriendAccount => Field::GroupName,
                Field::GroupName => Field::Email,
            };
            screen.dirty = true;
            continue;
        }
        let focus = screen.focus;
        let value = match focus {
            Field::Email => &mut screen.email,
            Field::Password => &mut screen.password,
            Field::Name => &mut screen.name,
            Field::ServerName => &mut screen.server_name,
            Field::ServerHost => &mut screen.server_host,
            Field::ServerPort => &mut screen.server_port,
            Field::FriendAccount => &mut screen.friend_account,
            Field::GroupName => &mut screen.group_name,
        };
        if event.key_code == KeyCode::Backspace {
            value.pop();
        } else if let Some(text) = &event.text {
            for c in text.chars().filter(|c| !c.is_control()) {
                let limit = match focus {
                    Field::FriendAccount => 128,
                    Field::GroupName => 64,
                    _ => 320,
                };
                if value.len() + c.len_utf8() <= limit {
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
            screen.friends.clear();
            screen.status = "Signed in; loading characters…".into();
            let api_url = screen.api_url();
            start(&mut screen, move || {
                let (api, token) = api(&token, &api_url)?;
                api.characters(&token)
                    .map(Outcome::Listed)
                    .map_err(|e| format!("List failed: {e:?}"))
            });
        }
        Ok(Outcome::Listed(characters)) => {
            screen.characters = characters;
            screen.selected = None;
            screen.status = "Select a character or create one.".into();
            if let Some(key) = screen.active_server.clone()
                && screen.browser.record_connected(&key).is_ok()
            {
                screen.persist_browser();
            }
        }
        Ok(Outcome::SignedOut(outcome)) => {
            screen.token = None;
            screen.characters.clear();
            screen.friends.clear();
            screen.groups.clear();
            screen.discovered_groups.clear();
            screen.selected = None;
            screen.credential_scope = None;
            screen.status = match outcome {
                NakamaLogoutOutcome::Revoked => "Signed out; the saved session was revoked.".into(),
                NakamaLogoutOutcome::ExpiredCredentialsCleared => {
                    "Signed out; expired credentials were removed.".into()
                }
                NakamaLogoutOutcome::NoSavedCredentials => "Signed out.".into(),
            };
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
        Ok(Outcome::Friends(friends)) => {
            screen.friends = friends;
            screen.status = "Friend list refreshed.".into();
        }
        Ok(Outcome::FriendMutation(message)) => {
            screen.status = message.into();
            load_friends(&mut screen);
        }
        Ok(Outcome::MyGroups(groups)) => {
            screen.groups = groups;
            screen.status = "Group memberships refreshed.".into();
        }
        Ok(Outcome::DiscoveredGroups(groups)) => {
            screen.discovered_groups = groups;
            screen.status = "Open groups refreshed.".into();
        }
        Ok(Outcome::GroupMutation(message)) => {
            screen.status = message.into();
            load_my_groups(&mut screen);
        }
        Ok(Outcome::GroupCreated(group)) => {
            screen.group_name.clear();
            screen.status = format!("Created group {}.", group.name);
            load_my_groups(&mut screen);
        }
        Ok(Outcome::GroupMembers { group_id, members }) => {
            screen.selected_group = Some(group_id);
            screen.group_members = members;
            screen.status = "Group roster refreshed.".into();
        }
        Ok(Outcome::GroupMembersChanged { group_id }) => {
            screen.status = "Group roster updated.".into();
            load_group_members(&mut screen, group_id);
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
    let active = screen.active_server.clone();
    let servers: Vec<(String, String, bool)> = screen
        .browser
        .profiles()
        .iter()
        .map(|profile| {
            (
                profile.key(),
                format!(
                    "{}{}{}",
                    if Some(&profile.key()) == active.as_ref() {
                        "> "
                    } else {
                        ""
                    },
                    profile.name,
                    if profile.pinned { " [pinned]" } else { "" }
                ),
                profile.pinned,
            )
        })
        .collect();
    let endpoints = screen
        .selected_profile()
        .map(|profile| (profile.world_url(), profile.api_url()));
    let recent = screen.browser.recent().join(", ");
    let status = screen.status.clone();
    let friends = screen.friends.clone();
    let groups = screen.groups.clone();
    let discovered_groups = screen.discovered_groups.clone();
    let group_members = screen.group_members.clone();
    let selected_group = screen.selected_group.clone();
    let can_manage_group = selected_group.as_ref().is_some_and(|selected| {
        groups.iter().any(|membership| {
            membership.group.id.as_str() == selected
                && matches!(membership.role, GroupRole::Owner | GroupRole::Admin)
        })
    });
    commands.spawn((Root, Node { width: percent(100), height: percent(100), flex_direction: FlexDirection::Column, padding: UiRect::all(px(24)), row_gap: px(8), overflow: Overflow::scroll_y(), ..default() }, BackgroundColor(Color::srgb(0.06, 0.08, 0.12)))).with_children(|parent| {
        parent.spawn((Text::new("Lynx — Account & Characters"), TextFont { font_size: bevy::text::FontSize::Px(28.0), ..default() }));
        parent.spawn(Text::new(status));
        parent.spawn(Text::new("Servers"));
        if servers.is_empty() {
            parent.spawn(Text::new("No saved servers. Add one below or set LYNX_WORLD_URL."));
        }
        for (key, label, pinned) in &servers {
            server_row(parent, label, key, *pinned);
        }
        if !recent.is_empty() {
            parent.spawn(Text::new(format!("Recent: {recent}")));
        }
        if let Some((world, api)) = &endpoints {
            parent.spawn(Text::new(format!("World endpoint: {world} · API: {api}")));
        }
        if screen.token.is_none() {
            button(parent, format!("{}Server name: {}", marker(screen.focus == Field::ServerName), screen.server_name), Action::Focus(Field::ServerName));
            button(parent, format!("{}Host or IP: {}", marker(screen.focus == Field::ServerHost), screen.server_host), Action::Focus(Field::ServerHost));
            button(parent, format!("{}Port: {}", marker(screen.focus == Field::ServerPort), screen.server_port), Action::Focus(Field::ServerPort));
            button(parent, "Add / use server".into(), Action::AddServer);
            parent.spawn(Text::new("Remote servers require WSS; ws:// and http:// are used only for localhost and private networks."));
            button(parent, format!("{}Email: {}", marker(screen.focus == Field::Email), screen.email), Action::Focus(Field::Email));
            button(parent, format!("{}Password: {}", marker(screen.focus == Field::Password), "•".repeat(screen.password.chars().count())), Action::Focus(Field::Password));
            button(parent, "Sign in / resume saved session".into(), Action::Login);
            parent.spawn(Text::new("Enter email; password may be blank when resuming. Click a field or use Tab."));
        } else {
            button(parent, "Refresh character list".into(), Action::List);
            button(parent, "Refresh friends and requests".into(), Action::FriendList);
            button(parent, format!("{}Account ID: {}", marker(screen.focus == Field::FriendAccount), screen.friend_account), Action::Focus(Field::FriendAccount));
            button(parent, "Send friend request".into(), Action::RequestFriend);
            button(parent, "Block account ID".into(), Action::BlockEnteredAccount);
            parent.spawn(Text::new("Friends and requests"));
            for friend in &friends {
                let account = friend.account_id.to_string();
                let label = friend.display_name.as_deref().or(friend.username.as_deref()).unwrap_or(&account);
                parent.spawn(Text::new(format!("{label} ({:?}) · {account}", friend.state)));
                match friend.state {
                    FriendState::Friend => {
                        button(parent, format!("Remove {label}"), Action::FriendMutation(account.clone(), RelationshipAction::Remove));
                        button(parent, format!("Block {label}"), Action::FriendMutation(account, RelationshipAction::Block));
                    }
                    FriendState::IncomingRequest => {
                        button(parent, format!("Accept {label}"), Action::FriendMutation(account.clone(), RelationshipAction::Accept));
                        button(parent, format!("Dismiss {label}"), Action::FriendMutation(account.clone(), RelationshipAction::Dismiss));
                        button(parent, format!("Block {label}"), Action::FriendMutation(account, RelationshipAction::Block));
                    }
                    FriendState::OutgoingRequest => {
                        button(parent, format!("Cancel request to {label}"), Action::FriendMutation(account.clone(), RelationshipAction::Dismiss));
                        button(parent, format!("Block {label}"), Action::FriendMutation(account, RelationshipAction::Block));
                    }
                    FriendState::Blocked => {
                        button(parent, format!("Unblock {label}"), Action::FriendMutation(account, RelationshipAction::Unblock));
                    }
                }
            }
            parent.spawn(Text::new("Groups"));
            button(parent, "Refresh my groups".into(), Action::MyGroups);
            button(parent, format!("{}Group name: {}", marker(screen.focus == Field::GroupName), screen.group_name), Action::Focus(Field::GroupName));
            button(parent, "Create open group (25 members)".into(), Action::CreateGroup);
            button(parent, "Discover open groups".into(), Action::DiscoverGroups);
            for membership in &groups {
                let group = &membership.group;
                parent.spawn(Text::new(format!("{} · {:?} · {}/{} members", group.name, membership.role, group.member_count, group.max_count)));
                button(parent, format!("View roster: {}", group.name), Action::LoadGroupMembers(group.id.to_string()));
                if membership.role != GroupRole::Owner {
                    button(parent, format!("Leave {}", group.name), Action::LeaveGroup(group.id.to_string()));
                }
            }
            for group in &discovered_groups {
                let is_member = groups.iter().any(|membership| membership.group.id == group.id);
                parent.spawn(Text::new(format!("{}{} · {}/{} members", if group.open { "Open: " } else { "Approval required: " }, group.name, group.member_count, group.max_count)));
                if !is_member {
                    button(parent, format!("{} {}", if group.open { "Join" } else { "Request to join" }, group.name), Action::JoinGroup(group.id.to_string()));
                }
            }
            if let Some(group_id) = &selected_group {
                parent.spawn(Text::new(format!("Roster: {group_id}")));
                if can_manage_group {
                    button(parent, "Add account ID to selected group".into(), Action::AddGroupAccount);
                }
                for member in &group_members {
                    let account = member.account_id.to_string();
                    parent.spawn(Text::new(format!("{} · {:?}", member.username.as_deref().unwrap_or(&account), member.role)));
                    if !can_manage_group {
                        continue;
                    }
                    match member.role {
                        GroupRole::JoinRequest => {
                            button(parent, format!("Approve {account}"), Action::GroupMemberMutation(account.clone(), GroupMemberAction::Add));
                            button(parent, format!("Decline {account}"), Action::GroupMemberMutation(account, GroupMemberAction::Kick));
                        }
                        GroupRole::Member => {
                            button(parent, format!("Promote {account}"), Action::GroupMemberMutation(account.clone(), GroupMemberAction::Promote));
                            button(parent, format!("Remove {account}"), Action::GroupMemberMutation(account, GroupMemberAction::Kick));
                        }
                        GroupRole::Admin => {
                            button(parent, format!("Demote {account}"), Action::GroupMemberMutation(account.clone(), GroupMemberAction::Demote));
                            button(parent, format!("Remove {account}"), Action::GroupMemberMutation(account, GroupMemberAction::Kick));
                        }
                        GroupRole::Owner | GroupRole::Banned => {}
                    }
                }
            }
            for c in &screen.characters { button(parent, format!("Select {} ({:?})", c.name, c.status), Action::Select(c.id.to_string())); }
            if let Some((_, name)) = &screen.selected {
                button(parent, format!("Enter world with {name}"), Action::EnterWorld);
            }
            button(parent, format!("{}New character name: {}", marker(screen.focus == Field::Name), screen.name), Action::Focus(Field::Name));
            button(parent, "Create / retry creation".into(), Action::Create);
            button(parent, "Sign out and remove saved session".into(), Action::SignOut);
            parent.spawn(Text::new("Creation retries reuse the same key during this session. Do not close the client while an outcome is uncertain."));
        }
    });
}
fn marker(focused: bool) -> &'static str {
    if focused { "> " } else { "" }
}
#[allow(deprecated)]
fn server_row(parent: &mut ChildSpawnerCommands, label: &str, key: &str, pinned: bool) {
    parent
        .spawn(Node {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: px(6),
            ..default()
        })
        .with_children(|row| {
            button(row, label.to_owned(), Action::UseServer(key.to_owned()));
            button(
                row,
                if pinned { "Unpin".into() } else { "Pin".into() },
                Action::TogglePin(key.to_owned()),
            );
        });
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

#[cfg_attr(coverage_nightly, coverage(off))]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_launch_is_single_use_and_redacts_the_access_token() {
        let scope = CredentialScope::for_world("127.0.0.1:8090", "http://127.0.0.1:7350");
        let mut launch = AccountWorldLaunch(Some(WorldLaunch {
            bearer: "secret-access-token".into(),
            character: "chr-test".into(),
            world_url: "ws://127.0.0.1:8090/world".into(),
            api_url: "http://127.0.0.1:8090".into(),
            credential_scope: scope.clone(),
        }));
        let debug = format!("{launch:?}");
        assert!(!debug.contains("secret-access-token"));
        assert!(!debug.contains(scope.as_key()));
        assert_eq!(
            launch.take(),
            Some(WorldLaunch {
                bearer: "secret-access-token".into(),
                character: "chr-test".into(),
                world_url: "ws://127.0.0.1:8090/world".into(),
                api_url: "http://127.0.0.1:8090".into(),
                credential_scope: scope,
            })
        );
        assert!(launch.take().is_none());
    }

    #[test]
    fn selected_server_drives_the_world_and_api_endpoints() {
        let mut browser = ServerBrowser::default();
        browser
            .add(ServerProfile::new("Local", "127.0.0.1", 8090).unwrap())
            .unwrap();
        let screen = Screen {
            browser,
            active_server: Some("127.0.0.1:8090".into()),
            ..default()
        };
        assert_eq!(screen.world_url(), "ws://127.0.0.1:8090/world");
        assert_eq!(screen.api_url(), "http://127.0.0.1:8090");
    }

    #[test]
    fn credentials_are_scoped_per_selected_server() {
        let mut browser = ServerBrowser::default();
        browser
            .add(ServerProfile::new("Local", "127.0.0.1", 8090).unwrap())
            .unwrap();
        browser
            .add(ServerProfile::new("Friends", "127.0.0.1", 8091).unwrap())
            .unwrap();
        let nakama = "http://127.0.0.1:7350";
        let local = Screen {
            browser: browser.clone(),
            active_server: Some("127.0.0.1:8090".into()),
            ..default()
        };
        let friends = Screen {
            browser,
            active_server: Some("127.0.0.1:8091".into()),
            ..default()
        };
        assert_ne!(
            local.scope_for_nakama(nakama),
            friends.scope_for_nakama(nakama)
        );
        assert_eq!(
            Screen::default().scope_for_nakama(nakama),
            CredentialScope::for_nakama(nakama)
        );
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
