//! Trusted world-state persistence for the demo service.
//!
//! The demo commits calendar and faction snapshots into a
//! [`backend_memory::MemoryBackend`]. There is no durable storage in this
//! increment: state resets on restart, and `replace_world_state` cross-validates
//! both aggregates at every commit so a bug in the command stage cannot persist
//! disagreeing state.

use engine_core::error::{EngineError, ErrorCode};
use engine_core::faction::FactionSnapshot;
use engine_core::world_event::CalendarSnapshot;
use world_gateway::CalendarPersistence;

/// Commits calendar and faction state as one pair.
#[derive(Debug, Clone)]
pub struct DemoCalendarCommit {
    backend: backend_memory::MemoryBackend,
}

impl DemoCalendarCommit {
    /// Wrap a memory backend as the demo's trusted commit seam.
    #[must_use]
    pub const fn new(backend: backend_memory::MemoryBackend) -> Self {
        Self { backend }
    }
}

impl CalendarPersistence for DemoCalendarCommit {
    fn commit(
        &self,
        calendar: &CalendarSnapshot,
        factions: &FactionSnapshot,
    ) -> Result<(), EngineError> {
        self.backend
            .replace_world_state(calendar, factions)
            .map_err(|error| {
                EngineError::new(ErrorCode::Internal, "the calendar could not be committed")
                    .with_detail("error", error.to_string())
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo;
    use engine_core::id::{DefinitionId, IdKind, StableId};
    use engine_core::time::Millis;

    #[test]
    fn an_open_demo_calendar_round_trips_through_the_backend() {
        let mut factions = demo::build_faction_world(1_000).unwrap();
        let calendar = demo::build_calendar(&mut factions, 1_000).unwrap();
        let commit = DemoCalendarCommit::new(backend_memory::MemoryBackend::new());
        commit
            .commit(&calendar.snapshot(), &factions.snapshot())
            .unwrap();
    }

    #[test]
    fn an_applied_resolution_commits_both_aggregates() {
        let mut factions = demo::build_faction_world(0).unwrap();
        let mut calendar = demo::build_calendar(&mut factions, 0).unwrap();
        let event = DefinitionId::new(demo::DEMO_EVENT).unwrap();
        let attacker = StableId::from_parts(IdKind::Character, 1, 1);
        calendar
            .enroll(
                &factions,
                &event,
                attacker,
                &DefinitionId::new("side.assaltantes").unwrap(),
                Millis::new(1_000),
            )
            .unwrap();
        calendar
            .activate(
                &mut factions,
                &event,
                demo::system_actor(),
                Millis::new(60_000),
            )
            .unwrap();
        calendar
            .resolve(
                &mut factions,
                &event,
                demo::system_actor(),
                Millis::new(120_000),
            )
            .unwrap();

        let commit = DemoCalendarCommit::new(backend_memory::MemoryBackend::new());
        commit
            .commit(&calendar.snapshot(), &factions.snapshot())
            .unwrap();
    }
}
