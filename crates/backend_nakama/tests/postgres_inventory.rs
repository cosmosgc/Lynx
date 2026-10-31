//! PostgreSQL inventory persistence tests require an isolated database with
//! migrations through 0009 applied. Set `LYNX_REQUIRE_POSTGRES_TEST` to require
//! configuration rather than skipping these tests.

use std::time::{SystemTime, UNIX_EPOCH};

use backend_nakama::PostgresInventoryStore;
use engine_core::inventory::{ItemStack, TransferReceipt};
use engine_core::{IdKind, StableId, id::DefinitionId};
use game_api::{ApiError, CharacterId, InventorySlot, InventoryStore, TransferInventoryUpdate};
use postgres::{Config, NoTls};

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
    let first = CharacterId::new(StableId::from_parts(IdKind::Character, 0, nonce))
        .expect("valid character ID");
    let second = CharacterId::new(StableId::from_parts(IdKind::Character, 0, nonce + 1))
        .expect("valid character ID");
    db.execute(
        "INSERT INTO lynx.accounts (account_id, nakama_user_id) VALUES ($1, md5($1)::uuid)",
        &[&account],
    )
    .expect("insert test account");
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
        Err(ApiError::Conflict(_))
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
    let destination_report = restarted.reconcile_character(second).unwrap();
    assert_eq!(destination_report.occupied_slots, 2);
    assert_eq!(destination_report.total_quantity, 4);
    assert_eq!(destination_report.transfer_receipts, 1);
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
        "DELETE FROM lynx.accounts WHERE account_id = $1",
        &[&account],
    )
    .unwrap();
}

fn slot(index: u16, definition: &str, quantity: u32) -> InventorySlot {
    InventorySlot {
        slot: index,
        stack: ItemStack::new(DefinitionId::new(definition).unwrap(), quantity),
    }
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
