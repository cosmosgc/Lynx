//! Process-local ownership and fair ticking for a set of authoritative zones.

use std::collections::BTreeMap;
use std::sync::Arc;

use engine_core::error::{EngineError, ErrorCode};
use engine_core::time::{Millis, Tick};
use engine_net::envelope::Envelope;
use engine_sim::ai::AiTickPolicy;
use world_gateway::{ConcurrentSessionPolicy, ResumeToken, SessionClaims};
use world_server::{ConnectionId, Opened, Outbound, WorldServer};

use crate::authority::ZoneAuthoritySet;
use crate::persistence::ZoneSnapshotStore;
use crate::scheduler::ZoneTickScheduler;
use crate::travel::{
    PlayerHandoffRequest, ZoneHandoffCoordinator, ZoneHandoffReceipt, ZoneHandoffStore,
};
use engine_content::WorldMapDefinition;

/// One scheduled zone tick and its session-specific outputs.
#[derive(Debug)]
pub struct ZoneTickOutcome {
    /// Stable zone instance ID.
    pub zone_id: String,
    /// Outputs produced by this zone's authoritative tick, or its tick error.
    pub result: Result<Vec<Outbound>, EngineError>,
}

/// A connection's zone-local routing address.
///
/// Connection IDs are allocated independently by each [`WorldServer`], so a
/// transport must retain the zone ID returned by admission alongside the
/// connection ID.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ZoneConnection {
    /// Stable zone instance ID.
    pub zone_id: String,
    /// Zone-local transport connection ID.
    pub connection: ConnectionId,
}

/// A successful open/resume paired with its zone-local routing address.
#[derive(Debug, Clone)]
pub struct ZoneOpened {
    /// Address to use for later input and disconnect events.
    pub route: ZoneConnection,
    /// Handshake and snapshot messages produced by the zone gateway.
    pub opened: Opened,
}

/// Owns a set of zone servers and schedules one tick quantum per selected zone.
///
/// Identity checks span all registered gateways, while each gateway continues
/// to own its own sessions and zone authority. The checkpoint store persists
/// independent zone snapshots; world-level aggregates remain outside this
/// owner until their coordinator is available.
#[derive(Debug)]
pub struct MultiZoneRuntime {
    zones: BTreeMap<String, WorldServer>,
    scheduler: ZoneTickScheduler,
    checkpoints: Arc<dyn ZoneSnapshotStore>,
    session_policy: Option<ConcurrentSessionPolicy>,
}

impl MultiZoneRuntime {
    /// Create a runtime with the checkpoint store used by its owned zones.
    #[must_use]
    pub fn new(checkpoints: Arc<dyn ZoneSnapshotStore>) -> Self {
        Self {
            zones: BTreeMap::new(),
            scheduler: ZoneTickScheduler::default(),
            checkpoints,
            session_policy: None,
        }
    }

    /// Register an already-restored zone server.
    ///
    /// The server's zone instance is its routing key. A duplicate zone ID is
    /// refused so the runtime cannot silently replace live session state.
    pub fn register(&mut self, server: WorldServer) -> Result<String, EngineError> {
        let session_policy = server.gateway().config().concurrent_session_policy;
        if self
            .session_policy
            .is_some_and(|existing| existing != session_policy)
        {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "all zones in a runtime must use the same session identity policy",
            ));
        }
        let zone_id = server.gateway().zone().config().instance.to_string();
        if self.zones.contains_key(&zone_id) {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "the zone instance is already registered in this runtime",
            )
            .with_detail("zone_id", zone_id));
        }
        self.scheduler.register(zone_id.clone());
        self.zones.insert(zone_id.clone(), server);
        self.session_policy = Some(session_policy);
        Ok(zone_id)
    }

    /// Remove an idle zone server. Retained reconnect windows also prevent
    /// removal because their tokens are scoped to the owning gateway.
    pub fn unregister(&mut self, zone_id: &str) -> Result<WorldServer, EngineError> {
        let server = self.zones.get(zone_id).ok_or_else(|| {
            EngineError::new(ErrorCode::NotFound, "the zone instance is not registered")
                .with_detail("zone_id", zone_id)
        })?;
        if server.connection_count() > 0 || server.gateway().retained_sessions() > 0 {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "a zone with active or retained sessions cannot be removed",
            )
            .with_detail("zone_id", zone_id));
        }
        self.scheduler.unregister(zone_id);
        let server = self.zones.remove(zone_id).expect("zone was checked above");
        if self.zones.is_empty() {
            self.session_policy = None;
        }
        Ok(server)
    }

    /// Registered zone IDs in stable order.
    pub fn zone_ids(&self) -> impl ExactSizeIterator<Item = &str> {
        self.zones.keys().map(String::as_str)
    }

    /// Open a session in a selected zone after checking identity conflicts
    /// across the complete runtime.
    pub fn open(
        &mut self,
        zone_id: &str,
        claims: SessionClaims,
        client: &engine_net::protocol::Handshake,
        now: Millis,
    ) -> Result<ZoneOpened, EngineError> {
        if self
            .zones
            .values()
            .any(|server| server.gateway().retains_identity(&claims))
        {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "an identity covered by the session policy already has a session in this runtime",
            ));
        }
        let server = self.zones.get_mut(zone_id).ok_or_else(|| {
            EngineError::new(ErrorCode::NotFound, "the requested zone is not available")
                .with_detail("zone_id", zone_id)
        })?;
        let opened = server.open(claims, client, now)?;
        Ok(ZoneOpened {
            route: ZoneConnection {
                zone_id: zone_id.to_owned(),
                connection: opened.connection,
            },
            opened,
        })
    }

    /// Resume a token in its owning zone. Callers retain the zone ID from the
    /// original admission alongside the token.
    pub fn resume(
        &mut self,
        route: &ZoneConnection,
        token: &ResumeToken,
        client: &engine_net::protocol::Handshake,
        last_ack_tick: Tick,
        now: Millis,
    ) -> Result<ZoneOpened, EngineError> {
        self.resume_in_zone(&route.zone_id, token, client, last_ack_tick, now)
    }

    /// Resume a session in a selected zone when the transport has the zone
    /// address but no prior connection ID, as with a reconnecting client.
    pub fn resume_in_zone(
        &mut self,
        zone_id: &str,
        token: &ResumeToken,
        client: &engine_net::protocol::Handshake,
        last_ack_tick: Tick,
        now: Millis,
    ) -> Result<ZoneOpened, EngineError> {
        let server = self.zones.get_mut(zone_id).ok_or_else(|| {
            EngineError::new(ErrorCode::NotFound, "the session's zone is not available")
                .with_detail("zone_id", zone_id)
        })?;
        let opened = server.resume(token, client, last_ack_tick, now)?;
        Ok(ZoneOpened {
            route: ZoneConnection {
                zone_id: zone_id.to_owned(),
                connection: opened.connection,
            },
            opened,
        })
    }

    /// Route one client envelope to its owning zone gateway.
    pub fn receive(
        &mut self,
        route: &ZoneConnection,
        envelope: &Envelope,
        now: Millis,
    ) -> Result<Vec<Outbound>, EngineError> {
        self.zones
            .get_mut(&route.zone_id)
            .ok_or_else(|| {
                EngineError::new(
                    ErrorCode::NotFound,
                    "the connection's zone is not available",
                )
                .with_detail("zone_id", &route.zone_id)
            })?
            .receive(route.connection, envelope, now)
    }

    /// Detach a transport from its owning zone while retaining its resume
    /// window according to that gateway's policy.
    pub fn disconnect(
        &mut self,
        route: &ZoneConnection,
        last_ack_tick: Tick,
        now: Millis,
    ) -> Result<Vec<Outbound>, EngineError> {
        self.zones
            .get_mut(&route.zone_id)
            .ok_or_else(|| {
                EngineError::new(
                    ErrorCode::NotFound,
                    "the connection's zone is not available",
                )
                .with_detail("zone_id", &route.zone_id)
            })?
            .disconnect(route.connection, last_ack_tick, now)
    }

    /// Tick up to `budget` distinct zones in fair round-robin order.
    ///
    /// A failing zone is returned as an individual outcome; the remaining
    /// scheduled zones still receive their tick quantum.
    pub fn tick_batch(
        &mut self,
        budget: usize,
        now: Millis,
        interval_ms: u32,
    ) -> Vec<ZoneTickOutcome> {
        self.scheduler
            .schedule(budget)
            .into_iter()
            .filter_map(|zone_id| {
                self.zones.get_mut(&zone_id).map(|server| ZoneTickOutcome {
                    zone_id,
                    result: server.tick_with_interval(now, interval_ms),
                })
            })
            .collect()
    }

    /// Persist one zone's current checkpoint independently of other zones.
    pub fn checkpoint_zone(&self, zone_id: &str) -> Result<(), EngineError> {
        let server = self.zones.get(zone_id).ok_or_else(|| {
            EngineError::new(ErrorCode::NotFound, "the zone instance is not registered")
                .with_detail("zone_id", zone_id)
        })?;
        self.checkpoints
            .save_zone(&server.gateway().zone().snapshot())
    }

    /// Persist every registered zone in stable ID order.
    ///
    /// Each zone checkpoint is independent. An error stops the operation and
    /// leaves any earlier zone checkpoints valid and recoverable.
    pub fn checkpoint_all(&self) -> Result<(), EngineError> {
        for zone_id in self.zones.keys() {
            self.checkpoint_zone(zone_id)?;
        }
        Ok(())
    }

    /// Read-only access to one registered server.
    #[must_use]
    pub fn zone_server(&self, zone_id: &str) -> Option<&WorldServer> {
        self.zones.get(zone_id)
    }

    /// Mutable access for trusted zone lifecycle and handoff coordination.
    #[must_use]
    pub fn zone_server_mut(&mut self, zone_id: &str) -> Option<&mut WorldServer> {
        self.zones.get_mut(zone_id)
    }

    /// Replace the server-owned AI policy for every zone as one authority-loop
    /// operation, so a faction relation refresh cannot split zones across catalogs.
    pub fn set_ai_tick_policy(&mut self, policy: Option<AiTickPolicy>) {
        for server in self.zones.values_mut() {
            server
                .gateway_mut()
                .zone_mut()
                .set_ai_tick_policy(policy.clone());
        }
    }

    /// Commit a durable entity handoff between two registered live zone servers.
    /// The authority loop calls this synchronously, so no tick can observe the
    /// temporary removal of the source server from the registry.
    pub fn handoff_player_durable(
        &mut self,
        coordinator: &mut ZoneHandoffCoordinator,
        atlas: &WorldMapDefinition,
        authorities: &ZoneAuthoritySet,
        source_id: &str,
        destination_id: &str,
        request: &PlayerHandoffRequest,
        store: &dyn ZoneHandoffStore,
    ) -> Result<ZoneHandoffReceipt, EngineError> {
        if source_id == destination_id {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "a player handoff requires distinct source and destination zones",
            ));
        }
        let mut source = self.zones.remove(source_id).ok_or_else(|| {
            EngineError::new(ErrorCode::NotFound, "the source zone is not registered")
                .with_detail("zone_id", source_id)
        })?;
        let result = match self.zones.get_mut(destination_id) {
            Some(destination) => coordinator.handoff_player_durable(
                atlas,
                authorities,
                source.gateway_mut().zone_mut(),
                destination.gateway_mut().zone_mut(),
                request,
                store,
            ),
            None => Err(EngineError::new(
                ErrorCode::NotFound,
                "the destination zone is not registered",
            )
            .with_detail("zone_id", destination_id)),
        };
        self.zones.insert(source_id.to_owned(), source);
        result
    }

    /// Resolve an existing zone-local transport connection.
    #[must_use]
    pub fn contains_connection(&self, route: &ZoneConnection) -> bool {
        self.zones
            .get(&route.zone_id)
            .is_some_and(|server| server.has_connection(route.connection))
    }
}
