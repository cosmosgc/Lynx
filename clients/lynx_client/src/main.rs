#[cfg(feature = "window")]
#[allow(clippy::too_many_lines)] // The entry point keeps the existing mode dispatch together.
fn main() {
    use bevy::prelude::*;
    use lynx_client::{
        LynxClientPlugin,
        online::OnlineClient,
        replay::{ReplayFixture, ReplayInput},
        windowed::{OfflineWindowPlugin, OnlineWindowPlugin},
    };
    use std::ffi::OsStr;

    load_repository_env();

    if std::env::args_os().nth(1).as_deref() == Some(OsStr::new("--account")) {
        if std::env::args_os().nth(2).is_some() {
            eprintln!("Usage: lynx_client --account");
            std::process::exit(2);
        }
        let mut account_app = App::new();
        account_app
            .add_plugins(DefaultPlugins)
            .insert_resource(lynx_client::account_ui::AccountWorldLaunch::default())
            .add_plugins(lynx_client::account_ui::AccountUiPlugin);
        let exit = account_app.run();
        if !exit.is_success() {
            std::process::exit(1);
        }
        if let Some((bearer, character)) = account_app
            .world_mut()
            .resource_mut::<lynx_client::account_ui::AccountWorldLaunch>()
            .take()
        {
            run_account_world(bearer, character);
        }
        return;
    }

    if character_command() {
        return;
    }

    if std::env::args_os().nth(1).as_deref() == Some(OsStr::new("--logout")) {
        if std::env::args_os().nth(2).is_some() {
            eprintln!("Usage: lynx_client --logout");
            std::process::exit(2);
        }
        logout_device();
        return;
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let first = args.first().map(String::as_str);
    let mut app = App::new();
    app.add_plugins(DefaultPlugins)
        .add_plugins(LynxClientPlugin);
    let _runtime = if let Some(first) =
        first.filter(|first| matches!(*first, "--demo-online" | "--login" | "--login-email"))
    {
        let (login, character, email) = match (first, args.get(1), args.get(2), args.get(3)) {
            ("--demo-online", None, None, None) => (false, None, None),
            ("--login", Some(character), None, None) => (true, Some(character.clone()), None),
            ("--login-email", Some(email), Some(character), None) => {
                (true, Some(character.clone()), Some(email.clone()))
            }
            _ => {
                eprintln!(
                    "Usage: lynx_client [--demo-online | --login <id> | --login-email <email> <id>]"
                );
                std::process::exit(2);
            }
        };
        let (bearer, character) = if login {
            let character = character.unwrap_or_default();
            authenticate_device(&character, email)
        } else {
            let Some(bearer) = std::env::var_os("LYNX_DEMO_TOKEN")
                .and_then(|s| s.into_string().ok())
                .filter(|s| !s.is_empty())
            else {
                eprintln!("Set LYNX_DEMO_TOKEN for development-only online mode.");
                std::process::exit(2);
            };
            (bearer, None)
        };
        let url = std::env::var("LYNX_WORLD_URL")
            .or_else(|_| std::env::var("LYNX_DEMO_URL"))
            .unwrap_or_else(|_| "ws://127.0.0.1:8090/world".into());
        let version =
            semver::Version::parse(env!("CARGO_PKG_VERSION")).expect("valid package version");
        let runtime = tokio::runtime::Runtime::new().expect("Tokio runtime");
        let mut client = OnlineClient::new(1.0).expect("valid prediction limit");
        {
            let _guard = runtime.enter();
            client
                .connect(lynx_client_net::Config {
                    url,
                    bearer,
                    character,
                    handshake: engine_net::protocol::Handshake::new(version.clone(), version),
                    last_ack_tick: engine_core::time::Tick::ZERO,
                })
                .expect("new connection generation");
        }
        app.insert_resource(client).add_plugins(OnlineWindowPlugin);
        Some(runtime)
    } else {
        let options = lynx_client::cli::parse_offline_args(args).unwrap_or_else(|error| {
            eprintln!("{error}");
            std::process::exit(2);
        });
        let scene_options = lynx_client::zone_scene::SceneLoadOptions {
            content_root: options.content_root.clone().or_else(default_content_root),
            zone: options.zone.clone(),
        };
        let scene = lynx_client::zone_scene::load_scene(&scene_options).unwrap_or_else(|error| {
            eprintln!("Could not load zone: {error}");
            std::process::exit(1);
        });
        report_scene(&scene, options.describe);
        let bindings = options
            .bindings
            .as_deref()
            .map_or_else(lynx_client::offline::InputBindings::default, load_bindings);
        let replay = options.replay.as_deref().map(|path| {
            let json = std::fs::read_to_string(path).unwrap_or_else(|error| {
                eprintln!("Could not read replay fixture: {error}");
                std::process::exit(2);
            });
            ReplayFixture::from_json(&json).unwrap_or_else(|error| {
                eprintln!("Invalid replay fixture: {error}");
                std::process::exit(2);
            })
        });
        app.insert_resource(lynx_client::offline::OfflineScene {
            avatar_position: scene.spawn,
        })
        .insert_resource(scene)
        .insert_resource(bindings)
        .insert_resource(lynx_client::offline::CameraController {
            mode: options.camera,
        })
        .add_plugins(OfflineWindowPlugin);
        if let Some(replay) = replay {
            app.insert_resource(ReplayInput::new(replay));
        }
        None
    };
    app.run();
}

#[cfg(feature = "window")]
fn default_content_root() -> Option<std::path::PathBuf> {
    if let Some(path) = std::env::var_os("LYNX_CONTENT_DIR")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_dir())
    {
        return Some(path);
    }
    let local = std::path::PathBuf::from("content");
    local.is_dir().then_some(local)
}

#[cfg(feature = "window")]
fn load_bindings(path: &std::path::Path) -> lynx_client::offline::InputBindings {
    let json = std::fs::read_to_string(path).unwrap_or_else(|error| {
        eprintln!("Could not read bindings file {}: {error}", path.display());
        std::process::exit(2);
    });
    lynx_client::offline::InputBindings::from_json(&json).unwrap_or_else(|error| {
        eprintln!("Invalid bindings file {}: {error}", path.display());
        std::process::exit(2);
    })
}

#[cfg(feature = "window")]
fn report_scene(scene: &lynx_client::zone_scene::ZoneScene, describe: bool) {
    for issue in scene.issues.iter().take(20) {
        eprintln!("Scene warning [{}]: {}", issue.code, issue.message);
    }
    if scene.issues.len() > 20 {
        eprintln!(
            "Scene warning: {} additional presentation warning(s) omitted",
            scene.issues.len() - 20
        );
    }
    if describe {
        println!("{}", scene.summary());
    }
}

#[cfg(feature = "window")]
fn run_account_world(bearer: String, character: String) {
    use bevy::prelude::*;
    use lynx_client::{LynxClientPlugin, online::OnlineClient, windowed::OnlineWindowPlugin};

    let url =
        std::env::var("LYNX_WORLD_URL").unwrap_or_else(|_| "ws://127.0.0.1:8090/world".into());
    let version = semver::Version::parse(env!("CARGO_PKG_VERSION")).expect("valid package version");
    let Ok(runtime) = tokio::runtime::Runtime::new() else {
        eprintln!("Could not initialize the world network runtime.");
        return;
    };
    let Ok(mut client) = OnlineClient::new(1.0) else {
        eprintln!("Could not initialize the world client.");
        return;
    };
    {
        let _guard = runtime.enter();
        if client
            .connect(lynx_client_net::Config {
                url,
                bearer,
                character: Some(character),
                handshake: engine_net::protocol::Handshake::new(version.clone(), version),
                last_ack_tick: engine_core::time::Tick::ZERO,
            })
            .is_err()
        {
            eprintln!("Could not start world admission.");
            return;
        }
    }
    let mut app = App::new();
    app.add_plugins(DefaultPlugins)
        .add_plugins(LynxClientPlugin)
        .insert_resource(client)
        .add_plugins(OnlineWindowPlugin);
    let _runtime = runtime;
    app.run();
}

fn load_repository_env() {
    let env_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.env");
    // Explicit process environment wins over the repository's local defaults.
    let _ = dotenvy::from_path(env_path);
}

#[cfg(feature = "window")]
fn character_command() -> bool {
    use game_api::{CharacterCreationKey, CreateCharacter};
    use lynx_client::nakama_credentials::{
        NakamaLoginError, OsNakamaCredentialStore, authenticate_email_or_resume,
        authenticate_or_resume,
    };
    use lynx_client_net::{NakamaAuthClient, NakamaAuthConfig};

    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first().map(String::as_str) else {
        return false;
    };
    if !matches!(
        command,
        "--characters" | "--create-character" | "--select-character" | "--email-login"
    ) {
        return false;
    }
    if !matches!(
        (command, args.len()),
        ("--characters", 1)
            | ("--create-character", 3)
            | ("--select-character" | "--email-login", 2)
    ) {
        eprintln!(
            "Usage: lynx_client --characters | --create-character <name> <intent-key> | --select-character <id> | --email-login <email>"
        );
        std::process::exit(2);
    }
    if command == "--create-character"
        && ((CreateCharacter {
            name: args[1].clone(),
        })
        .validate()
        .is_err()
            || CharacterCreationKey::new(&args[2]).is_err())
    {
        eprintln!(
            "Invalid name or intent key. Names must contain 1–32 characters without surrounding whitespace or control characters; keys use 1–128 ASCII letters, digits, underscores or hyphens."
        );
        std::process::exit(2);
    }
    let required = |key: &str| {
        std::env::var(key).unwrap_or_else(|_| {
            eprintln!("Missing required environment setting {key}.");
            std::process::exit(2);
        })
    };
    let base_url = required("LYNX_NAKAMA_URL");
    let server_key = required("LYNX_NAKAMA_SERVER_KEY");
    let api_url =
        std::env::var("LYNX_WORLD_API_URL").unwrap_or_else(|_| "http://127.0.0.1:8090".into());
    let result = std::thread::spawn(move || -> Result<(), String> {
        let auth = NakamaAuthClient::new(NakamaAuthConfig::new(&base_url, server_key)).map_err(|e| format!("{e:?}"))?;
        let session = if args[0] == "--email-login" {
            authenticate_email_or_resume(
                &auth,
                &base_url,
                &OsNakamaCredentialStore,
                &args[1],
                || {
                    rpassword::prompt_password("Password: ")
                        .map_err(|_| NakamaLoginError::PasswordInput)
                },
            )
        } else {
            authenticate_or_resume(
                &auth,
                &base_url,
                &OsNakamaCredentialStore,
                engine_core::Locale::default(),
            )
        }
        .map_err(|e| format!("{e:?}"))?;
        if args[0] == "--email-login" {
            println!("Signed in to the Nakama account. Use --characters to list or --create-character to make a character.");
            return Ok(());
        }
        run_character_api(&args, api_url, session.access_token())
    }).join();
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            eprintln!(
                "Character operation failed ({error}). For an uncertain creation outcome, retry the same name and intent key."
            );
            std::process::exit(1);
        }
        Err(_) => {
            eprintln!(
                "Character operation worker failed. Retry creation using the same name and intent key."
            );
            std::process::exit(1);
        }
    }
    true
}

#[cfg(feature = "window")]
fn run_character_api(args: &[String], api_url: String, access_token: &str) -> Result<(), String> {
    use game_api::{CreateCharacter, SessionToken};
    use lynx_client_net::CharacterApiClient;

    let token = SessionToken::new(access_token).map_err(|_| "invalid access session".to_owned())?;
    let api = CharacterApiClient::new(api_url).map_err(|error| format!("{error:?}"))?;
    match args[0].as_str() {
        "--characters" => {
            let characters = api
                .characters(&token)
                .map_err(|error| format!("{error:?}"))?;
            if characters.is_empty() {
                println!(
                    "No characters yet. Create one with --create-character <name> <intent-key>."
                );
            }
            for character in characters {
                println!(
                    "{}\t{}\t{:?}",
                    character.id, character.name, character.status
                );
            }
        }
        "--create-character" => {
            let receipt = api
                .create_character(
                    &token,
                    &args[2],
                    &CreateCharacter {
                        name: args[1].clone(),
                    },
                )
                .map_err(|error| format!("{error:?}"))?;
            println!(
                "Character created (or previously created): {}. Select with --select-character or enter with --login.",
                receipt.character_id
            );
            println!(
                "World admission requires a corresponding live actor; character creation does not provision one."
            );
        }
        _ => {
            let character = api
                .select_character(&token, &args[1])
                .map_err(|error| format!("{error:?}"))?;
            println!(
                "Selected {} ({}). Selection verifies ownership but does not admit an actor.",
                character.name, character.id
            );
            println!(
                "Use --login {} to attempt world entry; admission is unavailable unless the server has a corresponding live actor.",
                character.id
            );
        }
    }
    Ok(())
}

#[cfg(feature = "window")]
fn logout_device() {
    use lynx_client::nakama_credentials::{
        NakamaLoginError, NakamaLogoutOutcome, OsNakamaCredentialStore, logout_saved_account,
    };
    use lynx_client_net::{NakamaAuthClient, NakamaAuthConfig};

    let required = |key: &str| {
        std::env::var(key).unwrap_or_else(|_| {
            eprintln!("Missing required environment setting {key}.");
            std::process::exit(2);
        })
    };
    let base_url = required("LYNX_NAKAMA_URL");
    let server_key = required("LYNX_NAKAMA_SERVER_KEY");
    let credential_server_url = base_url.clone();
    let result = std::thread::spawn(move || {
        let auth = NakamaAuthClient::new(NakamaAuthConfig::new(base_url, server_key))
            .map_err(NakamaLoginError::Authentication)?;
        logout_saved_account(&auth, &credential_server_url, &OsNakamaCredentialStore)
    })
    .join();
    match result {
        Ok(Ok(NakamaLogoutOutcome::NoSavedCredentials)) => {
            println!("No Nakama credentials are saved for this server.");
        }
        Ok(Ok(NakamaLogoutOutcome::Revoked)) => {
            println!("Nakama session revoked and credentials removed.");
        }
        Ok(Ok(NakamaLogoutOutcome::ExpiredCredentialsCleared)) => {
            println!("Expired Nakama credentials removed; no active session remained.");
        }
        Ok(Err(error)) => {
            eprintln!(
                "Nakama logout failed ({error:?}); saved credentials were retained when possible."
            );
            std::process::exit(1);
        }
        Err(_) => {
            eprintln!("Nakama logout worker failed.");
            std::process::exit(1);
        }
    }
}

#[cfg(feature = "window")]
fn authenticate_device(character: &str, email: Option<String>) -> (String, Option<String>) {
    use lynx_client::nakama_credentials::{
        NakamaLoginError, OsNakamaCredentialStore, authenticate_email_or_resume,
        authenticate_or_resume,
    };
    use lynx_client_net::{CharacterApiClient, NakamaAuthClient, NakamaAuthConfig};
    let required = |key: &str| {
        std::env::var(key).unwrap_or_else(|_| {
            eprintln!("Missing required environment setting {key}.");
            std::process::exit(2);
        })
    };
    if character.is_empty() {
        eprintln!("Usage: lynx_client --login <character-id>");
        std::process::exit(2);
    }
    let base_url = required("LYNX_NAKAMA_URL");
    let server_key = required("LYNX_NAKAMA_SERVER_KEY");
    let character_api_url =
        std::env::var("LYNX_WORLD_API_URL").unwrap_or_else(|_| "http://127.0.0.1:8090".into());
    let character_id = character.to_owned();
    let credential_server_url = base_url.clone();
    let result = std::thread::spawn(move || -> Result<_, String> {
        let auth = NakamaAuthClient::new(NakamaAuthConfig::new(base_url, server_key))
            .map_err(|error| format!("{error:?}"))?;
        let session = if let Some(email) = email {
            authenticate_email_or_resume(&auth, &credential_server_url, &OsNakamaCredentialStore, &email, || {
                rpassword::prompt_password("Password: ").map_err(|_| NakamaLoginError::PasswordInput)
            })
        } else { authenticate_or_resume(
            &auth,
            &credential_server_url,
            &OsNakamaCredentialStore,
            engine_core::Locale::default(),
        ) }
        .map_err(|error| format!("{error:?}"))?;
        let character_api =
            CharacterApiClient::new(character_api_url).map_err(|error| format!("{error:?}"))?;
        let token = game_api::SessionToken::new(session.access_token())
            .map_err(|_| "invalid access session".to_owned())?;
        character_api
            .select_character(&token, &character_id)
            .map_err(|error| format!("{error:?}"))?;
        eprintln!("Character ownership verified. World admission requires a corresponding live actor; account creation does not provision one.");
        Ok(session)
    })
    .join();
    match result {
        Ok(Ok(session)) => (
            session.access_token().to_owned(),
            Some(character.to_owned()),
        ),
        Ok(Err(error)) => {
            eprintln!(
                "Login or character selection failed ({error}); check configuration and ownership. If an email session expired, use --login-email to re-enter your password."
            );
            std::process::exit(1);
        }
        Err(_) => {
            eprintln!("Nakama login worker failed.");
            std::process::exit(1);
        }
    }
}

#[cfg(not(feature = "window"))]
fn main() {
    use lynx_client::{
        cli::parse_offline_args,
        offline::{CameraController, InputBindings, OfflineScene},
        replay::{ReplayFixture, ReplayInput},
        zone_scene::{SceneLoadOptions, load_scene},
    };

    load_repository_env();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let options = parse_offline_args(args).unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(2);
    });
    let scene = load_scene(&SceneLoadOptions {
        content_root: options.content_root.clone().or_else(default_content_root),
        zone: options.zone.clone(),
    })
    .unwrap_or_else(|error| {
        eprintln!("Could not load zone: {error}");
        std::process::exit(1);
    });
    for issue in scene.issues.iter().take(20) {
        eprintln!("Scene warning [{}]: {}", issue.code, issue.message);
    }
    if scene.issues.len() > 20 {
        eprintln!(
            "Scene warning: {} additional presentation warning(s) omitted",
            scene.issues.len() - 20
        );
    }
    let bindings = options
        .bindings
        .as_deref()
        .map_or_else(InputBindings::default, |path| {
            let json = std::fs::read_to_string(path).unwrap_or_else(|error| {
                eprintln!("Could not read bindings file {}: {error}", path.display());
                std::process::exit(2);
            });
            InputBindings::from_json(&json).unwrap_or_else(|error| {
                eprintln!("Invalid bindings file {}: {error}", path.display());
                std::process::exit(2);
            })
        });

    let mut app = lynx_client::headless_app();
    app.insert_resource(OfflineScene {
        avatar_position: scene.spawn,
    })
    .insert_resource(scene)
    .insert_resource(bindings)
    .insert_resource(CameraController {
        mode: options.camera,
    });

    if let Some(path) = options.replay {
        let json = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            eprintln!("Could not read replay fixture: {error}");
            std::process::exit(2);
        });
        let fixture = ReplayFixture::from_json(&json).unwrap_or_else(|error| {
            eprintln!("Invalid replay fixture: {error}");
            std::process::exit(2);
        });
        let frame_count = fixture.frames.len();
        app.insert_resource(ReplayInput::new(fixture));
        for _ in 0..frame_count {
            app.update();
        }
        let position = app.world().resource::<OfflineScene>().avatar_position;
        println!("replayed {frame_count} frames; presentation position: {position:?}");
    } else {
        app.update();
    }
    if options.describe {
        println!(
            "{}",
            app.world()
                .resource::<lynx_client::zone_scene::ZoneScene>()
                .summary()
        );
    }
}

#[cfg(not(feature = "window"))]
fn default_content_root() -> Option<std::path::PathBuf> {
    if let Some(path) = std::env::var_os("LYNX_CONTENT_DIR")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_dir())
    {
        return Some(path);
    }
    let local = std::path::PathBuf::from("content");
    local.is_dir().then_some(local)
}
