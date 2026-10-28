//! Bounded admission and authenticated-session token buckets.
//!
//! The transport gives this module only the peer IP address before admission and
//! the server-issued session ID afterwards. It never receives client claims as
//! a limiter key, and all maps have explicit upper bounds and idle eviction.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use engine_core::error::{EngineError, ErrorCode};
use engine_net::envelope::Envelope;
use engine_net::replication::SessionId;
use serde::Serialize;

const NANOS_PER_SECOND: u128 = 1_000_000_000;
const MAX_BUCKET_VALUE: u32 = 10_000;
const MAX_TRACKED_IDENTITIES: usize = 100_000;
const MAX_IDLE_TTL: Duration = Duration::from_secs(3_600);

/// A bounded token bucket's capacity and refill rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenBucketConfig {
    capacity: u32,
    refill_per_second: u32,
}

impl TokenBucketConfig {
    /// Construct a bucket that accepts `capacity` immediate messages and
    /// replenishes `refill_per_second` messages each second.
    ///
    /// # Errors
    /// Returns an error if either rate is zero or exceeds the safe hard cap.
    pub fn new(capacity: u32, refill_per_second: u32) -> anyhow::Result<Self> {
        anyhow::ensure!(
            (1..=MAX_BUCKET_VALUE).contains(&capacity),
            "token bucket capacity must be between 1 and {MAX_BUCKET_VALUE}"
        );
        anyhow::ensure!(
            (1..=MAX_BUCKET_VALUE).contains(&refill_per_second),
            "token bucket refill rate must be between 1 and {MAX_BUCKET_VALUE} per second"
        );
        Ok(Self {
            capacity,
            refill_per_second,
        })
    }

    /// The immediate burst capacity.
    #[must_use]
    pub const fn capacity(self) -> u32 {
        self.capacity
    }

    /// The steady-state refill rate, in messages per second.
    #[must_use]
    pub const fn refill_per_second(self) -> u32 {
        self.refill_per_second
    }
}

/// Bounded configuration for pre-authentication per-IP admission controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionLimitConfig {
    handshake: TokenBucketConfig,
    failed_auth: TokenBucketConfig,
    connection_churn: TokenBucketConfig,
    max_ips: usize,
    idle_ttl: Duration,
}

impl AdmissionLimitConfig {
    /// Build admission controls.
    ///
    /// # Errors
    /// Returns an error for an unbounded/zero identity map or an invalid idle
    /// eviction interval.
    pub fn new(
        handshake: TokenBucketConfig,
        failed_auth: TokenBucketConfig,
        connection_churn: TokenBucketConfig,
        max_ips: usize,
        idle_ttl: Duration,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            (1..=MAX_TRACKED_IDENTITIES).contains(&max_ips),
            "tracked IP limit must be between 1 and {MAX_TRACKED_IDENTITIES}"
        );
        anyhow::ensure!(
            !idle_ttl.is_zero() && idle_ttl <= MAX_IDLE_TTL,
            "IP idle eviction must be between 1 ns and 1 hour"
        );
        Ok(Self {
            handshake,
            failed_auth,
            connection_churn,
            max_ips,
            idle_ttl,
        })
    }
}

impl Default for AdmissionLimitConfig {
    fn default() -> Self {
        Self {
            handshake: TokenBucketConfig {
                capacity: 20,
                refill_per_second: 10,
            },
            failed_auth: TokenBucketConfig {
                capacity: 5,
                refill_per_second: 1,
            },
            connection_churn: TokenBucketConfig {
                capacity: 10,
                refill_per_second: 2,
            },
            max_ips: 16_384,
            idle_ttl: Duration::from_secs(300),
        }
    }
}

/// Per-session total and message-kind token buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionLimitConfig {
    total: TokenBucketConfig,
    action: TokenBucketConfig,
    attack: TokenBucketConfig,
    chat: TokenBucketConfig,
    movement: TokenBucketConfig,
    calendar: TokenBucketConfig,
    ui: TokenBucketConfig,
    max_sessions: usize,
    idle_ttl: Duration,
}

impl SessionLimitConfig {
    /// Build session controls. Every kind has a separate bounded bucket in
    /// addition to the total budget; attack is a specific action-type bucket.
    ///
    /// # Errors
    /// Returns an error for an unbounded/zero session map or invalid eviction
    /// interval.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        total: TokenBucketConfig,
        action: TokenBucketConfig,
        attack: TokenBucketConfig,
        chat: TokenBucketConfig,
        movement: TokenBucketConfig,
        calendar: TokenBucketConfig,
        ui: TokenBucketConfig,
        max_sessions: usize,
        idle_ttl: Duration,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            (1..=MAX_TRACKED_IDENTITIES).contains(&max_sessions),
            "tracked session limit must be between 1 and {MAX_TRACKED_IDENTITIES}"
        );
        anyhow::ensure!(
            !idle_ttl.is_zero() && idle_ttl <= MAX_IDLE_TTL,
            "session idle eviction must be between 1 ns and 1 hour"
        );
        Ok(Self {
            total,
            action,
            attack,
            chat,
            movement,
            calendar,
            ui,
            max_sessions,
            idle_ttl,
        })
    }

    /// Match the rate limiter's identity ceiling to the authoritative retained
    /// session cap. This prevents stale limiter state from becoming unbounded.
    pub fn with_max_sessions(mut self, max_sessions: usize) -> anyhow::Result<Self> {
        anyhow::ensure!(
            (1..=MAX_TRACKED_IDENTITIES).contains(&max_sessions),
            "tracked session limit must be between 1 and {MAX_TRACKED_IDENTITIES}"
        );
        self.max_sessions = max_sessions;
        Ok(self)
    }

    fn with_idle_ttl(mut self, idle_ttl: Duration) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !idle_ttl.is_zero() && idle_ttl <= MAX_IDLE_TTL,
            "session idle eviction must be between 1 ns and 1 hour"
        );
        self.idle_ttl = idle_ttl;
        Ok(self)
    }
}

impl Default for SessionLimitConfig {
    fn default() -> Self {
        Self {
            total: TokenBucketConfig {
                capacity: 100,
                refill_per_second: 100,
            },
            action: TokenBucketConfig {
                capacity: 20,
                refill_per_second: 20,
            },
            attack: TokenBucketConfig {
                capacity: 8,
                refill_per_second: 8,
            },
            chat: TokenBucketConfig {
                capacity: 10,
                refill_per_second: 5,
            },
            movement: TokenBucketConfig {
                capacity: 30,
                refill_per_second: 30,
            },
            calendar: TokenBucketConfig {
                capacity: 10,
                refill_per_second: 5,
            },
            ui: TokenBucketConfig {
                capacity: 20,
                refill_per_second: 10,
            },
            max_sessions: 1_024,
            idle_ttl: Duration::from_secs(120),
        }
    }
}

/// Complete bounded transport rate-limit configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RateLimitConfig {
    admission: AdmissionLimitConfig,
    session: SessionLimitConfig,
}

impl RateLimitConfig {
    /// Build complete admission and session limits.
    #[must_use]
    pub const fn new(admission: AdmissionLimitConfig, session: SessionLimitConfig) -> Self {
        Self { admission, session }
    }

    /// Match retained limiter state to the configured gateway session maximum.
    pub fn with_max_sessions(mut self, max_sessions: usize) -> anyhow::Result<Self> {
        self.session = self.session.with_max_sessions(max_sessions)?;
        Ok(self)
    }

    /// Retain a disconnected session's budget for no longer than its server
    /// resume window, so reconnecting cannot reset a burst and expired sessions
    /// do not occupy the bounded map.
    pub fn with_session_retention(mut self, retention: Duration) -> anyhow::Result<Self> {
        self.session = self.session.with_idle_ttl(retention)?;
        Ok(self)
    }

    /// Override only the authenticated total-message bucket while retaining
    /// bounded per-kind defaults. Useful for controlled tests and small shards.
    #[must_use]
    pub const fn with_session_total(mut self, total: TokenBucketConfig) -> Self {
        self.session.total = total;
        self
    }
}

/// The live bounded limiters constructed from one configuration.
#[derive(Debug)]
pub struct RateLimits {
    admission: AdmissionLimiter,
    sessions: SessionLimiter,
}

impl RateLimits {
    /// Create live admission and session limiters.
    #[must_use]
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            admission: AdmissionLimiter::new(config.admission),
            sessions: SessionLimiter::new(config.session),
        }
    }

    /// The pre-authentication peer-IP limiter.
    #[must_use]
    pub const fn admission(&self) -> &AdmissionLimiter {
        &self.admission
    }

    /// The authenticated server-session limiter.
    #[must_use]
    pub const fn sessions(&self) -> &SessionLimiter {
        &self.sessions
    }

    /// All rejection counters in stable category order.
    #[must_use]
    pub fn metrics(&self) -> Vec<RejectionMetric> {
        let mut metrics = self.admission.metrics();
        metrics.extend(self.sessions.metrics());
        metrics.sort_by_key(|metric| (metric.kind, metric.code));
        metrics
    }
}

/// A bounded request family used for accounting and metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum RequestClass {
    Other,
    Action,
    Attack,
    Chat,
    Movement,
    Calendar,
    Ui,
    Handshake,
    FailedAuth,
    ConnectionChurn,
    AdmissionCapacity,
}

impl RequestClass {
    const fn name(self) -> &'static str {
        match self {
            Self::Other => "other",
            Self::Action => "action",
            Self::Attack => "action.attack",
            Self::Chat => "chat",
            Self::Movement => "movement",
            Self::Calendar => "calendar",
            Self::Ui => "ui",
            Self::Handshake => "handshake",
            Self::FailedAuth => "failed_auth",
            Self::ConnectionChurn => "connection_churn",
            Self::AdmissionCapacity => "admission_capacity",
        }
    }
}

/// One rejection-counter row for alerting or an HTTP metrics collector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RejectionMetric {
    /// Normalized, bounded message or admission kind.
    pub kind: &'static str,
    /// Stable error code.
    pub code: &'static str,
    /// Count since process start.
    pub count: u64,
}

/// Thread-safe, bounded rate-limit counters.
#[derive(Debug, Default)]
pub struct RateLimitMetrics {
    counters: Mutex<BTreeMap<(RequestClass, &'static str), u64>>,
}

impl RateLimitMetrics {
    fn reject(&self, class: RequestClass, code: ErrorCode) {
        let mut counters = self
            .counters
            .lock()
            .expect("rate limit metrics lock poisoned");
        let count = counters.entry((class, code.as_str())).or_default();
        *count = count.saturating_add(1);
    }

    /// Return rate-limit rejection counts in stable order.
    #[must_use]
    pub fn rejections(&self) -> Vec<RejectionMetric> {
        self.counters
            .lock()
            .expect("rate limit metrics lock poisoned")
            .iter()
            .map(|((class, code), count)| RejectionMetric {
                kind: class.name(),
                code,
                count: *count,
            })
            .collect()
    }
}

#[derive(Debug)]
struct TokenBucket {
    config: TokenBucketConfig,
    credits: u128,
    updated: Instant,
}

impl TokenBucket {
    fn new(config: TokenBucketConfig, now: Instant) -> Self {
        Self {
            config,
            credits: u128::from(config.capacity) * NANOS_PER_SECOND,
            updated: now,
        }
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.updated);
        self.updated = now;
        let gained = elapsed.as_nanos() * u128::from(self.config.refill_per_second);
        let maximum = u128::from(self.config.capacity) * NANOS_PER_SECOND;
        self.credits = self.credits.saturating_add(gained).min(maximum);
    }

    fn retry_after(&self) -> Duration {
        let missing = NANOS_PER_SECOND.saturating_sub(self.credits);
        let rate = u128::from(self.config.refill_per_second);
        let nanos = missing.saturating_add(rate - 1) / rate;
        Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }

    fn can_consume(&mut self, now: Instant) -> Result<(), Duration> {
        self.refill(now);
        (self.credits >= NANOS_PER_SECOND)
            .then_some(())
            .ok_or_else(|| self.retry_after())
    }

    fn consume(&mut self) {
        self.credits = self.credits.saturating_sub(NANOS_PER_SECOND);
    }
}

#[derive(Debug)]
struct IpBudget {
    handshake: TokenBucket,
    failed_auth: TokenBucket,
    churn: TokenBucket,
    last_seen: Instant,
}

impl IpBudget {
    fn new(config: AdmissionLimitConfig, now: Instant) -> Self {
        Self {
            handshake: TokenBucket::new(config.handshake, now),
            failed_auth: TokenBucket::new(config.failed_auth, now),
            churn: TokenBucket::new(config.connection_churn, now),
            last_seen: now,
        }
    }
}

/// Per-IP controls used before a session has been authenticated.
#[derive(Debug)]
pub struct AdmissionLimiter {
    config: AdmissionLimitConfig,
    entries: Mutex<BTreeMap<IpAddr, IpBudget>>,
    metrics: RateLimitMetrics,
}

impl AdmissionLimiter {
    /// Create the bounded admission limiter.
    #[must_use]
    pub fn new(config: AdmissionLimitConfig) -> Self {
        Self {
            config,
            entries: Mutex::new(BTreeMap::new()),
            metrics: RateLimitMetrics::default(),
        }
    }

    /// Count and check an HTTP/WebSocket handshake attempt from one peer IP.
    pub fn handshake(&self, ip: IpAddr, now: Instant) -> Result<(), EngineError> {
        self.check(ip, now, RequestClass::Handshake)
    }

    /// Count a rejected credential without exposing it to metrics or logs.
    pub fn failed_auth(&self, ip: IpAddr, now: Instant) -> Result<(), EngineError> {
        self.check(ip, now, RequestClass::FailedAuth)
    }

    /// Count and check a valid upgrade attempt, limiting rapid reconnect churn.
    pub fn connection_churn(&self, ip: IpAddr, now: Instant) -> Result<(), EngineError> {
        self.check(ip, now, RequestClass::ConnectionChurn)
    }

    /// Rejection counters grouped by bounded kind and stable code.
    #[must_use]
    pub fn metrics(&self) -> Vec<RejectionMetric> {
        self.metrics.rejections()
    }

    fn check(&self, ip: IpAddr, now: Instant, class: RequestClass) -> Result<(), EngineError> {
        let mut entries = self
            .entries
            .lock()
            .expect("admission limiter lock poisoned");
        entries.retain(|_, entry| {
            now.saturating_duration_since(entry.last_seen) <= self.config.idle_ttl
        });
        if !entries.contains_key(&ip) && entries.len() >= self.config.max_ips {
            self.metrics
                .reject(RequestClass::AdmissionCapacity, ErrorCode::CapacityExceeded);
            return Err(EngineError::new(
                ErrorCode::CapacityExceeded,
                "admission identity capacity exceeded",
            ));
        }
        let entry = entries
            .entry(ip)
            .or_insert_with(|| IpBudget::new(self.config, now));
        entry.last_seen = now;
        let bucket = match class {
            RequestClass::Handshake => &mut entry.handshake,
            RequestClass::FailedAuth => &mut entry.failed_auth,
            RequestClass::ConnectionChurn => &mut entry.churn,
            _ => unreachable!("admission limiter only receives admission classes"),
        };
        if let Err(retry) = bucket.can_consume(now) {
            self.metrics.reject(class, ErrorCode::RateLimited);
            return Err(rate_limited(class, retry));
        }
        bucket.consume();
        Ok(())
    }
}

#[derive(Debug)]
struct SessionBudget {
    total: TokenBucket,
    action: TokenBucket,
    attack: TokenBucket,
    chat: TokenBucket,
    movement: TokenBucket,
    calendar: TokenBucket,
    ui: TokenBucket,
    last_seen: Instant,
}

impl SessionBudget {
    fn new(config: SessionLimitConfig, now: Instant) -> Self {
        Self {
            total: TokenBucket::new(config.total, now),
            action: TokenBucket::new(config.action, now),
            attack: TokenBucket::new(config.attack, now),
            chat: TokenBucket::new(config.chat, now),
            movement: TokenBucket::new(config.movement, now),
            calendar: TokenBucket::new(config.calendar, now),
            ui: TokenBucket::new(config.ui, now),
            last_seen: now,
        }
    }

    fn kind_bucket(&mut self, class: RequestClass) -> Option<&mut TokenBucket> {
        match class {
            RequestClass::Action => Some(&mut self.action),
            RequestClass::Attack => Some(&mut self.attack),
            RequestClass::Chat => Some(&mut self.chat),
            RequestClass::Movement => Some(&mut self.movement),
            RequestClass::Calendar => Some(&mut self.calendar),
            RequestClass::Ui => Some(&mut self.ui),
            _ => None,
        }
    }
}

/// Per-server session budgets keyed only by server-issued session IDs.
#[derive(Debug)]
pub struct SessionLimiter {
    config: SessionLimitConfig,
    entries: Mutex<BTreeMap<SessionId, SessionBudget>>,
    metrics: RateLimitMetrics,
}

impl SessionLimiter {
    /// Create the bounded session limiter.
    #[must_use]
    pub fn new(config: SessionLimitConfig) -> Self {
        Self {
            config,
            entries: Mutex::new(BTreeMap::new()),
            metrics: RateLimitMetrics::default(),
        }
    }

    /// Register an authenticated server-issued session. Resumes preserve the
    /// existing budget rather than allowing reconnects to reset a burst.
    pub fn register(&self, session: &SessionId, now: Instant) -> Result<(), EngineError> {
        let mut entries = self.entries.lock().expect("session limiter lock poisoned");
        entries.retain(|_, entry| {
            now.saturating_duration_since(entry.last_seen) <= self.config.idle_ttl
        });
        if let Some(entry) = entries.get_mut(session) {
            entry.last_seen = now;
            return Ok(());
        }
        if entries.len() >= self.config.max_sessions {
            self.metrics
                .reject(RequestClass::AdmissionCapacity, ErrorCode::CapacityExceeded);
            return Err(EngineError::new(
                ErrorCode::CapacityExceeded,
                "session rate-limit capacity exceeded",
            ));
        }
        entries.insert(session.clone(), SessionBudget::new(self.config, now));
        Ok(())
    }

    /// Check an authenticated envelope against total, family, and action-type
    /// buckets. On rejection nothing is consumed from any bucket.
    pub fn check(
        &self,
        session: &SessionId,
        envelope: &Envelope,
        now: Instant,
    ) -> Result<(), EngineError> {
        let class = classify(envelope);
        let mut entries = self.entries.lock().expect("session limiter lock poisoned");
        let Some(entry) = entries.get_mut(session) else {
            return Err(EngineError::new(
                ErrorCode::SessionExpired,
                "session rate limit state is unavailable",
            ));
        };
        entry.last_seen = now;
        let total_retry = entry.total.can_consume(now).err();
        let action_retry = matches!(class, RequestClass::Attack)
            .then(|| entry.action.can_consume(now).err())
            .flatten();
        let kind_retry = entry
            .kind_bucket(class)
            .and_then(|bucket| bucket.can_consume(now).err());
        if let Some(retry) = total_retry
            .into_iter()
            .chain(action_retry)
            .chain(kind_retry)
            .max()
        {
            self.metrics.reject(class, ErrorCode::RateLimited);
            return Err(rate_limited(class, retry));
        }
        entry.total.consume();
        if matches!(class, RequestClass::Attack) {
            entry.action.consume();
        }
        if let Some(bucket) = entry.kind_bucket(class) {
            bucket.consume();
        }
        Ok(())
    }

    /// Consume only the total budget for a malformed envelope that cannot be
    /// classified safely. This keeps malformed-frame floods bounded without
    /// creating an attacker-controlled metrics label.
    pub fn check_unknown(&self, session: &SessionId, now: Instant) -> Result<(), EngineError> {
        self.check_class(session, RequestClass::Other, now)
    }

    /// Rejection counters grouped by bounded kind and stable code.
    #[must_use]
    pub fn metrics(&self) -> Vec<RejectionMetric> {
        self.metrics.rejections()
    }

    fn check_class(
        &self,
        session: &SessionId,
        class: RequestClass,
        now: Instant,
    ) -> Result<(), EngineError> {
        let mut entries = self.entries.lock().expect("session limiter lock poisoned");
        let Some(entry) = entries.get_mut(session) else {
            return Err(EngineError::new(
                ErrorCode::SessionExpired,
                "session rate limit state is unavailable",
            ));
        };
        entry.last_seen = now;
        if let Err(retry) = entry.total.can_consume(now) {
            self.metrics.reject(class, ErrorCode::RateLimited);
            return Err(rate_limited(class, retry));
        }
        entry.total.consume();
        Ok(())
    }
}

fn classify(envelope: &Envelope) -> RequestClass {
    match envelope.kind.as_str() {
        "action.request"
            if envelope.body.get("verb").and_then(|value| value.as_str()) == Some("attack") =>
        {
            RequestClass::Attack
        }
        "action.request" => RequestClass::Action,
        "movement.request" => RequestClass::Movement,
        "calendar.command" | "calendar.query" => RequestClass::Calendar,
        "ui.action" | "ui.close" => RequestClass::Ui,
        kind if kind.starts_with("chat.") => RequestClass::Chat,
        _ => RequestClass::Other,
    }
}

fn rate_limited(class: RequestClass, retry: Duration) -> EngineError {
    EngineError::new(ErrorCode::RateLimited, "request rate limit exceeded")
        .with_detail("kind", class.name())
        .with_detail(
            "retry_after_ms",
            retry.as_millis().saturating_add(1).to_string(),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_net::envelope::{MessageId, MessageKind};
    use serde_json::json;

    fn envelope(kind: &str, body: serde_json::Value) -> Envelope {
        Envelope::new(
            MessageId::parse("request").unwrap(),
            MessageKind::parse(kind).unwrap(),
            body,
        )
    }

    #[test]
    fn token_buckets_burst_then_refill() {
        let config = TokenBucketConfig::new(2, 1).unwrap();
        let now = Instant::now();
        let mut bucket = TokenBucket::new(config, now);
        assert!(bucket.can_consume(now).is_ok());
        bucket.consume();
        assert!(bucket.can_consume(now).is_ok());
        bucket.consume();
        assert_eq!(bucket.can_consume(now), Err(Duration::from_secs(1)));
        assert!(bucket.can_consume(now + Duration::from_secs(1)).is_ok());
    }

    #[test]
    fn sessions_are_isolated_and_reconnect_cannot_reset_a_budget() {
        let session = SessionLimitConfig::new(
            TokenBucketConfig::new(2, 1).unwrap(),
            TokenBucketConfig::new(10, 10).unwrap(),
            TokenBucketConfig::new(10, 10).unwrap(),
            TokenBucketConfig::new(10, 10).unwrap(),
            TokenBucketConfig::new(10, 10).unwrap(),
            TokenBucketConfig::new(10, 10).unwrap(),
            TokenBucketConfig::new(10, 10).unwrap(),
            2,
            Duration::from_secs(60),
        )
        .unwrap();
        let limiter = SessionLimiter::new(session);
        let first = SessionId::new("first").unwrap();
        let second = SessionId::new("second").unwrap();
        let now = Instant::now();
        limiter.register(&first, now).unwrap();
        limiter.register(&second, now).unwrap();
        let request = envelope("movement.request", json!({}));
        assert!(limiter.check(&first, &request, now).is_ok());
        assert!(limiter.check(&first, &request, now).is_ok());
        assert_eq!(
            limiter.check(&first, &request, now).unwrap_err().code(),
            ErrorCode::RateLimited
        );
        assert!(limiter.check(&second, &request, now).is_ok());
        limiter.register(&first, now).unwrap();
        assert_eq!(
            limiter.check(&first, &request, now).unwrap_err().code(),
            ErrorCode::RateLimited
        );
        assert!(
            limiter
                .check(&first, &request, now + Duration::from_secs(1))
                .is_ok()
        );
        assert_eq!(limiter.metrics()[0].kind, "movement");
    }

    #[test]
    fn attack_has_its_own_action_type_bucket() {
        let session = SessionLimitConfig::new(
            TokenBucketConfig::new(10, 10).unwrap(),
            TokenBucketConfig::new(10, 10).unwrap(),
            TokenBucketConfig::new(1, 1).unwrap(),
            TokenBucketConfig::new(10, 10).unwrap(),
            TokenBucketConfig::new(10, 10).unwrap(),
            TokenBucketConfig::new(10, 10).unwrap(),
            TokenBucketConfig::new(10, 10).unwrap(),
            1,
            Duration::from_secs(60),
        )
        .unwrap();
        let limiter = SessionLimiter::new(session);
        let id = SessionId::new("session").unwrap();
        let now = Instant::now();
        limiter.register(&id, now).unwrap();
        let attack = envelope("action.request", json!({"verb": "attack"}));
        let inspect = envelope("action.request", json!({"verb": "inspect"}));
        assert!(limiter.check(&id, &attack, now).is_ok());
        assert_eq!(
            limiter.check(&id, &attack, now).unwrap_err().code(),
            ErrorCode::RateLimited
        );
        assert!(limiter.check(&id, &inspect, now).is_ok());
    }

    #[test]
    fn ip_admission_limits_failed_auth_and_churn_independently() {
        let config = AdmissionLimitConfig::new(
            TokenBucketConfig::new(1, 1).unwrap(),
            TokenBucketConfig::new(1, 1).unwrap(),
            TokenBucketConfig::new(1, 1).unwrap(),
            2,
            Duration::from_secs(60),
        )
        .unwrap();
        let limiter = AdmissionLimiter::new(config);
        let first = "127.0.0.1".parse().unwrap();
        let second = "127.0.0.2".parse().unwrap();
        let now = Instant::now();
        limiter.handshake(first, now).unwrap();
        assert_eq!(
            limiter.handshake(first, now).unwrap_err().code(),
            ErrorCode::RateLimited
        );
        limiter.handshake(second, now).unwrap();
        limiter.failed_auth(second, now).unwrap();
        assert_eq!(
            limiter.failed_auth(second, now).unwrap_err().code(),
            ErrorCode::RateLimited
        );
        limiter.connection_churn(second, now).unwrap();
        assert_eq!(
            limiter.connection_churn(second, now).unwrap_err().code(),
            ErrorCode::RateLimited
        );
    }
}
