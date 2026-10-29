#[cfg(feature = "window")]
fn main() {
    use bevy::prelude::*;
    use lynx_client::{
        LynxClientPlugin,
        online::OnlineClient,
        replay::{ReplayFixture, ReplayInput},
        windowed::{OfflineWindowPlugin, OnlineWindowPlugin},
    };

    let mut args = std::env::args_os().skip(1);
    let mode = match (args.next(), args.next(), args.next()) {
        (None, None, None) => (None, None),
        (Some(mode), None, None) if mode == "--demo-online" => (Some(false), None),
        (Some(mode), Some(character), None) if mode == "--login" => (Some(true), Some(character)),
        (Some(mode), Some(path), None) if mode == "--replay" => (None, Some(path)),
        _ => {
            eprintln!(
                "Usage: lynx_client [--demo-online | --login <character-id> | --replay <fixture.json>]"
            );
            std::process::exit(2);
        }
    };
    let (login, replay_path) = mode;
    let replay = replay_path.filter(|_| login.is_none()).map(|path| {
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
            authenticate_device(std::env::args().nth(2).as_deref().unwrap_or_default())
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

#[cfg(feature = "window")]
fn authenticate_device(character: &str) -> (String, Option<String>) {
    use game_api::DeviceLogin;
    use lynx_client_net::{NakamaAuthClient, NakamaAuthConfig};
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
    // New, intentionally ephemeral identity: this avoids persisting a device ID,
    // but means this account cannot be recovered after the process exits.
    let mut random = [0_u8; 16];
    if getrandom::fill(&mut random).is_err() {
        eprintln!("Could not generate a temporary device identifier.");
        std::process::exit(2);
    }
    let hex = b"0123456789abcdef";
    let mut device_id = String::with_capacity(random.len() * 2);
    for byte in random {
        device_id.push(char::from(hex[usize::from(byte >> 4)]));
        device_id.push(char::from(hex[usize::from(byte & 0x0f)]));
    }
    let result = std::thread::spawn(move || {
        let auth = NakamaAuthClient::new(NakamaAuthConfig::new(base_url, server_key))?;
        auth.authenticate_device(&DeviceLogin {
            device_id,
            username: None,
            create: true,
            locale: engine_core::Locale::default(),
        })
    })
    .join();
    match result {
        Ok(Ok(session)) => (
            session.access_token().to_owned(),
            Some(character.to_owned()),
        ),
        Ok(Err(error)) => {
            eprintln!(
                "Nakama login failed ({error:?}); check local configuration, network, and account availability."
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
