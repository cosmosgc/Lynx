//! Engine-local player movement intents. This is not a network protocol.

use engine_core::error::{EngineError, ErrorCode};
use engine_core::id::StableId;
use engine_core::math::Vec3;
use engine_core::time::Tick;

use crate::entity::{EntityRecord, MovementProfile};
use crate::mind::{ControlCapability, Controller};
use crate::zone::{Diagnostic, MOVEMENT_DIAGNOSTIC_LIMIT, TickStats, Zone};

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
        && entity
            .movement_profile()
            .is_some_and(MovementProfile::can_move)
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
    ///
    /// Every endpoint is revalidated against control, liveness, locomotion
    /// status, zone bounds, the nav grid, destination-cell occupancy, and
    /// circular bodies along the segment. Mutations are applied in stable entity
    /// order, so a later intent sees earlier accepted positions and contested
    /// space has one deterministic winner. Rejections add a bounded,
    /// server-only diagnostic without exposing geometry to the client.
    pub fn advance_movement_tick(
        &mut self,
        elapsed_ms: u32,
        speed_m_per_s: f32,
    ) -> (TickStats, Vec<MovementOutcome>) {
        let pending = std::mem::take(&mut self.movement_pending);
        let mut outcomes = Vec::with_capacity(pending.len());
        for (actor, intent) in pending {
            let applied = self.entities.get(&actor).is_some_and(|entity| {
                let Some(profile) = entity.movement_profile() else {
                    return false;
                };
                authorized(entity, &intent)
                    && self.config.contains(intent.destination)
                    && self.nav.allows_displacement(
                        entity.position(),
                        intent.destination,
                        speed_m_per_s * profile.speed_multiplier,
                        elapsed_ms,
                    )
                    && self.destination_is_clear(
                        actor,
                        entity.position(),
                        intent.destination,
                        profile.radius_m,
                    )
            });
            if applied && let Some(entity) = self.entities.get_mut(&actor) {
                entity.transform.position = intent.destination;
            } else {
                self.record_movement_rejection(actor);
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

    /// Reject destination-cell occupancy and horizontal body intersections.
    ///
    /// Iteration uses the zone's stable entity order. A tick applies accepted
    /// moves in that same order, so contested movement has one deterministic
    /// winner without a wall-clock race. This server-only check deliberately
    /// returns only a boolean: clients receive an applied/rejected outcome, not
    /// a map of blockers or hidden collision geometry.
    fn destination_is_clear(
        &self,
        actor: StableId,
        from: Vec3,
        destination: Vec3,
        actor_radius: f32,
    ) -> bool {
        let Some(destination_cell) = self.nav.cell_of(destination) else {
            return false;
        };
        self.entities
            .values()
            .filter(|entity| entity.id != actor)
            .all(|other| {
                let Some(profile) = other.movement_profile() else {
                    return true;
                };
                if !profile.occupies {
                    return true;
                }
                if self.nav.cell_of(other.position()) == Some(destination_cell) {
                    return false;
                }
                segment_clear(
                    from,
                    destination,
                    other.position(),
                    actor_radius + profile.radius_m,
                )
            })
    }

    fn record_movement_rejection(&mut self, actor: StableId) {
        if self.movement_diagnostics.len() >= MOVEMENT_DIAGNOSTIC_LIMIT {
            let _ = self.movement_diagnostics.pop_front();
        }
        self.movement_diagnostics.push_back(Diagnostic {
            code: "movement.rejected".into(),
            entity: Some(actor),
            message: "a queued movement intent was rejected by server validation".into(),
        });
    }
}

/// Whether a horizontal movement segment remains outside a blocking circle.
fn segment_clear(from: Vec3, to: Vec3, center: Vec3, radius: f32) -> bool {
    let start_x = f64::from(from.x);
    let start_z = f64::from(from.z);
    let delta_x = f64::from(to.x) - start_x;
    let delta_z = f64::from(to.z) - start_z;
    let length_squared = delta_x * delta_x + delta_z * delta_z;
    let projection = if length_squared == 0.0 {
        0.0
    } else {
        ((f64::from(center.x) - start_x) * delta_x + (f64::from(center.z) - start_z) * delta_z)
            / length_squared
    }
    .clamp(0.0, 1.0);
    let closest_x = start_x + projection * delta_x;
    let closest_z = start_z + projection * delta_z;
    let offset_x = f64::from(center.x) - closest_x;
    let offset_z = f64::from(center.z) - closest_z;
    let combined_radius = f64::from(radius);
    offset_x * offset_x + offset_z * offset_z >= combined_radius * combined_radius
}
