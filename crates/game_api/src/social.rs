//! Social relationships owned by the platform adapter.

use serde::{Deserialize, Serialize};

use crate::service::{AccountId, ApiError, SessionToken};

/// Relationship state between the authenticated account and another account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FriendState {
    /// The accounts are mutual friends.
    Friend,
    /// The authenticated account blocked the other account.
    Blocked,
}

/// One social relationship as seen by the authenticated account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Friend {
    /// The other account.
    pub account_id: AccountId,
    /// Platform username, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Display name, when one differs from the username.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Relationship from the caller's point of view.
    pub state: FriendState,
}

/// Friends and blocks for the authenticated account.
///
/// Adapters must derive the caller from `token`; a client never submits its own
/// account ID. Platform friendships are normally symmetric, but blocks are
/// directional and must hide the blocked account from both parties' friend
/// lists.
pub trait SocialService {
    /// List friends and blocked accounts for the authenticated account.
    fn friends(&self, token: &SessionToken) -> Result<Vec<Friend>, ApiError>;

    /// Add a mutual friend relationship.
    fn add_friend(&self, token: &SessionToken, friend: &AccountId) -> Result<(), ApiError>;

    /// Remove a mutual friend relationship.
    fn remove_friend(&self, token: &SessionToken, friend: &AccountId) -> Result<(), ApiError>;

    /// Block an account and remove any existing friendship.
    fn block_account(&self, token: &SessionToken, account: &AccountId) -> Result<(), ApiError>;

    /// Remove a block. This does not restore a previous friendship.
    fn unblock_account(&self, token: &SessionToken, account: &AccountId) -> Result<(), ApiError>;
}
