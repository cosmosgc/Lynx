//! Engine-local player movement intents. This is not a network protocol.

use engine_core::error::{EngineError, ErrorCode};
use engine_core::id::StableId;
use engine_core::math::Vec3;
use engine_core::time::Tick;

use crate::entity::EntityRecord;
use crate::mind::{ControlCapability, Controller};
use crate::zone::{TickStats, Zone};

/// A proposed absolute horizontal endpoint, not an authoritative transform.
/// The principal and session must be resolved by a trusted caller.
#[derive(Debug, Clone, PartialEq)]
pub struct MovementIntent {
    /// Controlled entity.
    pub actor: StableId,
    /// Authenticated character, supplied by the server.
    pub principal: StableId,
    /// Authenticated session, supplied by the server.
    pub session: String,
    /// Strictly increasing within this actor's current control session.
    pub sequence: u64,
    /// Requested endpoint at the actor's current height.
    pub destination: Vec3,
}

pub(crate) struct MovementSequence {
    principal: StableId,
    session: String,
    sequence: u64,
}

/// Outcome of a queued intent. Rejections intentionally do not disclose geometry.
#[derive(Debug, Clone, PartialEq)]
pub struct MovementOutcome {
    /// Actor whose intent was processed.
    pub actor: StableId,
    /// Control session that queued the input; not for public replication.
    pub session: String,
    /// Principal that queued the input.
    pub principal: StableId,
    /// Input sequence processed, whether accepted or rejected.
    pub sequence: u64,
    /// Whether the endpoint was applied.
    pub applied: bool,
    /// Authoritative position after processing.
    pub position: Vec3,
    /// Tick at which the input was processed.
    pub tick: Tick,
}

fn authorized(entity: &EntityRecord, intent: &MovementIntent) -> bool {
    entity.is_alive()
        && entity.mind.as_ref().is_some_and(|mind| {
            mind.can(ControlCapability::Move)
                && matches!(&mind.controller, Controller::Player { character, session }
                if *character == intent.principal && *session == intent.session)
        })
}

impl Zone {
    /// Last queue-accepted sequence for exactly this control session.
    #[must_use]
    pub fn last_movement_sequence(
        &self,
        actor: StableId,
        principal: StableId,
        session: &str,
    ) -> Option<u64> {
        self.movement_sequences
            .get(&actor)
            .filter(|last| last.principal == principal && last.session == session)
            .map(|last| last.sequence)
    }

    /// Cancel a queued intent for this exact control session, if present.
    /// The accepted sequence remains consumed, including across reconnects.
    /// Trusted callers must invoke this when session authority is revoked or
    /// detached; the zone cannot independently check gateway session state.
    pub fn cancel_movement(&mut self, actor: StableId, principal: StableId, session: &str) {
        if self
            .movement_pending
            .get(&actor)
            .is_some_and(|intent| intent.principal == principal && intent.session == session)
        {
            self.movement_pending.remove(&actor);
        }
    }

    /// Queue at most one intent per actor and at most `max_actions` per tick.
    /// A queued sequence is consumed even if it fails at processing time.
    ///
    /// # Errors
    /// Returns a generic rejection for unauthorized, stale, or over-budget input.
    pub fn queue_movement(&mut self, intent: MovementIntent) -> Result<(), EngineError> {
        let allowed = self
            .entities
            .get(&intent.actor)
            .is_some_and(|e| authorized(e, &intent));
        let stale = self
            .movement_sequences
            .get(&intent.actor)
            .is_some_and(|last| {
                last.principal == intent.principal
                    && last.session == intent.session
                    && intent.sequence <= last.sequence
            });
        if !allowed
            || stale
            || self.movement_pending.contains_key(&intent.actor)
            || self.movement_pending.len() >= self.config.budget.max_actions
        {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "movement intent rejected",
            ));
        }
        self.movement_sequences.insert(
            intent.actor,
            MovementSequence {
                principal: intent.principal,
                session: intent.session.clone(),
                sequence: intent.sequence,
            },
        );
        self.movement_pending.insert(intent.actor, intent);
        Ok(())
    }

    /// Process queued movement in stable actor order, then advance the zone tick.
    /// `elapsed_ms` and `speed_m_per_s` are trusted server-owned tick policy,
    /// never client input. Each intent gets one budget; unused budget does not
    /// accumulate. The ordinary `advance_tick` does not process this queue.
    /// No occupancy or fine-geometry collision is performed.
    pub fn advance_movement_tick(
        &mut self,
        elapsed_ms: u32,
        speed_m_per_s: f32,
    ) -> (TickStats, Vec<MovementOutcome>) {
        let pending = std::mem::take(&mut self.movement_pending);
        let mut outcomes = Vec::with_capacity(pending.len());
        for (actor, intent) in pending {
            let applied = self.entities.get(&actor).is_some_and(|entity| {
                authorized(entity, &intent)
                    && self.config.contains(intent.destination)
                    && self.nav.allows_displacement(
                        entity.position(),
                        intent.destination,
                        speed_m_per_s,
                        elapsed_ms,
                    )
            });
            if applied && let Some(entity) = self.entities.get_mut(&actor) {
                entity.transform.position = intent.destination;
            }
            // Despawn removes pending input, so the actor still exists here.
            let position = self
                .entities
                .get(&actor)
                .expect("pending movement actor exists")
                .position();
            outcomes.push(MovementOutcome {
                actor,
                session: intent.session,
                principal: intent.principal,
                sequence: intent.sequence,
                applied,
                position,
                tick: self.tick.next(),
            });
        }
        (self.advance_tick(), outcomes)
    }
}
