use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, ensure};
use clap::{Parser, ValueEnum};
use engine_core::math::Vec3;
use lynx_test_harness::{HeadlessClient, TestServer};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, get_current_pid};
use tokio::time::Instant;

#[derive(Debug, Parser)]
#[command(about = "Run a repeatable load scenario against an in-process Lynx world")]
struct Args {
    /// Number of authenticated clients; localhost transport admission allows at most 16.
    #[arg(long, default_value_t = 8)]
    clients: usize,

    /// Duration of the measured run, in seconds.
    #[arg(long, default_value_t = 30)]
    duration_seconds: u64,

    /// Aggregate operation dispatch rate for non-idle scenarios.
    #[arg(long, default_value_t = 100)]
    operations_per_second: u64,

    /// Workload to apply while the world runs.
    #[arg(long, value_enum, default_value_t = Scenario::Movement)]
    scenario: Scenario,

    /// Interval between memory and reply-latency window samples.
    #[arg(long, default_value_t = 5)]
    sample_interval_seconds: u64,

    /// Optional destination for the per-window memory and latency CSV.
    #[arg(long)]
    samples_csv: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Scenario {
    Idle,
    Movement,
    Chat,
    Action,
    Reconnect,
}

impl Scenario {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Movement => "movement",
            Self::Chat => "chat",
            Self::Action => "action",
            Self::Reconnect => "reconnect",
        }
    }
}

#[derive(Debug, Default)]
struct Results {
    operations_offered: u64,
    operations_accepted: u64,
    queue_rejections: u64,
    reply_latency: Vec<Duration>,
    window_latency: Vec<Duration>,
    pending: Vec<VecDeque<Instant>>,
    observed_messages: Vec<u64>,
    samples: Vec<WindowSample>,
}

#[derive(Debug, Clone, Copy)]
struct WindowSample {
    elapsed_ms: u128,
    rss_bytes: u64,
    reply_samples: usize,
    reply_p95_ns: u128,
}

#[derive(Debug)]
struct MemorySampler {
    system: System,
    pid: Pid,
}

impl MemorySampler {
    fn new() -> anyhow::Result<Self> {
        Ok(Self {
            system: System::new(),
            pid: get_current_pid().map_err(anyhow::Error::msg)?,
        })
    }

    fn rss_bytes(&mut self) -> anyhow::Result<u64> {
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[self.pid]),
            false,
            ProcessRefreshKind::nothing().with_memory(),
        );
        self.system
            .process(self.pid)
            .map(sysinfo::Process::memory)
            .context("read load harness resident memory")
    }
}

impl Results {
    fn record_reply(&mut self, latency: Duration) {
        self.reply_latency.push(latency);
        self.window_latency.push(latency);
    }

    fn record_window(&mut self, started: Instant, sampled_at: Instant, rss_bytes: u64) {
        self.samples.push(WindowSample {
            elapsed_ms: sampled_at.duration_since(started).as_millis(),
            rss_bytes,
            reply_samples: self.window_latency.len(),
            reply_p95_ns: percentile_ns(&self.window_latency, 95),
        });
        self.window_latency.clear();
    }
}

#[tokio::main]
#[expect(
    clippy::too_many_lines,
    reason = "the benchmark owns setup, workload, sampling, and one summary row"
)]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    ensure!(
        (1..=16).contains(&args.clients),
        "--clients must be between 1 and 16"
    );
    ensure!(
        (1..=3600).contains(&args.duration_seconds),
        "--duration-seconds must be between 1 and 3600"
    );
    ensure!(
        (1..=1000).contains(&args.operations_per_second),
        "--operations-per-second must be between 1 and 1000"
    );
    ensure!(
        (1..=60).contains(&args.sample_interval_seconds),
        "--sample-interval-seconds must be between 1 and 60"
    );

    let tokens = (0..args.clients)
        .map(|index| format!("lynx-load-token-{index:02}"))
        .collect::<Vec<_>>();
    let token_refs = tokens.iter().map(String::as_str).collect::<Vec<_>>();
    let server = TestServer::start(&token_refs)
        .await
        .context("start in-process load target")?;
    let handshake = lynx_world::demo::handshake().context("build demo handshake")?;
    let mut clients = tokens
        .iter()
        .map(|token| HeadlessClient::connect(server.url(), token, None, handshake.clone()))
        .collect::<Vec<_>>();
    for client in &mut clients {
        client
            .wait_ready(Duration::from_secs(10))
            .await
            .context("wait for a load client to receive its world baseline")?;
    }

    let mut results = Results {
        pending: (0..args.clients).map(|_| VecDeque::new()).collect(),
        observed_messages: clients
            .iter()
            .map(HeadlessClient::recorded_message_count)
            .collect(),
        ..Results::default()
    };
    let mut memory = MemorySampler::new()?;
    let start = Instant::now();
    let deadline = start + Duration::from_secs(args.duration_seconds);
    let sample_interval = Duration::from_secs(args.sample_interval_seconds);
    let initial_rss = memory.rss_bytes()?;
    results.record_window(start, start, initial_rss);
    let starting_metrics = server.metrics();
    let starting_tick_count = metric_value(&starting_metrics, "lynx_world_ticks_total")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_default();
    let starting_tick_duration =
        metric_value(&starting_metrics, "lynx_world_tick_duration_seconds_total")
            .and_then(|value| value.parse::<f64>().ok())
            .unwrap_or_default();
    let starting_tick_overruns = metric_value(&starting_metrics, "lynx_world_tick_overruns_total")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_default();
    let interval = Duration::from_nanos(1_000_000_000 / args.operations_per_second);
    let mut next_operation = start;
    let mut next_sample = start + sample_interval;
    let mut operation_index = 0_usize;

    while Instant::now() < deadline {
        for (index, client) in clients.iter_mut().enumerate() {
            client.poll().context("poll load client")?;
            let count = client.recorded_message_count();
            let arrived = count.saturating_sub(results.observed_messages[index]);
            results.observed_messages[index] = count;
            for _ in 0..arrived {
                if let Some(sent_at) = results.pending[index].pop_front() {
                    results.record_reply(sent_at.elapsed());
                }
            }
        }

        while !matches!(args.scenario, Scenario::Idle)
            && Instant::now() >= next_operation
            && Instant::now() < deadline
        {
            let client_index = operation_index % clients.len();
            let dispatched = match args.scenario {
                Scenario::Idle => unreachable!(),
                Scenario::Movement => {
                    let column = client_index % 6;
                    let row = client_index / 6;
                    #[allow(clippy::cast_precision_loss)]
                    let destination = Vec3::new(
                        60.05 + (column as f32 * 120.0),
                        0.0,
                        60.0 + (row as f32 * 120.0),
                    );
                    clients[client_index].move_to(destination).map(|_| ())
                }
                Scenario::Chat => {
                    clients[client_index].send_chat(&format!("load-message-{operation_index}"))
                }
                Scenario::Action => {
                    let actor = clients[client_index].actor();
                    clients[client_index].send_action(actor, "inspect", &[])
                }
                Scenario::Reconnect => {
                    let before = Instant::now();
                    let result = clients[client_index]
                        .reconnect(Duration::from_secs(10))
                        .await;
                    if result.is_ok() {
                        results.record_reply(before.elapsed());
                    }
                    result
                }
            };
            if dispatched.is_ok() {
                results.operations_accepted = results.operations_accepted.saturating_add(1);
                if !matches!(args.scenario, Scenario::Reconnect) {
                    results.pending[client_index].push_back(Instant::now());
                }
            } else {
                results.queue_rejections = results.queue_rejections.saturating_add(1);
            }
            results.operations_offered = results.operations_offered.saturating_add(1);
            operation_index = operation_index.saturating_add(1);
            next_operation += interval;
        }
        let sampled_at = Instant::now();
        if sampled_at >= next_sample && sampled_at < deadline {
            results.record_window(start, sampled_at, memory.rss_bytes()?);
            next_sample = sampled_at + sample_interval;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    for (index, client) in clients.iter_mut().enumerate() {
        client.poll().context("drain load client")?;
        let count = client.recorded_message_count();
        let arrived = count.saturating_sub(results.observed_messages[index]);
        results.observed_messages[index] = count;
        for _ in 0..arrived {
            if let Some(sent_at) = results.pending[index].pop_front() {
                results.record_reply(sent_at.elapsed());
            }
        }
    }

    let elapsed = start.elapsed();
    results.record_window(start, start + elapsed, memory.rss_bytes()?);
    let (outbound_messages, inbound_messages, outbound_bytes, inbound_bytes) = clients
        .iter()
        .map(HeadlessClient::io_totals)
        .fold((0_u64, 0_u64, 0_u64, 0_u64), |total, item| {
            (
                total.0.saturating_add(item.0),
                total.1.saturating_add(item.1),
                total.2.saturating_add(item.2),
                total.3.saturating_add(item.3),
            )
        });
    let metrics = server.metrics();
    let tick_count_value = metric_value(&metrics, "lynx_world_ticks_total")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_default()
        .saturating_sub(starting_tick_count);
    let tick_failures = counter_delta(
        &starting_metrics,
        &metrics,
        "lynx_world_tick_failures_total",
        None,
    );
    let tick_duration = metric_value(&metrics, "lynx_world_tick_duration_seconds_total")
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or_default()
        - starting_tick_duration;
    let tick_overruns = metric_value(&metrics, "lynx_world_tick_overruns_total")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_default()
        .saturating_sub(starting_tick_overruns);
    let tick_mean_ms = if tick_count_value == 0 {
        0.0
    } else {
        tick_duration.max(0.0) * 1_000.0 / tick_count_value as f64
    };
    let queue_full_events = counter_delta(
        &starting_metrics,
        &metrics,
        "lynx_world_outbound_queue_full_total",
        None,
    );
    let replication_enqueued_bytes = counter_delta(
        &starting_metrics,
        &metrics,
        "lynx_world_replication_enqueued_bytes_total",
        None,
    );
    let snapshot_write_successes = counter_delta(
        &starting_metrics,
        &metrics,
        "lynx_world_replication_socket_write_frames_total",
        Some("kind=\"snapshot\",outcome=\"success\""),
    );
    let snapshot_write_failures = counter_delta(
        &starting_metrics,
        &metrics,
        "lynx_world_replication_socket_write_frames_total",
        Some("kind=\"snapshot\",outcome=\"failure\""),
    );
    let delta_write_successes = counter_delta(
        &starting_metrics,
        &metrics,
        "lynx_world_replication_socket_write_frames_total",
        Some("kind=\"delta\",outcome=\"success\""),
    );
    let delta_write_failures = counter_delta(
        &starting_metrics,
        &metrics,
        "lynx_world_replication_socket_write_frames_total",
        Some("kind=\"delta\",outcome=\"failure\""),
    );
    let snapshot_written_bytes = counter_delta(
        &starting_metrics,
        &metrics,
        "lynx_world_replication_socket_written_bytes_total",
        Some("kind=\"snapshot\""),
    );
    let delta_written_bytes = counter_delta(
        &starting_metrics,
        &metrics,
        "lynx_world_replication_socket_written_bytes_total",
        Some("kind=\"delta\""),
    );
    let queue_depth_high_water =
        metric_value(&metrics, "lynx_world_outbound_queue_depth_high_water")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or_default();
    let session_queue_depth_high_water = metric_value(
        &metrics,
        "lynx_world_outbound_queue_session_depth_high_water",
    )
    .and_then(|value| value.parse::<u64>().ok())
    .unwrap_or_default();
    let queue_capacity_total = metric_value(&metrics, "lynx_world_outbound_queue_capacity")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_default();
    let connection_count = metric_value(&metrics, "lynx_world_connections")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_default();
    let session_queue_capacity = if connection_count == 0 {
        0
    } else {
        queue_capacity_total / connection_count
    };
    let reply_samples = results.reply_latency.len();
    let p50 = percentile_ns(&results.reply_latency, 50);
    let p95 = percentile_ns(&results.reply_latency, 95);
    let p99 = percentile_ns(&results.reply_latency, 99);
    let first_reply_window = results
        .samples
        .iter()
        .find(|sample| sample.reply_samples > 0);
    let last_reply_window = results
        .samples
        .iter()
        .rev()
        .find(|sample| sample.reply_samples > 0);
    let rss_start = results.samples.first().map_or(0, |sample| sample.rss_bytes);
    let rss_end = results.samples.last().map_or(0, |sample| sample.rss_bytes);
    let rss_peak = results
        .samples
        .iter()
        .map(|sample| sample.rss_bytes)
        .max()
        .unwrap_or_default();
    let rss_growth = i128::from(rss_end) - i128::from(rss_start);

    if let Some(path) = &args.samples_csv {
        write_samples_csv(path, &results.samples)?;
    }

    println!(
        "scenario,clients,duration_s,target_ops_per_s,offered_ops,accepted_ops,queue_rejections,pending_replies,reply_samples,achieved_ops_per_s,outbound_envelopes,inbound_envelopes,outbound_bytes,inbound_bytes,reply_p50_ns,reply_p95_ns,reply_p99_ns,ticks,tick_failures,tick_duration_s,tick_mean_ms,tick_overruns,server_queue_full_events,queue_depth_high_water_frames,session_queue_depth_high_water_frames,queue_capacity_total_frames,session_queue_capacity_frames,replication_enqueued_bytes,snapshot_socket_write_successes,snapshot_socket_write_failures,delta_socket_write_successes,delta_socket_write_failures,snapshot_socket_written_bytes,delta_socket_written_bytes,rss_start_bytes,rss_peak_bytes,rss_end_bytes,rss_growth_bytes,first_window_reply_samples,first_window_p95_ns,last_window_reply_samples,last_window_p95_ns"
    );
    let row = [
        args.scenario.as_str().to_owned(),
        args.clients.to_string(),
        format!("{:.3}", elapsed.as_secs_f64()),
        args.operations_per_second.to_string(),
        results.operations_offered.to_string(),
        results.operations_accepted.to_string(),
        results.queue_rejections.to_string(),
        results
            .pending
            .iter()
            .map(VecDeque::len)
            .sum::<usize>()
            .to_string(),
        reply_samples.to_string(),
        format!(
            "{:.2}",
            operations_per_second(results.operations_offered, elapsed)
        ),
        outbound_messages.to_string(),
        inbound_messages.to_string(),
        outbound_bytes.to_string(),
        inbound_bytes.to_string(),
        p50.to_string(),
        p95.to_string(),
        p99.to_string(),
        tick_count_value.to_string(),
        tick_failures.to_string(),
        format!("{:.9}", tick_duration.max(0.0)),
        format!("{tick_mean_ms:.4}"),
        tick_overruns.to_string(),
        queue_full_events.to_string(),
        queue_depth_high_water.to_string(),
        session_queue_depth_high_water.to_string(),
        queue_capacity_total.to_string(),
        session_queue_capacity.to_string(),
        replication_enqueued_bytes.to_string(),
        snapshot_write_successes.to_string(),
        snapshot_write_failures.to_string(),
        delta_write_successes.to_string(),
        delta_write_failures.to_string(),
        snapshot_written_bytes.to_string(),
        delta_written_bytes.to_string(),
        rss_start.to_string(),
        rss_peak.to_string(),
        rss_end.to_string(),
        rss_growth.to_string(),
        first_reply_window
            .map_or(0, |sample| sample.reply_samples)
            .to_string(),
        first_reply_window
            .map_or(0, |sample| sample.reply_p95_ns)
            .to_string(),
        last_reply_window
            .map_or(0, |sample| sample.reply_samples)
            .to_string(),
        last_reply_window
            .map_or(0, |sample| sample.reply_p95_ns)
            .to_string(),
    ];
    println!("{}", row.join(","));

    server
        .shutdown()
        .await
        .context("stop in-process load target")?;
    Ok(())
}

fn write_samples_csv(path: &Path, samples: &[WindowSample]) -> anyhow::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).context("create sample report directory")?;
    }
    let mut csv = String::from("elapsed_ms,rss_bytes,reply_samples,reply_p95_ns\n");
    for sample in samples {
        use std::fmt::Write as _;
        writeln!(
            csv,
            "{},{},{},{}",
            sample.elapsed_ms, sample.rss_bytes, sample.reply_samples, sample.reply_p95_ns
        )?;
    }
    std::fs::write(path, csv).with_context(|| format!("write sample report {}", path.display()))
}

#[allow(
    clippy::cast_precision_loss,
    reason = "the CLI bounds samples below the exact integer range of f64"
)]
fn operations_per_second(operations: u64, elapsed: Duration) -> f64 {
    operations as f64 / elapsed.as_secs_f64()
}

fn percentile_ns(samples: &[Duration], percentile: u8) -> u128 {
    if samples.is_empty() {
        return 0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let index = (sorted.len() - 1)
        .saturating_mul(usize::from(percentile))
        .div_ceil(100);
    sorted[index].as_nanos()
}

fn metric_value<'a>(metrics: &'a str, name: &str) -> Option<&'a str> {
    metrics.lines().find_map(|line| {
        line.strip_prefix(name)
            .filter(|rest| rest.starts_with(' '))
            .map(str::trim)
    })
}

fn metric_value_with_labels<'a>(metrics: &'a str, name: &str, labels: &str) -> Option<&'a str> {
    let prefix = format!("{name}{{{labels}}}");
    metrics.lines().find_map(|line| {
        line.strip_prefix(&prefix)
            .filter(|rest| rest.starts_with(' '))
            .map(str::trim)
    })
}

fn counter_delta(start: &str, end: &str, name: &str, labels: Option<&str>) -> u64 {
    let value = |metrics: &str| {
        let sample = labels.map_or_else(
            || metric_value(metrics, name),
            |labels| metric_value_with_labels(metrics, name, labels),
        );
        sample
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or_default()
    };
    value(end).saturating_sub(value(start))
}
