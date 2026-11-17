//! Frame pacing diagnostics for the graphical client, using real wall time.

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;

use crate::online::{OnlineClient, OnlineStatus};

#[derive(Debug)]
pub struct ClientPerformancePlugin;

impl Plugin for ClientPerformancePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(FrameTimeDiagnosticsPlugin::default())
            .add_systems(Startup, setup)
            .add_systems(Update, display_frame_time);
        if let Some(seconds) = std::env::var("LYNX_CLIENT_BENCHMARK_SECONDS")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|seconds| (1..=120).contains(seconds))
        {
            app.insert_resource(FrameBenchmark {
                seconds: f64::from(seconds),
                frames: Vec::new(),
            })
            .add_systems(Last, benchmark);
        }
    }
}

#[derive(Component)]
struct FrameTimeHud;

fn setup(mut commands: Commands) {
    commands.spawn((
        FrameTimeHud,
        Text::new("FPS: warming up"),
        TextFont {
            font_size: bevy::text::FontSize::Px(14.0),
            ..default()
        },
        Node {
            position_type: PositionType::Absolute,
            bottom: px(12),
            right: px(12),
            padding: UiRect::all(px(6)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.03, 0.04, 0.06, 0.85)),
    ));
}

#[allow(clippy::needless_pass_by_value)]
fn display_frame_time(
    time: Res<Time<Real>>,
    diagnostics: Res<DiagnosticsStore>,
    mut next_refresh: Local<f64>,
    mut hud: Query<&mut Text, With<FrameTimeHud>>,
) {
    if time.elapsed_secs_f64() < *next_refresh {
        return;
    }
    *next_refresh = time.elapsed_secs_f64() + 0.5;
    let Some(ms) = diagnostics
        .get(&FrameTimeDiagnosticsPlugin::FRAME_TIME)
        .and_then(bevy::diagnostic::Diagnostic::smoothed)
        .filter(|ms| *ms > 0.0)
    else {
        return;
    };
    for mut text in &mut hud {
        text.0 = format!("{:.0} FPS · {ms:.1} ms/frame", 1_000.0 / ms);
    }
}

#[derive(Resource)]
struct FrameBenchmark {
    seconds: f64,
    frames: Vec<f64>,
}

#[allow(clippy::needless_pass_by_value)]
fn benchmark(
    time: Res<Time<Real>>,
    client: Option<Res<OnlineClient>>,
    mut sample: ResMut<FrameBenchmark>,
    mut exit: MessageWriter<AppExit>,
) {
    // Exclude startup, shader compilation, and admission from the sample.
    let elapsed = time.elapsed_secs_f64();
    if elapsed < 5.0 {
        return;
    }
    let active = client.is_none_or(|client| client.status() == OnlineStatus::Active);
    if elapsed < 5.0 + sample.seconds {
        if active && sample.frames.len() < 120_000 {
            sample.frames.push(time.delta_secs_f64());
        }
        return;
    }
    if sample.frames.is_empty() || !active {
        eprintln!("FRAME_BENCHMARK failed: no active world frames");
        exit.write(AppExit::Error(std::num::NonZeroU8::MIN));
        return;
    }
    sample.frames.sort_by(f64::total_cmp);
    let count = u32::try_from(sample.frames.len()).expect("benchmark sample count is capped");
    let total = sample.frames.iter().sum::<f64>();
    let mean = total / f64::from(count);
    let p95 = sample.frames[(sample.frames.len() - 1) * 95 / 100];
    eprintln!(
        "FRAME_BENCHMARK frames={count} mean_fps={:.2} mean_ms={:.2} p95_ms={:.2}",
        1.0 / mean,
        mean * 1_000.0,
        p95 * 1_000.0
    );
    exit.write(AppExit::Success);
}
