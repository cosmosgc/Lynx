//! Command-line configuration for the world service.
//!
//! Every tunable is server-owned, validated before the first connection, and
//! never accepted from a client. Token hashes may be supplied on the command
//! line or through `LYNX_WORLD_TOKEN_HASHES` (comma- or whitespace-separated).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Args;
use engine_core::time::DurationMs;
use world_gateway::GatewayConfig;

use crate::rate_limit::{RateLimitConfig, TokenBucketConfig};

/// Default listen address. Loopback keeps an unconfigured process off the
/// network.
pub const DEFAULT_BIND: &str = "127.0.0.1:8090";

/// Environment variable holding additional token hashes.
pub const TOKEN_HASH_ENV: &str = "LYNX_WORLD_TOKEN_HASHES";

/// Default interval between world snapshot writes, in milliseconds.
pub const DEFAULT_SNAPSHOT_MS: u64 = 10_000;

/// Arguments for `lynx-world serve`.
#[derive(Debug, Clone, Args)]
#[allow(clippy::struct_excessive_bools)] // CLI flags are independent process options.
pub struct ServeArgs {
    /// Address to listen on.
    #[arg(long, default_value = DEFAULT_BIND)]
    pub bind: SocketAddr,

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

    /// How often a configured snapshot file is rewritten while the service
    /// runs, in milliseconds.
    #[arg(long, default_value_t = DEFAULT_SNAPSHOT_MS)]
    pub snapshot_ms: u64,

    /// SHA-256 hash of a bearer token. Repeat once per demo character.
    #[arg(long = "token-hash", value_name = "SHA256")]
    pub token_hashes: Vec<String>,
}

impl ServeArgs {
    /// Reject network-exposed static demo authentication unless explicitly opted in.
    ///
    /// # Errors
    ///
    /// Returns an error for a non-loopback bind without the dangerous opt-in.
    pub fn validate_demo_auth_bind(&self) -> Result<()> {
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
            .with_max_sessions(self.max_sessions);
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

    /// Combine consistency checks for the snapshot configuration.
    ///
    /// # Errors
    ///
    /// Returns an error when both stores are configured, or when `--fresh` is
    /// set without a store to restore into.
    pub fn validate_snapshot_args(&self) -> Result<()> {
        if self.state_file.is_some() && self.state_postgres {
            bail!("--state-file and --state-postgres are mutually exclusive");
        }
        if self.fresh && self.state_file.is_none() && !self.state_postgres {
            bail!(
                "--fresh requires --state-file or --state-postgres: there is nothing to overwrite"
            );
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
        let mut values = self.token_hashes.clone();
        if let Ok(environment) = std::env::var(TOKEN_HASH_ENV) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ServeArgs {
        ServeArgs {
            bind: DEFAULT_BIND.parse().unwrap(),
            dangerous_allow_non_loopback_demo_auth: false,
            tick_ms: 50,
            resume_ttl_ms: 30_000,
            view_radius_m: 60.0,
            max_sessions: 1_024,
            message_burst: 100,
            message_rate: 100,
            allowed_origins: Vec::new(),
            staff: false,
            state_file: None,
            state_postgres: false,
            fresh: false,
            snapshot_ms: DEFAULT_SNAPSHOT_MS,
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
    fn defaults_form_a_valid_gateway() {
        let gateway = sample().gateway_config().unwrap();
        assert_eq!(gateway.resume_ttl, DurationMs::new(30_000));
        assert_eq!(gateway.view_radius_m, 60.0);
        assert_eq!(gateway.max_sessions, 1_024);
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
}
