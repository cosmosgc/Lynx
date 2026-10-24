//! Minds, controllers, and possession rules.
//!
//! A player is one kind of controller, not a special entity class. Every entity
//! may have a mind, and control transfer is explicit and server-validated.

use std::collections::BTreeSet;

use engine_core::error::{EngineError, ErrorCode};
use engine_core::id::{DefinitionId, StableId};
use engine_core::math::Vec3;
use engine_core::time::Millis;
use serde::{Deserialize, Serialize};

/// What a controller is allowed to do with the controlled entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlCapability {
    /// Request movement.
    Move,
    /// Use interaction verbs.
    Interact,
    /// Attack.
    Attack,
    /// Talk and use chat-like interfaces.
    Speak,
    /// Access the entity's inventory.
    Inventory,
    /// Possess another entity through this one.
    Possess,
    /// Administrative control.
    Admin,
}

/// Who is currently driving an entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Controller {
    /// No active controller; the entity idles.
    None,
    /// A player character driven by an authenticated session.
    Player {
        /// The character principal.
        character: StableId,
        /// Opaque session identifier, for diagnostics and reconnect.
        session: String,
    },
    /// An AI behavior.
    Ai {
        /// Behavior definition driving the entity.
        behavior: DefinitionId,
    },
    /// A deterministic scripted process.
    Scripted {
        /// Script definition.
        script: DefinitionId,
    },
    /// A remote device or terminal controlling the entity.
    Remote {
        /// Controlling device entity.
        device: StableId,
    },
}

impl Controller {
    /// Whether a player session currently controls the entity.
    #[must_use]
    pub fn is_player(&self) -> bool {
        matches!(self, Self::Player { .. })
    }

    /// The player character principal, if any.
    #[must_use]
    pub fn player_character(&self) -> Option<StableId> {
        match self {
            Self::Player { character, .. } => Some(*character),
            _ => None,
        }
    }

    /// A short diagnostic name.
    #[must_use]
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Player { .. } => "player",
            Self::Ai { .. } => "ai",
            Self::Scripted { .. } => "scripted",
            Self::Remote { .. } => "remote",
        }
    }
}

/// Rules for transferring control of an entity.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TransferPolicy {
    /// Control cannot be transferred.
    Forbidden,
    /// Control may be transferred within a range, optionally with consent.
    Allowed {
        /// Maximum horizontal distance in meters.
        max_range_m: f32,
        /// Whether the target entity's controller must consent.
        requires_consent: bool,
    },
    /// Only server/admin processes may transfer control.
    ServerOnly,
}

/// A time-bounded claim that prevents conflicting controllers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlLease {
    /// Entity holding the lease.
    pub holder: StableId,
    /// When the lease stops being exclusive.
    pub expires_at: Millis,
    /// Whether the lease blocks other controllers.
    pub exclusive: bool,
}

impl ControlLease {
    /// Whether the lease blocks `claimant` at `now`.
    #[must_use]
    pub fn blocks(&self, claimant: StableId, now: Millis) -> bool {
        self.exclusive && self.holder != claimant && now < self.expires_at
    }
}

/// The control slot of an entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mind {
    /// Current controller.
    pub controller: Controller,
    /// Capabilities granted to the controller.
    pub capabilities: BTreeSet<ControlCapability>,
    /// Transfer rules.
    pub transfer: TransferPolicy,
    /// Optional exclusive claim.
    pub lease: Option<ControlLease>,
}

impl Mind {
    /// An idle mind with no capabilities.
    #[must_use]
    pub fn idle() -> Self {
        Self {
            controller: Controller::None,
            capabilities: BTreeSet::new(),
            transfer: TransferPolicy::Forbidden,
            lease: None,
        }
    }

    /// An AI-driven mind with standard capabilities.
    #[must_use]
    pub fn ai(behavior: DefinitionId) -> Self {
        Self {
            controller: Controller::Ai { behavior },
            capabilities: BTreeSet::from([
                ControlCapability::Move,
                ControlCapability::Interact,
                ControlCapability::Attack,
                ControlCapability::Speak,
            ]),
            transfer: TransferPolicy::Forbidden,
            lease: None,
        }
    }

    /// A player-driven mind.
    #[must_use]
    pub fn player(character: StableId, session: impl Into<String>) -> Self {
        Self {
            controller: Controller::Player {
                character,
                session: session.into(),
            },
            capabilities: BTreeSet::from([
                ControlCapability::Move,
                ControlCapability::Interact,
                ControlCapability::Attack,
                ControlCapability::Speak,
                ControlCapability::Inventory,
            ]),
            transfer: TransferPolicy::Forbidden,
            lease: None,
        }
    }

    /// Allow possession within `max_range_m`.
    #[must_use]
    pub fn with_possession(mut self, max_range_m: f32, requires_consent: bool) -> Self {
        self.transfer = TransferPolicy::Allowed {
            max_range_m,
            requires_consent,
        };
        self
    }

    /// Grant an exclusive lease until `expires_at`.
    #[must_use]
    pub fn with_lease(mut self, holder: StableId, expires_at: Millis) -> Self {
        self.lease = Some(ControlLease {
            holder,
            expires_at,
            exclusive: true,
        });
        self
    }

    /// Whether the granted capabilities include `capability`.
    #[must_use]
    pub fn can(&self, capability: ControlCapability) -> bool {
        self.capabilities.contains(&capability)
    }
}

/// An accepted possession relationship.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PossessionLink {
    /// Entity being controlled.
    pub controlled: StableId,
    /// Controlling principal.
    pub controller: StableId,
    /// When control started.
    pub since: Millis,
    /// Capabilities granted by the link.
    pub capabilities: BTreeSet<ControlCapability>,
}

/// A validated request to control another entity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PossessionRequest {
    /// Requesting principal (character entity).
    pub actor: StableId,
    /// Actor's world position.
    pub actor_position: Vec3,
    /// Target entity.
    pub target: StableId,
    /// Target's world position.
    pub target_position: Vec3,
    /// Whether the target's current controller consented.
    pub consent: bool,
    /// Whether the request carries admin authority.
    pub admin: bool,
    /// Current time.
    pub now: Millis,
}

/// Validate a possession request against the target's mind.
///
/// # Errors
///
/// - [`ErrorCode::Conflict`] when the target is already player-controlled by
///   someone else or an exclusive lease blocks the claimant.
/// - [`ErrorCode::PermissionDenied`] for forbidden/server-only transfers or
///   missing consent.
/// - [`ErrorCode::OutOfRange`] when the target is beyond the transfer range.
pub fn validate_possession(
    mind: &Mind,
    request: PossessionRequest,
) -> Result<PossessionLink, EngineError> {
    if let Some(character) = mind.controller.player_character()
        && character != request.actor
    {
        return Err(EngineError::new(
            ErrorCode::Conflict,
            "the entity is already controlled by another player",
        )
        .with_detail("controlled_by", character.to_string()));
    }

    if let Some(lease) = &mind.lease
        && lease.blocks(request.actor, request.now)
    {
        return Err(EngineError::new(
            ErrorCode::Conflict,
            "an exclusive control lease blocks this request",
        )
        .with_detail("lease_holder", lease.holder.to_string())
        .with_detail(
            "remaining_ms",
            request.now.remaining_until(lease.expires_at).to_string(),
        ));
    }

    match mind.transfer {
        TransferPolicy::Forbidden => {
            return Err(EngineError::new(
                ErrorCode::PermissionDenied,
                "this entity does not allow control transfer",
            ));
        }
        TransferPolicy::ServerOnly if !request.admin => {
            return Err(EngineError::new(
                ErrorCode::PermissionDenied,
                "only the server may transfer control of this entity",
            ));
        }
        TransferPolicy::Allowed {
            max_range_m,
            requires_consent,
        } => {
            if request.admin && mind.transfer == TransferPolicy::ServerOnly {
                // Admin path is handled above; this arm cannot be reached for
                // ServerOnly, but keeps the match exhaustive.
            }
            let distance = request.actor_position.distance_xz(request.target_position);
            if distance > max_range_m {
                return Err(EngineError::new(
                    ErrorCode::OutOfRange,
                    "the target is too far away to control",
                )
                .with_detail("distance_m", format!("{distance:.2}"))
                .with_detail("max_range_m", format!("{max_range_m:.2}")));
            }
            if requires_consent && !request.consent {
                return Err(EngineError::new(
                    ErrorCode::PermissionDenied,
                    "the target's controller did not consent",
                ));
            }
        }
        TransferPolicy::ServerOnly => {}
    }

    Ok(PossessionLink {
        controlled: request.target,
        controller: request.actor,
        since: request.now,
        capabilities: mind.capabilities.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn character(seed: u128) -> StableId {
        StableId::from_parts(engine_core::id::IdKind::Character, 1, seed)
    }

    fn entity(seed: u128) -> StableId {
        StableId::from_parts(engine_core::id::IdKind::Entity, 1, seed)
    }

    fn request(actor: StableId, target: StableId) -> PossessionRequest {
        PossessionRequest {
            actor,
            actor_position: Vec3::ZERO,
            target,
            target_position: Vec3::new(1.0, 0.0, 0.0),
            consent: false,
            admin: false,
            now: Millis::new(100),
        }
    }

    #[test]
    fn default_minds_forbid_transfer() {
        let mind = Mind::idle();
        let error = validate_possession(&mind, request(character(1), entity(2))).unwrap_err();
        assert_eq!(error.code(), ErrorCode::PermissionDenied);
    }

    #[test]
    fn allowed_transfer_checks_range_and_consent() {
        let mut mind = Mind::idle().with_possession(2.0, true);
        let actor = character(1);
        let target = entity(2);

        let mut req = request(actor, target);
        req.consent = true;
        assert!(validate_possession(&mind, req).is_ok());

        req.consent = false;
        assert_eq!(
            validate_possession(&mind, req).unwrap_err().code(),
            ErrorCode::PermissionDenied
        );

        req.consent = true;
        req.target_position = Vec3::new(10.0, 0.0, 0.0);
        assert_eq!(
            validate_possession(&mind, req).unwrap_err().code(),
            ErrorCode::OutOfRange
        );

        // A ServerOnly transfer cannot be opened by a player-facing toggle.
        mind.transfer = TransferPolicy::ServerOnly;
        assert_eq!(
            validate_possession(&mind, req).unwrap_err().code(),
            ErrorCode::PermissionDenied
        );
        req.admin = true;
        assert!(validate_possession(&mind, req).is_ok());
    }

    #[test]
    fn another_players_control_blocks_possession() {
        let target = entity(2);
        let mind = Mind::player(character(9), "session-a");
        let mut req = request(character(1), target);
        req.admin = true;
        let error = validate_possession(&mind, req).unwrap_err();
        assert_eq!(error.code(), ErrorCode::Conflict);
        assert!(error.details().contains_key("controlled_by"));
    }

    #[test]
    fn exclusive_leases_expire() {
        let actor = character(1);
        let target = entity(2);
        let mind = Mind::idle()
            .with_possession(5.0, false)
            .with_lease(character(7), Millis::new(200));
        let mut req = request(actor, target);
        req.consent = true;

        assert_eq!(
            validate_possession(&mind, req).unwrap_err().code(),
            ErrorCode::Conflict
        );

        req.now = Millis::new(200);
        assert!(validate_possession(&mind, req).is_ok());
    }

    #[test]
    fn possession_grants_the_minds_capabilities() {
        let mut mind = Mind::ai(DefinitionId::new("behavior.wolf").unwrap());
        mind.capabilities = BTreeSet::from([ControlCapability::Move]);
        mind.transfer = TransferPolicy::Allowed {
            max_range_m: 10.0,
            requires_consent: false,
        };
        let link = validate_possession(&mind, request(character(1), entity(2))).unwrap();
        assert_eq!(link.capabilities, BTreeSet::from([ControlCapability::Move]));
        assert_eq!(link.controller, character(1));
    }
}
