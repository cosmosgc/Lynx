//! Read-only presentation of the controlled actor's private inventory.
//!
//! Owner state is replicated only to the owning session. The panel therefore
//! shows the actor's server-provided stacks without optimistically moving,
//! equipping, splitting, or merging them; those operations still require a
//! server-validated protocol.

use bevy::input::ButtonState;
use bevy::input::keyboard::KeyboardInput;
use bevy::picking::Pickable;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_immediate::{
    BevyImmediatePlugin, ImmCtx,
    ui::{CapsUi, floating_window_plugin::FloatingWindowPlugin},
};

use engine_core::inventory::{Inventory, ItemStack};

use crate::online::{OnlineClient, OnlineStatus};
use crate::ui::chat::ChatComposer;
use crate::ui::window::{ImmediateWindowPlugin, WindowMemory, WindowSpec, label, show_window};
use crate::windowed::prototype_name;

/// Stable window identity for remembered position and size.
pub const INVENTORY_WINDOW_ID: u64 = 3;

const INVENTORY_DEFAULT_SIZE: Vec2 = Vec2::new(360.0, 300.0);
const INVENTORY_MIN_SIZE: Vec2 = Vec2::new(280.0, 180.0);
const INVENTORY_EDGE_MARGIN: f32 = 16.0;

/// Whether the read-only inventory window is currently open.
#[derive(Debug, Default, Resource)]
pub struct InventoryPanel {
    open: bool,
}

impl InventoryPanel {
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }
}

/// Adds the owner-inventory window and its keyboard toggle to the online client.
#[derive(Debug)]
pub struct InventoryPanelPlugin;

impl Plugin for InventoryPanelPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<InventoryPanel>()
            .add_plugins(BevyImmediatePlugin::<CapsUi>::new());
        if !app.is_plugin_added::<ImmediateWindowPlugin>() {
            app.add_plugins(ImmediateWindowPlugin);
        }
        if !app.is_plugin_added::<FloatingWindowPlugin>() {
            app.add_plugins(FloatingWindowPlugin);
        }
        app.add_systems(
            Update,
            (inventory_keyboard, draw)
                .chain()
                .after(crate::online::poll_online),
        );
    }
}

/// Toggle the panel with `I`, or close it with `Escape`, unless chat owns the
/// keyboard. An active session with replicated inventory is required to open.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn inventory_keyboard(
    mut events: MessageReader<KeyboardInput>,
    windows: Query<&Window, With<PrimaryWindow>>,
    chat: Res<ChatComposer>,
    mut panel: ResMut<InventoryPanel>,
    client: Res<OnlineClient>,
) {
    if chat.active || !windows.single().is_ok_and(|window| window.focused) {
        return;
    }
    for event in events.read() {
        if event.state != ButtonState::Pressed {
            continue;
        }
        match event.key_code {
            KeyCode::KeyI => {
                if client.status() == OnlineStatus::Active && client.world().inventory().is_some() {
                    panel.open = !panel.open;
                }
            }
            KeyCode::Escape if panel.open => panel.open = false,
            _ => {}
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
fn draw(
    ctx: ImmCtx<CapsUi>,
    client: Res<OnlineClient>,
    mut panel: ResMut<InventoryPanel>,
    memory: Res<WindowMemory>,
    windows: Query<&Window, With<PrimaryWindow>>,
) {
    ctx.build_immediate_root("online-inventory-root")
        .ch_id("canvas")
        .on_spawn_insert(|| {
            (
                Node {
                    position_type: PositionType::Absolute,
                    width: percent(100),
                    height: percent(100),
                    ..default()
                },
                Pickable::IGNORE,
                GlobalZIndex(15),
            )
        })
        .add(|ui| {
            if !panel.open {
                return;
            }
            if client.status() != OnlineStatus::Active {
                panel.open = false;
                return;
            }
            let Some(inventory) = client.world().inventory() else {
                panel.open = false;
                return;
            };

            let stored = memory.get(INVENTORY_WINDOW_ID);
            let size = stored.map_or(INVENTORY_DEFAULT_SIZE, |state| state.size);
            let position = stored.map_or_else(
                || {
                    windows
                        .single()
                        .ok()
                        .map_or(Vec2::splat(INVENTORY_EDGE_MARGIN), |window| {
                            Vec2::new(
                                (window.width() - size.x - INVENTORY_EDGE_MARGIN)
                                    .max(INVENTORY_EDGE_MARGIN),
                                INVENTORY_EDGE_MARGIN,
                            )
                        })
                },
                |state| state.position,
            );
            let spec = WindowSpec {
                min_size: INVENTORY_MIN_SIZE,
                position,
                size,
                subtitle: Some(inventory_summary(inventory)),
                ..WindowSpec::new(INVENTORY_WINDOW_ID, "Inventário", INVENTORY_DEFAULT_SIZE)
            };
            let rows: Vec<(usize, String)> = inventory
                .entries()
                .map(|(slot, stack)| (slot, describe_slot(slot, stack)))
                .collect();

            let outcome = show_window(ui.ch_id("inventory-window"), spec, |ui| {
                if rows.is_empty() {
                    label(ui, "empty", 13.0, "Inventário vazio.");
                    return;
                }
                for (slot, text) in &rows {
                    label(ui, &format!("inventory-slot-{slot}"), 14.0, text.clone());
                }
            });
            if outcome.close_requested {
                panel.open = false;
            }
        });
}

/// Human-readable summary of occupied slots for the window subtitle.
#[must_use]
pub fn inventory_summary(inventory: &Inventory) -> String {
    format!(
        "{} de {} slots ocupados",
        inventory.occupied_slots(),
        inventory.capacity()
    )
}

/// One display row: slot number, derived item name, and quantity.
#[must_use]
pub fn describe_slot(slot: usize, stack: &ItemStack) -> String {
    let name = prototype_name(&stack.definition);
    if stack.is_unique() {
        format!("Slot {}: {} (único)", slot + 1, name)
    } else {
        format!("Slot {}: {} x{}", slot + 1, name, stack.quantity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::id::{DefinitionId, IdKind, StableId};

    fn definition(name: &str) -> DefinitionId {
        DefinitionId::new(name).unwrap()
    }

    #[test]
    fn stackable_slots_show_a_readable_name_and_quantity() {
        let stack = ItemStack::new(definition("item.healing_potion"), 4);
        assert_eq!(describe_slot(0, &stack), "Slot 1: Healing Potion x4");
    }

    #[test]
    fn unique_items_are_labelled_instead_of_showing_a_quantity() {
        let stack = ItemStack::unique(
            definition("item.vault_key"),
            StableId::from_parts(IdKind::Item, 1, 7),
        );
        assert_eq!(describe_slot(2, &stack), "Slot 3: Vault Key (único)");
    }

    #[test]
    fn summary_counts_occupied_slots_against_capacity() {
        let mut inventory = Inventory::with_capacity(8).unwrap();
        inventory
            .try_add(
                ItemStack::new(definition("item.torch"), 2),
                &engine_core::inventory::UniformItemRules::new(99),
            )
            .unwrap();
        assert_eq!(inventory_summary(&inventory), "1 de 8 slots ocupados");
    }
}
