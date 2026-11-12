//! Simulation time and tick types.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A monotonic simulation tick.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Tick(pub u64);

impl Tick {
    /// The tick before the first tick.
    pub const ZERO: Self = Self(0);

    /// The next tick.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    /// Tick difference, saturating at zero.
    #[must_use]
    pub const fn elapsed_since(self, earlier: Self) -> u64 {
        self.0.saturating_sub(earlier.0)
    }
}

impl fmt::Display for Tick {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "tick:{}", self.0)
    }
}

/// Wall-clock milliseconds since the Unix epoch.
///
/// Domain code never reads the clock directly; callers pass a value in, and
/// tests inject a fixed one.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Millis(pub i64);

impl Millis {
    /// Create a timestamp.
    #[must_use]
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    /// The raw millisecond value.
    #[must_use]
    pub const fn value(self) -> i64 {
        self.0
    }

    /// Whether this instant is at or after `deadline`.
    #[must_use]
    pub const fn is_at_or_after(self, deadline: Self) -> bool {
        self.0 >= deadline.0
    }

    /// Time remaining until `deadline`, or zero if passed.
    #[must_use]
    pub const fn remaining_until(self, deadline: Self) -> i64 {
        let remaining = deadline.0.saturating_sub(self.0);
        if remaining > 0 { remaining } else { 0 }
    }
}

/// A recurring daily window in UTC simulation time.
///
/// The interval is start-inclusive and end-exclusive. Windows may cross
/// midnight; equal endpoints and minutes outside `0..1440` are invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DailySchedule {
    /// UTC minute at which the window begins (`0..1440`).
    pub start_minute_utc: u16,
    /// UTC minute at which the window ends (`0..1440`).
    pub end_minute_utc: u16,
}

impl DailySchedule {
    /// Whether this window has valid, non-empty UTC minute bounds.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.start_minute_utc < 1440
            && self.end_minute_utc < 1440
            && self.start_minute_utc != self.end_minute_utc
    }

    /// Whether `at` falls within this recurring window.
    #[must_use]
    pub fn contains(self, at: Millis) -> bool {
        if !self.is_valid() {
            return false;
        }
        let minute = at.0.rem_euclid(86_400_000) / 60_000;
        let start = i64::from(self.start_minute_utc);
        let end = i64::from(self.end_minute_utc);
        if start < end {
            minute >= start && minute < end
        } else {
            minute >= start || minute < end
        }
    }
}

/// A duration in whole milliseconds.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct DurationMs(pub u64);

impl DurationMs {
    /// Create a duration.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Convert to wall-clock milliseconds for arithmetic with [`Millis`].
    #[must_use]
    pub const fn as_i64(self) -> i64 {
        self.0.cast_signed()
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_advance_without_overflow() {
        assert_eq!(Tick(4).next(), Tick(5));
        assert_eq!(Tick(u64::MAX).next(), Tick(u64::MAX));
        assert_eq!(Tick(9).elapsed_since(Tick(4)), 5);
        assert_eq!(Tick(4).elapsed_since(Tick(9)), 0);
    }

    #[test]
    fn deadlines_saturate() {
        let now = Millis::new(1_000);
        assert!(!now.is_at_or_after(Millis::new(1_001)));
        assert!(now.is_at_or_after(Millis::new(999)));
        assert_eq!(now.remaining_until(Millis::new(1_250)), 250);
        assert_eq!(now.remaining_until(Millis::new(10)), 0);
    }
}
