//! Headless, connection-generation-scoped view of public world state.
//!
//! The caller owns the transport and must tag every event with the generation
//! returned by `begin_connection`. Never retry a movement after ambiguous
//! delivery: disconnect and obtain a new private snapshot baseline instead.

use std::collections::BTreeMap;

use engine_core::id::{DefinitionId, StableId};
use engine_net::envelope::{Envelope, MessageId, MessageKind};
use engine_net::interest::{InterestUpdate, MAX_INTEREST_ENTITIES};
use engine_net::replication::{ReplicationBatch, ReplicationCategory, SessionId};
use serde::Deserialize;
use serde_json::json;

use crate::reconciliation::{self, Position, Reconciliation, SessionDomain};

/// Public presentation fields only; arbitrary replication payloads are not retained.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PublicEntity {
    pub entity: StableId,
    pub prototype: DefinitionId,
    pub position: WirePosition,
    pub yaw_degrees: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct WirePosition {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl From<WirePosition> for Position {
    fn from(p: WirePosition) -> Self {
        Self {
            x: p.x,
            y: p.y,
            z: p.z,
        }
    }
}

#[derive(Debug, Deserialize)]
struct Ready {
    session: SessionId,
    actor: StableId,
    resumed: bool,
}

#[derive(Debug, Deserialize)]
struct Movement {
    tick: u64,
    position: WirePosition,
    last_consumed_sequence: Option<u64>,
    applied: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct Update {
    tick: u64,
    interest: InterestUpdate,
    batch: ReplicationBatch,
    movement: Option<Movement>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    StaleGeneration,
    UnexpectedMessage,
    InvalidMessage,
    /// This client has no consumer for the reliable replication category.
    UnsupportedReplication,
    StaleTick,
    Capacity,
    Reconciliation(reconciliation::Error),
}

impl From<reconciliation::Error> for Error {
    fn from(value: reconciliation::Error) -> Self {
        Self::Reconciliation(value)
    }
}

/// State is bounded by the protocol interest limit and one pending request.
#[derive(Debug)]
pub struct WorldState {
    generation: u64,
    connected: bool,
    ready: Option<Ready>,
    previous: Option<(SessionId, StableId)>,
    domain: SessionDomain,
    tick: Option<u64>,
    entities: BTreeMap<StableId, (u64, u64, PublicEntity)>,
    pending: Option<(MessageId, u64)>,
    reconciliation: Reconciliation,
}

impl WorldState {
    pub fn new(max_prediction_distance: f32) -> Result<Self, Error> {
        Ok(Self {
            generation: 0,
            connected: false,
            ready: None,
            previous: None,
            domain: SessionDomain::New,
            tick: None,
            entities: BTreeMap::new(),
            pending: None,
            reconciliation: Reconciliation::new(max_prediction_distance)?,
        })
    }

    /// Invalidates the old connection, including all of its queued events.
    pub fn begin_connection(&mut self) -> Result<u64, Error> {
        let next = self.generation.checked_add(1).ok_or(Error::Capacity)?;
        self.clear_connection();
        self.generation = next;
        self.connected = true;
        Ok(next)
    }

    /// Ignore disconnect notifications from a replaced connection.
    pub fn disconnect(&mut self, generation: u64) -> Result<(), Error> {
        self.check_generation(generation)?;
        self.clear_connection();
        Ok(())
    }

    fn clear_connection(&mut self) {
        self.connected = false;
        self.ready = None;
        self.tick = None;
        self.entities.clear();
        self.pending = None;
        self.reconciliation.disconnect();
    }

    fn check_generation(&self, generation: u64) -> Result<(), Error> {
        if !self.connected || self.generation != generation {
            Err(Error::StaleGeneration)
        } else {
            Ok(())
        }
    }

    pub fn entities(&self) -> impl Iterator<Item = &PublicEntity> {
        self.entities.values().map(|(_, _, entity)| entity)
    }
    pub fn tick(&self) -> Option<u64> {
        self.tick
    }
    pub fn reconciliation(&self) -> &Reconciliation {
        &self.reconciliation
    }

    pub(crate) fn is_pending_reply(&self, envelope: &Envelope) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|(id, _)| envelope.correlation.as_ref() == Some(id))
    }

    /// Creates exactly one request; the caller sends it once. On uncertain send
    /// or connection failure, call `disconnect`, not this method again.
    pub fn movement_request(&mut self, destination: Position) -> Result<Envelope, Error> {
        if !self.connected || self.tick.is_none() {
            return Err(Error::UnexpectedMessage);
        }
        let request = self.reconciliation.predict(destination)?;
        let id = MessageId::parse(&format!("move-{}-{}", self.generation, request.sequence))
            .expect("bounded numeric id");
        self.pending = Some((id.clone(), request.sequence));
        Ok(Envelope::new(
            id,
            MessageKind::parse("movement.request").expect("literal"),
            json!({"sequence": request.sequence, "destination": {"x": destination.x, "y": destination.y, "z": destination.z}}),
        ))
    }

    /// Consume an envelope obtained from the corresponding transport. Other
    /// message kinds (including bearer tokens) are ignored and never retained.
    pub fn receive(&mut self, generation: u64, envelope: &Envelope) -> Result<(), Error> {
        self.check_generation(generation)?;
        envelope.validate().map_err(|_| Error::InvalidMessage)?;
        match envelope.kind.as_str() {
            "world.ready" => {
                if self.ready.is_some() {
                    return Err(Error::UnexpectedMessage);
                }
                let ready: Ready = serde_json::from_value(envelope.body.clone())
                    .map_err(|_| Error::InvalidMessage)?;
                if ready.resumed
                    && self.previous.as_ref() != Some(&(ready.session.clone(), ready.actor))
                {
                    return Err(Error::UnexpectedMessage);
                }
                self.domain = if ready.resumed {
                    SessionDomain::Resume
                } else {
                    SessionDomain::New
                };
                self.previous = Some((ready.session.clone(), ready.actor));
                self.ready = Some(ready);
                Ok(())
            }
            "world.snapshot" | "world.delta" => self.update(envelope),
            "movement.queued" | "protocol.error" => {
                let Some((id, sequence)) = &self.pending else {
                    return Ok(());
                };
                if envelope.correlation.as_ref() != Some(id) {
                    return Ok(());
                }
                if envelope.kind.as_str() == "movement.queued" {
                    if envelope
                        .body
                        .get("sequence")
                        .and_then(serde_json::Value::as_u64)
                        != Some(*sequence)
                    {
                        return Err(Error::InvalidMessage);
                    }
                    self.reconciliation.queued(*sequence)?;
                } else {
                    self.reconciliation.queue_rejected(*sequence)?;
                    self.pending = None;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn update(&mut self, envelope: &Envelope) -> Result<(), Error> {
        if self.ready.is_none() {
            return Err(Error::UnexpectedMessage);
        }
        let snapshot = envelope.kind.as_str() == "world.snapshot";
        if snapshot != self.tick.is_none() {
            return Err(Error::UnexpectedMessage);
        }
        let update: Update =
            serde_json::from_value(envelope.body.clone()).map_err(|_| Error::InvalidMessage)?;
        if self.tick.is_some_and(|tick| update.tick <= tick) {
            return Err(Error::StaleTick);
        }
        if update.batch.tick.0 != update.tick || update.batch.validate().is_err() {
            return Err(Error::InvalidMessage);
        }
        if update.interest.set.len() > MAX_INTEREST_ENTITIES
            || update.interest.added.len() > MAX_INTEREST_ENTITIES
            || update.interest.removed.len() > MAX_INTEREST_ENTITIES
        {
            return Err(Error::Capacity);
        }
        let mut entities = if snapshot {
            BTreeMap::new()
        } else {
            self.entities.clone()
        };
        entities.retain(|id, _| update.interest.set.contains(id));
        for item in &update.batch.updates {
            if !matches!(
                item.category,
                ReplicationCategory::InterestManaged | ReplicationCategory::StateSnapshot
            ) {
                return Err(Error::UnsupportedReplication);
            }
            let id = item.entity.ok_or(Error::InvalidMessage)?;
            if !update.interest.set.contains(&id) {
                return Err(Error::InvalidMessage);
            }
            let entity: PublicEntity =
                serde_json::from_value(item.payload.clone()).map_err(|_| Error::InvalidMessage)?;
            if entity.entity != id
                || !entity.position.x.is_finite()
                || !entity.position.y.is_finite()
                || !entity.position.z.is_finite()
                || !entity.yaw_degrees.is_finite()
            {
                return Err(Error::InvalidMessage);
            }
            let order = (item.sequence, item.tick.0);
            if entities
                .get(&id)
                .is_none_or(|(seq, tick, _)| order > (*seq, *tick))
            {
                entities.insert(id, (order.0, order.1, entity));
            }
        }
        // The set is authoritative, including on snapshots and when an entity
        // enters interest without being named in `added`. Never install a view
        // for which the client has no public state.
        if update
            .interest
            .set
            .iter()
            .any(|id| !entities.contains_key(&id))
        {
            return Err(Error::InvalidMessage);
        }
        match (snapshot, update.movement) {
            (true, Some(m)) if m.tick == update.tick && m.applied.is_none() => {
                self.reconciliation.baseline(
                    m.position.into(),
                    m.tick,
                    m.last_consumed_sequence,
                    self.domain,
                )?;
                self.pending = None;
            }
            (false, Some(m))
                if m.tick == update.tick
                    && m.applied.is_some()
                    && m.last_consumed_sequence.is_some() =>
            {
                self.reconciliation.processed(
                    m.last_consumed_sequence.unwrap(),
                    m.tick,
                    m.position.into(),
                    m.applied.unwrap(),
                )?;
                self.pending = None;
            }
            (false, None) => {}
            _ => return Err(Error::InvalidMessage),
        }
        self.entities = entities;
        self.tick = Some(update.tick);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::id::IdKind;
    use serde_json::{Value, json};

    fn actor(n: u128) -> StableId {
        StableId::from_parts(IdKind::Entity, 1, n)
    }
    fn message(kind: &str, body: Value) -> Envelope {
        Envelope::new(
            MessageId::parse("server-1").unwrap(),
            MessageKind::parse(kind).unwrap(),
            body,
        )
    }
    fn ready(resumed: bool, session: &str) -> Envelope {
        message(
            "world.ready",
            json!({"session": session, "actor": actor(1), "resumed": resumed, "negotiated": {}}),
        )
    }
    fn world(
        kind: &str,
        tick: u64,
        ids: &[StableId],
        updates: &Value,
        movement: &Value,
    ) -> Envelope {
        message(
            kind,
            json!({"tick": tick, "interest": {"set": ids, "added": [], "removed": []},
            "batch": {"tick": tick, "updates": updates}, "movement": movement}),
        )
    }
    fn public(id: StableId, tick: u64, sequence: u64, x: f32) -> Value {
        json!({"category": "state_snapshot", "entity": id, "tick": tick, "sequence": sequence,
            "payload": {"entity": id, "prototype": "character.player", "position": {"x": x, "y": 0, "z": 0}, "yaw_degrees": 0, "inventory": "not public"}})
    }
    fn movement(tick: u64, x: f32, seq: Option<u64>, applied: Option<bool>) -> Value {
        json!({"tick": tick, "position": {"x": x, "y": 0, "z": 0}, "last_consumed_sequence": seq, "applied": applied})
    }
    fn pos(x: f32) -> Position {
        Position { x, y: 0.0, z: 0.0 }
    }
    fn start() -> (WorldState, u64) {
        let mut state = WorldState::new(2.0).unwrap();
        let g = state.begin_connection().unwrap();
        state.receive(g, &ready(false, "session-1")).unwrap();
        state
            .receive(
                g,
                &world(
                    "world.snapshot",
                    10,
                    &[actor(1), actor(2)],
                    &json!([public(actor(1), 10, 1, 0.0), public(actor(2), 10, 1, 4.0)]),
                    &movement(10, 0.0, None, None),
                ),
            )
            .unwrap();
        (state, g)
    }

    #[test]
    fn snapshot_delta_and_private_correction() {
        let (mut state, g) = start();
        assert_eq!(state.entities().count(), 2);
        assert!(!format!("{state:?}").contains("secret"));
        assert!(!format!("{state:?}").contains("not public"));
        let request = state.movement_request(pos(1.0)).unwrap();
        assert_eq!(
            request.body,
            json!({"sequence": 1, "destination": {"x": 1.0, "y": 0.0, "z": 0.0}})
        );
        let queued =
            message("movement.queued", json!({"sequence": 1})).with_correlation(request.id);
        state.receive(g, &queued).unwrap();
        assert_eq!(state.reconciliation().predicted_position(), Some(pos(1.0)));
        let delta = world(
            "world.delta",
            11,
            &[actor(1)],
            &json!([public(actor(1), 11, 2, 0.25)]),
            &movement(11, 0.25, Some(1), Some(false)),
        );
        state.receive(g, &delta).unwrap();
        assert_eq!(state.entities().count(), 1);
        assert_eq!(state.reconciliation().predicted_position(), Some(pos(0.25)));
        assert_eq!(state.receive(g, &delta), Err(Error::StaleTick));
    }

    #[test]
    fn generation_resume_and_no_retry() {
        let (mut state, g) = start();
        state.movement_request(pos(1.0)).unwrap();
        let next = state.begin_connection().unwrap();
        assert_eq!(
            state.receive(g, &ready(false, "other")),
            Err(Error::StaleGeneration)
        );
        assert_eq!(state.disconnect(g), Err(Error::StaleGeneration));
        assert!(state.movement_request(pos(1.0)).is_err());
        assert_eq!(
            state.receive(next, &ready(true, "other")),
            Err(Error::UnexpectedMessage)
        );
        state.receive(next, &ready(true, "session-1")).unwrap();
        state
            .receive(
                next,
                &world(
                    "world.snapshot",
                    12,
                    &[actor(1)],
                    &json!([public(actor(1), 12, 2, 0.5)]),
                    &movement(12, 0.5, Some(1), None),
                ),
            )
            .unwrap();
        assert_eq!(
            state.movement_request(pos(1.0)).unwrap().body["sequence"],
            2
        );
        let new = state.begin_connection().unwrap();
        state.receive(new, &ready(false, "session-2")).unwrap();
        state
            .receive(
                new,
                &world(
                    "world.snapshot",
                    1,
                    &[],
                    &json!([]),
                    &movement(1, 0.0, None, None),
                ),
            )
            .unwrap();
        assert_eq!(
            state.movement_request(pos(1.0)).unwrap().body["sequence"],
            1
        );
    }

    #[test]
    fn bad_updates_are_atomic_and_rejections_correlated() {
        let (mut state, g) = start();
        let request = state.movement_request(pos(1.0)).unwrap();
        state
            .receive(
                g,
                &message("protocol.error", json!({"code":"invalid_request"}))
                    .with_correlation(MessageId::parse("other").unwrap()),
            )
            .unwrap();
        assert_eq!(state.reconciliation().predicted_position(), Some(pos(1.0)));
        let invalid = world(
            "world.delta",
            11,
            &[actor(1)],
            &json!([public(actor(2), 11, 2, 2.0)]),
            &Value::Null,
        );
        assert_eq!(state.receive(g, &invalid), Err(Error::InvalidMessage));
        assert_eq!(state.entities().count(), 2);
        assert_eq!(state.tick(), Some(10));
        state
            .receive(
                g,
                &message("protocol.error", json!({"code":"invalid_request"}))
                    .with_correlation(request.id),
            )
            .unwrap();
        assert_eq!(state.reconciliation().predicted_position(), Some(pos(0.0)));
        assert_eq!(
            state.movement_request(pos(1.0)).unwrap().body["sequence"],
            2
        );
    }

    #[test]
    fn bounded_interest_and_per_entity_ordering() {
        let (mut state, g) = start();
        let many: Vec<_> = (1..=MAX_INTEREST_ENTITIES as u128 + 1).map(actor).collect();
        let oversized = world("world.delta", 11, &many, &json!([]), &Value::Null);
        assert_eq!(state.receive(g, &oversized), Err(Error::Capacity));
        assert_eq!(state.tick(), Some(10));
        let older = world(
            "world.delta",
            11,
            &[actor(1), actor(2)],
            &json!([public(actor(1), 11, 0, 99.0)]),
            &Value::Null,
        );
        state.receive(g, &older).unwrap();
        assert_eq!(
            state
                .entities()
                .find(|entity| entity.entity == actor(1))
                .unwrap()
                .position
                .x,
            0.0
        );
    }

    #[test]
    fn incomplete_views_and_unsupported_reliable_updates_are_atomic() {
        let mut state = WorldState::new(2.0).unwrap();
        let g = state.begin_connection().unwrap();
        state.receive(g, &ready(false, "session-1")).unwrap();
        let incomplete = world(
            "world.snapshot",
            10,
            &[actor(1)],
            &json!([]),
            &movement(10, 0.0, None, None),
        );
        assert_eq!(state.receive(g, &incomplete), Err(Error::InvalidMessage));
        assert_eq!(state.tick(), None);
        assert_eq!(state.reconciliation().authoritative_position(), None);

        state
            .receive(
                g,
                &world(
                    "world.snapshot",
                    10,
                    &[actor(1)],
                    &json!([public(actor(1), 10, 1, 0.0)]),
                    &movement(10, 0.0, None, None),
                ),
            )
            .unwrap();
        state.movement_request(pos(1.0)).unwrap();
        let incomplete = world(
            "world.delta",
            11,
            &[actor(1), actor(2)],
            &json!([public(actor(1), 11, 2, 0.5)]),
            &movement(11, 0.5, Some(1), Some(true)),
        );
        assert_eq!(state.receive(g, &incomplete), Err(Error::InvalidMessage));
        assert_eq!(state.tick(), Some(10));
        assert_eq!(state.entities().next().unwrap().position.x, 0.0);
        assert_eq!(state.reconciliation().predicted_position(), Some(pos(1.0)));

        for category in ["owner_only", "reliable_event", "property_scoped", "admin"] {
            let reliable = json!({"category": category, "entity": actor(1), "session": "session-1", "tick": 11, "payload": {"secret": true}});
            let delta = world(
                "world.delta",
                11,
                &[actor(1)],
                &json!([public(actor(1), 11, 2, 0.5), reliable]),
                &movement(11, 0.5, Some(1), Some(true)),
            );
            assert_eq!(state.receive(g, &delta), Err(Error::UnsupportedReplication));
            assert_eq!(state.tick(), Some(10));
            assert_eq!(state.entities().next().unwrap().position.x, 0.0);
            assert_eq!(state.reconciliation().predicted_position(), Some(pos(1.0)));
        }
        state
            .receive(
                g,
                &world(
                    "world.delta",
                    11,
                    &[actor(1), actor(2)],
                    &json!([public(actor(2), 11, 1, 4.0)]),
                    &Value::Null,
                ),
            )
            .unwrap();
        assert_eq!(state.entities().count(), 2);
    }
}
