//! Transport-agnostic authoritative world gateway for Lynx.
//!
//! The gateway composes the existing crates without adding a transport or a
//! runtime:
//!
//! - `engine_sim` owns the authoritative [`Zone`](engine_sim::zone::Zone).
//! - `engine_net` owns envelopes, negotiation, replication categories, and the
//!   per-session interest manager.
//! - `game_api` supplies the authenticated [`AccountId`](game_api::AccountId).
//! - This crate owns session lifetime, actor resolution, action routing,
//!   server-owned UI panel state, reconnect tokens, dirty-entity tracking, and
//!   per-session delivery.
//!
//! See `docs/adr/0013-world-gateway-and-reconnect.md` and
//! `docs/adr/0015-ui-panel-session-state.md`.

pub mod gateway;
pub mod replication;
pub mod session;

pub use gateway::{
    Connection, ERROR_KIND, GatewayConfig, UI_ACTION_KIND, UI_CLOSE_KIND, UI_STATE_KIND, UiState,
    WorldGateway,
};
pub use replication::{
    DoorSnapshot, EntitySnapshot, HealthSnapshot, InteractionSnapshot, OwnerState, SessionDelivery,
    VISIBILITY_HIDDEN_TAG, VISIBILITY_OWNER_ONLY_TAG, VISIBILITY_STAFF_ONLY_TAG, visibility_tags,
};
pub use session::{
    GatewaySession, MAX_RESUME_TOKEN_LEN, MIN_RESUME_TOKEN_LEN, OpenPanel, ResumeToken,
    ResumeTokenSource, SequentialResumeTokens, SessionClaims, SessionRegistry, SystemResumeTokens,
};
