//! Transient realtime parties. Persistent guild-like groups live in
//! [`crate::group`] and are intentionally a separate contract.

use serde::{Deserialize, Serialize};

use crate::{AccountId, ApiError};

/// A realtime party that exists only while its socket membership is alive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealtimeParty {
    /// Platform party identifier.
    pub id: String,
    /// Whether join requests require leader approval.
    pub open: bool,
    /// Whether the party is hidden from party listings.
    pub hidden: bool,
    /// Maximum party size.
    pub max_size: u32,
    /// Optional party label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// A member presence used by party moderation operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartyPresence {
    /// Account represented by the member.
    pub account_id: AccountId,
    /// Member's active realtime session ID.
    pub session_id: String,
    /// Platform username, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

/// Party creation, membership, and join-request moderation on one live
/// realtime session. Nakama remains authoritative for membership and leader
/// permissions; dropping the socket ends ephemeral party membership.
///
/// This contract is statically composed with [`crate::RealtimeChatSession`];
/// it is not intended to be used as a trait object.
#[allow(async_fn_in_trait)]
pub trait RealtimePartySession {
    /// Create a transient party on the active session.
    async fn create_party(
        &mut self,
        open: bool,
        hidden: bool,
        max_size: u32,
        label: Option<&str>,
    ) -> Result<RealtimeParty, ApiError>;

    /// Join a party by its platform-issued identifier.
    async fn join_party(&mut self, party_id: &str) -> Result<(), ApiError>;

    /// Leave a party without closing the realtime session.
    async fn leave_party(&mut self, party_id: &str) -> Result<(), ApiError>;

    /// Close a party; the platform checks that the caller is its leader.
    async fn close_party(&mut self, party_id: &str) -> Result<(), ApiError>;

    /// List native join requests for a party.
    async fn party_join_requests(&mut self, party_id: &str)
    -> Result<Vec<PartyPresence>, ApiError>;

    /// Approve a party join request.
    async fn accept_party_join_request(
        &mut self,
        party_id: &str,
        presence: &PartyPresence,
    ) -> Result<(), ApiError>;

    /// Reject a party join request or remove a current member.
    async fn remove_party_member(
        &mut self,
        party_id: &str,
        presence: &PartyPresence,
    ) -> Result<(), ApiError>;

    /// Promote a current member to party leader.
    async fn promote_party_member(
        &mut self,
        party_id: &str,
        presence: &PartyPresence,
    ) -> Result<(), ApiError>;
}

/// Opens account-authenticated sockets for transient party lifecycles.
#[allow(async_fn_in_trait)]
pub trait RealtimePartyService {
    /// Concrete realtime session type owned by this adapter.
    type Session: RealtimePartySession;

    /// Open a socket whose lifetime controls Nakama party membership.
    async fn connect_party(&self, token: &crate::SessionToken) -> Result<Self::Session, ApiError>;
}
