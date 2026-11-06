//! Nakama-backed social groups used for persistent teams and guild-like play.

use serde::{Deserialize, Serialize};

use crate::service::{AccountId, ApiError, SessionToken};

/// Maximum group listing/member page size.
pub const MAX_GROUP_PAGE: u16 = 100;

/// Stable Nakama group identifier.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct GroupId(String);

impl GroupId {
    /// Validate and wrap a platform group identifier.
    pub fn new(value: impl Into<String>) -> Result<Self, ApiError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 128
            || !value
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        {
            return Err(ApiError::InvalidRequest("invalid group ID".into()));
        }
        Ok(Self(value))
    }

    /// The platform identifier as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for GroupId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for GroupId {
    type Error = ApiError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// A Nakama group roster role or membership request state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupRole {
    /// Group owner; Nakama calls this role superadmin.
    Owner,
    /// Group administrator.
    Admin,
    /// Ordinary group member.
    Member,
    /// A request to join a closed group.
    JoinRequest,
    /// A user banned from the group.
    Banned,
}

/// Public group data returned by Nakama.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocialGroup {
    /// Nakama group identifier.
    pub id: GroupId,
    /// Account that created the group.
    pub creator_id: AccountId,
    /// Unique group name.
    pub name: String,
    /// Group description.
    pub description: String,
    /// Whether any user may join immediately.
    pub open: bool,
    /// Current number of group users.
    pub member_count: u32,
    /// Group membership capacity.
    pub max_count: u32,
}

/// One group visible to the current account and its Nakama role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupMembership {
    /// Public group data.
    pub group: SocialGroup,
    /// Current account's role or join-request state.
    pub role: GroupRole,
}

/// One member or pending join request in a group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupMember {
    /// Member account ID.
    pub account_id: AccountId,
    /// Platform username, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Member role or join-request state.
    pub role: GroupRole,
}

/// Cursor page returned by Nakama.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupPage<T> {
    /// Page records.
    pub items: Vec<T>,
    /// Cursor for the next page, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// Validated input for creating a persistent social group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateGroup {
    /// Unique group name.
    pub name: String,
    /// Description shown in group discovery.
    pub description: String,
    /// Whether anyone can join without approval.
    pub open: bool,
    /// Maximum number of members.
    pub max_count: u32,
}

impl CreateGroup {
    /// Validate user-authored group fields before forwarding them to Nakama.
    pub fn validate(&self) -> Result<(), ApiError> {
        if self.name.trim().is_empty()
            || self.name.len() > 64
            || self.name.chars().any(char::is_control)
        {
            return Err(ApiError::InvalidRequest("invalid group name".into()));
        }
        if self.description.len() > 1000 || self.description.chars().any(char::is_control) {
            return Err(ApiError::InvalidRequest("invalid group description".into()));
        }
        if !(2..=1000).contains(&self.max_count) {
            return Err(ApiError::InvalidRequest(
                "group capacity must be between 2 and 1000".into(),
            ));
        }
        Ok(())
    }
}

/// Authenticated operations on Nakama groups.
pub trait GroupService {
    /// List groups for the caller, including join requests and current role.
    fn my_groups(
        &self,
        token: &SessionToken,
        limit: u16,
        cursor: Option<&str>,
    ) -> Result<GroupPage<GroupMembership>, ApiError>;

    /// List group members or pending join requests.
    fn group_members(
        &self,
        token: &SessionToken,
        group: &GroupId,
        limit: u16,
        cursor: Option<&str>,
    ) -> Result<GroupPage<GroupMember>, ApiError>;

    /// Create a Nakama group with the caller as owner.
    fn create_group(
        &self,
        token: &SessionToken,
        request: &CreateGroup,
    ) -> Result<SocialGroup, ApiError>;

    /// Join an open group or request membership in a closed group.
    fn request_join(&self, token: &SessionToken, group: &GroupId) -> Result<(), ApiError>;

    /// Leave the group.
    fn leave_group(&self, token: &SessionToken, group: &GroupId) -> Result<(), ApiError>;

    /// Add accounts to a group. Nakama enforces the caller's role.
    fn add_members(
        &self,
        token: &SessionToken,
        group: &GroupId,
        accounts: &[AccountId],
    ) -> Result<(), ApiError>;

    /// Remove accounts from a group. Nakama enforces the caller's role.
    fn kick_members(
        &self,
        token: &SessionToken,
        group: &GroupId,
        accounts: &[AccountId],
    ) -> Result<(), ApiError>;

    /// Promote members one Nakama role level.
    fn promote_members(
        &self,
        token: &SessionToken,
        group: &GroupId,
        accounts: &[AccountId],
    ) -> Result<(), ApiError>;

    /// Demote members one Nakama role level.
    fn demote_members(
        &self,
        token: &SessionToken,
        group: &GroupId,
        accounts: &[AccountId],
    ) -> Result<(), ApiError>;
}

/// Validate page bounds and optional cursor size.
pub fn validate_group_page(limit: u16, cursor: Option<&str>) -> Result<(), ApiError> {
    if limit == 0 || limit > MAX_GROUP_PAGE {
        return Err(ApiError::InvalidRequest(format!(
            "group page limit must be between 1 and {MAX_GROUP_PAGE}"
        )));
    }
    if cursor.is_some_and(|value| value.is_empty() || value.len() > 4096) {
        return Err(ApiError::InvalidRequest("invalid group page cursor".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> CreateGroup {
        CreateGroup {
            name: "Explorers".into(),
            description: "World travelers".into(),
            open: false,
            max_count: 50,
        }
    }

    #[test]
    fn group_input_and_pagination_are_bounded() {
        assert!(valid().validate().is_ok());
        let mut request = valid();
        request.name = "  ".into();
        assert!(request.validate().is_err());
        let mut request = valid();
        request.max_count = 1;
        assert!(request.validate().is_err());
        assert!(GroupId::new("group/../admin").is_err());
        assert!(validate_group_page(1, None).is_ok());
        assert!(validate_group_page(MAX_GROUP_PAGE, Some("cursor")).is_ok());
        assert!(validate_group_page(0, None).is_err());
        assert!(validate_group_page(MAX_GROUP_PAGE + 1, None).is_err());
        assert!(validate_group_page(1, Some("x".repeat(4097).as_str())).is_err());
    }
}
