//! Faction controllers, roles, membership, and configurable permissions.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::error::{EngineError, ErrorCode};
use crate::id::{IdKind, StableId};
use crate::time::Millis;

/// Default maximum number of members in one faction.
pub const DEFAULT_MAX_MEMBERS: usize = 100;

/// Default per-transaction treasury spend limit for officers.
pub const DEFAULT_OFFICER_SPEND_LIMIT: u64 = 10_000;

/// Who ultimately controls a faction's server-side decisions.
///
/// Controller type changes where authority comes from, not which operations are
/// available. NPC, system, and player factions pass through the same
/// authorization and validation rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactionController {
    /// Directed by authenticated players.
    Player,
    /// Directed by server AI.
    Npc,
    /// Directed by server scripts or operators.
    System,
}

impl FactionController {
    /// The wire and persistence string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Player => "player",
            Self::Npc => "npc",
            Self::System => "system",
        }
    }
}

/// A member's rank inside a faction.
///
/// Exactly one member is the leader at all times. Succession is explicit; a
/// leader is never removed by dropping the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactionRole {
    /// Full authority, including succession and policy changes.
    Leader,
    /// Delegated authority configured by the game.
    Officer,
    /// Baseline membership, usually allowed to deposit but not spend.
    Member,
}

impl FactionRole {
    /// The wire and persistence string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Leader => "leader",
            Self::Officer => "officer",
            Self::Member => "member",
        }
    }
}

/// A capability checked by the server before a faction action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactionPermission {
    /// Add another character or agent to the faction.
    InviteMember,
    /// Remove a non-leader member.
    RemoveMember,
    /// Change a non-leader member's role.
    SetRole,
    /// Transfer leadership to another member.
    TransferLeadership,
    /// Move value into the faction treasury.
    DepositTreasury,
    /// Move value out of the faction treasury.
    SpendTreasury,
    /// Claim an unclaimed territory.
    ClaimTerritory,
    /// Take a territory from another faction.
    CaptureTerritory,
    /// Give up a territory without a new owner.
    ReleaseTerritory,
    /// Declare or join a scheduled faction conflict.
    DeclareConflict,
}

impl FactionPermission {
    /// Every permission in declaration order.
    pub const ALL: [Self; 10] = [
        Self::InviteMember,
        Self::RemoveMember,
        Self::SetRole,
        Self::TransferLeadership,
        Self::DepositTreasury,
        Self::SpendTreasury,
        Self::ClaimTerritory,
        Self::CaptureTerritory,
        Self::ReleaseTerritory,
        Self::DeclareConflict,
    ];

    /// The wire and persistence string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InviteMember => "invite_member",
            Self::RemoveMember => "remove_member",
            Self::SetRole => "set_role",
            Self::TransferLeadership => "transfer_leadership",
            Self::DepositTreasury => "deposit_treasury",
            Self::SpendTreasury => "spend_treasury",
            Self::ClaimTerritory => "claim_territory",
            Self::CaptureTerritory => "capture_territory",
            Self::ReleaseTerritory => "release_territory",
            Self::DeclareConflict => "declare_conflict",
        }
    }
}

/// Data-driven role permissions and treasury limits.
///
/// A missing role mapping grants no permissions. A role that may spend must
/// also declare a positive per-transaction limit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernancePolicy {
    /// Who directs the faction.
    pub controller: FactionController,
    /// Permissions granted to each role.
    pub role_permissions: BTreeMap<FactionRole, BTreeSet<FactionPermission>>,
    /// Maximum value a role may spend in one action.
    pub spend_limits: BTreeMap<FactionRole, u64>,
    /// Hard member cap, including the leader.
    pub max_members: usize,
}

impl GovernancePolicy {
    /// A conservative default: leaders have full authority, officers may
    /// recruit and act within a limited budget, and members may deposit.
    #[must_use]
    pub fn new(controller: FactionController) -> Self {
        let mut role_permissions = BTreeMap::new();
        role_permissions.insert(
            FactionRole::Leader,
            FactionPermission::ALL.into_iter().collect(),
        );
        role_permissions.insert(
            FactionRole::Officer,
            [
                FactionPermission::InviteMember,
                FactionPermission::RemoveMember,
                FactionPermission::DepositTreasury,
                FactionPermission::SpendTreasury,
                FactionPermission::ClaimTerritory,
                FactionPermission::CaptureTerritory,
                FactionPermission::ReleaseTerritory,
            ]
            .into_iter()
            .collect(),
        );
        role_permissions.insert(
            FactionRole::Member,
            [FactionPermission::DepositTreasury].into_iter().collect(),
        );

        let spend_limits = BTreeMap::from([
            (FactionRole::Leader, u64::MAX),
            (FactionRole::Officer, DEFAULT_OFFICER_SPEND_LIMIT),
        ]);

        Self {
            controller,
            role_permissions,
            spend_limits,
            max_members: DEFAULT_MAX_MEMBERS,
        }
    }

    /// Validate the policy before it is persisted or attached to a faction.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] for an empty or excessive member
    /// cap, a leader who cannot transfer leadership, or a spending role without
    /// a positive limit.
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.max_members == 0 || self.max_members > 10_000 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "faction member cap must be between 1 and 10000",
            )
            .with_detail("max_members", self.max_members.to_string()));
        }

        let leader_can_succeed =
            self.role_permissions
                .get(&FactionRole::Leader)
                .is_some_and(|permissions| {
                    permissions.contains(&FactionPermission::TransferLeadership)
                });
        if !leader_can_succeed {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "the leader role must be allowed to transfer leadership",
            ));
        }

        for (role, permissions) in &self.role_permissions {
            if permissions.contains(&FactionPermission::SpendTreasury) {
                match self.spend_limits.get(role) {
                    Some(limit) if *limit > 0 => {}
                    _ => {
                        return Err(EngineError::new(
                            ErrorCode::InvalidRequest,
                            "a role that may spend treasury must declare a positive limit",
                        )
                        .with_detail("role", role.as_str()));
                    }
                }
            }
        }

        for (role, limit) in &self.spend_limits {
            if *limit == 0 {
                return Err(EngineError::new(
                    ErrorCode::InvalidRequest,
                    "faction spend limits must be positive when present",
                )
                .with_detail("role", role.as_str()));
            }
        }

        Ok(())
    }

    /// Whether `role` may perform `permission`.
    #[must_use]
    pub fn allows(&self, role: FactionRole, permission: FactionPermission) -> bool {
        self.role_permissions
            .get(&role)
            .is_some_and(|permissions| permissions.contains(&permission))
    }

    /// The per-transaction spend limit for `role`, when it may spend.
    #[must_use]
    pub fn spend_limit(&self, role: FactionRole) -> Option<u64> {
        self.spend_limits.get(&role).copied()
    }
}

impl Default for GovernancePolicy {
    fn default() -> Self {
        Self::new(FactionController::Player)
    }
}

/// One member of a faction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactionMember {
    /// Character or AI entity identifier.
    pub id: StableId,
    /// Current rank.
    pub role: FactionRole,
    /// When the member joined.
    pub joined_at: Millis,
}

impl FactionMember {
    /// Validate the member subject and role.
    pub fn validate(&self) -> Result<(), EngineError> {
        validate_member_id(self.id)
    }
}

/// Faction membership with an explicit, validated leader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Membership {
    members: BTreeMap<StableId, FactionMember>,
}

impl Membership {
    /// Create a faction with `founder` as its leader.
    pub fn new(founder: StableId, at: Millis) -> Result<Self, EngineError> {
        validate_member_id(founder)?;
        let leader = FactionMember {
            id: founder,
            role: FactionRole::Leader,
            joined_at: at,
        };
        Ok(Self {
            members: BTreeMap::from([(founder, leader)]),
        })
    }

    /// Look up a member.
    #[must_use]
    pub fn get(&self, member: StableId) -> Option<&FactionMember> {
        self.members.get(&member)
    }

    /// The member's role, if any.
    #[must_use]
    pub fn role_of(&self, member: StableId) -> Option<FactionRole> {
        self.members.get(&member).map(|member| member.role)
    }

    /// The current leader.
    #[must_use]
    pub fn leader(&self) -> Option<&FactionMember> {
        self.members
            .values()
            .find(|member| member.role == FactionRole::Leader)
    }

    /// Every member, ordered by stable ID.
    pub fn iter(&self) -> impl Iterator<Item = &FactionMember> {
        self.members.values()
    }

    /// The number of members.
    #[must_use]
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Whether the faction has no members.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// Check that `actor` belongs to the faction and may perform `permission`.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::PermissionDenied`] for a non-member or a role
    /// lacking the permission.
    pub fn authorize(
        &self,
        actor: StableId,
        policy: &GovernancePolicy,
        permission: FactionPermission,
    ) -> Result<FactionRole, EngineError> {
        let role = self.role_of(actor).ok_or_else(|| {
            EngineError::new(ErrorCode::PermissionDenied, "actor is not a faction member")
                .with_detail("actor", actor.to_string())
                .with_detail("permission", permission.as_str())
        })?;
        if !policy.allows(role, permission) {
            return Err(EngineError::new(
                ErrorCode::PermissionDenied,
                "faction role lacks the required permission",
            )
            .with_detail("actor", actor.to_string())
            .with_detail("role", role.as_str())
            .with_detail("permission", permission.as_str()));
        }
        Ok(role)
    }

    /// Check membership, permission, and the per-transaction spend limit.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::PermissionDenied`] when the actor may not spend or
    /// the amount exceeds the role's limit.
    pub fn authorize_spend(
        &self,
        actor: StableId,
        policy: &GovernancePolicy,
        amount: u64,
    ) -> Result<FactionRole, EngineError> {
        let role = self.authorize(actor, policy, FactionPermission::SpendTreasury)?;
        let limit = policy.spend_limit(role).unwrap_or(0);
        if amount > limit {
            return Err(EngineError::new(
                ErrorCode::PermissionDenied,
                "treasury spend exceeds the role's limit",
            )
            .with_detail("actor", actor.to_string())
            .with_detail("role", role.as_str())
            .with_detail("amount", amount.to_string())
            .with_detail("limit", limit.to_string()));
        }
        Ok(role)
    }

    /// Add a member under the caller's `InviteMember` permission.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::PermissionDenied`] when the caller may not invite,
    /// [`ErrorCode::Conflict`] for a duplicate member,
    /// [`ErrorCode::CapacityExceeded`] at the member cap, and
    /// [`ErrorCode::InvalidRequest`] for the leader role or an invalid subject.
    pub fn add(
        &mut self,
        actor: StableId,
        policy: &GovernancePolicy,
        subject: StableId,
        role: FactionRole,
        at: Millis,
    ) -> Result<FactionMember, EngineError> {
        self.authorize(actor, policy, FactionPermission::InviteMember)?;
        validate_member_id(subject)?;
        if role == FactionRole::Leader {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "leadership is transferred explicitly, not granted by invitation",
            ));
        }
        if self.members.contains_key(&subject) {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "subject is already a faction member",
            )
            .with_detail("member", subject.to_string()));
        }
        if self.members.len() >= policy.max_members {
            return Err(EngineError::new(
                ErrorCode::CapacityExceeded,
                "faction member cap reached",
            )
            .with_detail("max_members", policy.max_members.to_string()));
        }
        let member = FactionMember {
            id: subject,
            role,
            joined_at: at,
        };
        self.members.insert(subject, member.clone());
        Ok(member)
    }

    /// Remove a non-leader member under the caller's `RemoveMember` permission.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::PermissionDenied`] when the caller may not remove,
    /// [`ErrorCode::NotFound`] for a missing member, and
    /// [`ErrorCode::InvalidState`] when the target is the leader.
    pub fn remove(
        &mut self,
        actor: StableId,
        policy: &GovernancePolicy,
        subject: StableId,
    ) -> Result<FactionMember, EngineError> {
        self.authorize(actor, policy, FactionPermission::RemoveMember)?;
        let member = self
            .members
            .get(&subject)
            .cloned()
            .ok_or_else(|| EngineError::not_found("faction member", subject))?;
        if member.role == FactionRole::Leader {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "the leader must transfer leadership before leaving or being removed",
            )
            .with_detail("member", subject.to_string()));
        }
        self.members.remove(&subject);
        Ok(member)
    }

    /// Remove the caller from the faction. Leaders must transfer first.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::NotFound`] for a non-member and
    /// [`ErrorCode::InvalidState`] when the caller is the leader.
    pub fn leave(&mut self, actor: StableId) -> Result<FactionMember, EngineError> {
        let member = self
            .members
            .get(&actor)
            .cloned()
            .ok_or_else(|| EngineError::not_found("faction member", actor))?;
        if member.role == FactionRole::Leader {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "the leader must transfer leadership before leaving",
            )
            .with_detail("member", actor.to_string()));
        }
        self.members.remove(&actor);
        Ok(member)
    }

    /// Change a non-leader member's role.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::PermissionDenied`] when the caller may not set
    /// roles, [`ErrorCode::NotFound`] for a missing member, and
    /// [`ErrorCode::InvalidRequest`] when the target role is leader or the
    /// subject is the leader.
    pub fn set_role(
        &mut self,
        actor: StableId,
        policy: &GovernancePolicy,
        subject: StableId,
        role: FactionRole,
    ) -> Result<FactionMember, EngineError> {
        self.authorize(actor, policy, FactionPermission::SetRole)?;
        if role == FactionRole::Leader {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "leadership is transferred explicitly, not assigned as a role",
            ));
        }
        let member = self
            .members
            .get_mut(&subject)
            .ok_or_else(|| EngineError::not_found("faction member", subject))?;
        if member.role == FactionRole::Leader {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "the leader's role cannot be changed directly",
            )
            .with_detail("member", subject.to_string()));
        }
        member.role = role;
        Ok(member.clone())
    }

    /// Transfer leadership from the caller to another current member.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::PermissionDenied`] when the caller is not the
    /// leader or lacks the succession permission, and
    /// [`ErrorCode::NotFound`] for a missing target.
    pub fn transfer_leadership(
        &mut self,
        actor: StableId,
        policy: &GovernancePolicy,
        subject: StableId,
    ) -> Result<(), EngineError> {
        let actor_role = self.authorize(actor, policy, FactionPermission::TransferLeadership)?;
        if actor_role != FactionRole::Leader || actor == subject {
            return Err(EngineError::new(
                ErrorCode::PermissionDenied,
                "only the current leader may transfer leadership to another member",
            )
            .with_detail("actor", actor.to_string())
            .with_detail("member", subject.to_string()));
        }
        if !self.members.contains_key(&subject) {
            return Err(EngineError::not_found("faction member", subject));
        }
        if let Some(previous) = self.members.get_mut(&actor) {
            previous.role = FactionRole::Officer;
        }
        if let Some(next) = self.members.get_mut(&subject) {
            next.role = FactionRole::Leader;
        }
        Ok(())
    }

    /// Validate the durable membership record.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidState`] unless exactly one validated leader
    /// exists.
    pub fn validate(&self) -> Result<(), EngineError> {
        let leaders = self
            .members
            .values()
            .filter(|member| member.role == FactionRole::Leader)
            .count();
        if leaders != 1 {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "a faction must have exactly one leader",
            )
            .with_detail("leaders", leaders.to_string())
            .with_detail("members", self.members.len().to_string()));
        }
        for member in self.members.values() {
            member.validate()?;
        }
        Ok(())
    }
}

fn validate_member_id(id: StableId) -> Result<(), EngineError> {
    if matches!(id.kind(), IdKind::Character | IdKind::Entity) {
        Ok(())
    } else {
        Err(EngineError::new(
            ErrorCode::InvalidRequest,
            "faction members must be characters or AI entities",
        )
        .with_detail("member", id.to_string())
        .with_detail("kind", id.kind().label()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor(seed: u128) -> StableId {
        StableId::from_parts(IdKind::Character, 0, seed)
    }

    fn membership() -> Membership {
        Membership::new(actor(1), Millis::new(0)).unwrap()
    }

    #[test]
    fn default_policy_separates_roles() {
        let policy = GovernancePolicy::default();
        policy.validate().unwrap();
        assert!(policy.allows(FactionRole::Leader, FactionPermission::SpendTreasury));
        assert!(policy.allows(FactionRole::Officer, FactionPermission::SpendTreasury));
        assert!(!policy.allows(FactionRole::Member, FactionPermission::SpendTreasury));
        assert!(policy.allows(FactionRole::Member, FactionPermission::DepositTreasury));
        assert_eq!(
            policy.spend_limit(FactionRole::Officer),
            Some(DEFAULT_OFFICER_SPEND_LIMIT)
        );
    }

    #[test]
    fn spending_roles_require_positive_limits() {
        let mut policy = GovernancePolicy::default();
        policy.spend_limits.insert(FactionRole::Officer, 0);
        assert_eq!(
            policy.validate().unwrap_err().code(),
            ErrorCode::InvalidRequest
        );

        let mut policy = GovernancePolicy::default();
        policy
            .role_permissions
            .get_mut(&FactionRole::Leader)
            .unwrap()
            .remove(&FactionPermission::TransferLeadership);
        assert_eq!(
            policy.validate().unwrap_err().code(),
            ErrorCode::InvalidRequest
        );
    }

    #[test]
    fn membership_requires_exactly_one_leader() {
        let mut members = membership();
        let policy = GovernancePolicy::default();
        members
            .add(
                actor(1),
                &policy,
                actor(2),
                FactionRole::Officer,
                Millis::new(1),
            )
            .unwrap();
        members.validate().unwrap();
        assert_eq!(members.len(), 2);
        assert_eq!(members.leader().unwrap().id, actor(1));

        members
            .transfer_leadership(actor(1), &policy, actor(2))
            .unwrap();
        assert_eq!(members.role_of(actor(1)), Some(FactionRole::Officer));
        assert_eq!(members.role_of(actor(2)), Some(FactionRole::Leader));
        members.validate().unwrap();
    }

    #[test]
    fn leader_cannot_be_removed_or_leave_without_succession() {
        let mut members = membership();
        let policy = GovernancePolicy::default();
        assert_eq!(
            members
                .remove(actor(1), &policy, actor(1))
                .unwrap_err()
                .code(),
            ErrorCode::InvalidState
        );
        assert_eq!(
            members.leave(actor(1)).unwrap_err().code(),
            ErrorCode::InvalidState
        );
    }

    #[test]
    fn invitation_checks_permission_role_duplicates_and_capacity() {
        let mut members = membership();
        let policy = GovernancePolicy {
            max_members: 2,
            ..GovernancePolicy::default()
        };

        assert_eq!(
            members
                .add(
                    actor(9),
                    &policy,
                    actor(2),
                    FactionRole::Member,
                    Millis::new(1)
                )
                .unwrap_err()
                .code(),
            ErrorCode::PermissionDenied
        );
        assert_eq!(
            members
                .add(
                    actor(1),
                    &policy,
                    actor(2),
                    FactionRole::Leader,
                    Millis::new(1)
                )
                .unwrap_err()
                .code(),
            ErrorCode::InvalidRequest
        );
        members
            .add(
                actor(1),
                &policy,
                actor(2),
                FactionRole::Member,
                Millis::new(1),
            )
            .unwrap();
        assert_eq!(
            members
                .add(
                    actor(1),
                    &policy,
                    actor(2),
                    FactionRole::Member,
                    Millis::new(2)
                )
                .unwrap_err()
                .code(),
            ErrorCode::Conflict
        );
        assert_eq!(
            members
                .add(
                    actor(1),
                    &policy,
                    actor(3),
                    FactionRole::Member,
                    Millis::new(2)
                )
                .unwrap_err()
                .code(),
            ErrorCode::CapacityExceeded
        );
    }

    #[test]
    fn officers_can_act_only_within_their_limits() {
        let policy = GovernancePolicy::default();
        let mut members = membership();
        members
            .add(
                actor(1),
                &policy,
                actor(2),
                FactionRole::Officer,
                Millis::new(1),
            )
            .unwrap();
        members
            .authorize_spend(actor(2), &policy, DEFAULT_OFFICER_SPEND_LIMIT)
            .unwrap();
        assert_eq!(
            members
                .authorize_spend(actor(2), &policy, DEFAULT_OFFICER_SPEND_LIMIT + 1)
                .unwrap_err()
                .code(),
            ErrorCode::PermissionDenied
        );
    }

    #[test]
    fn entity_agents_use_the_same_membership_rules() {
        let mut members = membership();
        let policy = GovernancePolicy::default();
        let agent = StableId::from_parts(IdKind::Entity, 0, 7);
        members
            .add(
                actor(1),
                &policy,
                agent,
                FactionRole::Officer,
                Millis::new(1),
            )
            .unwrap();
        assert_eq!(members.role_of(agent), Some(FactionRole::Officer));
        assert!(matches!(
            members.add(
                actor(1),
                &policy,
                StableId::from_parts(IdKind::Item, 0, 8),
                FactionRole::Member,
                Millis::new(2)
            ),
            Err(error) if error.code() == ErrorCode::InvalidRequest
        ));
    }
}
