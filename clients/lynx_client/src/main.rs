#[cfg(feature = "window")]
fn main() {
    use bevy::prelude::*;
    use lynx_client::{LynxClientPlugin, windowed::OfflineWindowPlugin};

    App::new()
        .add_plugins(DefaultPlugins)
        .add_plugins(LynxClientPlugin)
        .add_plugins(OfflineWindowPlugin)
        .run();
}

#[cfg(not(feature = "window"))]
fn main() {
    lynx_client::headless_app().update();
}
