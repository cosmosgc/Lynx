//! Command-line configuration for the world service.
//!
//! Every tunable is server-owned, validated before the first connection, and
//! never accepted from a client. Token hashes may be supplied on the command
//! line or through `LYNX_WORLD_TOKEN_HASHES` (comma- or whitespace-separated).

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Args;
use engine_core::time::DurationMs;
use world_gateway::GatewayConfig;

/// Default listen address. Loopback keeps an unconfigured process off the
/// network.
pub const DEFAULT_BIND: &str = "127.0.0.1:8090";

/// Environment variable holding additional token hashes.
pub const TOKEN_HASH_ENV: &str = "LYNX_WORLD_TOKEN_HASHES";

/// Arguments for `lynx-world serve`.
#[derive(Debug, Clone, Args)]
pub struct ServeArgs {
    /// Address to listen on.
    #[arg(long, default_value = DEFAULT_BIND)]
    pub bind: SocketAddr,

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

    /// Grant every demo session a staff role. Development aid only.
    #[arg(long)]
    pub staff: bool,

    /// SHA-256 hash of a bearer token. Repeat once per demo character.
    #[arg(long = "token-hash", value_name = "SHA256")]
    pub token_hashes: Vec<String>,
}

impl ServeArgs {
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

    /// The configured tick interval.
    #[must_use]
    pub const fn tick_interval(&self) -> Duration {
        Duration::from_millis(self.tick_ms)
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
            tick_ms: 50,
            resume_ttl_ms: 30_000,
            view_radius_m: 60.0,
            max_sessions: 1_024,
            staff: false,
            token_hashes: vec![crate::auth::hash_hex("token")],
        }
    }

    #[test]
    fn defaults_form_a_valid_gateway() {
        let gateway = sample().gateway_config().unwrap();
        assert_eq!(gateway.resume_ttl, DurationMs::new(30_000));
        assert_eq!(gateway.view_radius_m, 60.0);
        assert_eq!(gateway.max_sessions, 1_024);
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
