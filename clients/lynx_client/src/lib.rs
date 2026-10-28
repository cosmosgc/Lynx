//! Client application setup, including a display-independent test harness.

use bevy::prelude::*;

pub mod offline;
#[cfg(feature = "window")]
pub mod windowed;

/// Shared client state used to verify that the application schedule is running.
#[derive(Debug, Default, Resource)]
pub struct ClientFrameCount(pub u64);

/// Systems shared by the windowed application and headless tests.
#[derive(Debug)]
pub struct LynxClientPlugin;

impl Plugin for LynxClientPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ClientFrameCount>()
            .add_systems(Update, count_frame)
            .add_plugins(offline::OfflineFoundationPlugin);
    }
}

fn count_frame(mut count: ResMut<ClientFrameCount>) {
    count.0 = count.0.saturating_add(1);
}

/// Construct an application that can be advanced without a display or GPU.
pub fn headless_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .add_plugins(LynxClientPlugin);
    app
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headless_app_runs_shared_systems() {
        let mut app = headless_app();
        assert_eq!(app.world().resource::<ClientFrameCount>().0, 0);
        app.update();
        app.update();
        assert_eq!(app.world().resource::<ClientFrameCount>().0, 2);
    }
}
