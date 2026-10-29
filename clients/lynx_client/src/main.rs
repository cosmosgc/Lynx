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
        (None, None, None) => (false, None),
        (Some(mode), None, None) if mode == "--demo-online" => (true, None),
        (Some(mode), Some(path), None) if mode == "--replay" => (false, Some(path)),
        _ => {
            eprintln!("Usage: lynx_client [--demo-online | --replay <fixture.json>]");
            std::process::exit(2);
        }
    };
    let (online, replay_path) = mode;
    let replay = replay_path.map(|path| {
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
    let _runtime = if online {
        let Some(bearer) = std::env::var_os("LYNX_DEMO_TOKEN")
            .and_then(|s| s.into_string().ok())
            .filter(|s| !s.is_empty())
        else {
            eprintln!("Set LYNX_DEMO_TOKEN for development-only online mode.");
            std::process::exit(2);
        };
        let url =
            std::env::var("LYNX_DEMO_URL").unwrap_or_else(|_| "ws://127.0.0.1:8090/world".into());
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
                    character: None,
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
