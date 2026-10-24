//! The transport-facing world server loop for Lynx.
//!
//! `world_gateway` decides authority, sessions, interest, reconnect, and UI
//! panel state. `world_server` owns the remaining connection-oriented work:
//! connection IDs, wire message kinds, snapshot/delta envelopes, UI control
//! replies, disconnect and resume bookkeeping, and closing connections whose
//! sessions ended.
//!
//! The crate deliberately contains no socket, thread, async runtime, or clock.
//! A transport supplies trusted [`SessionClaims`](world_gateway::SessionClaims),
//! calls [`WorldServer::open`] or [`WorldServer::resume`], and moves
//! [`Outbound`] values to its peer. Authentication, TLS, framing, and
//! backpressure stay in the transport layer.
//!
//! ```text
//! bytes → framing (engine_net) → transport/auth → WorldServer → WorldGateway
//! ```

pub mod runtime;

pub use runtime::{
    CloseReason, ConnectionId, DELTA_KIND, Opened, Outbound, READY_KIND, Ready, SNAPSHOT_KIND,
    WorldServer, WorldUpdate,
};
