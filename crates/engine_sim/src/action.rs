//! The authoritative action pipeline.
//!
//! Clients and AI controllers send intent. The server validates control,
//! distance, visibility, permission, state, and cooldowns before applying an
//! effect. Errors use stable codes so the UI can present them consistently.

use std::collections::BTreeMap;
use std::fmt;

use engine_core::error::{EngineError, ErrorCode};
use engine_core::id::{DefinitionId, StableId};
use engine_core::inventory::Inventory;
use engine_core::math::Vec3;
use engine_core::time::{Millis, Tick};
use serde::{Deserialize, Serialize};

use crate::entity::{Door, EntityRecord, Health, Interactable, InteractionPermission, UiProvider};
use crate::link::LinkEvent;
use crate::mind::{ControlCapability, Controller};
use crate::minigame::{DigitHint, Minigame, MinigameTurn};
use crate::zone::{ACTION_CACHE_LIMIT, Zone};

/// Cooldown key: actor, verb, and optional target.
pub type ActionKey = (StableId, Verb, Option<StableId>);

/// Longest accepted custom action name, in bytes.
pub const MAX_ACTION_NAME_LEN: usize = 64;

/// Validated inputs for the effect stage of the action pipeline.
struct ActionContext {
    target_id: StableId,
    interactable: Interactable,
    cooldown_key: ActionKey,
    target_prototype: DefinitionId,
    target_tags: Vec<String>,
    target_ui: Option<UiProvider>,
    target_door: Option<Door>,
    target_health: Option<Health>,
    target_inventory: Option<Inventory>,
    target_minigame: Option<Minigame>,
}

/// Data-driven interaction verbs.
///
/// Availability is derived from an entity's components, permissions, and
/// state; it is never assumed for every target.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verb {
    /// Read descriptive state.
    Inspect,
    /// Generic use, usually opening an interface.
    Use,
    /// Open a door, container, or interface.
    Open,
    /// Close an open object.
    Close,
    /// Take an item.
    Take,
    /// Insert an item.
    Insert,
    /// Equip an item.
    Equip,
    /// Start a conversation.
    Talk,
    /// Repair an object.
    Repair,
    /// Activate a mechanism.
    Activate,
    /// Attempt unauthorized access.
    Hack,
    /// Play one turn of an entity-provided minigame.
    Play,
    /// Attack a target.
    Attack,
    /// Game-specific verb introduced by content.
    Custom(String),
}

impl Verb {
    /// Wire string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Inspect => "inspect",
            Self::Use => "use",
            Self::Open => "open",
            Self::Close => "close",
            Self::Take => "take",
            Self::Insert => "insert",
            Self::Equip => "equip",
            Self::Talk => "talk",
            Self::Repair => "repair",
            Self::Activate => "activate",
            Self::Hack => "hack",
            Self::Play => "play",
            Self::Attack => "attack",
            Self::Custom(name) => name,
        }
    }

    /// Parse a wire or panel action name.
    ///
    /// Known engine verbs map to their variants. Other compact identifiers
    /// become [`Verb::Custom`] and are rejected by the simulation until a
    /// content system implements them. Empty, oversized, whitespace, or
    /// control-character names are rejected outright.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let verb = match value {
            "inspect" => Self::Inspect,
            "use" => Self::Use,
            "open" => Self::Open,
            "close" => Self::Close,
            "take" => Self::Take,
            "insert" => Self::Insert,
            "equip" => Self::Equip,
            "talk" => Self::Talk,
            "repair" => Self::Repair,
            "activate" => Self::Activate,
            "hack" => Self::Hack,
            "play" => Self::Play,
            "attack" => Self::Attack,
            _ => {
                if value.is_empty()
                    || value.len() > MAX_ACTION_NAME_LEN
                    || value
                        .chars()
                        .any(|ch| ch.is_whitespace() || ch.is_control())
                {
                    return None;
                }
                Self::Custom(value.to_string())
            }
        };
        Some(verb)
    }
}

impl fmt::Display for Verb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A client- or AI-issued request to act on a target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionRequest {
    /// Sender-generated ID used for deduplication and replies.
    pub request_id: String,
    /// Acting entity, resolved by the gateway from the session.
    pub actor: StableId,
    /// Target entity, when the verb requires one.
    pub target: Option<StableId>,
    /// Requested verb.
    pub verb: Verb,
    /// Typed string parameters, validated per verb.
    pub params: BTreeMap<String, String>,
    /// Client's last known server tick, for diagnostics and prediction.
    pub client_tick: Tick,
    /// Server-resolved principal. Clients cannot set this to another player.
    pub principal: Option<StableId>,
}

impl ActionRequest {
    /// Start a request.
    #[must_use]
    pub fn new(request_id: impl Into<String>, actor: StableId, verb: Verb) -> Self {
        Self {
            request_id: request_id.into(),
            actor,
            target: None,
            verb,
            params: BTreeMap::new(),
            client_tick: Tick::ZERO,
            principal: None,
        }
    }

    /// Set the target.
    #[must_use]
    pub fn on(mut self, target: StableId) -> Self {
        self.target = Some(target);
        self
    }

    /// Add a parameter.
    #[must_use]
    pub fn with_param(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.params.insert(key.into(), value.into());
        self
    }

    /// Record the authenticated principal.
    #[must_use]
    pub fn by(mut self, principal: StableId) -> Self {
        self.principal = Some(principal);
        self
    }

    /// Record the client's known tick.
    #[must_use]
    pub const fn at_tick(mut self, tick: Tick) -> Self {
        self.client_tick = tick;
        self
    }
}

/// A validated state change produced by an action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActionEffect {
    /// Descriptive state was read.
    Inspected {
        /// Target entity.
        target: StableId,
        /// Safe summary to display.
        summary: String,
    },
    /// A door changed state.
    DoorToggled {
        /// Door entity.
        target: StableId,
        /// New open state.
        open: bool,
    },
    /// An entity-provided interface should open.
    UiOpened {
        /// Providing entity.
        target: StableId,
        /// Panel schema.
        schema: DefinitionId,
        /// Panel title.
        title: String,
    },
    /// An entity-provided interface should close.
    UiClosed {
        /// Providing entity.
        target: StableId,
    },
    /// A minigame attempt started or was refreshed.
    MinigameStarted {
        /// Providing entity.
        target: StableId,
        /// Content definition.
        definition: DefinitionId,
        /// Combination length in digits.
        length: u8,
        /// Remaining attempts.
        attempts_left: u8,
        /// Whether the player already solved it.
        solved: bool,
        /// Tick before which no guess is accepted, after a lockout.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_after_tick: Option<Tick>,
    },
    /// One minigame guess was accepted.
    MinigameAttempt {
        /// Providing entity.
        target: StableId,
        /// Per-digit comparison, in combination order.
        hints: Vec<DigitHint>,
        /// Remaining attempts.
        attempts_left: u8,
        /// Whether this guess solved the challenge.
        solved: bool,
    },
    /// A minigame was solved.
    MinigameSolved {
        /// Providing entity.
        target: StableId,
    },
    /// A minigame exhausted its attempt budget.
    MinigameFailed {
        /// Providing entity.
        target: StableId,
        /// Tick before which no new guess is accepted.
        retry_after_tick: Tick,
    },
    /// A door was unlocked.
    DoorUnlocked {
        /// Door entity.
        target: StableId,
    },
    /// Items moved between inventories.
    ItemTaken {
        /// Source inventory owner.
        source: StableId,
        /// Destination inventory owner.
        destination: StableId,
        /// Item definition.
        definition: DefinitionId,
        /// Quantity moved.
        quantity: u32,
    },
    /// A target lost hit points.
    Damaged {
        /// Target entity.
        target: StableId,
        /// Damage actually applied.
        amount: i32,
        /// Remaining hit points.
        remaining: i32,
    },
    /// A target died.
    EntityDied {
        /// Dead entity.
        target: StableId,
    },
    /// A validated link fired.
    Activated {
        /// Emitting entity.
        source: StableId,
        /// Receiving entity.
        target: StableId,
        /// Link type.
        link_type: DefinitionId,
    },
}

/// Whether an action applied or was rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ActionOutcome {
    /// The action was applied and produced effects.
    Applied {
        /// Ordered effects for replication and UI.
        effects: Vec<ActionEffect>,
    },
    /// The action failed validation.
    Rejected {
        /// Structured failure.
        error: EngineError,
    },
}

/// The complete server response to an action request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionResult {
    /// Echo of the request ID.
    pub request_id: String,
    /// Acting entity.
    pub actor: StableId,
    /// Requested verb.
    pub verb: Verb,
    /// Server tick that processed the request.
    pub server_tick: Tick,
    /// Outcome.
    pub outcome: ActionOutcome,
}

impl ActionResult {
    /// Whether the action was applied.
    #[must_use]
    pub fn is_applied(&self) -> bool {
        matches!(self.outcome, ActionOutcome::Applied { .. })
    }

    /// The applied effects, or an empty slice.
    #[must_use]
    pub fn effects(&self) -> &[ActionEffect] {
        match &self.outcome {
            ActionOutcome::Applied { effects } => effects,
            ActionOutcome::Rejected { .. } => &[],
        }
    }

    /// The rejection, if any.
    #[must_use]
    pub fn error(&self) -> Option<&EngineError> {
        match &self.outcome {
            ActionOutcome::Rejected { error } => Some(error),
            ActionOutcome::Applied { .. } => None,
        }
    }
}

impl Zone {
    /// Validate and apply an action.
    ///
    /// A repeated `request_id` returns the original result and never applies
    /// the action twice.
    pub fn apply_action(&mut self, request: &ActionRequest, _now: Millis) -> ActionResult {
        if let Some(previous) = self.action_results.get(&request.request_id) {
            return previous.clone();
        }

        let outcome = match self.apply_action_inner(request) {
            Ok(effects) => {
                self.stats.applied_actions = self.stats.applied_actions.saturating_add(1);
                ActionOutcome::Applied { effects }
            }
            Err(error) => ActionOutcome::Rejected { error },
        };
        let result = ActionResult {
            request_id: request.request_id.clone(),
            actor: request.actor,
            verb: request.verb.clone(),
            server_tick: self.tick,
            outcome,
        };
        self.remember_action(&result);
        result
    }

    fn remember_action(&mut self, result: &ActionResult) {
        self.action_results
            .insert(result.request_id.clone(), result.clone());
        self.action_order.push_back(result.request_id.clone());
        while self.action_order.len() > ACTION_CACHE_LIMIT {
            if let Some(oldest) = self.action_order.pop_front() {
                self.action_results.remove(&oldest);
            }
        }
    }

    fn apply_action_inner(
        &mut self,
        request: &ActionRequest,
    ) -> Result<Vec<ActionEffect>, EngineError> {
        let context = self.validate_action(request)?;

        let effects = match &request.verb {
            Verb::Inspect => {
                let summary = context.target_ui.as_ref().map_or_else(
                    || {
                        if context.target_tags.is_empty() {
                            format!("{}", context.target_prototype)
                        } else {
                            format!(
                                "{} [{}]",
                                context.target_prototype,
                                context.target_tags.join(", ")
                            )
                        }
                    },
                    |ui| format!("{}: {}", ui.title, ui.schema),
                );
                vec![ActionEffect::Inspected {
                    target: context.target_id,
                    summary,
                }]
            }
            Verb::Open | Verb::Close | Verb::Activate | Verb::Use => self.apply_open_like(
                request,
                context.target_id,
                context.target_door,
                context.target_ui,
                context.target_minigame.is_some(),
            )?,
            Verb::Talk => {
                let ui = context.target_ui.ok_or_else(|| {
                    EngineError::new(ErrorCode::Unsupported, "target has no dialogue interface")
                        .with_detail("target", context.target_id.to_string())
                })?;
                vec![ActionEffect::UiOpened {
                    target: context.target_id,
                    schema: ui.schema,
                    title: ui.title,
                }]
            }
            Verb::Take => self.apply_take(request, context.target_id, context.target_inventory)?,
            Verb::Play => self.apply_play(request, context.target_id)?,
            Verb::Attack => self.apply_attack(request, context.target_id, context.target_health)?,
            Verb::Insert | Verb::Equip | Verb::Repair | Verb::Hack | Verb::Custom(_) => {
                return Err(EngineError::new(
                    ErrorCode::Unsupported,
                    "verb is not implemented by this engine build",
                )
                .with_detail("verb", request.verb.as_str())
                .with_detail("target", context.target_id.to_string()));
            }
        };

        if context.interactable.cooldown_ticks > 0 {
            let until = Tick(
                self.tick
                    .0
                    .saturating_add(context.interactable.cooldown_ticks),
            );
            self.cooldowns.insert(context.cooldown_key, until);
        }
        Ok(effects)
    }

    /// Validate the acting entity, its controller, and its capabilities.
    fn validate_actor(&self, request: &ActionRequest) -> Result<(StableId, Vec3), EngineError> {
        if request.request_id.is_empty() {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "an action requires a request id",
            ));
        }
        let actor_id = request.actor;
        let actor = self
            .entities
            .get(&actor_id)
            .ok_or_else(|| EngineError::not_found("actor", actor_id))?;
        authorize_actor(actor, request.principal)?;
        if !actor.is_alive() {
            return Err(
                EngineError::new(ErrorCode::InvalidState, "a dead entity cannot act")
                    .with_detail("entity", actor_id.to_string()),
            );
        }
        let actor_position = actor.position();
        let mind = actor
            .mind
            .as_ref()
            .ok_or_else(|| EngineError::new(ErrorCode::PermissionDenied, "entity has no mind"))?;
        let capability = required_capability(&request.verb);
        if !mind.can(capability) {
            return Err(EngineError::new(
                ErrorCode::PermissionDenied,
                "the controller lacks the required capability",
            )
            .with_detail("verb", request.verb.as_str())
            .with_detail("capability", format!("{capability:?}")));
        }
        Ok((actor_id, actor_position))
    }

    /// Validate the target and build the owned context the effect stage needs.
    fn validate_action(&self, request: &ActionRequest) -> Result<ActionContext, EngineError> {
        let (actor_id, actor_position) = self.validate_actor(request)?;
        let target_id = request.target.ok_or_else(|| {
            EngineError::new(ErrorCode::InvalidRequest, "this verb requires a target")
                .with_detail("verb", request.verb.as_str())
        })?;
        if target_id == actor_id {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "an entity cannot target itself",
            ));
        }
        let target = self
            .entities
            .get(&target_id)
            .ok_or_else(|| EngineError::not_found("target", target_id))?;
        let interactable = target.interactable.clone().ok_or_else(|| {
            EngineError::new(ErrorCode::InvalidState, "target is not interactable")
                .with_detail("target", target_id.to_string())
        })?;
        if !interactable.enabled {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "target interaction is disabled",
            )
            .with_detail("target", target_id.to_string()));
        }
        if !interactable.supports(&request.verb) {
            return Err(EngineError::new(
                ErrorCode::Unsupported,
                "target does not support this verb",
            )
            .with_detail("target", target_id.to_string())
            .with_detail("verb", request.verb.as_str())
            .with_detail(
                "available",
                interactable
                    .verbs
                    .iter()
                    .map(Verb::as_str)
                    .collect::<Vec<_>>()
                    .join(","),
            ));
        }

        let distance = actor_position.distance_xz(target.position());
        if distance > interactable.range_m {
            return Err(EngineError::new(
                ErrorCode::OutOfRange,
                "target is out of interaction range",
            )
            .with_detail("distance_m", format!("{distance:.2}"))
            .with_detail("max_range_m", format!("{:.2}", interactable.range_m)));
        }
        if interactable.requires_line_of_sight
            && !self.nav.line_of_sight(actor_position, target.position())
        {
            return Err(EngineError::new(
                ErrorCode::NotVisible,
                "target is not visible from the actor's position",
            )
            .with_detail("target", target_id.to_string()));
        }
        authorize_target(
            interactable.permission,
            target.owner,
            request.principal,
            actor_id,
        )?;

        let cooldown_key = (actor_id, request.verb.clone(), Some(target_id));
        if let Some(until) = self.cooldowns.get(&cooldown_key)
            && self.tick < *until
        {
            return Err(
                EngineError::new(ErrorCode::Cooldown, "action is on cooldown").with_detail(
                    "remaining_ticks",
                    until.elapsed_since(self.tick).to_string(),
                ),
            );
        }

        Ok(ActionContext {
            target_id,
            interactable,
            cooldown_key,
            target_prototype: target.prototype.clone(),
            target_tags: target.tags.iter().cloned().collect(),
            target_ui: target.ui.clone(),
            target_door: target.door.clone(),
            target_health: target.health,
            target_inventory: target.inventory.clone(),
            target_minigame: target.minigame.clone(),
        })
    }

    fn apply_open_like(
        &mut self,
        request: &ActionRequest,
        target_id: StableId,
        door: Option<crate::entity::Door>,
        ui: Option<crate::entity::UiProvider>,
        has_minigame: bool,
    ) -> Result<Vec<ActionEffect>, EngineError> {
        match (&request.verb, door) {
            (Verb::Open | Verb::Close | Verb::Activate | Verb::Use, Some(door)) => {
                if door.locked {
                    if matches!(request.verb, Verb::Use | Verb::Activate)
                        && has_minigame
                        && let Some(ui) = ui
                    {
                        return self.open_minigame(request, target_id, ui);
                    }
                    return Err(
                        EngineError::new(ErrorCode::InvalidState, "the door is locked")
                            .with_detail("target", target_id.to_string()),
                    );
                }
                let open = match request.verb {
                    Verb::Open => true,
                    Verb::Close => false,
                    _ => !door.open,
                };
                if let Some(entity) = self.entities.get_mut(&target_id)
                    && let Some(state) = entity.door.as_mut()
                {
                    state.open = open;
                }
                Ok(vec![ActionEffect::DoorToggled {
                    target: target_id,
                    open,
                }])
            }
            (Verb::Use | Verb::Activate, None) => {
                if let Some(ui) = ui {
                    if has_minigame {
                        return self.open_minigame(request, target_id, ui);
                    }
                    return Ok(vec![ActionEffect::UiOpened {
                        target: target_id,
                        schema: ui.schema,
                        title: ui.title,
                    }]);
                }
                if request.verb == Verb::Activate
                    && !self
                        .links
                        .targets(target_id, LinkEvent::Activated)
                        .is_empty()
                {
                    let link_request = format!("{}:links", request.request_id);
                    return self.activate_links(&link_request, target_id, LinkEvent::Activated);
                }
                Err(
                    EngineError::new(ErrorCode::Unsupported, "target has no openable state")
                        .with_detail("target", target_id.to_string()),
                )
            }
            _ => Err(EngineError::new(
                ErrorCode::Unsupported,
                "target does not support this operation",
            )
            .with_detail("target", target_id.to_string())
            .with_detail("verb", request.verb.as_str())),
        }
    }

    /// Open a minigame panel and report the player's current status.
    fn open_minigame(
        &mut self,
        request: &ActionRequest,
        target_id: StableId,
        ui: UiProvider,
    ) -> Result<Vec<ActionEffect>, EngineError> {
        let player = request.principal.unwrap_or(request.actor);
        let tick = self.tick;
        let entity = self
            .entities
            .get_mut(&target_id)
            .ok_or_else(|| EngineError::not_found("target", target_id))?;
        let minigame = entity.minigame.as_mut().ok_or_else(|| {
            EngineError::new(ErrorCode::InvalidState, "target has no minigame")
                .with_detail("target", target_id.to_string())
        })?;
        let status = minigame.status(player, tick);
        Ok(vec![
            ActionEffect::UiOpened {
                target: target_id,
                schema: ui.schema,
                title: ui.title,
            },
            ActionEffect::MinigameStarted {
                target: target_id,
                definition: status.definition,
                length: status.length,
                attempts_left: status.attempts_left,
                solved: status.solved,
                retry_after_tick: status.retry_after_tick,
            },
        ])
    }

    /// Apply one guess to a minigame.
    fn apply_play(
        &mut self,
        request: &ActionRequest,
        target_id: StableId,
    ) -> Result<Vec<ActionEffect>, EngineError> {
        let player = request.principal.unwrap_or(request.actor);
        let guess = request.params.get("guess").ok_or_else(|| {
            EngineError::new(ErrorCode::InvalidRequest, "play requires a guess")
                .with_detail("parameter", "guess")
        })?;
        let tick = self.tick;
        let entity = self
            .entities
            .get_mut(&target_id)
            .ok_or_else(|| EngineError::not_found("target", target_id))?;
        let minigame = entity.minigame.as_mut().ok_or_else(|| {
            EngineError::new(ErrorCode::InvalidState, "target has no minigame")
                .with_detail("target", target_id.to_string())
        })?;
        let turn = minigame.play(player, guess, tick)?;
        let MinigameTurn {
            hints,
            attempts_left,
            solved,
            retry_after_tick,
        } = turn;

        let mut effects = vec![ActionEffect::MinigameAttempt {
            target: target_id,
            hints,
            attempts_left,
            solved,
        }];
        if solved {
            effects.push(ActionEffect::MinigameSolved { target: target_id });
            if let Some(door) = entity.door.as_mut()
                && door.locked
            {
                door.locked = false;
                effects.push(ActionEffect::DoorUnlocked { target: target_id });
            }
            effects.push(ActionEffect::UiClosed { target: target_id });
        } else if let Some(retry_after_tick) = retry_after_tick {
            effects.push(ActionEffect::MinigameFailed {
                target: target_id,
                retry_after_tick,
            });
        }
        Ok(effects)
    }

    fn apply_take(
        &mut self,
        request: &ActionRequest,
        target_id: StableId,
        target_inventory: Option<Inventory>,
    ) -> Result<Vec<ActionEffect>, EngineError> {
        let slot = param_usize(request, "slot", 0)?;
        let quantity = param_u32(request, "quantity", 1)?;
        let mut source = target_inventory.ok_or_else(|| {
            EngineError::new(ErrorCode::InvalidState, "target has no inventory")
                .with_detail("target", target_id.to_string())
        })?;
        let mut destination = self
            .entities
            .get(&request.actor)
            .and_then(|entity| entity.inventory.clone())
            .ok_or_else(|| {
                EngineError::new(ErrorCode::InvalidState, "actor has no inventory")
                    .with_detail("actor", request.actor.to_string())
            })?;

        let key = format!("action:{}", request.request_id);
        let receipt = self.transfers.transfer(
            &key,
            &mut source,
            target_id,
            slot,
            &mut destination,
            request.actor,
            quantity,
            self.rules.as_ref(),
        )?;

        if let Some(entity) = self.entities.get_mut(&target_id) {
            entity.inventory = Some(source);
        }
        if let Some(entity) = self.entities.get_mut(&request.actor) {
            entity.inventory = Some(destination);
        }

        Ok(vec![ActionEffect::ItemTaken {
            source: target_id,
            destination: request.actor,
            definition: receipt.definition,
            quantity: receipt.quantity,
        }])
    }

    fn apply_attack(
        &mut self,
        request: &ActionRequest,
        target_id: StableId,
        health: Option<crate::entity::Health>,
    ) -> Result<Vec<ActionEffect>, EngineError> {
        let damage = param_u32(request, "damage", 1)?;
        if damage == 0 || damage > 1_000 {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "damage must be between 1 and 1000",
            ));
        }
        if health.is_none() {
            return Err(
                EngineError::new(ErrorCode::InvalidState, "target has no health")
                    .with_detail("target", target_id.to_string()),
            );
        }

        let mut effects = Vec::new();
        let mut died = false;
        if let Some(target) = self.entities.get_mut(&target_id) {
            if let Some(health) = target.health.as_mut() {
                let applied = health.damage(damage.cast_signed());
                effects.push(ActionEffect::Damaged {
                    target: target_id,
                    amount: applied,
                    remaining: health.current,
                });
                died = !health.is_alive();
            }
            if died {
                if let Some(mind) = target.mind.as_mut() {
                    mind.controller = Controller::None;
                    mind.lease = None;
                }
                effects.push(ActionEffect::EntityDied { target: target_id });
            }
        }
        Ok(effects)
    }
}

fn authorize_actor(actor: &EntityRecord, principal: Option<StableId>) -> Result<(), EngineError> {
    let mind = actor.mind.as_ref().ok_or_else(|| {
        EngineError::new(ErrorCode::PermissionDenied, "entity has no controller")
            .with_detail("entity", actor.id.to_string())
    })?;
    match &mind.controller {
        Controller::None => Err(EngineError::new(
            ErrorCode::InvalidState,
            "entity has no active controller",
        )
        .with_detail("entity", actor.id.to_string())),
        Controller::Player { character, .. } => {
            if principal == Some(*character) {
                Ok(())
            } else {
                Err(EngineError::new(
                    ErrorCode::PermissionDenied,
                    "principal does not control this character",
                )
                .with_detail("entity", actor.id.to_string()))
            }
        }
        Controller::Ai { .. } | Controller::Scripted { .. } | Controller::Remote { .. } => {
            if principal.is_some() {
                Err(EngineError::new(
                    ErrorCode::PermissionDenied,
                    "a client cannot drive a server-controlled entity",
                )
                .with_detail("entity", actor.id.to_string()))
            } else {
                Ok(())
            }
        }
    }
}

fn authorize_target(
    permission: InteractionPermission,
    owner: Option<StableId>,
    principal: Option<StableId>,
    actor: StableId,
) -> Result<(), EngineError> {
    let owns = owner.is_some() && (owner == Some(actor) || owner == principal);
    match permission {
        InteractionPermission::Public => Ok(()),
        InteractionPermission::Owner | InteractionPermission::OwnerOrPublic if owns => Ok(()),
        InteractionPermission::OwnerOrPublic if owner.is_none() => Ok(()),
        InteractionPermission::Owner
        | InteractionPermission::OwnerOrPublic
        | InteractionPermission::ServerOnly => Err(EngineError::new(
            ErrorCode::PermissionDenied,
            "actor does not have permission to interact with the target",
        )
        .with_detail(
            "owner",
            owner.map_or_else(|| "none".into(), |id| id.to_string()),
        )),
    }
}

fn required_capability(verb: &Verb) -> ControlCapability {
    match verb {
        Verb::Attack => ControlCapability::Attack,
        Verb::Talk => ControlCapability::Speak,
        Verb::Take | Verb::Insert | Verb::Equip => ControlCapability::Inventory,
        Verb::Inspect
        | Verb::Use
        | Verb::Open
        | Verb::Close
        | Verb::Repair
        | Verb::Activate
        | Verb::Hack
        | Verb::Play
        | Verb::Custom(_) => ControlCapability::Interact,
    }
}

fn param_u32(request: &ActionRequest, key: &str, default: u32) -> Result<u32, EngineError> {
    match request.params.get(key) {
        None => Ok(default),
        Some(value) => value.parse::<u32>().map_err(|_| {
            EngineError::new(
                ErrorCode::InvalidRequest,
                "parameter must be an unsigned integer",
            )
            .with_detail("parameter", key)
            .with_detail("value", value)
        }),
    }
}

fn param_usize(request: &ActionRequest, key: &str, default: usize) -> Result<usize, EngineError> {
    match request.params.get(key) {
        None => Ok(default),
        Some(value) => value.parse::<usize>().map_err(|_| {
            EngineError::new(ErrorCode::InvalidRequest, "parameter must be an index")
                .with_detail("parameter", key)
                .with_detail("value", value)
        }),
    }
}

/// Distance helper used by tests and tooling.
#[must_use]
pub fn distance_between(a: Vec3, b: Vec3) -> f32 {
    a.distance_xz(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verb_names_round_trip_and_custom_names_are_bounded() {
        for verb in [
            Verb::Inspect,
            Verb::Use,
            Verb::Open,
            Verb::Close,
            Verb::Take,
            Verb::Insert,
            Verb::Equip,
            Verb::Talk,
            Verb::Repair,
            Verb::Activate,
            Verb::Hack,
            Verb::Play,
            Verb::Attack,
        ] {
            assert_eq!(Verb::parse(verb.as_str()), Some(verb));
        }
        assert_eq!(
            Verb::parse("dance"),
            Some(Verb::Custom("dance".to_string()))
        );
        assert_eq!(Verb::parse(""), None);
        assert_eq!(Verb::parse("two words"), None);
        assert_eq!(Verb::parse(&"x".repeat(MAX_ACTION_NAME_LEN + 1)), None);
    }
}
