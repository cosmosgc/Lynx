//! Deterministic round-robin allocation of authoritative zone tick slots.
//!
//! This small scheduler owns only the ordering policy; callers still own each
//! zone and execute the returned tick slots synchronously. A quantum is one
//! complete authoritative zone tick. At most one quantum per zone is returned
//! by a `schedule` call, which prevents a busy zone from consuming the whole
//! service budget before other zones receive work.

use std::collections::{BTreeSet, VecDeque};

/// Stable, bounded allocator for per-zone tick work.
#[derive(Debug, Default)]
pub struct ZoneTickScheduler {
    registered: BTreeSet<String>,
    order: VecDeque<String>,
}

impl ZoneTickScheduler {
    /// Register a zone for scheduling.
    ///
    /// Returns `false` for an empty ID or an already registered zone.
    pub fn register(&mut self, zone_id: impl Into<String>) -> bool {
        let zone_id = zone_id.into();
        if zone_id.is_empty() || !self.registered.insert(zone_id.clone()) {
            return false;
        }
        self.order.push_back(zone_id);
        true
    }

    /// Remove a zone from future scheduling.
    pub fn unregister(&mut self, zone_id: &str) -> bool {
        if !self.registered.remove(zone_id) {
            return false;
        }
        self.order.retain(|candidate| candidate != zone_id);
        true
    }

    /// Allocate up to `budget` zone-tick quanta in stable round-robin order.
    ///
    /// No zone appears more than once in the returned batch. If `budget` is
    /// smaller than the number of registered zones, the cursor advances so
    /// zones at the end of one batch are first in a later batch. Registration
    /// order is stable; removals do not reset the cursor.
    #[must_use]
    pub fn schedule(&mut self, budget: usize) -> Vec<String> {
        let count = budget.min(self.order.len());
        let mut scheduled = Vec::with_capacity(count);
        for _ in 0..count {
            if let Some(zone_id) = self.order.pop_front() {
                scheduled.push(zone_id.clone());
                self.order.push_back(zone_id);
            }
        }
        scheduled
    }

    /// Number of registered zones.
    #[must_use]
    pub fn len(&self) -> usize {
        self.registered.len()
    }

    /// Whether the scheduler has no registered zones.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.registered.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_budgets_rotate_fairly_across_registered_zones() {
        let mut scheduler = ZoneTickScheduler::default();
        assert!(scheduler.register("zone-c"));
        assert!(scheduler.register("zone-a"));
        assert!(scheduler.register("zone-b"));

        assert_eq!(scheduler.schedule(2), ["zone-c", "zone-a"]);
        assert_eq!(scheduler.schedule(2), ["zone-b", "zone-c"]);
        assert_eq!(scheduler.schedule(2), ["zone-a", "zone-b"]);
    }

    #[test]
    fn a_batch_never_spends_multiple_quanta_on_one_zone() {
        let mut scheduler = ZoneTickScheduler::default();
        scheduler.register("zone-a");
        scheduler.register("zone-b");

        assert_eq!(scheduler.schedule(100), ["zone-a", "zone-b"]);
        assert_eq!(scheduler.schedule(1), ["zone-a"]);
        assert!(scheduler.schedule(0).is_empty());
    }

    #[test]
    fn registration_and_removal_do_not_duplicate_or_resurrect_zones() {
        let mut scheduler = ZoneTickScheduler::default();
        assert!(!scheduler.register(""));
        assert!(scheduler.register("zone-a"));
        assert!(!scheduler.register("zone-a"));
        assert!(scheduler.register("zone-b"));
        assert_eq!(scheduler.schedule(1), ["zone-a"]);
        assert!(scheduler.unregister("zone-b"));
        assert!(!scheduler.unregister("zone-b"));
        assert_eq!(scheduler.schedule(10), ["zone-a"]);
        assert_eq!(scheduler.len(), 1);
        assert!(!scheduler.is_empty());
        assert!(scheduler.unregister("zone-a"));
        assert!(scheduler.is_empty());
        assert!(scheduler.schedule(1).is_empty());
    }
}
