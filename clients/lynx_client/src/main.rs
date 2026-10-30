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
        App::new()
            .add_plugins(DefaultPlugins)
            .add_plugins(lynx_client::account_ui::AccountUiPlugin)
            .run();
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
    let mut args = std::env::args_os().skip(1);
    let mode = match (args.next(), args.next(), args.next(), args.next()) {
        (None, None, None, None) => (None, None, None),
        (Some(mode), None, None, None) if mode == "--demo-online" => (Some(false), None, None),
        (Some(mode), Some(character), None, None) if mode == "--login" => {
            (Some(true), Some(character), None)
        }
        (Some(mode), Some(email), Some(character), None) if mode == "--login-email" => {
            (Some(true), Some(character), Some(email))
        }
        (Some(mode), Some(path), None, None) if mode == "--replay" => (None, Some(path), None),
        _ => {
            eprintln!(
                "Usage: lynx_client [--account | --characters | --create-character <name> <intent-key> | --select-character <id> | --email-login <email> | --demo-online | --login <id> | --login-email <email> <id> | --logout | --replay <fixture.json>]"
            );
            std::process::exit(2);
        }
    };
    let (login, path, email) = mode;
    let replay = path.clone().filter(|_| login.is_none()).map(|path| {
        let json = std::fs::read_to_string(path).unwrap_or_else(|error| {
            eprintln!("Could not read replay fixture: {error}");
            std::process::exit(2);
        });
        ReplayFixture::from_json(&json).unwrap_or_else(|error| {
            eprintln!("Invalid replay fixture: {error}");
            std::process::exit(2);
        })
    });
    let mut app = App::new();
    app.add_plugins(DefaultPlugins)
        .add_plugins(LynxClientPlugin);
    // Keep the runtime alive until the window and its network task have exited.
    let _runtime = if let Some(login) = login {
        let (bearer, character) = if login {
            let character = path.and_then(|s| s.into_string().ok()).unwrap_or_default();
            let email = email.map(|s| s.into_string().unwrap_or_default());
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
        app.add_plugins(OfflineWindowPlugin);
        if let Some(replay) = replay {
            app.insert_resource(ReplayInput::new(replay));
        }
        None
    };
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
        offline::OfflineScene,
        replay::{ReplayFixture, ReplayInput},
    };

    load_repository_env();

    let mut args = std::env::args_os().skip(1);
    let mut app = lynx_client::headless_app();
    if let Some(flag) = args.next() {
        if flag != "--replay" {
            eprintln!("Usage: lynx_client [--replay <fixture.json>]");
            std::process::exit(2);
        }
        let Some(path) = args.next() else {
            eprintln!("Usage: lynx_client [--replay <fixture.json>]");
            std::process::exit(2);
        };
        if args.next().is_some() {
            eprintln!("Usage: lynx_client [--replay <fixture.json>]");
            std::process::exit(2);
        }
        let json = std::fs::read_to_string(path).unwrap_or_else(|error| {
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
}
