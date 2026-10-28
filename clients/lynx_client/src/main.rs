#[cfg(feature = "window")]
fn main() {
    use bevy::prelude::*;
    use lynx_client::{
        LynxClientPlugin,
        online::OnlineClient,
        windowed::{OfflineWindowPlugin, OnlineWindowPlugin},
    };

    let mut args = std::env::args_os().skip(1);
    let online = match (args.next(), args.next()) {
        (None, None) => false,
        (Some(mode), None) if mode == "--demo-online" => true,
        _ => {
            eprintln!("Usage: lynx_client [--demo-online]");
            std::process::exit(2);
        }
    };
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
                    handshake: engine_net::protocol::Handshake::new(version.clone(), version),
                    last_ack_tick: engine_core::time::Tick::ZERO,
                })
                .expect("new connection generation");
        }
        app.insert_resource(client).add_plugins(OnlineWindowPlugin);
        Some(runtime)
    } else {
        app.add_plugins(OfflineWindowPlugin);
        None
    };
    app.run();
}

#[cfg(not(feature = "window"))]
fn main() {
    lynx_client::headless_app().update();
}
