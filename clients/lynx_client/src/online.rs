//! Explicit, headless online orchestration. This is not a login flow.
//!
//! Construct and connect on a live Tokio runtime. The runtime must remain alive
//! while Bevy polls; no reconnect or movement retry is performed automatically.
//! Resume tokens are deliberately discarded, so end-to-end resume is not provided.
//! Reliable replication (including owner inventory) is not implemented: a
//! delivery containing it fails the connection rather than acknowledging it.

use bevy::prelude::{ResMut, Resource};
use engine_net::envelope::{Envelope, MessageId, MessageKind};
use lynx_client_net::{Client, Config, Failure, SendError, Status};
use serde_json::json;

use crate::{
    reconciliation::Position,
    world_state::{self, WorldState},
};

const POLL_BUDGET: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnlineStatus {
    Disconnected,
    Connecting,
    AwaitingSnapshot,
    Active,
    Failed(OnlineError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnlineError {
    Transport(Failure),
    Closed,
    Protocol,
    State(world_state::Error),
    Send(SendError),
}

/// Insert this resource to enable polling in the shared Bevy schedule.
/// Neither this resource nor its debug output retains server bearer tokens.
#[derive(Debug, Resource)]
pub struct OnlineClient {
    world: WorldState,
    client: Option<Client>,
    generation: Option<u64>,
    status: OnlineStatus,
    pending_ack: Option<u64>,
}

impl OnlineClient {
    pub fn new(max_prediction_distance: f32) -> Result<Self, world_state::Error> {
        Ok(Self {
            world: WorldState::new(max_prediction_distance)?,
            client: None,
            generation: None,
            status: OnlineStatus::Disconnected,
            pending_ack: None,
        })
    }

    /// Replaces any prior connection. Must be called inside a Tokio runtime.
    /// The caller supplies credentials and the hello acknowledgement explicitly.
    pub fn connect(&mut self, config: Config) -> Result<(), world_state::Error> {
        self.disconnect();
        let generation = self.world.begin_connection()?;
        self.generation = Some(generation);
        self.client = Some(Client::connect(config));
        self.status = OnlineStatus::Connecting;
        Ok(())
    }

    pub fn status(&self) -> OnlineStatus {
        self.status
    }
    pub fn world(&self) -> &WorldState {
        &self.world
    }

    /// Advance presentation-only correction smoothing. This never produces a
    /// request or mutates the authoritative world view.
    pub fn smooth_presentation(&mut self, fraction: f32) {
        self.world.reconciliation_mut().smooth(fraction);
    }

    pub fn disconnect(&mut self) {
        if let Some(mut client) = self.client.take() {
            client.close();
        }
        if let Some(generation) = self.generation.take() {
            let _ = self.world.disconnect(generation);
        }
        self.pending_ack = None;
        self.status = OnlineStatus::Disconnected;
    }

    fn fail(&mut self, error: OnlineError) {
        self.disconnect();
        self.status = OnlineStatus::Failed(error);
    }

    /// Queue one action exactly once. A send failure invalidates the baseline;
    /// the caller must explicitly reconnect and wait for a new snapshot.
    pub fn move_to(&mut self, destination: Position) -> Result<(), OnlineError> {
        if self.status != OnlineStatus::Active {
            return Err(OnlineError::Protocol);
        }
        let request = self
            .world
            .movement_request(destination)
            .map_err(OnlineError::State)?;
        let result = self
            .client
            .as_ref()
            .expect("active client")
            .try_send(&request);
        if let Err(error) = result {
            let error = OnlineError::Send(error);
            self.fail(error);
            return Err(error);
        }
        Ok(())
    }

    /// Process at most a bounded number of messages and coalesce tick acks.
    pub fn poll(&mut self) {
        let Some(client) = self.client.as_ref() else {
            return;
        };
        match client.status() {
            Status::Failed(error) => {
                self.fail(OnlineError::Transport(error));
                return;
            }
            Status::Closed => {
                self.fail(OnlineError::Closed);
                return;
            }
            Status::AwaitingReady | Status::Ready if self.status == OnlineStatus::Connecting => {
                self.status = OnlineStatus::AwaitingSnapshot;
            }
            _ => {}
        }
        for _ in 0..POLL_BUDGET {
            let Ok(envelope) = self.client.as_mut().expect("connected").try_recv() else {
                break;
            };
            if envelope.kind.as_str() == "protocol.error" && !self.world.is_pending_reply(&envelope)
            {
                self.fail(OnlineError::Protocol);
                return;
            }
            let generation = self.generation.expect("connected generation");
            if let Err(error) = self.world.receive(generation, &envelope) {
                self.fail(OnlineError::State(error));
                return;
            }
            if matches!(envelope.kind.as_str(), "world.snapshot" | "world.delta") {
                self.pending_ack = self.world.tick();
                self.status = OnlineStatus::Active;
            }
        }
        if let Some(tick) = self.pending_ack {
            let ack = Envelope::new(
                MessageId::parse(&format!(
                    "ack-{}-{tick}",
                    self.generation.expect("connected generation")
                ))
                .expect("bounded numeric id"),
                MessageKind::parse("session.ack").expect("literal"),
                json!({"tick": tick}),
            );
            match self.client.as_ref().expect("connected").try_send(&ack) {
                Ok(()) => self.pending_ack = None,
                Err(SendError::Full) => {} // Cumulative acks can safely be coalesced.
                Err(error) => self.fail(OnlineError::Send(error)),
            }
        }
    }
}

pub(crate) fn poll_online(client: Option<ResMut<OnlineClient>>) {
    if let Some(mut client) = client {
        client.poll();
    }
}

/// Keep smoothing bounded and independent from transport polling. A fixed
/// fraction avoids a local frame clock becoming gameplay input.
pub(crate) fn smooth_online(client: Option<ResMut<OnlineClient>>) {
    if let Some(mut client) = client {
        client.smooth_presentation(0.25);
    }
}
