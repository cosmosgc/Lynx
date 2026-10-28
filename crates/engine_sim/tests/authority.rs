//! End-to-end authority tests for the headless zone simulation.
//!
//! These tests prove that the server, not the caller, decides range,
//! visibility, permission, control, cooldowns, and item conservation.

use std::sync::Arc;

use engine_core::error::ErrorCode;
use engine_core::faction::{FactionCatalog, Thresholds};
use engine_core::id::{DefinitionId, IdKind, StableId};
use engine_core::inventory::{Inventory, ItemStack, UniformItemRules};
use engine_core::math::Vec3;
use engine_core::time::Millis;
use engine_sim::action::{ActionEffect, ActionRequest, Verb};
use engine_sim::entity::{Door, EntityRecord, Interactable, InteractionPermission, UiProvider};
use engine_sim::link::{EntityLink, LinkEvent};
use engine_sim::mind::Mind;
use engine_sim::minigame::{DigitHint, Minigame};
use engine_sim::nav::NavGrid;
use engine_sim::zone::{Zone, ZoneConfig};

fn text(value: &str) -> DefinitionId {
    DefinitionId::new(value).unwrap()
}

fn entity(seed: u128) -> StableId {
    StableId::from_parts(IdKind::Entity, 1, seed)
}

fn character(seed: u128) -> StableId {
    StableId::from_parts(IdKind::Character, 1, seed)
}

fn test_zone() -> Zone {
    let config = ZoneConfig::new(
        text("zone.test_room"),
        StableId::from_parts(IdKind::Zone, 1, 1),
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(20.0, 4.0, 20.0),
    )
    .unwrap();
    let nav = NavGrid::new(20, 20, 1.0, Vec3::ZERO).unwrap();
    Zone::new(config, nav, Arc::new(UniformItemRules::new(10))).unwrap()
}

fn player_at(zone: &mut Zone, seed: u128, position: Vec3, capacity: usize) -> (StableId, StableId) {
    let actor = entity(seed);
    let principal = character(seed);
    zone.spawn(
        EntityRecord::new(actor, text("character.player"), position)
            .with_health(50)
            .with_mind(Mind::player(principal, "session-1"))
            .with_inventory(Inventory::with_capacity(capacity).unwrap())
            .with_interactable(Interactable::new(3.0, std::iter::empty()).without_line_of_sight()),
    )
    .unwrap();
    (actor, principal)
}

fn door_at(zone: &mut Zone, seed: u128, position: Vec3, verbs: &[Verb]) -> StableId {
    let door = entity(seed);
    zone.spawn(
        EntityRecord::new(door, text("prop.door"), position)
            .with_interactable(Interactable::new(3.0, verbs.iter().cloned()))
            .with_door(Door::new()),
    )
    .unwrap();
    door
}

fn action(actor: StableId, principal: StableId, target: StableId, verb: Verb) -> ActionRequest {
    ActionRequest::new(format!("req-{target}-{verb}"), actor, verb)
        .on(target)
        .by(principal)
        .at_tick(engine_core::time::Tick::ZERO)
}

fn vault_at(zone: &mut Zone, seed: u128, position: Vec3) -> StableId {
    let vault = entity(seed);
    zone.spawn(
        EntityRecord::new(vault, text("prop.vault"), position)
            .with_interactable(
                Interactable::new(3.0, [Verb::Use, Verb::Play, Verb::Open]).without_line_of_sight(),
            )
            .with_door(Door {
                open: false,
                locked: true,
                key: None,
            })
            .with_ui(UiProvider::new(text("ui.lockpick"), "Vault Lock"))
            .with_minigame(Minigame::new(text("minigame.lockpick"), 3, 4, 5, 42).unwrap()),
    )
    .unwrap();
    vault
}

fn secret(zone: &Zone, vault: StableId, principal: StableId) -> String {
    zone.entity(vault)
        .unwrap()
        .minigame
        .as_ref()
        .unwrap()
        .secret_for(principal)
        .iter()
        .map(|digit| char::from(b'0' + digit))
        .collect()
}

fn play(
    actor: StableId,
    principal: StableId,
    vault: StableId,
    request_id: &str,
    guess: &str,
) -> ActionRequest {
    ActionRequest::new(request_id, actor, Verb::Play)
        .on(vault)
        .by(principal)
        .with_param("guess", guess)
}

#[test]
fn inspect_succeeds_in_range() {
    let mut zone = test_zone();
    let (actor, principal) = player_at(&mut zone, 1, Vec3::ZERO, 2);
    let terminal = entity(2);
    zone.spawn(
        EntityRecord::new(terminal, text("prop.terminal"), Vec3::new(2.0, 0.0, 0.0))
            .with_interactable(Interactable::new(
                3.0,
                [Verb::Inspect, Verb::Use, Verb::Talk],
            ))
            .with_ui(UiProvider::new(text("ui.terminal"), "Terminal")),
    )
    .unwrap();

    let result = zone.apply_action(
        &action(actor, principal, terminal, Verb::Inspect),
        Millis::new(1),
    );
    assert!(result.is_applied());
    assert!(matches!(
        result.effects().first(),
        Some(ActionEffect::Inspected { target, .. }) if *target == terminal
    ));
}

#[test]
fn out_of_range_is_rejected() {
    let mut zone = test_zone();
    let (actor, principal) = player_at(&mut zone, 1, Vec3::ZERO, 2);
    let door = door_at(&mut zone, 2, Vec3::new(10.0, 0.0, 0.0), &[Verb::Open]);

    let result = zone.apply_action(&action(actor, principal, door, Verb::Open), Millis::new(1));
    let error = result.error().unwrap();
    assert_eq!(error.code(), ErrorCode::OutOfRange);
    assert!(error.details().contains_key("distance_m"));
    assert!(!zone.entity(door).unwrap().door.as_ref().unwrap().open);
}

#[test]
fn a_principal_cannot_drive_another_players_character() {
    let mut zone = test_zone();
    let (actor, _principal) = player_at(&mut zone, 1, Vec3::ZERO, 2);
    let door = door_at(&mut zone, 2, Vec3::new(2.0, 0.0, 0.0), &[Verb::Open]);
    let attacker_principal = character(99);

    let result = zone.apply_action(
        &action(actor, attacker_principal, door, Verb::Open),
        Millis::new(1),
    );
    assert_eq!(result.error().unwrap().code(), ErrorCode::PermissionDenied);
    assert!(!zone.entity(door).unwrap().door.as_ref().unwrap().open);
}

#[test]
fn owners_only_doors_reject_strangers() {
    let mut zone = test_zone();
    let (actor, principal) = player_at(&mut zone, 1, Vec3::ZERO, 2);
    let door = door_at(&mut zone, 2, Vec3::new(2.0, 0.0, 0.0), &[Verb::Open]);
    zone.entity_mut(door).unwrap().interactable = Some(
        Interactable::new(3.0, [Verb::Open]).with_permission(InteractionPermission::OwnerOrPublic),
    );
    zone.entity_mut(door).unwrap().owner = Some(character(77));

    let result = zone.apply_action(&action(actor, principal, door, Verb::Open), Millis::new(1));
    assert_eq!(result.error().unwrap().code(), ErrorCode::PermissionDenied);
}

#[test]
fn line_of_sight_is_enforced() {
    let mut zone = test_zone();
    let (actor, principal) = player_at(&mut zone, 1, Vec3::new(0.5, 0.0, 0.5), 2);
    let door = door_at(&mut zone, 2, Vec3::new(2.5, 0.0, 0.5), &[Verb::Open]);
    for z in 0..20 {
        zone.nav_mut().set_blocked(1, z, true);
    }

    let result = zone.apply_action(&action(actor, principal, door, Verb::Open), Millis::new(1));
    assert_eq!(result.error().unwrap().code(), ErrorCode::NotVisible);
}

#[test]
fn cooldowns_are_tick_based_and_server_controlled() {
    let mut zone = test_zone();
    let (actor, principal) = player_at(&mut zone, 1, Vec3::ZERO, 2);
    let door = door_at(&mut zone, 2, Vec3::new(2.0, 0.0, 0.0), &[Verb::Open]);
    zone.entity_mut(door).unwrap().interactable =
        Some(Interactable::new(3.0, [Verb::Open]).with_cooldown(5));

    let first = zone.apply_action(&action(actor, principal, door, Verb::Open), Millis::new(1));
    assert!(first.is_applied());

    let second = zone.apply_action(
        &ActionRequest::new("req-second", actor, Verb::Open)
            .on(door)
            .by(principal),
        Millis::new(1),
    );
    assert_eq!(second.error().unwrap().code(), ErrorCode::Cooldown);

    for _ in 0..5 {
        zone.advance_tick();
    }
    let third = zone.apply_action(
        &ActionRequest::new("req-third", actor, Verb::Open)
            .on(door)
            .by(principal),
        Millis::new(1),
    );
    assert!(third.is_applied());
}

#[test]
fn duplicate_request_ids_return_the_original_result_without_reapplying() {
    let mut zone = test_zone();
    let (actor, principal) = player_at(&mut zone, 1, Vec3::ZERO, 2);
    let door = door_at(
        &mut zone,
        2,
        Vec3::new(2.0, 0.0, 0.0),
        &[Verb::Open, Verb::Activate],
    );
    let request = action(actor, principal, door, Verb::Activate);

    let first = zone.apply_action(&request, Millis::new(1));
    assert!(first.is_applied());
    assert!(zone.entity(door).unwrap().door.as_ref().unwrap().open);

    let second = zone.apply_action(&request, Millis::new(2));
    assert_eq!(first, second);
    // Toggling twice would have closed it; the dedupe must keep it open.
    assert!(zone.entity(door).unwrap().door.as_ref().unwrap().open);
    assert_eq!(zone.stats().applied_actions, 1);
}

#[test]
fn locked_doors_stay_closed() {
    let mut zone = test_zone();
    let (actor, principal) = player_at(&mut zone, 1, Vec3::ZERO, 2);
    let door = entity(2);
    zone.spawn(
        EntityRecord::new(door, text("prop.door"), Vec3::new(2.0, 0.0, 0.0))
            .with_interactable(Interactable::new(3.0, [Verb::Open]))
            .with_door(Door::locked_with(text("item.brass_key"))),
    )
    .unwrap();

    let result = zone.apply_action(&action(actor, principal, door, Verb::Open), Millis::new(1));
    assert_eq!(result.error().unwrap().code(), ErrorCode::InvalidState);
    assert!(!zone.entity(door).unwrap().door.as_ref().unwrap().open);
}

#[test]
fn taking_items_conserves_them_and_checks_capacity() {
    let mut zone = test_zone();
    let (actor, principal) = player_at(&mut zone, 1, Vec3::ZERO, 1);
    let chest = entity(2);
    let torch = text("item.torch");
    let mut chest_inventory = Inventory::with_capacity(4).unwrap();
    chest_inventory
        .try_add(ItemStack::new(torch.clone(), 5), &UniformItemRules::new(10))
        .unwrap();
    zone.spawn(
        EntityRecord::new(chest, text("prop.chest"), Vec3::new(1.0, 0.0, 0.0))
            .with_interactable(Interactable::new(2.0, [Verb::Inspect, Verb::Take]))
            .with_inventory(chest_inventory),
    )
    .unwrap();

    let result = zone.apply_action(
        &ActionRequest::new("take-1", actor, Verb::Take)
            .on(chest)
            .by(principal)
            .with_param("quantity", "2"),
        Millis::new(1),
    );
    assert!(result.is_applied(), "{:?}", result.error());
    assert!(matches!(
        result.effects().first(),
        Some(ActionEffect::ItemTaken {
            source,
            destination,
            quantity: 2,
            ..
        }) if *source == chest && *destination == actor
    ));

    assert_eq!(
        zone.entity(chest)
            .unwrap()
            .inventory
            .as_ref()
            .unwrap()
            .total_quantity(&torch),
        3
    );
    assert_eq!(
        zone.entity(actor)
            .unwrap()
            .inventory
            .as_ref()
            .unwrap()
            .total_quantity(&torch),
        2
    );

    // The actor's one-slot inventory is now full (2 of max 10? no: capacity
    // one slot, room remains), so taking more than the stack limit is rejected
    // without changing either side.
    let before_chest = zone.entity(chest).unwrap().inventory.clone().unwrap();
    let before_actor = zone.entity(actor).unwrap().inventory.clone().unwrap();
    let result = zone.apply_action(
        &ActionRequest::new("take-2", actor, Verb::Take)
            .on(chest)
            .by(principal)
            .with_param("quantity", "11"),
        Millis::new(1),
    );
    assert_eq!(result.error().unwrap().code(), ErrorCode::InvalidRequest);
    assert_eq!(
        zone.entity(chest).unwrap().inventory.as_ref().unwrap(),
        &before_chest
    );
    assert_eq!(
        zone.entity(actor).unwrap().inventory.as_ref().unwrap(),
        &before_actor
    );
}

#[test]
fn attacks_apply_damage_and_release_control_on_death() {
    let mut zone = test_zone();
    let (actor, principal) = player_at(&mut zone, 1, Vec3::ZERO, 1);
    let target = entity(2);
    zone.spawn(
        EntityRecord::new(target, text("npc.rat"), Vec3::new(1.0, 0.0, 0.0))
            .with_health(1)
            .with_mind(Mind::ai(text("behavior.rat")))
            .with_interactable(Interactable::new(2.0, [Verb::Attack])),
    )
    .unwrap();

    let result = zone.apply_action(
        &ActionRequest::new("attack-1", actor, Verb::Attack)
            .on(target)
            .by(principal),
        Millis::new(1),
    );
    assert!(result.is_applied(), "{:?}", result.error());
    assert!(result.effects().iter().any(|effect| matches!(
        effect,
        ActionEffect::EntityDied { target: dead } if *dead == target
    )));
    let target = zone.entity(target).unwrap();
    assert_eq!(target.health.unwrap().current, 0);
    assert!(!target.is_controlled());
}

#[test]
fn attack_rejects_client_selected_damage_without_changing_health() {
    let mut zone = test_zone();
    let (actor, principal) = player_at(&mut zone, 1, Vec3::ZERO, 1);
    let target = entity(2);
    zone.spawn(
        EntityRecord::new(target, text("npc.rat"), Vec3::new(1.0, 0.0, 0.0))
            .with_health(3)
            .with_interactable(Interactable::new(2.0, [Verb::Attack])),
    )
    .unwrap();

    let forged = zone.apply_action(
        &ActionRequest::new("forged-attack", actor, Verb::Attack)
            .on(target)
            .by(principal)
            .with_param("damage", "1000"),
        Millis::new(1),
    );
    assert_eq!(forged.error().unwrap().code(), ErrorCode::InvalidRequest);
    assert_eq!(zone.entity(target).unwrap().health.unwrap().current, 3);

    let valid = zone.apply_action(
        &ActionRequest::new("valid-attack", actor, Verb::Attack)
            .on(target)
            .by(principal),
        Millis::new(1),
    );
    assert!(valid.effects().iter().any(|effect| matches!(
        effect,
        ActionEffect::Damaged {
            amount: 1,
            remaining: 2,
            ..
        }
    )));
}

#[test]
fn a_lever_opens_a_linked_door() {
    let mut zone = test_zone();
    let (actor, principal) = player_at(&mut zone, 1, Vec3::ZERO, 2);
    let lever = entity(2);
    let door = door_at(&mut zone, 3, Vec3::new(3.0, 0.0, 0.0), &[Verb::Open]);
    zone.spawn(
        EntityRecord::new(lever, text("prop.lever"), Vec3::new(1.0, 0.0, 0.0))
            .with_interactable(Interactable::new(3.0, [Verb::Activate])),
    )
    .unwrap();
    zone.add_link(EntityLink::new(
        text("link.lever_to_door"),
        lever,
        door,
        [LinkEvent::Activated],
    ))
    .unwrap();

    let result = zone.apply_action(
        &action(actor, principal, lever, Verb::Activate),
        Millis::new(1),
    );
    assert!(result.is_applied(), "{:?}", result.error());
    assert!(zone.entity(door).unwrap().door.as_ref().unwrap().open);
}

#[test]
fn link_activation_is_deduplicated_by_request() {
    let mut zone = test_zone();
    let (actor, principal) = player_at(&mut zone, 1, Vec3::ZERO, 2);
    let lever = entity(2);
    let door = door_at(&mut zone, 3, Vec3::new(3.0, 0.0, 0.0), &[Verb::Open]);
    zone.spawn(
        EntityRecord::new(lever, text("prop.lever"), Vec3::new(1.0, 0.0, 0.0))
            .with_interactable(Interactable::new(3.0, [Verb::Activate])),
    )
    .unwrap();
    zone.add_link(EntityLink::new(
        text("link.lever_to_door"),
        lever,
        door,
        [LinkEvent::Activated],
    ))
    .unwrap();

    let request = action(actor, principal, lever, Verb::Activate);
    assert!(zone.apply_action(&request, Millis::new(1)).is_applied());
    assert!(zone.entity(door).unwrap().door.as_ref().unwrap().open);
    // The action cache catches the replay before the link layer.
    assert!(zone.apply_action(&request, Millis::new(2)).is_applied());
    assert!(zone.entity(door).unwrap().door.as_ref().unwrap().open);
}

#[test]
fn link_cycles_are_refused() {
    let mut zone = test_zone();
    let a = entity(2);
    let b = door_at(&mut zone, 3, Vec3::new(2.0, 0.0, 0.0), &[Verb::Open]);
    zone.spawn(
        EntityRecord::new(a, text("prop.lever"), Vec3::new(1.0, 0.0, 0.0))
            .with_interactable(Interactable::new(3.0, [Verb::Activate])),
    )
    .unwrap();
    zone.add_link(EntityLink::new(
        text("link.lever_to_door"),
        a,
        b,
        [LinkEvent::Activated],
    ))
    .unwrap();

    let error = zone
        .add_link(EntityLink::new(
            text("link.door_to_lever"),
            b,
            a,
            [LinkEvent::Activated],
        ))
        .unwrap_err();
    assert!(error.message().contains("cycle"));
}

#[test]
fn cross_zone_links_are_diagnosed_and_do_not_fire() {
    let mut zone = test_zone();
    let (actor, principal) = player_at(&mut zone, 1, Vec3::ZERO, 2);
    let lever = entity(2);
    let door = door_at(&mut zone, 3, Vec3::new(3.0, 0.0, 0.0), &[Verb::Open]);
    zone.spawn(
        EntityRecord::new(lever, text("prop.lever"), Vec3::new(1.0, 0.0, 0.0))
            .with_interactable(Interactable::new(3.0, [Verb::Activate])),
    )
    .unwrap();
    let mut link = EntityLink::new(
        text("link.lever_to_door"),
        lever,
        door,
        [LinkEvent::Activated],
    );
    link.cross_zone = true;
    zone.add_link(link).unwrap();

    assert!(
        zone.diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "link.cross_zone_requires_route")
    );

    let result = zone.apply_action(
        &action(actor, principal, lever, Verb::Activate),
        Millis::new(1),
    );
    assert_eq!(result.error().unwrap().code(), ErrorCode::Unsupported);
    assert!(!zone.entity(door).unwrap().door.as_ref().unwrap().open);
}

#[test]
fn unused_factions_do_not_change_combat_authority() {
    let mut zone = test_zone();
    let (actor, principal) = player_at(&mut zone, 1, Vec3::ZERO, 1);
    let target = entity(2);
    zone.spawn(
        EntityRecord::new(target, text("npc.rat"), Vec3::new(1.0, 0.0, 0.0))
            .with_health(2)
            .with_interactable(Interactable::new(2.0, [Verb::Attack])),
    )
    .unwrap();
    let catalog = FactionCatalog::new();
    assert_eq!(
        zone.perceive_hostiles(actor, &catalog, Thresholds::default(), 10.0, Millis::new(1)),
        Vec::<engine_sim::ai::PerceivedTarget>::new()
    );

    let result = zone.apply_action(
        &ActionRequest::new("attack", actor, Verb::Attack)
            .on(target)
            .by(principal),
        Millis::new(1),
    );
    assert!(result.is_applied());
}

#[test]
fn a_locked_vault_opens_a_minigame_panel_instead_of_the_door() {
    let mut zone = test_zone();
    let (actor, principal) = player_at(&mut zone, 1, Vec3::ZERO, 2);
    let vault = vault_at(&mut zone, 2, Vec3::new(2.0, 0.0, 0.0));

    let result = zone.apply_action(&action(actor, principal, vault, Verb::Use), Millis::new(1));
    assert!(result.is_applied());
    assert!(matches!(
        result.effects().first(),
        Some(ActionEffect::UiOpened { target, schema, .. })
            if *target == vault && schema.as_str() == "ui.lockpick"
    ));
    assert!(matches!(
        result.effects().get(1),
        Some(ActionEffect::MinigameStarted {
            target,
            length: 3,
            attempts_left: 4,
            solved: false,
            ..
        }) if *target == vault
    ));

    let open = zone.apply_action(&action(actor, principal, vault, Verb::Open), Millis::new(2));
    assert_eq!(open.error().unwrap().code(), ErrorCode::InvalidState);
    assert!(zone.entity(vault).unwrap().door.as_ref().unwrap().locked);
}

#[test]
fn solving_a_minigame_unlocks_the_door_and_closes_the_panel() {
    let mut zone = test_zone();
    let (actor, principal) = player_at(&mut zone, 1, Vec3::ZERO, 2);
    let vault = vault_at(&mut zone, 2, Vec3::new(2.0, 0.0, 0.0));
    let secret = secret(&zone, vault, principal);
    let opened = zone.apply_action(&action(actor, principal, vault, Verb::Use), Millis::new(1));
    assert!(opened.is_applied());

    let wrong = if secret == "000" { "111" } else { "000" };
    let attempt = zone.apply_action(
        &play(actor, principal, vault, "guess-1", wrong),
        Millis::new(2),
    );
    assert!(attempt.is_applied());
    let ActionEffect::MinigameAttempt {
        hints,
        attempts_left,
        solved,
        ..
    } = &attempt.effects()[0]
    else {
        panic!("expected a minigame attempt effect");
    };
    assert!(!solved);
    assert_eq!(*attempts_left, 3);
    assert_eq!(hints.len(), 3);
    assert!(hints.iter().any(|hint| *hint != DigitHint::Exact));
    assert!(zone.entity(vault).unwrap().door.as_ref().unwrap().locked);

    let solved = zone.apply_action(
        &play(actor, principal, vault, "guess-2", &secret),
        Millis::new(3),
    );
    assert!(solved.is_applied());
    assert!(
        solved
            .effects()
            .iter()
            .any(|effect| matches!(effect, ActionEffect::MinigameSolved { .. }))
    );
    assert!(
        solved
            .effects()
            .iter()
            .any(|effect| matches!(effect, ActionEffect::DoorUnlocked { .. }))
    );
    assert!(
        solved
            .effects()
            .iter()
            .any(|effect| matches!(effect, ActionEffect::UiClosed { .. }))
    );
    assert!(!zone.entity(vault).unwrap().door.as_ref().unwrap().locked);

    let replay = zone.apply_action(
        &play(actor, principal, vault, "guess-3", &secret),
        Millis::new(4),
    );
    assert_eq!(replay.error().unwrap().code(), ErrorCode::InvalidState);

    let open = zone.apply_action(&action(actor, principal, vault, Verb::Open), Millis::new(5));
    assert!(open.is_applied());
    assert!(zone.entity(vault).unwrap().door.as_ref().unwrap().open);
}

#[test]
fn malformed_guesses_are_rejected_without_consuming_attempts() {
    let mut zone = test_zone();
    let (actor, principal) = player_at(&mut zone, 1, Vec3::ZERO, 2);
    let vault = vault_at(&mut zone, 2, Vec3::new(2.0, 0.0, 0.0));
    let opened = zone.apply_action(&action(actor, principal, vault, Verb::Use), Millis::new(1));
    assert!(opened.is_applied());

    let missing = zone.apply_action(
        &ActionRequest::new("guess-missing", actor, Verb::Play)
            .on(vault)
            .by(principal),
        Millis::new(2),
    );
    assert_eq!(missing.error().unwrap().code(), ErrorCode::InvalidRequest);
    for (index, guess) in ["", "12", "1234", "12a"].iter().enumerate() {
        let result = zone.apply_action(
            &play(
                actor,
                principal,
                vault,
                &format!("guess-bad-{index}"),
                guess,
            ),
            Millis::new(3),
        );
        assert_eq!(result.error().unwrap().code(), ErrorCode::InvalidRequest);
    }

    let progress = zone
        .entity(vault)
        .unwrap()
        .minigame
        .as_ref()
        .unwrap()
        .progress
        .get(&principal)
        .unwrap();
    assert_eq!(progress.attempts_left, 4);
    assert!(zone.entity(vault).unwrap().door.as_ref().unwrap().locked);
}

#[test]
fn minigame_attempts_are_per_player_and_lock_out_after_exhaustion() {
    let mut zone = test_zone();
    let (alice_actor, alice) = player_at(&mut zone, 1, Vec3::ZERO, 2);
    let (bob_actor, bob) = player_at(&mut zone, 3, Vec3::new(0.0, 0.0, 1.0), 2);
    let vault = vault_at(&mut zone, 2, Vec3::new(2.0, 0.0, 0.0));
    let alice_secret = secret(&zone, vault, alice);
    let bob_secret = secret(&zone, vault, bob);
    let wrong = if alice_secret == "000" { "111" } else { "000" };

    let alice_open = zone.apply_action(
        &action(alice_actor, alice, vault, Verb::Use),
        Millis::new(1),
    );
    assert!(alice_open.is_applied());
    let bob_open = zone.apply_action(&action(bob_actor, bob, vault, Verb::Use), Millis::new(1));
    assert!(bob_open.is_applied());

    for index in 0..4 {
        let result = zone.apply_action(
            &play(
                alice_actor,
                alice,
                vault,
                &format!("alice-guess-{index}"),
                wrong,
            ),
            Millis::new(2),
        );
        assert!(result.is_applied(), "attempt {index} is accepted");
    }
    let locked = zone.apply_action(
        &play(alice_actor, alice, vault, "alice-guess-locked", wrong),
        Millis::new(3),
    );
    assert_eq!(locked.error().unwrap().code(), ErrorCode::Cooldown);

    let bob_turn = zone.apply_action(
        &play(bob_actor, bob, vault, "bob-guess-1", &bob_secret),
        Millis::new(3),
    );
    assert!(bob_turn.is_applied(), "Bob's budget is independent");
    assert!(!zone.entity(vault).unwrap().door.as_ref().unwrap().locked);

    for _ in 0..5 {
        zone.advance_tick();
    }
    let recovered = zone.apply_action(
        &play(
            alice_actor,
            alice,
            vault,
            "alice-guess-recovered",
            &alice_secret,
        ),
        Millis::new(9),
    );
    assert!(
        recovered.is_applied(),
        "the attempt budget resets after the lockout"
    );
    assert!(
        recovered
            .effects()
            .iter()
            .any(|effect| matches!(effect, ActionEffect::MinigameSolved { .. }))
    );
}

#[test]
fn minigame_play_requires_range() {
    let mut zone = test_zone();
    let (actor, principal) = player_at(&mut zone, 1, Vec3::ZERO, 2);
    let vault = vault_at(&mut zone, 2, Vec3::new(10.0, 0.0, 0.0));
    let secret = secret(&zone, vault, principal);

    let result = zone.apply_action(
        &play(actor, principal, vault, "guess-far", &secret),
        Millis::new(1),
    );
    assert_eq!(result.error().unwrap().code(), ErrorCode::OutOfRange);
    assert!(zone.entity(vault).unwrap().door.as_ref().unwrap().locked);
}
