//! Command-line configuration for the world service.
//!
//! Every tunable is server-owned, validated before the first connection, and
//! never accepted from a client. Token hashes may be supplied on the command
//! line or through `LYNX_WORLD_TOKEN_HASHES` (comma- or whitespace-separated);
//! prefer the environment for deployment because command-line arguments can be
//! visible to local process inspection.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};
use engine_core::time::DurationMs;
use world_gateway::{ConcurrentSessionPolicy, GatewayConfig};

use crate::rate_limit::{RateLimitConfig, TokenBucketConfig};

/// Default listen address. Loopback keeps an unconfigured process off the
/// network.
pub const DEFAULT_BIND: &str = "127.0.0.1:8090";

/// Environment variable holding additional token hashes.
pub const TOKEN_HASH_ENV: &str = "LYNX_WORLD_TOKEN_HASHES";

/// Default interval between world snapshot writes, in milliseconds.
pub const DEFAULT_SNAPSHOT_MS: u64 = 10_000;

/// Default zone lease lifetime for `--state-postgres`, in milliseconds.
pub const DEFAULT_LEASE_MS: u64 = 30_000;

/// Default frames queued per connection before it is considered too slow.
pub const DEFAULT_OUTBOUND_QUEUE: usize = 64;

/// Smallest supported per-connection outbound queue. A smaller queue turns
/// ordinary scheduling jitter into disconnects.
pub const MIN_OUTBOUND_QUEUE: usize = 8;

/// Largest supported per-connection outbound queue. Each queued frame can be
/// up to the protocol frame limit, so larger queues multiply worst-case
/// per-session memory.
pub const MAX_OUTBOUND_QUEUE: usize = 256;

/// Authentication path selected for one world process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AuthMode {
    /// Hashed static tokens and demo actors. Development only.
    Demo,
    /// Nakama sessions plus PostgreSQL-owned character selection.
    Nakama,
}

/// Scope used to prevent concurrent retained sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SessionPolicyArg {
    /// Limit a character to one retained session.
    PerCharacter,
    /// Limit an account to one retained session across all characters.
    PerAccount,
}

impl From<SessionPolicyArg> for ConcurrentSessionPolicy {
    fn from(value: SessionPolicyArg) -> Self {
        match value {
            SessionPolicyArg::PerCharacter => Self::PerCharacter,
            SessionPolicyArg::PerAccount => Self::PerAccount,
        }
    }
}

/// Arguments for `lynx-world serve`.
#[derive(Clone, Args)]
#[allow(clippy::struct_excessive_bools)] // CLI flags are independent process options.
pub struct ServeArgs {
    /// Address to listen on.
    #[arg(long, default_value = DEFAULT_BIND)]
    pub bind: SocketAddr,

    /// Trusted admin listener for health and metrics; unauthenticated.
    #[arg(long, default_value = "127.0.0.1:8091")]
    pub admin_bind: SocketAddr,

    /// Authentication provider. Demo mode is loopback-only unless the
    /// dangerous development opt-in is present; Nakama mode validates each
    /// fresh bearer session and requires `X-Lynx-Character` on `/world`.
    #[arg(long, value_enum, default_value_t = AuthMode::Demo)]
    pub auth_mode: AuthMode,

    /// Allow static demo authentication on a non-loopback listener. Dangerous:
    /// this is for controlled development only, not production authentication.
    #[arg(long)]
    pub dangerous_allow_non_loopback_demo_auth: bool,

    /// Authoritative tick interval, in milliseconds.
    #[arg(long, default_value_t = 50)]
    pub tick_ms: u64,

    /// How long a disconnected session stays resumable, in milliseconds.
    #[arg(long, default_value_t = 30_000)]
    pub resume_ttl_ms: u64,

    /// Interest radius for ordinary sessions, in meters.
    #[arg(long, default_value_t = 60.0)]
    pub view_radius_m: f32,

    /// Maximum retained sessions, connected or disconnected.
    #[arg(long, default_value_t = 1_024)]
    pub max_sessions: usize,

    /// Frames queued per connection before the client is treated as too slow
    /// and detached. Must be between 8 and 256; the default tolerates a short
    /// scheduler stall without multiplying worst-case per-session memory.
    #[arg(long, default_value_t = DEFAULT_OUTBOUND_QUEUE)]
    pub outbound_queue: usize,

    /// Duplicate-session scope, including disconnected resume windows.
    #[arg(long, value_enum, default_value_t = SessionPolicyArg::PerCharacter)]
    pub session_policy: SessionPolicyArg,

    /// Authenticated total-message token-bucket burst capacity. Per-kind
    /// budgets remain bounded server defaults.
    #[arg(long, default_value_t = 100)]
    pub message_burst: u32,

    /// Authenticated total-message token-bucket refill rate per second.
    #[arg(long, default_value_t = 100)]
    pub message_rate: u32,

    /// Allowed browser Origin, exactly as serialized (repeatable). If omitted,
    /// Origin checking is disabled; requests without Origin are always allowed.
    #[arg(long = "allow-origin", value_name = "ORIGIN")]
    pub allowed_origins: Vec<String>,

    /// Grant every demo session a staff role. Development aid only.
    #[arg(long)]
    pub staff: bool,

    /// Write durable world snapshots to this JSON file and load one at
    /// startup. The file is the only persistent state; omit it to run the
    /// previous memory-only behavior.
    #[arg(long = "state-file", value_name = "PATH")]
    pub state_file: Option<PathBuf>,

    /// Store durable world snapshots in PostgreSQL and load one at startup.
    /// Reads `LYNX_PG_HOST`, `LYNX_PG_PORT`, `LYNX_PG_DATABASE`,
    /// `LYNX_PG_USER`, and `LYNX_PG_PASSWORD`, the same variables the setup
    /// scripts use. Mutually exclusive with `--state-file`.
    #[arg(long = "state-postgres")]
    pub state_postgres: bool,

    /// Ignore the existing snapshot and re-save the freshly authored demo
    /// world over it at startup. The played world is discarded. Requires
    /// `--state-file` or `--state-postgres`.
    #[arg(long)]
    pub fresh: bool,

    /// Load and validate a versioned content bundle from this directory.
    /// The directory must contain `manifest.json`.
    #[arg(long = "content-bundle", value_name = "PATH")]
    pub content_bundle: Option<PathBuf>,

    /// Serve the zones placed by this map in the configured content bundle.
    /// Multi-zone serving currently requires PostgreSQL leases/checkpoints.
    #[arg(long = "world-map", value_name = "DEFINITION_ID")]
    pub world_map: Option<String>,

    /// Reviewed persisted-instance migration plan to apply when the saved
    /// snapshot is pinned to another content manifest.
    #[arg(long = "content-migration-plan", value_name = "PATH")]
    pub content_migration_plan: Option<PathBuf>,

    /// Prepare and print the migration candidate without writing the snapshot.
    #[arg(long = "dry-run-content-migration")]
    pub dry_run_content_migration: bool,

    /// Restore the previous content/snapshot pair if no target-world state has
    /// changed since the migration checkpoint.
    #[arg(long = "rollback-content-migration")]
    pub rollback_content_migration: bool,

    /// How often a configured snapshot file is rewritten while the service
    /// runs, in milliseconds.
    #[arg(long, default_value_t = DEFAULT_SNAPSHOT_MS)]
    pub snapshot_ms: u64,

    /// Zone lease lifetime for `--state-postgres`, in milliseconds. The
    /// authority renews it every third of this value and stops serving when it
    /// cannot; a crashed holder's zone becomes available after it expires.
    #[arg(long, default_value_t = DEFAULT_LEASE_MS)]
    pub lease_ttl_ms: u64,

    /// SHA-256 hash of a bearer token. Repeat once per demo character. Prefer
    /// `LYNX_WORLD_TOKEN_HASHES` in deployments to avoid process-list exposure.
    #[arg(long = "token-hash", value_name = "SHA256")]
    pub token_hashes: Vec<String>,
}

impl fmt::Debug for ServeArgs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServeArgs")
            .field("bind", &self.bind)
            .field("admin_bind", &self.admin_bind)
            .field("auth_mode", &self.auth_mode)
            .field(
                "dangerous_allow_non_loopback_demo_auth",
                &self.dangerous_allow_non_loopback_demo_auth,
            )
            .field("tick_ms", &self.tick_ms)
            .field("resume_ttl_ms", &self.resume_ttl_ms)
            .field("view_radius_m", &self.view_radius_m)
            .field("max_sessions", &self.max_sessions)
            .field("outbound_queue", &self.outbound_queue)
            .field("session_policy", &self.session_policy)
            .field("message_burst", &self.message_burst)
            .field("message_rate", &self.message_rate)
            .field("allowed_origins", &self.allowed_origins)
            .field("staff", &self.staff)
            .field("state_file", &self.state_file)
            .field("state_postgres", &self.state_postgres)
            .field("fresh", &self.fresh)
            .field("content_bundle", &self.content_bundle)
            .field("world_map", &self.world_map)
            .field("content_migration_plan", &self.content_migration_plan)
            .field("dry_run_content_migration", &self.dry_run_content_migration)
            .field(
                "rollback_content_migration",
                &self.rollback_content_migration,
            )
            .field("snapshot_ms", &self.snapshot_ms)
            .field("lease_ttl_ms", &self.lease_ttl_ms)
            .field("token_hash_count", &self.token_hashes.len())
            .finish()
    }
}

impl ServeArgs {
    /// Reject network-exposed static demo authentication unless explicitly opted in.
    ///
    /// # Errors
    ///
    /// Returns an error for a non-loopback bind without the dangerous opt-in.
    pub fn validate_demo_auth_bind(&self) -> Result<()> {
        if self.auth_mode != AuthMode::Demo {
            if self.staff {
                bail!("--staff is available only with --auth-mode demo");
            }
            return Ok(());
        }
        if !self.bind.ip().is_loopback() && !self.dangerous_allow_non_loopback_demo_auth {
            bail!(
                "static demo authentication requires a loopback --bind; \
                 use --dangerous-allow-non-loopback-demo-auth only for controlled development"
            );
        }
        Ok(())
    }

    /// Validate the arguments and produce gateway tuning.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid tick interval, view radius, session
    /// limit, or gateway configuration.
    pub fn gateway_config(&self) -> Result<GatewayConfig> {
        if !(5..=1_000).contains(&self.tick_ms) {
            bail!("--tick-ms must be between 5 and 1000");
        }
        if self.max_sessions == 0 {
            bail!("--max-sessions must be at least 1");
        }
        if !self.view_radius_m.is_finite() || self.view_radius_m <= 0.0 {
            bail!("--view-radius-m must be a positive finite number");
        }

        let config = GatewayConfig::new(DurationMs::new(self.resume_ttl_ms))
            .with_view_radius(self.view_radius_m)
            .with_staff_view_radius(self.view_radius_m)
            .with_max_sessions(self.max_sessions)
            .with_concurrent_session_policy(self.session_policy.into());
        config.validate().context("invalid gateway configuration")?;
        Ok(config)
    }

    /// Construct bounded admission and session rate limits.
    ///
    /// The total message bucket is operator-tunable; dedicated action, chat,
    /// movement, calendar, UI, and admission budgets retain conservative,
    /// bounded defaults so one broad knob cannot disable abuse protection.
    ///
    /// # Errors
    /// Returns an error when either total-message value exceeds the hard cap.
    pub fn rate_limit_config(&self) -> Result<RateLimitConfig> {
        let total = TokenBucketConfig::new(self.message_burst, self.message_rate)
            .context("invalid message rate configuration")?;
        RateLimitConfig::default()
            .with_max_sessions(self.max_sessions)
            .context("invalid session rate-limit capacity")
            .and_then(|config| {
                config
                    .with_session_retention(Duration::from_millis(self.resume_ttl_ms))
                    .context("invalid session rate-limit retention")
            })
            .map(|config| config.with_session_total(total))
    }

    /// The configured tick interval.
    #[must_use]
    pub const fn tick_interval(&self) -> Duration {
        Duration::from_millis(self.tick_ms)
    }

    /// The snapshot write interval for the authority loop.
    ///
    /// # Errors
    ///
    /// Returns an error when the interval is zero or the tick interval is
    /// faster than the snapshot interval can divide into.
    pub fn snapshot_interval(&self) -> Result<Duration> {
        if self.snapshot_ms == 0 {
            bail!("--snapshot-ms must be positive");
        }
        if self.tick_ms > self.snapshot_ms {
            bail!("--snapshot-ms must be at least the tick interval");
        }
        Ok(Duration::from_millis(self.snapshot_ms))
    }

    /// The zone lease lifetime used with `--state-postgres`.
    ///
    /// # Errors
    ///
    /// Returns an error when the lifetime is outside one second to ten minutes
    /// or shorter than three tick intervals, which would leave no room to renew
    /// before expiry.
    pub fn lease_ttl(&self) -> Result<Duration> {
        if !(1_000..=600_000).contains(&self.lease_ttl_ms) {
            bail!("--lease-ttl-ms must be between 1000 and 600000");
        }
        if self.lease_ttl_ms < self.tick_ms.saturating_mul(3) {
            bail!("--lease-ttl-ms must be at least three tick intervals");
        }
        Ok(Duration::from_millis(self.lease_ttl_ms))
    }

    /// The validated per-connection outbound queue capacity.
    ///
    /// # Errors
    ///
    /// Returns an error outside [`MIN_OUTBOUND_QUEUE`] to
    /// [`MAX_OUTBOUND_QUEUE`].
    pub fn outbound_queue(&self) -> Result<usize> {
        if !(MIN_OUTBOUND_QUEUE..=MAX_OUTBOUND_QUEUE).contains(&self.outbound_queue) {
            bail!("--outbound-queue must be between {MIN_OUTBOUND_QUEUE} and {MAX_OUTBOUND_QUEUE}");
        }
        Ok(self.outbound_queue)
    }

    /// Combine consistency checks for the snapshot configuration.
    ///
    /// # Errors
    ///
    /// Returns an error when both stores are configured, or when `--fresh` is
    /// set without a store to restore into.
    pub fn validate_snapshot_args(&self) -> Result<()> {
        if self.world_map.is_some() && self.content_bundle.is_none() {
            bail!("--world-map requires --content-bundle");
        }
        if self.world_map.is_some() && !self.state_postgres {
            bail!(
                "--world-map currently requires --state-postgres for zone leases and checkpoints"
            );
        }
        if self.world_map.is_some()
            && (self.fresh
                || self.content_migration_plan.is_some()
                || self.dry_run_content_migration
                || self.rollback_content_migration)
        {
            bail!("--world-map cannot be combined with whole-world reset or migration options");
        }
        if self.state_file.is_some() && self.state_postgres {
            bail!("--state-file and --state-postgres are mutually exclusive");
        }
        if self.fresh && self.state_file.is_none() && !self.state_postgres {
            bail!(
                "--fresh requires --state-file or --state-postgres: there is nothing to overwrite"
            );
        }
        let has_store = self.state_file.is_some() || self.state_postgres;
        if self.content_migration_plan.is_some() && !has_store {
            bail!("--content-migration-plan requires --state-file or --state-postgres");
        }
        if self.content_migration_plan.is_some() && self.content_bundle.is_none() {
            bail!("--content-migration-plan requires --content-bundle");
        }
        if self.dry_run_content_migration && self.content_migration_plan.is_none() {
            bail!("--dry-run-content-migration requires --content-migration-plan");
        }
        if self.rollback_content_migration && !has_store {
            bail!("--rollback-content-migration requires --state-file or --state-postgres");
        }
        if self.rollback_content_migration && self.content_bundle.is_none() {
            bail!("--rollback-content-migration requires the source --content-bundle");
        }
        if self.rollback_content_migration
            && (self.fresh
                || self.content_migration_plan.is_some()
                || self.dry_run_content_migration)
        {
            bail!(
                "--rollback-content-migration cannot be combined with --fresh or a migration plan"
            );
        }
        if self.content_migration_plan.is_some() && self.fresh {
            bail!("--content-migration-plan cannot be combined with --fresh");
        }
        Ok(())
    }

    /// Token hashes from the command line plus the environment, parsed and
    /// deduplicated.
    ///
    /// # Errors
    ///
    /// Returns an error for a malformed hash.
    pub fn token_hashes(&self) -> Result<Vec<[u8; 32]>> {
        if self.auth_mode != AuthMode::Demo {
            return Ok(Vec::new());
        }
        let mut values = self.token_hashes.clone();
        if let Some(environment) = secret_environment(TOKEN_HASH_ENV)? {
            values.extend(
                environment
                    .split([',', ';'])
                    .flat_map(str::split_whitespace)
                    .map(str::to_owned),
            );
        }
        if values.is_empty() {
            bail!("configure at least one --token-hash or {TOKEN_HASH_ENV}");
        }
        let mut hashes = Vec::with_capacity(values.len());
        for (index, value) in values.iter().enumerate() {
            let hash = crate::auth::parse_hash(value)
                .with_context(|| format!("invalid token hash at position {index}"))?;
            if hashes.contains(&hash) {
                bail!("token hash {index} is configured more than once");
            }
            hashes.push(hash);
        }
        Ok(hashes)
    }
}

fn secret_environment(name: &str) -> Result<Option<String>> {
    let file_name = format!("{name}_FILE");
    match (std::env::var_os(name), std::env::var_os(&file_name)) {
        (Some(value), Some(path)) => {
            let value = value
                .into_string()
                .map_err(|_| anyhow::anyhow!("{name} is not valid UTF-8"))?;
            if !value.is_empty() {
                bail!("configure either {name} or {file_name}, not both");
            }
            let value = std::fs::read_to_string(path)
                .map_err(|_| anyhow::anyhow!("could not read {file_name}"))?;
            let value = value.trim_end_matches(['\r', '\n']).to_owned();
            if value.trim().is_empty() {
                bail!("{file_name} is empty");
            }
            Ok(Some(value))
        }
        (Some(value), None) => {
            Ok(Some(value.into_string().map_err(|_| {
                anyhow::anyhow!("{name} is not valid UTF-8")
            })?))
        }
        (None, Some(path)) => {
            let value = std::fs::read_to_string(path)
                .map_err(|_| anyhow::anyhow!("could not read {file_name}"))?;
            let value = value.trim_end_matches(['\r', '\n']).to_owned();
            if value.trim().is_empty() {
                bail!("{file_name} is empty");
            }
            Ok(Some(value))
        }
        (None, None) => Ok(None),
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ServeArgs {
        ServeArgs {
            admin_bind: "127.0.0.1:8091".parse().unwrap(),
            bind: DEFAULT_BIND.parse().unwrap(),
            auth_mode: AuthMode::Demo,
            dangerous_allow_non_loopback_demo_auth: false,
            tick_ms: 50,
            resume_ttl_ms: 30_000,
            view_radius_m: 60.0,
            max_sessions: 1_024,
            outbound_queue: DEFAULT_OUTBOUND_QUEUE,
            session_policy: SessionPolicyArg::PerCharacter,
            message_burst: 100,
            message_rate: 100,
            allowed_origins: Vec::new(),
            staff: false,
            state_file: None,
            state_postgres: false,
            fresh: false,
            content_bundle: None,
            world_map: None,
            content_migration_plan: None,
            dry_run_content_migration: false,
            rollback_content_migration: false,
            snapshot_ms: DEFAULT_SNAPSHOT_MS,
            lease_ttl_ms: DEFAULT_LEASE_MS,
            token_hashes: vec![crate::auth::hash_hex("token")],
        }
    }

    #[test]
    fn demo_auth_bind_requires_explicit_opt_in_for_non_loopback() {
        let mut args = sample();
        for address in ["127.0.0.1:8090", "127.2.3.4:8090", "[::1]:8090"] {
            args.bind = address.parse().unwrap();
            assert!(args.validate_demo_auth_bind().is_ok(), "{address}");
        }
        for address in [
            "0.0.0.0:8090",
            "192.168.1.2:8090",
            "[::]:8090",
            "[2001:db8::1]:8090",
            "[::ffff:127.0.0.1]:8090",
        ] {
            args.bind = address.parse().unwrap();
            let error = args.validate_demo_auth_bind().unwrap_err().to_string();
            assert!(error.contains("--dangerous-allow-non-loopback-demo-auth"));
            args.dangerous_allow_non_loopback_demo_auth = true;
            assert!(args.validate_demo_auth_bind().is_ok(), "{address}");
            args.dangerous_allow_non_loopback_demo_auth = false;
        }
    }

    #[test]
    fn fresh_requires_a_durable_store() {
        let mut sample = sample();
        assert!(sample.validate_snapshot_args().is_ok());

        sample.state_file = Some(std::path::PathBuf::from("world.json"));
        sample.fresh = true;
        assert!(sample.validate_snapshot_args().is_ok());

        let mut reset_without_file = sample.clone();
        reset_without_file.state_file = None;
        reset_without_file.fresh = true;
        assert!(reset_without_file.validate_snapshot_args().is_err());

        let mut reset_postgres = sample.clone();
        reset_postgres.state_file = None;
        reset_postgres.state_postgres = true;
        assert!(reset_postgres.validate_snapshot_args().is_ok());

        let mut both_stores = sample;
        both_stores.state_postgres = true;
        assert!(both_stores.validate_snapshot_args().is_err());
    }

    #[test]
    fn snapshot_interval_follows_the_tick_interval() {
        assert_eq!(
            sample().snapshot_interval().unwrap(),
            Duration::from_millis(10_000)
        );

        let mut tight = sample();
        tight.tick_ms = 1_000;
        tight.snapshot_ms = 2_000;
        assert_eq!(
            tight.snapshot_interval().unwrap(),
            Duration::from_millis(2_000)
        );

        let mut faster_than_ticks = sample();
        faster_than_ticks.snapshot_ms = 10;
        assert!(faster_than_ticks.snapshot_interval().is_err());

        let mut zero = sample();
        zero.snapshot_ms = 0;
        assert!(zero.snapshot_interval().is_err());
    }

    #[test]
    fn lease_ttl_is_bounded_and_leaves_renewal_room() {
        assert_eq!(
            sample().lease_ttl().unwrap(),
            Duration::from_millis(DEFAULT_LEASE_MS)
        );

        let mut too_short = sample();
        too_short.lease_ttl_ms = 500;
        assert!(too_short.lease_ttl().is_err());

        let mut too_long = sample();
        too_long.lease_ttl_ms = 600_001;
        assert!(too_long.lease_ttl().is_err());

        let mut crowded = sample();
        crowded.tick_ms = 1_000;
        crowded.lease_ttl_ms = 2_000;
        assert!(
            crowded.lease_ttl().is_err(),
            "two ticks of lifetime cannot hold a renewal cycle"
        );

        crowded.lease_ttl_ms = 3_000;
        assert!(crowded.lease_ttl().is_ok());
    }

    #[test]
    fn outbound_queue_capacity_is_bounded() {
        let mut args = sample();
        assert_eq!(args.outbound_queue().unwrap(), DEFAULT_OUTBOUND_QUEUE);

        for capacity in [
            MIN_OUTBOUND_QUEUE,
            DEFAULT_OUTBOUND_QUEUE,
            MAX_OUTBOUND_QUEUE,
        ] {
            args.outbound_queue = capacity;
            assert_eq!(args.outbound_queue().unwrap(), capacity);
        }

        for capacity in [0, MIN_OUTBOUND_QUEUE - 1, MAX_OUTBOUND_QUEUE + 1] {
            args.outbound_queue = capacity;
            assert!(
                args.outbound_queue().is_err(),
                "{capacity} must be rejected"
            );
        }
    }

    #[test]
    fn defaults_form_a_valid_gateway() {
        let gateway = sample().gateway_config().unwrap();
        assert_eq!(gateway.resume_ttl, DurationMs::new(30_000));
        assert_eq!(gateway.view_radius_m, 60.0);
        assert_eq!(gateway.max_sessions, 1_024);
        assert_eq!(
            gateway.concurrent_session_policy,
            ConcurrentSessionPolicy::PerCharacter
        );
        assert!(sample().rate_limit_config().is_ok());
    }

    #[test]
    fn degenerate_tuning_is_rejected() {
        let mut invalid = sample();
        invalid.tick_ms = 0;
        assert!(invalid.gateway_config().is_err());

        let mut invalid = sample();
        invalid.view_radius_m = 0.0;
        assert!(invalid.gateway_config().is_err());

        let mut invalid = sample();
        invalid.view_radius_m = f32::NAN;
        assert!(invalid.gateway_config().is_err());

        let mut invalid = sample();
        invalid.max_sessions = 0;
        assert!(invalid.gateway_config().is_err());

        let mut invalid = sample();
        invalid.message_rate = 0;
        assert!(invalid.rate_limit_config().is_err());
    }

    #[test]
    fn hashes_are_parsed_and_deduplicated() {
        let mut args = sample();
        args.token_hashes = vec![
            crate::auth::hash_hex("first"),
            crate::auth::hash_hex("second"),
        ];
        assert_eq!(args.token_hashes().unwrap().len(), 2);

        let duplicate = crate::auth::hash_hex("first");
        let mut value = sample();
        value.token_hashes = vec![duplicate.clone(), duplicate];
        assert!(value.token_hashes().is_err());

        let mut value = sample();
        value.token_hashes.clear();
        // The environment is process-wide; only assert the empty case when it
        // is not configured for this test run.
        if std::env::var(TOKEN_HASH_ENV).is_err() {
            assert!(value.token_hashes().is_err());
        }
    }

    #[test]
    fn serve_args_debug_reports_hash_count_without_exposing_hashes() {
        let sentinel = "sensitive-token-hash-sentinel";
        let mut args = sample();
        args.token_hashes = vec![sentinel.into()];

        let rendered = format!("{args:?}");
        assert!(!rendered.contains(sentinel));
        assert!(rendered.contains("token_hash_count: 1"));
    }

    #[test]
    fn nakama_mode_does_not_require_demo_hashes_or_loopback() {
        let mut args = sample();
        args.auth_mode = AuthMode::Nakama;
        args.bind = "0.0.0.0:8090".parse().unwrap();
        args.token_hashes.clear();
        args.session_policy = SessionPolicyArg::PerAccount;
        assert!(args.validate_demo_auth_bind().is_ok());
        assert_eq!(args.token_hashes().unwrap(), Vec::<[u8; 32]>::new());
        assert_eq!(
            args.gateway_config().unwrap().concurrent_session_policy,
            ConcurrentSessionPolicy::PerAccount
        );
    }
}
