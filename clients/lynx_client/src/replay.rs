//! Deterministic local input fixtures for presentation iteration.
//!
//! Replay frames only provide the same controls as a local keyboard adapter.
//! They are not server messages and never make authoritative gameplay decisions.

use bevy::prelude::*;
use serde::Deserialize;

use crate::offline::{Control, OfflineSet, PressedControls};

/// A fixture consists of one pressed-control set per client update.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayFixture {
    pub frames: Vec<ReplayFrame>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayFrame {
    #[serde(default)]
    pub pressed: Vec<Control>,
}

impl ReplayFixture {
    /// Parse a replay fixture from JSON.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

/// Installed by the CLI for exactly one client update per fixture frame.
#[derive(Debug, Resource)]
pub struct ReplayInput {
    frames: Vec<ReplayFrame>,
    next_frame: usize,
}

impl ReplayInput {
    #[must_use]
    pub fn new(fixture: ReplayFixture) -> Self {
        Self {
            frames: fixture.frames,
            next_frame: 0,
        }
    }

    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }
}

/// Advances scripted input alongside the regular offline presentation systems.
#[derive(Debug)]
pub struct ReplayPlugin;

impl Plugin for ReplayPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, replay_controls.in_set(OfflineSet::Input));
    }
}

#[allow(clippy::needless_pass_by_value)] // Bevy system parameters must be passed by value.
fn replay_controls(mut replay: Option<ResMut<ReplayInput>>, mut pressed: ResMut<PressedControls>) {
    let Some(replay) = replay.as_deref_mut() else {
        return;
    };
    pressed.0 = replay
        .frames
        .get(replay.next_frame)
        .map_or_else(Default::default, |frame| {
            frame.pressed.iter().copied().collect()
        });
    replay.next_frame = replay.next_frame.saturating_add(1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        headless_app,
        offline::{OfflineInputEnabled, OfflineScene},
    };

    #[test]
    fn fixture_drives_deterministic_offline_updates() {
        let fixture =
            ReplayFixture::from_json(r#"{"frames":[{"pressed":["W"]},{"pressed":["D"]},{}]}"#)
                .unwrap();
        let mut app = headless_app();
        let count = fixture.frames.len();
        app.insert_resource(ReplayInput::new(fixture));
        for _ in 0..count {
            app.update();
        }
        assert_eq!(
            app.world().resource::<OfflineScene>().avatar_position,
            Vec3::new(1.0, 0.0, -1.0)
        );
    }

    #[test]
    fn malformed_or_unknown_input_is_rejected() {
        assert!(ReplayFixture::from_json(r#"{"frames":[{"pressed":["Unknown"]}]}"#).is_err());
        assert!(ReplayFixture::from_json(r#"{"frames":[],"server_state":{}}"#).is_err());
    }

    #[test]
    fn suppressed_replay_frames_are_consumed_not_delayed() {
        let fixture =
            ReplayFixture::from_json(r#"{"frames":[{"pressed":["W"]},{"pressed":["D"]}]}"#)
                .unwrap();
        let mut app = headless_app();
        app.insert_resource(ReplayInput::new(fixture));
        app.world_mut().resource_mut::<OfflineInputEnabled>().0 = false;
        app.update();
        app.world_mut().resource_mut::<OfflineInputEnabled>().0 = true;
        app.update();
        assert_eq!(
            app.world().resource::<OfflineScene>().avatar_position,
            Vec3::X
        );
    }
}
