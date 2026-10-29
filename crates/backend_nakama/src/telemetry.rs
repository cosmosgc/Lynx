//! Fixed-size, process-local backend operation telemetry.

use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Inclusive upper bounds in seconds; the final bucket is +Inf.
pub const DURATION_BUCKETS_SECONDS: [f64; 5] = [0.001, 0.01, 0.1, 1.0, 10.0];

/// A coherent snapshot. Buckets are cumulative and include failures.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TelemetrySnapshot {
    /// Completed operations, including failures.
    pub count: u64,
    /// Operations returning an error, including validation and domain errors.
    pub failures: u64,
    /// Sum of elapsed wall-clock durations in seconds.
    pub duration_seconds: f64,
    /// Cumulative counts for `DURATION_BUCKETS_SECONDS`, followed by +Inf.
    pub duration_buckets: [u64; 6],
}

/// Cheaply cloned, thread-safe handle to an adapter's aggregate measurements.
#[derive(Clone, Debug, Default)]
pub struct Telemetry(Arc<Mutex<TelemetrySnapshot>>);

impl Telemetry {
    /// Read a consistent snapshot without resetting counters.
    #[must_use]
    pub fn snapshot(&self) -> TelemetrySnapshot {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn measure<T, E>(&self, operation: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
        let start = Instant::now();
        let result = operation();
        let seconds = start.elapsed().as_secs_f64();
        let mut snapshot = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        snapshot.count = snapshot.count.saturating_add(1);
        if result.is_err() {
            snapshot.failures = snapshot.failures.saturating_add(1);
        }
        snapshot.duration_seconds += seconds;
        for (index, bound) in DURATION_BUCKETS_SECONDS.iter().enumerate() {
            if seconds <= *bound {
                snapshot.duration_buckets[index] =
                    snapshot.duration_buckets[index].saturating_add(1);
            }
        }
        snapshot.duration_buckets[5] = snapshot.duration_buckets[5].saturating_add(1);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_snapshots_count_concurrent_success_and_failure() {
        let telemetry = Telemetry::default();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let telemetry = telemetry.clone();
                scope.spawn(move || {
                    for _ in 0..100 {
                        let _: Result<(), ()> = telemetry.measure(|| Err(()));
                        let _: Result<(), ()> = telemetry.measure(|| Ok(()));
                    }
                });
            }
        });
        let snapshot = telemetry.snapshot();
        assert_eq!(snapshot.count, 800);
        assert_eq!(snapshot.failures, 400);
        assert_eq!(snapshot.duration_buckets[5], 800);
        assert!(
            snapshot
                .duration_buckets
                .windows(2)
                .all(|pair| pair[0] <= pair[1])
        );
    }
}
