//! Headless, connection-generation-scoped view of public world state.
//!
//! The caller owns the transport and must tag every event with the generation
//! returned by `begin_connection`. Never retry a movement after ambiguous
//! delivery: disconnect and obtain a new private snapshot baseline instead.

use std::collections::{BTreeMap, VecDeque};

use engine_core::id::{DefinitionId, StableId};
use engine_core::inventory::{Inventory, UniformItemRules};
use engine_net::envelope::{Envelope, MessageId, MessageKind};
use engine_net::interest::{InterestUpdate, MAX_INTEREST_ENTITIES};
use engine_net::replication::{
    ReplicationBatch, ReplicationCategory, ReplicationUpdate, SessionId,
};
use serde::Deserialize;
use serde_json::json;

use crate::reconciliation::{self, Position, Reconciliation, SessionDomain};

/// How many movement attempts are retained for desync diagnostics.
pub const MOVEMENT_LOG_LIMIT: usize = 32;

/// One bounded movement attempt and its authoritative resolution.
///
/// The log contains only protocol identifiers and world positions. Credentials,
/// tokens, and private owner state are never copied into it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MovementTrace {
    /// Client-issued sequence number.
    pub sequence: u64,
    /// Destination the client predicted locally.
    pub predicted: Position,
    /// Authoritative position before the attempt.
    pub authoritative_before: Option<Position>,
    /// Server tick that resolved the attempt, once processed.
    pub server_tick: Option<u64>,
    /// Authoritative position after the attempt, once processed.
    pub authoritative_after: Option<Position>,
    /// Whether the server applied (`true`) or rejected (`false`) the attempt.
    pub applied: Option<bool>,
}

/// Public presentation fields only; arbitrary replication payloads are not retained.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PublicEntity {
    pub entity: StableId,
    pub prototype: DefinitionId,
    pub position: WirePosition,
    pub yaw_degrees: f32,
    #[serde(default)]
    pub health: Option<PublicHealth>,
    #[serde(default)]
    pub interaction: Option<PublicInteraction>,
    #[serde(default)]
    pub stunned: bool,
    #[serde(default)]
    pub slowed: bool,
}

/// Public hit points supplied by the authoritative entity projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct PublicHealth {
    pub current: i32,
    pub max: i32,
}

/// Minimal server-advertised interaction surface for contextual presentation.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PublicInteraction {
    #[serde(default)]
    pub verbs: Vec<serde_json::Value>,
    #[serde(default)]
    pub enabled: bool,
}

impl PublicInteraction {
    #[must_use]
    pub fn is_available(&self) -> bool {
        self.enabled && !self.verbs.is_empty()
    }
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
    #[serde(default)]
    reset: bool,
    #[serde(default)]
    slow_multiplier: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct Update {
    tick: u64,
    interest: InterestUpdate,
    batch: ReplicationBatch,
    movement: Option<Movement>,
}

/// Private owner-state payload; the inventory is never retained for other entities.
#[derive(Debug, Deserialize)]
struct OwnerInventory {
    inventory: Inventory,
}

#[derive(Clone)]
struct OwnedInventory {
    sequence: u64,
    tick: u64,
    inventory: Inventory,
}

// Owner state is private. Keep it out of diagnostics even if the surrounding
// `WorldState` is formatted.
impl std::fmt::Debug for OwnedInventory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OwnedInventory")
            .field("sequence", &self.sequence)
            .field("tick", &self.tick)
            .finish_non_exhaustive()
    }
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
    owner_inventory: Option<OwnedInventory>,
    pending: Option<(MessageId, u64)>,
    last_movement_applied: Option<bool>,
    movement_log: VecDeque<MovementTrace>,
    slow_multiplier: Option<f32>,
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
            owner_inventory: None,
            pending: None,
            last_movement_applied: None,
            movement_log: VecDeque::with_capacity(MOVEMENT_LOG_LIMIT),
            slow_multiplier: None,
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
        self.owner_inventory = None;
        self.pending = None;
        self.last_movement_applied = None;
        self.movement_log.clear();
        self.slow_multiplier = None;
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
    /// The controlled actor's private inventory, once the server replicated it.
    ///
    /// Owner state for other entities is never retained.
    pub fn inventory(&self) -> Option<&Inventory> {
        self.owner_inventory.as_ref().map(|owned| &owned.inventory)
    }
    pub fn tick(&self) -> Option<u64> {
        self.tick
    }
    /// Whether this connection resumed an existing control session.
    pub fn resumed(&self) -> bool {
        self.ready.as_ref().is_some_and(|ready| ready.resumed)
    }
    /// The authenticated actor for this session, once `world.ready` arrived.
    pub fn actor(&self) -> Option<StableId> {
        self.ready.as_ref().map(|ready| ready.actor)
    }
    /// True while a movement request is awaiting its authoritative result.
    pub fn has_pending_movement(&self) -> bool {
        self.pending.is_some()
    }
    /// Consume the last processed movement outcome, if any. `false` means the
    /// server rejected the endpoint and the local position was corrected.
    pub(crate) fn take_movement_applied(&mut self) -> Option<bool> {
        self.last_movement_applied.take()
    }
    /// Exact timed-slow factor for this controlled actor, from private state.
    pub fn slow_multiplier(&self) -> Option<f32> {
        self.slow_multiplier
    }
    pub fn reconciliation(&self) -> &Reconciliation {
        &self.reconciliation
    }
    /// Bounded movement attempts, oldest first, for desync reproduction.
    ///
    /// Entries are cleared when the connection is invalidated, so they always
    /// belong to the current control session.
    pub fn movement_log(&self) -> impl DoubleEndedIterator<Item = &MovementTrace> {
        self.movement_log.iter()
    }
    fn record_movement_request(&mut self, sequence: u64, predicted: Position) {
        if self.movement_log.len() == MOVEMENT_LOG_LIMIT {
            self.movement_log.pop_front();
        }
        self.movement_log.push_back(MovementTrace {
            sequence,
            predicted,
            authoritative_before: self.reconciliation.authoritative_position(),
            server_tick: None,
            authoritative_after: None,
            applied: None,
        });
    }
    fn record_movement_result(
        &mut self,
        sequence: u64,
        predicted: Position,
        authoritative: Position,
        server_tick: u64,
        applied: bool,
    ) {
        if let Some(trace) = self
            .movement_log
            .iter_mut()
            .rev()
            .find(|trace| trace.sequence == sequence)
        {
            trace.predicted = predicted;
            trace.server_tick = Some(server_tick);
            trace.authoritative_after = Some(authoritative);
            trace.applied = Some(applied);
        }
    }
    pub(crate) fn reconciliation_mut(&mut self) -> &mut Reconciliation {
        &mut self.reconciliation
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
        self.record_movement_request(request.sequence, destination);
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
        let actor = self
            .ready
            .as_ref()
            .map(|ready| ready.actor)
            .ok_or(Error::UnexpectedMessage)?;
        let mut entities = if snapshot {
            BTreeMap::new()
        } else {
            self.entities.clone()
        };
        let mut owner_inventory = if snapshot {
            None
        } else {
            self.owner_inventory.clone()
        };
        entities.retain(|id, _| update.interest.set.contains(id));
        for item in &update.batch.updates {
            match item.category {
                ReplicationCategory::InterestManaged | ReplicationCategory::StateSnapshot => {
                    let id = item.entity.ok_or(Error::InvalidMessage)?;
                    if !update.interest.set.contains(&id) {
                        return Err(Error::InvalidMessage);
                    }
                    let entity: PublicEntity = serde_json::from_value(item.payload.clone())
                        .map_err(|_| Error::InvalidMessage)?;
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
                ReplicationCategory::OwnerOnly => {
                    Self::apply_owner_update(&mut owner_inventory, actor, item)?;
                }
                _ => return Err(Error::UnsupportedReplication),
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
        self.apply_movement_update(snapshot, update.tick, update.movement)?;
        self.entities = entities;
        self.owner_inventory = owner_inventory;
        self.tick = Some(update.tick);
        Ok(())
    }

    fn apply_owner_update(
        owner_inventory: &mut Option<OwnedInventory>,
        actor: StableId,
        item: &ReplicationUpdate,
    ) -> Result<(), Error> {
        let id = item.entity.ok_or(Error::InvalidMessage)?;
        // The server also sends private state for other entities the principal
        // owns. Only the controlled actor is presentable.
        if id != actor {
            return Ok(());
        }
        let state: OwnerInventory =
            serde_json::from_value(item.payload.clone()).map_err(|_| Error::InvalidMessage)?;
        state
            .inventory
            .validate(&UniformItemRules::new(u32::MAX))
            .map_err(|_| Error::InvalidMessage)?;
        let order = (item.sequence, item.tick.0);
        if owner_inventory
            .as_ref()
            .is_none_or(|owned| order > (owned.sequence, owned.tick))
        {
            *owner_inventory = Some(OwnedInventory {
                sequence: order.0,
                tick: order.1,
                inventory: state.inventory,
            });
        }
        Ok(())
    }

    fn apply_movement_update(
        &mut self,
        snapshot: bool,
        tick: u64,
        movement: Option<Movement>,
    ) -> Result<(), Error> {
        match (snapshot, movement) {
            (true, Some(m))
                if m.tick == tick
                    && m.applied.is_none()
                    && !m.reset
                    && valid_slow_multiplier(m.slow_multiplier) =>
            {
                self.reconciliation.baseline(
                    m.position.into(),
                    m.tick,
                    m.last_consumed_sequence,
                    self.domain,
                )?;
                self.slow_multiplier = m.slow_multiplier;
                self.pending = None;
            }
            (false, Some(m))
                if m.tick == tick
                    && m.applied.is_some()
                    && !m.reset
                    && m.last_consumed_sequence.is_some()
                    && valid_slow_multiplier(m.slow_multiplier) =>
            {
                let sequence = m.last_consumed_sequence.unwrap();
                let predicted = self
                    .reconciliation
                    .predicted_position()
                    .unwrap_or_else(|| m.position.into());
                self.reconciliation.processed(
                    sequence,
                    m.tick,
                    m.position.into(),
                    m.applied.unwrap(),
                )?;
                self.record_movement_result(
                    sequence,
                    predicted,
                    m.position.into(),
                    m.tick,
                    m.applied.unwrap(),
                );
                self.last_movement_applied = m.applied;
                self.slow_multiplier = m.slow_multiplier;
                self.pending = None;
            }
            (false, Some(m))
                if m.tick == tick
                    && m.applied.is_none()
                    && valid_slow_multiplier(m.slow_multiplier) =>
            {
                if m.reset {
                    self.reconciliation.reset(
                        m.position.into(),
                        m.tick,
                        m.last_consumed_sequence,
                        self.domain,
                    )?;
                    self.pending = None;
                    self.last_movement_applied = None;
                }
                self.slow_multiplier = m.slow_multiplier;
            }
            (false, None) => {}
            _ => return Err(Error::InvalidMessage),
        }
        Ok(())
    }
}

fn valid_slow_multiplier(multiplier: Option<f32>) -> bool {
    multiplier.is_none_or(|value| value.is_finite() && (0.0..1.0).contains(&value))
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
    fn torch(quantity: u32) -> Value {
        json!({"definition": "item.torch", "quantity": quantity, "instance": null})
    }
    fn owner(id: StableId, tick: u64, sequence: u64, slots: &[Value]) -> Value {
        json!({"category": "owner_only", "entity": id, "tick": tick, "sequence": sequence,
            "payload": {"inventory": {"capacity": slots.len(), "slots": slots}}})
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
    fn public_interaction_only_advertises_enabled_nonempty_actions() {
        let available: PublicInteraction = serde_json::from_value(json!({
            "verbs": ["talk"], "range_m": 2.0, "enabled": true
        }))
        .unwrap();
        let disabled: PublicInteraction = serde_json::from_value(json!({
            "verbs": ["talk"], "enabled": false
        }))
        .unwrap();
        let empty: PublicInteraction = serde_json::from_value(json!({
            "verbs": [], "enabled": true
        }))
        .unwrap();
        assert!(available.is_available());
        assert!(!disabled.is_available());
        assert!(!empty.is_available());
    }

    #[test]
    fn public_stunned_status_is_optional_and_decoded() {
        let mut value = public(actor(1), 1, 1, 0.0);
        assert!(
            !serde_json::from_value::<PublicEntity>(value["payload"].clone())
                .unwrap()
                .stunned
        );
        value["payload"]["stunned"] = json!(true);
        assert!(
            serde_json::from_value::<PublicEntity>(value["payload"].clone())
                .unwrap()
                .stunned
        );
    }

    #[test]
    fn public_slowed_status_is_optional_and_decoded_without_modifier() {
        let mut value = public(actor(1), 1, 1, 0.0);
        let decoded: PublicEntity = serde_json::from_value(value["payload"].clone()).unwrap();
        assert!(!decoded.slowed);
        value["payload"]["slowed"] = json!(true);
        let decoded: PublicEntity = serde_json::from_value(value["payload"].clone()).unwrap();
        assert!(decoded.slowed);
        assert!(value["payload"].get("slow").is_none());
        assert!(value["payload"].get("multiplier").is_none());
    }

    #[test]
    fn private_slow_updates_change_prediction_without_resetting_position() {
        let (mut state, generation) = start();
        let before = state.reconciliation().authoritative_position();
        let mut slowed = movement(11, 99.0, None, None);
        slowed["slow_multiplier"] = json!(0.35);
        state
            .receive(
                generation,
                &world(
                    "world.delta",
                    11,
                    &[actor(1), actor(2)],
                    &json!([]),
                    &slowed,
                ),
            )
            .unwrap();
        assert_eq!(state.slow_multiplier(), Some(0.35));
        assert_eq!(state.reconciliation().authoritative_position(), before);

        let mut cleared = movement(12, 77.0, None, None);
        cleared["slow_multiplier"] = Value::Null;
        state
            .receive(
                generation,
                &world(
                    "world.delta",
                    12,
                    &[actor(1), actor(2)],
                    &json!([]),
                    &cleared,
                ),
            )
            .unwrap();
        assert_eq!(state.slow_multiplier(), None);
        assert_eq!(state.reconciliation().authoritative_position(), before);

        let mut invalid = movement(13, 55.0, None, None);
        invalid["slow_multiplier"] = json!(1.0);
        assert_eq!(
            state.receive(
                generation,
                &world(
                    "world.delta",
                    13,
                    &[actor(1), actor(2)],
                    &json!([]),
                    &invalid
                ),
            ),
            Err(Error::InvalidMessage)
        );
        assert_eq!(state.tick(), Some(12));
        assert_eq!(state.slow_multiplier(), None);
    }

    #[test]
    fn private_discontinuity_resets_prediction_to_authoritative_position() {
        let (mut state, generation) = start();
        let mut reset = movement(11, 9.0, None, None);
        reset["reset"] = json!(true);
        reset["last_consumed_sequence"] = json!(0);
        state
            .receive(
                generation,
                &world("world.delta", 11, &[actor(1), actor(2)], &json!([]), &reset),
            )
            .unwrap();
        assert_eq!(
            state.reconciliation().authoritative_position().unwrap().x,
            9.0
        );
        assert_eq!(
            state.reconciliation().predicted_position(),
            state.reconciliation().authoritative_position()
        );
        assert!(!state.has_pending_movement());
    }

    #[test]
    fn destination_snapshot_starts_a_fresh_prediction_domain_for_zone_transfer() {
        let (mut state, _) = start();
        state.movement_request(pos(1.0)).unwrap();
        assert!(state.has_pending_movement());

        // A zone transfer establishes a new control session and installs the
        // destination's private snapshot; no old-zone endpoint is replayed.
        let destination_generation = state.begin_connection().unwrap();
        state
            .receive(destination_generation, &ready(false, "destination-session"))
            .unwrap();
        state
            .receive(
                destination_generation,
                &world(
                    "world.snapshot",
                    1,
                    &[actor(1)],
                    &json!([public(actor(1), 1, 1, 50.0)]),
                    &movement(1, 50.0, None, None),
                ),
            )
            .unwrap();

        assert_eq!(
            state.reconciliation().authoritative_position(),
            Some(pos(50.0))
        );
        assert_eq!(state.reconciliation().predicted_position(), Some(pos(50.0)));
        assert!(!state.has_pending_movement());
        assert_eq!(
            state.movement_request(pos(51.0)).unwrap().body["sequence"],
            1
        );
    }

    #[test]
    fn public_health_updates_and_removal_follow_authority() {
        let (mut state, generation) = start();
        assert!(state.entities().all(|entity| entity.health.is_none()));
        for (tick, health) in [
            (11, json!({"current": 75, "max": 100})),
            (12, json!({"current": 25, "max": 100})),
            (13, Value::Null),
        ] {
            let mut update = public(actor(2), tick, tick, 4.0);
            update["payload"]["health"] = health.clone();
            state
                .receive(
                    generation,
                    &world(
                        "world.delta",
                        tick,
                        &[actor(1), actor(2)],
                        &json!([update]),
                        &Value::Null,
                    ),
                )
                .unwrap();
            let expected: Option<PublicHealth> = serde_json::from_value(health).unwrap();
            assert_eq!(
                state
                    .entities()
                    .find(|entity| entity.entity == actor(2))
                    .unwrap()
                    .health,
                expected
            );
        }
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
    fn movement_log_records_prediction_and_authoritative_correction() {
        let (mut state, g) = start();
        let request = state.movement_request(pos(1.0)).unwrap();
        assert_eq!(state.movement_log().count(), 1);
        let entry = state.movement_log().next().unwrap();
        assert_eq!(entry.sequence, 1);
        assert_eq!(entry.predicted, pos(1.0));
        assert_eq!(entry.authoritative_before, Some(pos(0.0)));
        assert_eq!(entry.server_tick, None);
        assert_eq!(entry.applied, None);

        let queued =
            message("movement.queued", json!({"sequence": 1})).with_correlation(request.id);
        state.receive(g, &queued).unwrap();
        let delta = world(
            "world.delta",
            11,
            &[actor(1)],
            &json!([public(actor(1), 11, 2, 0.25)]),
            &movement(11, 0.25, Some(1), Some(false)),
        );
        state.receive(g, &delta).unwrap();

        let entry = state.movement_log().next_back().unwrap();
        assert_eq!(entry.sequence, 1);
        assert_eq!(entry.predicted, pos(1.0));
        assert_eq!(entry.authoritative_before, Some(pos(0.0)));
        assert_eq!(entry.server_tick, Some(11));
        assert_eq!(entry.authoritative_after, Some(pos(0.25)));
        assert_eq!(entry.applied, Some(false));
        assert!(!format!("{state:?}").contains("secret"));
    }

    #[test]
    fn movement_log_is_bounded_and_cleared_with_the_connection() {
        let (mut state, g) = start();
        for step in 0..(MOVEMENT_LOG_LIMIT + 8) {
            let request = state.movement_request(pos(1.0)).unwrap();
            let sequence = request.body["sequence"].as_u64().unwrap();
            let queued = message("movement.queued", json!({"sequence": sequence}))
                .with_correlation(request.id.clone());
            state.receive(g, &queued).unwrap();
            let delta = world(
                "world.delta",
                11 + step as u64,
                &[actor(1)],
                &json!([public(actor(1), 11 + step as u64, 2, 0.25)]),
                &movement(11 + step as u64, 0.25, Some(sequence), Some(true)),
            );
            state.receive(g, &delta).unwrap();
        }
        assert_eq!(state.movement_log().count(), MOVEMENT_LOG_LIMIT);
        state.disconnect(g).unwrap();
        assert_eq!(state.movement_log().count(), 0);
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

        for category in ["reliable_event", "property_scoped", "admin"] {
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

    fn definition(name: &str) -> DefinitionId {
        DefinitionId::new(name).unwrap()
    }

    #[test]
    fn owner_inventory_tracks_the_controlled_actor_with_latest_wins() {
        let (mut state, g) = start();
        assert!(state.inventory().is_none());

        state
            .receive(
                g,
                &world(
                    "world.delta",
                    11,
                    &[actor(1), actor(2)],
                    &json!([owner(actor(1), 11, 1, &[torch(2)])]),
                    &Value::Null,
                ),
            )
            .unwrap();
        let inventory = state.inventory().unwrap();
        assert_eq!(inventory.capacity(), 1);
        assert_eq!(inventory.occupied_slots(), 1);
        assert_eq!(inventory.total_quantity(&definition("item.torch")), 2);

        // A delta without owner state leaves the private inventory unchanged.
        state
            .receive(
                g,
                &world(
                    "world.delta",
                    12,
                    &[actor(1), actor(2)],
                    &json!([]),
                    &Value::Null,
                ),
            )
            .unwrap();
        assert_eq!(
            state
                .inventory()
                .unwrap()
                .total_quantity(&definition("item.torch")),
            2
        );

        state
            .receive(
                g,
                &world(
                    "world.delta",
                    13,
                    &[actor(1), actor(2)],
                    &json!([owner(actor(1), 13, 2, &[torch(5)])]),
                    &Value::Null,
                ),
            )
            .unwrap();
        assert_eq!(
            state
                .inventory()
                .unwrap()
                .total_quantity(&definition("item.torch")),
            5
        );

        // A reliable update from an older sequence is ignored even at a newer tick.
        state
            .receive(
                g,
                &world(
                    "world.delta",
                    14,
                    &[actor(1), actor(2)],
                    &json!([owner(actor(1), 14, 1, &[torch(9)])]),
                    &Value::Null,
                ),
            )
            .unwrap();
        assert_eq!(
            state
                .inventory()
                .unwrap()
                .total_quantity(&definition("item.torch")),
            5
        );
        assert_eq!(state.tick(), Some(14));
    }

    #[test]
    fn owner_inventory_for_other_owned_entities_is_ignored() {
        let (mut state, g) = start();
        state
            .receive(
                g,
                &world(
                    "world.delta",
                    11,
                    &[actor(1), actor(2)],
                    &json!([owner(actor(2), 11, 1, &[torch(99)])]),
                    &Value::Null,
                ),
            )
            .unwrap();
        assert!(state.inventory().is_none());
        assert_eq!(state.tick(), Some(11));
    }

    #[test]
    fn owner_inventory_is_scoped_to_a_connection_and_cleared_by_snapshots() {
        let (mut state, g) = start();
        state
            .receive(
                g,
                &world(
                    "world.delta",
                    11,
                    &[actor(1), actor(2)],
                    &json!([owner(actor(1), 11, 1, &[torch(2)])]),
                    &Value::Null,
                ),
            )
            .unwrap();
        state.disconnect(g).unwrap();
        assert!(state.inventory().is_none());

        let next = state.begin_connection().unwrap();
        state.receive(next, &ready(true, "session-1")).unwrap();
        state
            .receive(
                next,
                &world(
                    "world.snapshot",
                    20,
                    &[actor(1)],
                    &json!([
                        public(actor(1), 20, 1, 0.0),
                        owner(actor(1), 20, 3, &[torch(4)])
                    ]),
                    &movement(20, 0.0, None, None),
                ),
            )
            .unwrap();
        assert_eq!(
            state
                .inventory()
                .unwrap()
                .total_quantity(&definition("item.torch")),
            4
        );

        // A fresh connection's snapshot is authoritative even when it carries
        // no owner state.
        state.disconnect(next).unwrap();
        let third = state.begin_connection().unwrap();
        state.receive(third, &ready(true, "session-1")).unwrap();
        state
            .receive(
                third,
                &world(
                    "world.snapshot",
                    21,
                    &[actor(1)],
                    &json!([public(actor(1), 21, 2, 0.0)]),
                    &movement(21, 0.0, None, None),
                ),
            )
            .unwrap();
        assert!(state.inventory().is_none());
    }

    #[test]
    fn invalid_owner_inventory_rejects_the_whole_update_atomically() {
        let (mut state, g) = start();
        state
            .receive(
                g,
                &world(
                    "world.delta",
                    11,
                    &[actor(1), actor(2)],
                    &json!([owner(actor(1), 11, 1, &[torch(2)])]),
                    &Value::Null,
                ),
            )
            .unwrap();

        let invalid_payloads = [
            owner(actor(1), 12, 2, &[torch(0)]),
            json!({
                "category": "owner_only",
                "entity": actor(1),
                "tick": 12,
                "sequence": 2,
                "payload": {"inventory": {"capacity": 0, "slots": []}}
            }),
            json!({
                "category": "owner_only",
                "entity": actor(1),
                "tick": 12,
                "sequence": 2,
                "payload": {}
            }),
        ];
        for owner_update in invalid_payloads {
            let delta = world(
                "world.delta",
                12,
                &[actor(1), actor(2)],
                &json!([public(actor(1), 12, 2, 9.0), owner_update]),
                &Value::Null,
            );
            assert_eq!(state.receive(g, &delta), Err(Error::InvalidMessage));
            assert_eq!(state.tick(), Some(11));
            assert_eq!(
                state
                    .entities()
                    .find(|entity| entity.entity == actor(1))
                    .unwrap()
                    .position
                    .x,
                0.0
            );
            assert_eq!(
                state
                    .inventory()
                    .unwrap()
                    .total_quantity(&definition("item.torch")),
                2
            );
        }
    }

    #[test]
    fn owner_inventory_is_redacted_from_debug_output() {
        let (mut state, g) = start();
        state
            .receive(
                g,
                &world(
                    "world.delta",
                    11,
                    &[actor(1), actor(2)],
                    &json!([owner(actor(1), 11, 1, &[torch(2)])]),
                    &Value::Null,
                ),
            )
            .unwrap();
        let debug = format!("{state:?}");
        assert!(debug.contains("OwnedInventory"));
        assert!(!debug.contains("item.torch"));
        assert!(!debug.contains("torch"));
    }
}
