//! PostgreSQL persistence for authoritative character inventories.

use std::sync::Mutex;

use engine_core::inventory::TransferReceipt;
use engine_core::{StableId, id::DefinitionId, inventory::ItemStack};
use game_api::{
    ApiError, CharacterId, InventoryReconciliation, InventorySlot, InventoryStore,
    TransferInventoryUpdate, VersionedInventory, validate_slots,
};
use postgres::{Client, Error, error::SqlState};

/// PostgreSQL inventory snapshots with atomic optimistic concurrency control.
pub struct PostgresInventoryStore {
    client: Mutex<Client>,
}

impl std::fmt::Debug for PostgresInventoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresInventoryStore")
            .finish_non_exhaustive()
    }
}

impl PostgresInventoryStore {
    /// Wrap a connection to a migrated Lynx database.
    #[must_use]
    pub fn new(client: Client) -> Self {
        Self {
            client: Mutex::new(client),
        }
    }

    fn client(&self) -> Result<std::sync::MutexGuard<'_, Client>, ApiError> {
        self.client.lock().map_err(|_| unavailable())
    }

    fn replace(
        &self,
        character_id: CharacterId,
        expected: Option<i64>,
        slots: &[InventorySlot],
    ) -> Result<i64, ApiError> {
        validate_slots(slots)?;
        let character_id = character_id.to_string();
        let mut client = self.client()?;
        let mut tx = client.transaction().map_err(|_| unavailable())?;
        let character_exists = tx
            .query_opt(
                "SELECT 1 FROM lynx.characters WHERE character_id = $1 AND status = 'active' AND deleted_at IS NULL FOR UPDATE",
                &[&character_id],
            )
            .map_err(|_| unavailable())?
            .is_some();
        if !character_exists {
            return Err(ApiError::NotFound("character not found".into()));
        }

        tx.execute(
            "INSERT INTO lynx.inventory_revisions (owner_id, version) VALUES ($1, 0) ON CONFLICT (owner_id) DO NOTHING",
            &[&character_id],
        )
        .map_err(|_| unavailable())?;
        let current: i64 = tx
            .query_one(
                "SELECT version FROM lynx.inventory_revisions WHERE owner_id = $1 FOR UPDATE",
                &[&character_id],
            )
            .map_err(|_| unavailable())?
            .get(0);
        if expected.is_some_and(|expected| expected != current) {
            return Err(ApiError::Conflict("inventory version changed".into()));
        }

        tx.execute(
            "DELETE FROM lynx.inventory_slots WHERE owner_id = $1",
            &[&character_id],
        )
        .map_err(|_| unavailable())?;
        for slot in slots {
            let slot_index = i32::from(slot.slot);
            let quantity = i32::try_from(slot.stack.quantity)
                .map_err(|_| ApiError::InvalidRequest("inventory quantity is too large".into()))?;
            let definition = slot.stack.definition.as_str();
            let instance = slot.stack.instance.map(|value| value.to_string());
            tx.execute(
                "INSERT INTO lynx.inventory_slots (owner_id, slot, item_definition, quantity, instance_id) VALUES ($1, $2, $3, $4, $5)",
                &[&character_id, &slot_index, &definition, &quantity, &instance],
            )
            .map_err(|error| map_write_error(&error))?;
        }
        let next = current
            .checked_add(1)
            .ok_or_else(|| ApiError::Unavailable("inventory version exhausted".into()))?;
        tx.execute(
            "UPDATE lynx.inventory_revisions SET version = $2, updated_at = now() WHERE owner_id = $1",
            &[&character_id, &next],
        )
        .map_err(|_| unavailable())?;
        tx.commit().map_err(|_| unavailable())?;
        Ok(next)
    }

    /// Return aggregate inventory and ledger counts for operator reconciliation.
    pub fn reconcile_character(
        &self,
        character_id: CharacterId,
    ) -> Result<InventoryReconciliation, ApiError> {
        let id = character_id.to_string();
        let mut client = self.client()?;
        let row = client
            .query_opt(
                "SELECT COALESCE(r.version, 0), COUNT(s.slot), COALESCE(SUM(s.quantity), 0), \
             (SELECT COUNT(*) FROM lynx.inventory_transfer_receipts t \
              WHERE t.source_owner_id = c.character_id OR t.destination_owner_id = c.character_id) \
             FROM lynx.characters c \
             LEFT JOIN lynx.inventory_revisions r ON r.owner_id = c.character_id \
             LEFT JOIN lynx.inventory_slots s ON s.owner_id = c.character_id \
             WHERE c.character_id = $1 GROUP BY c.character_id, r.version",
                &[&id],
            )
            .map_err(|_| unavailable())?
            .ok_or_else(|| ApiError::NotFound("character not found".into()))?;
        Ok(InventoryReconciliation {
            character_id,
            version: row.get(0),
            occupied_slots: row.get(1),
            total_quantity: row.get(2),
            transfer_receipts: row.get(3),
        })
    }
}

impl InventoryStore for PostgresInventoryStore {
    fn load_inventory(&self, character_id: CharacterId) -> Result<Vec<InventorySlot>, ApiError> {
        Ok(self.load_versioned_inventory(character_id)?.slots)
    }

    fn load_versioned_inventory(
        &self,
        character_id: CharacterId,
    ) -> Result<VersionedInventory, ApiError> {
        let character_id = character_id.to_string();
        let mut client = self.client()?;
        let rows = client
            .query(
                "SELECT COALESCE(r.version, 0), s.slot, s.item_definition, s.quantity, s.instance_id \
                 FROM lynx.characters c \
                 LEFT JOIN lynx.inventory_revisions r ON r.owner_id = c.character_id \
                 LEFT JOIN lynx.inventory_slots s ON s.owner_id = c.character_id \
                 WHERE c.character_id = $1 AND c.status = 'active' AND c.deleted_at IS NULL \
                 ORDER BY s.slot",
                &[&character_id],
            )
            .map_err(|_| unavailable())?;
        if rows.is_empty() {
            return Err(ApiError::NotFound("character not found".into()));
        }
        let version = rows[0].get(0);
        let mut slots = Vec::new();
        for row in rows {
            let Some(slot) = row.get::<_, Option<i32>>(1) else {
                continue;
            };
            let definition: String = row.get(2);
            let quantity: i32 = row.get(3);
            let instance: Option<String> = row.get(4);
            let stack = ItemStack {
                definition: DefinitionId::new(&definition)
                    .map_err(|_| ApiError::Unavailable("stored inventory is invalid".into()))?,
                quantity: u32::try_from(quantity)
                    .map_err(|_| ApiError::Unavailable("stored inventory is invalid".into()))?,
                instance: instance
                    .map(|value| StableId::parse(&value))
                    .transpose()
                    .map_err(|_| ApiError::Unavailable("stored inventory is invalid".into()))?,
            };
            slots.push(InventorySlot {
                slot: u16::try_from(slot)
                    .map_err(|_| ApiError::Unavailable("stored inventory is invalid".into()))?,
                stack,
            });
        }
        validate_slots(&slots)
            .map_err(|_| ApiError::Unavailable("stored inventory is invalid".into()))?;
        Ok(VersionedInventory { version, slots })
    }

    fn replace_inventory(
        &self,
        character_id: CharacterId,
        slots: &[InventorySlot],
    ) -> Result<(), ApiError> {
        self.replace(character_id, None, slots).map(|_| ())
    }

    fn replace_inventory_if_version(
        &self,
        character_id: CharacterId,
        expected: i64,
        slots: &[InventorySlot],
    ) -> Result<i64, ApiError> {
        if expected < 0 {
            return Err(ApiError::InvalidRequest(
                "inventory version is invalid".into(),
            ));
        }
        self.replace(character_id, Some(expected), slots)
    }

    #[allow(clippy::too_many_lines)] // Slots, revisions, and receipts share one SQL transaction.
    fn commit_transfer(
        &self,
        key: &str,
        receipt: &TransferReceipt,
        source: TransferInventoryUpdate<'_>,
        destination: TransferInventoryUpdate<'_>,
    ) -> Result<TransferReceipt, ApiError> {
        if key.is_empty()
            || key.len() > 256
            || receipt.key != key
            || receipt.quantity == 0
            || source.character_id == destination.character_id
        {
            return Err(ApiError::InvalidRequest(
                "transfer receipt is invalid".into(),
            ));
        }
        if source.expected_version < 0 || destination.expected_version < 0 {
            return Err(ApiError::InvalidRequest(
                "inventory version is invalid".into(),
            ));
        }
        validate_slots(source.slots)?;
        validate_slots(destination.slots)?;

        let source_id = source.character_id.to_string();
        let destination_id = destination.character_id.to_string();
        let receipt_json = serde_json::to_value(receipt).map_err(|_| {
            ApiError::InvalidRequest("transfer receipt cannot be serialized".into())
        })?;
        let mut client = self.client()?;
        let mut tx = client.transaction().map_err(|_| unavailable())?;
        tx.query_one(
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            &[&key],
        )
        .map_err(|_| unavailable())?;
        if let Some(row) = tx
            .query_opt(
                "SELECT source_owner_id, destination_owner_id, receipt \
                 FROM lynx.inventory_transfer_receipts WHERE idempotency_key = $1",
                &[&key],
            )
            .map_err(|_| unavailable())?
        {
            let stored_source: String = row.get(0);
            let stored_destination: String = row.get(1);
            let stored_receipt: serde_json::Value = row.get(2);
            if stored_source != source_id
                || stored_destination != destination_id
                || stored_receipt != receipt_json
            {
                return Err(ApiError::Conflict(
                    "idempotency key was used for another transfer".into(),
                ));
            }
            return Ok(receipt.clone());
        }

        let mut owners = [&source_id, &destination_id];
        owners.sort();
        for owner in owners {
            if tx.query_opt(
                "SELECT 1 FROM lynx.characters WHERE character_id = $1 AND status = 'active' AND deleted_at IS NULL FOR UPDATE",
                &[owner],
            ).map_err(|_| unavailable())?.is_none() {
                return Err(ApiError::NotFound("character not found".into()));
            }
        }
        let mut expected = [
            (source_id.as_str(), source.expected_version),
            (destination_id.as_str(), destination.expected_version),
        ];
        expected.sort_by_key(|(owner, _)| *owner);
        for (owner, expected_version) in expected {
            tx.execute(
                "INSERT INTO lynx.inventory_revisions (owner_id, version) VALUES ($1, 0) ON CONFLICT (owner_id) DO NOTHING",
                &[&owner],
            ).map_err(|_| unavailable())?;
            let current: i64 = tx
                .query_one(
                    "SELECT version FROM lynx.inventory_revisions WHERE owner_id = $1 FOR UPDATE",
                    &[&owner],
                )
                .map_err(|_| unavailable())?
                .get(0);
            if current != expected_version {
                return Err(ApiError::Conflict("inventory version changed".into()));
            }
        }

        for (owner, slots) in [
            (&source_id, source.slots),
            (&destination_id, destination.slots),
        ] {
            tx.execute(
                "DELETE FROM lynx.inventory_slots WHERE owner_id = $1",
                &[owner],
            )
            .map_err(|_| unavailable())?;
            for slot in slots {
                let slot_index = i32::from(slot.slot);
                let quantity = i32::try_from(slot.stack.quantity).map_err(|_| {
                    ApiError::InvalidRequest("inventory quantity is too large".into())
                })?;
                let definition = slot.stack.definition.as_str();
                let instance = slot.stack.instance.map(|value| value.to_string());
                tx.execute(
                    "INSERT INTO lynx.inventory_slots (owner_id, slot, item_definition, quantity, instance_id) VALUES ($1, $2, $3, $4, $5)",
                    &[owner, &slot_index, &definition, &quantity, &instance],
                ).map_err(|error| map_write_error(&error))?;
            }
        }
        for (owner, version) in [
            (&source_id, source.expected_version),
            (&destination_id, destination.expected_version),
        ] {
            let next = version
                .checked_add(1)
                .ok_or_else(|| ApiError::Unavailable("inventory version exhausted".into()))?;
            tx.execute(
                "UPDATE lynx.inventory_revisions SET version = $2, updated_at = now() WHERE owner_id = $1",
                &[owner, &next],
            ).map_err(|_| unavailable())?;
        }
        tx.execute(
            "INSERT INTO lynx.inventory_transfer_receipts (idempotency_key, source_owner_id, destination_owner_id, receipt) VALUES ($1, $2, $3, $4)",
            &[&key, &source_id, &destination_id, &receipt_json],
        ).map_err(|error| map_write_error(&error))?;
        tx.commit().map_err(|_| unavailable())?;
        Ok(receipt.clone())
    }
}

fn map_write_error(error: &Error) -> ApiError {
    if error.code() == Some(&SqlState::UNIQUE_VIOLATION) {
        ApiError::Conflict("item instance is already assigned to an inventory".into())
    } else {
        unavailable()
    }
}

fn unavailable() -> ApiError {
    ApiError::Unavailable("PostgreSQL inventory store is unavailable".into())
}
