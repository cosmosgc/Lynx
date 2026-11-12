//! PostgreSQL inventory persistence tests require an isolated database with
//! current migrations applied. Set `LYNX_REQUIRE_POSTGRES_TEST` to require
//! configuration rather than skipping these tests.

use std::time::{SystemTime, UNIX_EPOCH};

use backend_nakama::PostgresInventoryStore;
use engine_core::inventory::{ItemStack, TransferReceipt};
use engine_core::{IdKind, StableId, id::DefinitionId};
use game_api::{
    AccountId, ApiError, CharacterId, InventorySlot, InventoryStore, TransferInventoryUpdate,
};
use postgres::{Config, NoTls};
use proptest::prelude::*;
use proptest::test_runner::{Config as PropertyConfig, TestRunner};

#[test]
#[allow(clippy::too_many_lines)] // One isolated fixture exercises the full transfer lifecycle.
fn versioned_inventory_survives_reconnection_conflicts_and_rolls_back() {
    let config = postgres_config();
    let Some(config) = config else {
        return;
    };
    let mut db = config
        .connect(NoTls)
        .expect("connect to isolated test database");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    let account = format!("pst-inventory-{nonce}");
    let other_account = format!("pst-inventory-other-{nonce}");
    let first = CharacterId::new(StableId::from_parts(IdKind::Character, 0, nonce))
        .expect("valid character ID");
    let second = CharacterId::new(StableId::from_parts(IdKind::Character, 0, nonce + 1))
        .expect("valid character ID");
    db.execute(
        "INSERT INTO lynx.accounts (account_id, nakama_user_id) VALUES ($1, md5($1)::uuid)",
        &[&account],
    )
    .expect("insert test account");
    db.execute(
        "INSERT INTO lynx.accounts (account_id, nakama_user_id) VALUES ($1, md5($1)::uuid)",
        &[&other_account],
    )
    .expect("insert other test account");
    for (id, suffix) in [(first, "a"), (second, "b")] {
        db.execute(
            "INSERT INTO lynx.characters (character_id, account_id, name) VALUES ($1, $2, $3)",
            &[
                &id.to_string(),
                &account,
                &format!("inventory{nonce}{suffix}"),
            ],
        )
        .expect("insert test character");
    }

    let unique = StableId::from_parts(IdKind::Item, 0, nonce + 2);
    let first_slots = vec![slot(0, "item.ore", 3)];
    let store = PostgresInventoryStore::new(config.connect(NoTls).unwrap());
    let owner = AccountId::new(account.clone()).unwrap();
    let other_owner = AccountId::new(other_account.clone()).unwrap();
    assert_eq!(
        store.load_inventory_for_account(&owner, first).unwrap(),
        Vec::<InventorySlot>::new()
    );
    assert!(matches!(
        store.load_inventory_for_account(&other_owner, first),
        Err(ApiError::NotFound(_))
    ));
    let initial = store.load_versioned_inventory(first).unwrap();
    assert_eq!(initial.version, 0);
    assert_eq!(
        store.replace_inventory_if_version(first, 0, &first_slots),
        Ok(1)
    );
    let durable = vec![InventorySlot {
        slot: 1,
        stack: ItemStack::unique(DefinitionId::new("item.sword").unwrap(), unique),
    }];
    assert_eq!(
        store.replace_inventory_if_version(second, 0, &durable),
        Ok(1)
    );

    let conflicting = vec![InventorySlot {
        slot: 0,
        stack: ItemStack::unique(DefinitionId::new("item.sword").unwrap(), unique),
    }];
    assert!(matches!(
        store.replace_inventory_if_version(first, 1, &conflicting),
        Err(ApiError::Conflict(_))
    ));
    assert!(matches!(
        store.replace_inventory_if_version(first, 0, &[]),
        Err(ApiError::Conflict(_))
    ));
    assert_eq!(
        store.load_versioned_inventory(first).unwrap().slots,
        first_slots
    );

    drop(store);
    let reopened = PostgresInventoryStore::new(config.connect(NoTls).unwrap());
    assert_eq!(reopened.load_versioned_inventory(first).unwrap().version, 1);
    assert_eq!(
        reopened.load_versioned_inventory(second).unwrap().slots,
        durable
    );

    let receipt = TransferReceipt {
        key: format!("transfer-{nonce}"),
        source: StableId::from_parts(IdKind::Entity, 0, nonce + 3),
        source_slot: 0,
        destination: StableId::from_parts(IdKind::Entity, 0, nonce + 4),
        definition: DefinitionId::new("item.ore").unwrap(),
        quantity: 3,
        destination_slots: vec![2],
    };
    let collision_id = StableId::from_parts(IdKind::Item, 0, nonce + 5);
    let source_attempt = vec![InventorySlot {
        slot: 0,
        stack: ItemStack::unique(DefinitionId::new("item.ring").unwrap(), collision_id),
    }];
    let destination_attempt = vec![
        durable[0].clone(),
        InventorySlot {
            slot: 2,
            stack: ItemStack::unique(DefinitionId::new("item.ring").unwrap(), collision_id),
        },
    ];
    let failed_receipt = TransferReceipt {
        key: format!("failed-transfer-{nonce}"),
        ..receipt.clone()
    };
    assert!(matches!(
        reopened.commit_transfer(
            &failed_receipt.key,
            &failed_receipt,
            TransferInventoryUpdate {
                character_id: first,
                expected_version: 1,
                slots: &source_attempt
            },
            TransferInventoryUpdate {
                character_id: second,
                expected_version: 1,
                slots: &destination_attempt
            },
        ),
        Err(ApiError::InvalidRequest(_))
    ));
    assert_eq!(reopened.load_versioned_inventory(first).unwrap().version, 1);
    assert_eq!(
        reopened.load_versioned_inventory(first).unwrap().slots,
        first_slots
    );
    assert_eq!(
        reopened.load_versioned_inventory(second).unwrap().version,
        1
    );
    assert_eq!(
        reopened.load_versioned_inventory(second).unwrap().slots,
        durable
    );

    let destination_after = vec![durable[0].clone(), slot(2, "item.ore", 3)];
    std::thread::scope(|scope| {
        let first_attempt = scope.spawn(|| {
            let store = PostgresInventoryStore::new(config.connect(NoTls).unwrap());
            store.commit_transfer(
                &receipt.key,
                &receipt,
                TransferInventoryUpdate {
                    character_id: first,
                    expected_version: 1,
                    slots: &[],
                },
                TransferInventoryUpdate {
                    character_id: second,
                    expected_version: 1,
                    slots: &destination_after,
                },
            )
        });
        let concurrent_attempt = scope.spawn(|| {
            let store = PostgresInventoryStore::new(config.connect(NoTls).unwrap());
            store.commit_transfer(
                &receipt.key,
                &receipt,
                TransferInventoryUpdate {
                    character_id: first,
                    expected_version: 1,
                    slots: &[],
                },
                TransferInventoryUpdate {
                    character_id: second,
                    expected_version: 1,
                    slots: &destination_after,
                },
            )
        });
        assert_eq!(first_attempt.join().unwrap().unwrap(), receipt);
        assert_eq!(concurrent_attempt.join().unwrap().unwrap(), receipt);
    });
    drop(reopened);
    let restarted = PostgresInventoryStore::new(config.connect(NoTls).unwrap());
    assert_eq!(
        restarted.load_versioned_inventory(first).unwrap().version,
        2
    );
    assert_eq!(
        restarted.load_versioned_inventory(second).unwrap().slots,
        destination_after
    );
    assert_eq!(
        restarted
            .commit_transfer(
                &receipt.key,
                &receipt,
                TransferInventoryUpdate {
                    character_id: first,
                    expected_version: 1,
                    slots: &first_slots
                },
                TransferInventoryUpdate {
                    character_id: second,
                    expected_version: 1,
                    slots: &durable
                },
            )
            .unwrap(),
        receipt
    );
    let mut mismatched_receipt = receipt.clone();
    mismatched_receipt.quantity += 1;
    assert!(matches!(
        restarted.commit_transfer(
            &receipt.key,
            &mismatched_receipt,
            TransferInventoryUpdate {
                character_id: first,
                expected_version: 1,
                slots: &first_slots
            },
            TransferInventoryUpdate {
                character_id: second,
                expected_version: 1,
                slots: &durable
            },
        ),
        Err(ApiError::Conflict(_))
    ));
    assert_eq!(restarted.load_versioned_inventory(first).unwrap().slots, []);
    let source_report = restarted.reconcile_character(first).unwrap();
    assert_eq!(source_report.version, 2);
    assert_eq!(source_report.occupied_slots, 0);
    assert_eq!(source_report.total_quantity, 0);
    assert_eq!(source_report.transfer_receipts, 1);
    assert_eq!(source_report.differences.len(), 0);
    let destination_report = restarted.reconcile_character(second).unwrap();
    assert_eq!(destination_report.occupied_slots, 2);
    assert_eq!(destination_report.total_quantity, 4);
    assert_eq!(destination_report.transfer_receipts, 1);
    assert_eq!(destination_report.differences.len(), 0);
    let ledger_rows = db
        .query(
            "SELECT event.source, event.actor_kind, event.actor_id, event.reason, \
                    entry.owner_character_id, entry.item_definition, entry.quantity_delta, \
                    entry.before_quantity, entry.after_quantity \
             FROM lynx.inventory_value_events event \
             JOIN lynx.inventory_value_entries entry USING (event_key) \
             WHERE event.correlation_id = $1 ORDER BY entry.owner_character_id",
            &[&receipt.key],
        )
        .unwrap();
    assert_eq!(ledger_rows.len(), 2);
    assert!(ledger_rows.iter().all(|row| {
        row.get::<_, String>(0) == "inventory.player_transfer"
            && row.get::<_, String>(1) == "character"
            && row.get::<_, String>(2) == first.to_string()
            && row.get::<_, String>(3) == "player inventory item transfer"
            && row.get::<_, String>(5) == "item.ore"
    }));
    let source_entry = ledger_rows
        .iter()
        .find(|row| row.get::<_, String>(4) == first.to_string())
        .unwrap();
    assert_eq!(source_entry.get::<_, i64>(6), -3);
    assert_eq!(source_entry.get::<_, i64>(7), 3);
    assert_eq!(source_entry.get::<_, i64>(8), 0);

    db.execute(
        "UPDATE lynx.inventory_slots SET quantity = quantity + 1 \
         WHERE owner_id = $1 AND item_definition = 'item.ore'",
        &[&second.to_string()],
    )
    .unwrap();
    let drift = restarted.reconcile_character(second).unwrap();
    assert_eq!(drift.differences.len(), 1);
    assert_eq!(drift.differences[0].item_definition, "item.ore");
    assert_eq!(drift.differences[0].inventory_quantity, 4);
    assert_eq!(drift.differences[0].ledger_quantity, 3);
    db.execute(
        "UPDATE lynx.inventory_slots SET quantity = quantity - 1 \
         WHERE owner_id = $1 AND item_definition = 'item.ore'",
        &[&second.to_string()],
    )
    .unwrap();
    assert_eq!(
        restarted
            .reconcile_character(second)
            .unwrap()
            .differences
            .len(),
        0
    );
    drop(restarted);

    let concurrent_left = vec![slot(0, "item.ore", 1)];
    let concurrent_right = vec![slot(0, "item.ore", 2)];
    let (left, right) = std::thread::scope(|scope| {
        let left = scope.spawn(|| {
            let store = PostgresInventoryStore::new(config.connect(NoTls).unwrap());
            store.replace_inventory_if_version(first, 2, &concurrent_left)
        });
        let right = scope.spawn(|| {
            let store = PostgresInventoryStore::new(config.connect(NoTls).unwrap());
            store.replace_inventory_if_version(first, 2, &concurrent_right)
        });
        (left.join().unwrap(), right.join().unwrap())
    });
    assert!(matches!(
        (&left, &right),
        (Ok(3), Err(ApiError::Conflict(_))) | (Err(ApiError::Conflict(_)), Ok(3))
    ));

    assert!(db.execute(
        "UPDATE lynx.inventory_transfer_receipts SET receipt = receipt WHERE idempotency_key = $1",
        &[&receipt.key],
    ).is_err());
    assert!(
        db.execute(
            "DELETE FROM lynx.inventory_transfer_receipts WHERE idempotency_key = $1",
            &[&receipt.key],
        )
        .is_err()
    );
    let mut cleanup = db.transaction().unwrap();
    cleanup
        .batch_execute("SET LOCAL lynx.allow_append_only_purge = 'on'")
        .unwrap();
    cleanup
        .execute(
            "DELETE FROM lynx.inventory_transfer_receipts WHERE idempotency_key IN ($1, $2)",
            &[&receipt.key, &failed_receipt.key],
        )
        .unwrap();
    cleanup.commit().unwrap();
    db.execute(
        "DELETE FROM lynx.inventory_slots WHERE owner_id IN ($1, $2)",
        &[&first.to_string(), &second.to_string()],
    )
    .unwrap();
    db.execute(
        "DELETE FROM lynx.inventory_revisions WHERE owner_id IN ($1, $2)",
        &[&first.to_string(), &second.to_string()],
    )
    .unwrap();
    db.execute(
        "DELETE FROM lynx.characters WHERE character_id IN ($1, $2)",
        &[&first.to_string(), &second.to_string()],
    )
    .unwrap();
    db.execute(
        "DELETE FROM lynx.accounts WHERE account_id IN ($1, $2)",
        &[&account, &other_account],
    )
    .unwrap();
}

fn slot(index: u16, definition: &str, quantity: u32) -> InventorySlot {
    InventorySlot {
        slot: index,
        stack: ItemStack::new(DefinitionId::new(definition).unwrap(), quantity),
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one generated case spans inventory mutation, ledger replay, reconciliation, and isolated cleanup"
)]
fn generated_player_value_transfers_conserve_inventory_and_ledger() {
    let Some(config) = postgres_config() else {
        return;
    };
    let mut runner = TestRunner::new(PropertyConfig {
        cases: 12,
        failure_persistence: None,
        ..PropertyConfig::default()
    });
    runner
        .run(&prop::collection::vec(1_u32..=8, 1..=6), |quantities| {
            let mut db = config
                .connect(NoTls)
                .expect("connect to isolated test database");
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock")
                .as_nanos();
            let account = format!("ledger-property-{nonce}");
            let source_owner = CharacterId::new(StableId::from_parts(IdKind::Character, 0, nonce))
                .expect("valid source character ID");
            let destination_owner =
                CharacterId::new(StableId::from_parts(IdKind::Character, 0, nonce + 1))
                    .expect("valid destination character ID");
            db.execute(
                "INSERT INTO lynx.accounts (account_id, nakama_user_id) VALUES ($1, md5($1)::uuid)",
                &[&account],
            )
            .expect("insert property-test account");
            for (character, suffix) in
                [(source_owner, "source"), (destination_owner, "destination")]
            {
                db.execute(
                "INSERT INTO lynx.characters (character_id, account_id, name) VALUES ($1, $2, $3)",
                &[&character.to_string(), &account, &format!("ledger{nonce}{suffix}")],
            )
            .expect("insert property-test character");
            }

            let total = quantities.iter().copied().sum::<u32>();
            let source = PostgresInventoryStore::new(config.connect(NoTls).unwrap());
            prop_assert_eq!(
                source.replace_inventory_if_version(source_owner, 0, &[slot(0, "item.ore", total)]),
                Ok(1),
            );
            prop_assert_eq!(
                source.replace_inventory_if_version(destination_owner, 0, &[]),
                Ok(1)
            );

            let mut remaining = total;
            let mut destination_total = 0_u32;
            for (index, quantity) in quantities.iter().copied().enumerate() {
                let expected_version =
                    1 + i64::try_from(index).expect("generated case index fits i64");
                let next_remaining = remaining - quantity;
                let next_destination = destination_total + quantity;
                let source_after = if next_remaining == 0 {
                    Vec::new()
                } else {
                    vec![slot(0, "item.ore", next_remaining)]
                };
                let destination_after = vec![slot(0, "item.ore", next_destination)];
                let key = format!("ledger-property-{nonce}-{index}");
                let receipt = TransferReceipt {
                    key: key.clone(),
                    source: StableId::from_parts(IdKind::Entity, 0, nonce + 2),
                    source_slot: 0,
                    destination: StableId::from_parts(IdKind::Entity, 0, nonce + 3),
                    definition: DefinitionId::new("item.ore").unwrap(),
                    quantity,
                    destination_slots: vec![0],
                };
                let source_update = TransferInventoryUpdate {
                    character_id: source_owner,
                    expected_version,
                    slots: &source_after,
                };
                let destination_update = TransferInventoryUpdate {
                    character_id: destination_owner,
                    expected_version,
                    slots: &destination_after,
                };
                prop_assert_eq!(
                    source.commit_transfer(
                        &key,
                        &receipt,
                        source_update.clone(),
                        destination_update.clone(),
                    ),
                    Ok(receipt.clone()),
                );
                // The original request remains idempotent after commit: replaying
                // its exact versions and snapshots must not append another event.
                prop_assert_eq!(
                    source.commit_transfer(&key, &receipt, source_update, destination_update),
                    Ok(receipt),
                );
                let event_key = format!("transfer:{key}");
                let entries = db
                    .query_one(
                        "SELECT COUNT(*), COALESCE(SUM(quantity_delta), 0) \
                     FROM lynx.inventory_value_entries WHERE event_key = $1",
                        &[&event_key],
                    )
                    .expect("read generated ledger event");
                prop_assert_eq!(entries.get::<_, i64>(0), 2);
                prop_assert_eq!(entries.get::<_, i64>(1), 0);

                remaining = next_remaining;
                destination_total = next_destination;
            }

            let source_report = source.reconcile_character(source_owner).unwrap();
            let destination_report = source.reconcile_character(destination_owner).unwrap();
            prop_assert!(source_report.differences.is_empty());
            prop_assert!(destination_report.differences.is_empty());
            prop_assert_eq!(source_report.total_quantity, i64::from(remaining));
            prop_assert_eq!(
                destination_report.total_quantity,
                i64::from(destination_total)
            );
            prop_assert_eq!(
                source_report.total_quantity + destination_report.total_quantity,
                i64::from(total),
            );

            let event_keys = db
                .query(
                    "SELECT DISTINCT event_key FROM lynx.inventory_value_entries \
                 WHERE owner_character_id IN ($1, $2)",
                    &[&source_owner.to_string(), &destination_owner.to_string()],
                )
                .expect("list generated ledger events")
                .into_iter()
                .map(|row| row.get::<_, String>(0))
                .collect::<Vec<_>>();
            let mut cleanup = db.transaction().unwrap();
            cleanup
                .batch_execute("SET LOCAL lynx.allow_append_only_purge = 'on'")
                .unwrap();
            cleanup
                .execute(
                    "DELETE FROM lynx.inventory_transfer_receipts WHERE idempotency_key LIKE $1",
                    &[&format!("ledger-property-{nonce}-%")],
                )
                .unwrap();
            cleanup
                .execute(
                    "DELETE FROM lynx.inventory_value_entries WHERE owner_character_id IN ($1, $2)",
                    &[&source_owner.to_string(), &destination_owner.to_string()],
                )
                .unwrap();
            for event_key in event_keys {
                cleanup
                    .execute(
                        "DELETE FROM lynx.inventory_value_events WHERE event_key = $1",
                        &[&event_key],
                    )
                    .unwrap();
            }
            cleanup.commit().unwrap();
            for character in [source_owner, destination_owner] {
                db.execute(
                    "DELETE FROM lynx.inventory_slots WHERE owner_id = $1",
                    &[&character.to_string()],
                )
                .unwrap();
                db.execute(
                    "DELETE FROM lynx.inventory_revisions WHERE owner_id = $1",
                    &[&character.to_string()],
                )
                .unwrap();
            }
            db.execute(
                "DELETE FROM lynx.characters WHERE character_id IN ($1, $2)",
                &[&source_owner.to_string(), &destination_owner.to_string()],
            )
            .unwrap();
            db.execute(
                "DELETE FROM lynx.accounts WHERE account_id = $1",
                &[&account],
            )
            .unwrap();
            Ok(())
        })
        .expect("generated inventory value transfers preserve the ledger invariants");
}

fn postgres_config() -> Option<Config> {
    let required = ["LYNX_PG_DATABASE", "LYNX_PG_USER", "LYNX_PG_PASSWORD"];
    if required.iter().any(|key| std::env::var_os(key).is_none()) {
        assert!(
            std::env::var_os("LYNX_REQUIRE_POSTGRES_TEST").is_none(),
            "PostgreSQL configuration missing"
        );
        eprintln!("skipping PostgreSQL inventory test: database configuration missing");
        return None;
    }
    let mut config = Config::new();
    config
        .host(&std::env::var("LYNX_PG_HOST").unwrap_or_else(|_| "127.0.0.1".into()))
        .port(
            std::env::var("LYNX_PG_PORT")
                .unwrap_or_else(|_| "5432".into())
                .parse()
                .expect("valid PostgreSQL port"),
        )
        .dbname(&std::env::var("LYNX_PG_DATABASE").unwrap())
        .user(&std::env::var("LYNX_PG_USER").unwrap())
        .password(std::env::var("LYNX_PG_PASSWORD").unwrap());
    Some(config)
}
