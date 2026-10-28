//! Set `LYNX_PG_DATABASE`, `LYNX_PG_USER` and `LYNX_PG_PASSWORD` to run against a
//! migrated, isolated test database. The test writes and deletes records and a
//! failure may leave fixtures behind. `LYNX_REQUIRE_POSTGRES_TEST` makes missing
//! configuration fail. Connections use `NoTls` and are intended for local tests.

use std::time::{SystemTime, UNIX_EPOCH};

use backend_nakama::{PostgresCharacterStore, PostgresIdentityStore};
use engine_core::{IdKind, Locale, StableId};
use game_api::{AccountId, ApiError, CharacterId, CharacterStatus, CreateCharacter};
use postgres::{Config, NoTls};

#[test]
fn account_scoped_reads_survive_reconnection() {
    let required = ["LYNX_PG_DATABASE", "LYNX_PG_USER", "LYNX_PG_PASSWORD"];
    if required.iter().any(|name| std::env::var_os(name).is_none()) {
        assert!(
            std::env::var_os("LYNX_REQUIRE_POSTGRES_TEST").is_none(),
            "PostgreSQL configuration missing"
        );
        eprintln!("skipping PostgreSQL character test: database configuration missing");
        return;
    }
    let mut config = Config::new();
    config
        .host(&std::env::var("LYNX_PG_HOST").unwrap_or_else(|_| "127.0.0.1".into()))
        .port(
            std::env::var("LYNX_PG_PORT")
                .unwrap_or_else(|_| "5432".into())
                .parse()
                .unwrap(),
        )
        .dbname(&std::env::var("LYNX_PG_DATABASE").unwrap())
        .user(&std::env::var("LYNX_PG_USER").unwrap())
        .password(std::env::var("LYNX_PG_PASSWORD").unwrap());
    let mut db = config.connect(NoTls).expect("connect to test database");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let owner = AccountId::new(format!("pst-test-{nonce}")).unwrap();
    let other = AccountId::new(format!("pst-other-{nonce}")).unwrap();
    let id = CharacterId::new(StableId::from_parts(IdKind::Character, 0, nonce)).unwrap();
    let suspended =
        CharacterId::new(StableId::from_parts(IdKind::Character, 0, nonce + 1)).unwrap();
    let deleted = CharacterId::new(StableId::from_parts(IdKind::Character, 0, nonce + 2)).unwrap();
    for account in [&owner, &other] {
        db.execute(
            "INSERT INTO lynx.accounts (account_id, nakama_user_id) VALUES ($1, md5($1)::uuid)",
            &[&account.as_str()],
        )
        .unwrap();
    }
    let identities = PostgresIdentityStore::new(config.connect(NoTls).unwrap());
    for account in [&owner, &other] {
        let user_id: String = db
            .query_one(
                "SELECT nakama_user_id::text FROM lynx.accounts WHERE account_id = $1",
                &[&account.as_str()],
            )
            .unwrap()
            .get(0);
        assert_eq!(identities.resolve_account(&user_id).unwrap(), *account);
        assert_eq!(
            identities.resolve_account(&user_id.to_uppercase()).unwrap(),
            *account
        );
    }
    let missing: String = db
        .query_one("SELECT md5($1)::uuid::text", &[&format!("missing-{nonce}")])
        .unwrap()
        .get(0);
    assert!(matches!(
        identities.resolve_account(&missing),
        Err(ApiError::Unauthenticated)
    ));
    assert!(matches!(
        identities.resolve_account("' OR true--"),
        Err(ApiError::Unauthenticated)
    ));
    let new_nakama_user: String = db
        .query_one("SELECT md5($1)::uuid::text", &[&format!("new-{nonce}")])
        .unwrap()
        .get(0);
    let provisioned = identities
        .provision_account(&new_nakama_user, &Locale::default())
        .unwrap();
    assert_eq!(provisioned.as_str(), new_nakama_user);
    assert_eq!(
        identities.resolve_account(&new_nakama_user).unwrap(),
        provisioned
    );
    for (character, status) in [
        (id, "active"),
        (suspended, "suspended"),
        (deleted, "deleted"),
    ] {
        let name = format!("t{nonce}{}", &character.to_string()[28..]);
        db.execute("INSERT INTO lynx.characters (character_id, account_id, name, status, deleted_at) VALUES ($1, $2, $3, $4, CASE WHEN $4 = 'deleted' THEN now() ELSE NULL END)", &[&character.to_string(), &owner.as_str(), &name, &status]).unwrap();
    }
    let store = PostgresCharacterStore::new(config.connect(NoTls).unwrap());
    let listed = store.characters(&owner).unwrap();
    assert_eq!(listed.len(), 2);
    assert!(
        listed
            .iter()
            .any(|c| c.id == suspended && c.status == CharacterStatus::Suspended)
    );
    let selected = store.select_character(&owner, id).unwrap();
    assert_eq!(selected.account_id, owner);
    assert!(selected.created_at.is_some());
    assert_eq!(store.select_character(&owner, id).unwrap(), selected);
    let absent = CharacterId::new(StableId::from_parts(IdKind::Character, 0, 0)).unwrap();
    let missing_error = store.select_character(&owner, absent).unwrap_err();
    assert!(matches!(missing_error, ApiError::NotFound(_)));
    for (account, character) in [
        (&other, id),
        (&other, suspended),
        (&other, deleted),
        (&owner, deleted),
        (&owner, suspended),
    ] {
        assert_eq!(
            store
                .select_character(account, character)
                .unwrap_err()
                .to_string(),
            missing_error.to_string()
        );
    }
    db.execute(
        "UPDATE lynx.characters SET status = 'active' WHERE character_id = $1",
        &[&suspended.to_string()],
    )
    .unwrap();
    assert!(
        store
            .select_character(&owner, suspended)
            .unwrap()
            .is_playable()
    );
    db.execute(
        "UPDATE lynx.characters SET status = 'suspended' WHERE character_id = $1",
        &[&suspended.to_string()],
    )
    .unwrap();
    assert_eq!(
        store
            .select_character(&owner, suspended)
            .unwrap_err()
            .to_string(),
        missing_error.to_string()
    );
    assert_eq!(store.characters(&other).unwrap(), []);
    drop(store);
    let reopened = PostgresCharacterStore::new(config.connect(NoTls).unwrap());
    assert_eq!(reopened.select_character(&owner, id).unwrap(), selected);
    check_world_entry(&config, &owner, &other, id, nonce);
    check_creation(&config, &mut db, &owner, &other, nonce);
    check_deletion(&config, &mut db, &owner, &other, id, suspended, deleted);
    db.execute(
        "DELETE FROM lynx.idempotency_keys WHERE scope = $1 OR scope = $2",
        &[&owner.as_str(), &other.as_str()],
    )
    .unwrap();
    let mut cleanup = db.transaction().unwrap();
    cleanup
        .batch_execute("SET LOCAL lynx.allow_append_only_purge = 'on'")
        .unwrap();
    cleanup
        .execute(
            "DELETE FROM lynx.audit_events WHERE target_id IN (SELECT character_id FROM lynx.characters WHERE account_id = $1)",
            &[&owner.as_str()],
        )
        .unwrap();
    cleanup.commit().unwrap();
    db.execute(
        "DELETE FROM lynx.accounts WHERE account_id = $1 OR account_id = $2 OR account_id = $3",
        &[&owner.as_str(), &other.as_str(), &provisioned.as_str()],
    )
    .unwrap();
}

fn check_world_entry(
    config: &Config,
    owner: &AccountId,
    other: &AccountId,
    character: CharacterId,
    nonce: u128,
) {
    let zone = StableId::from_parts(IdKind::Zone, 0, nonce).to_string();
    let store = PostgresCharacterStore::new(config.connect(NoTls).unwrap());
    let entered = store.enter_character(owner, character, &zone).unwrap();
    assert_eq!(entered.zone_id.as_deref(), Some(zone.as_str()));
    assert!(matches!(
        store.enter_character(other, character, &zone),
        Err(ApiError::NotFound(_))
    ));
    drop(store);

    let reopened = PostgresCharacterStore::new(config.connect(NoTls).unwrap());
    assert_eq!(
        reopened.select_character(owner, character).unwrap().zone_id,
        Some(zone)
    );
    assert_eq!(
        reopened.leave_character(owner, character).unwrap().zone_id,
        None
    );
}

fn check_deletion(
    config: &Config,
    db: &mut postgres::Client,
    owner: &AccountId,
    other: &AccountId,
    id: CharacterId,
    suspended: CharacterId,
    deleted: CharacterId,
) {
    let store = PostgresCharacterStore::new(config.connect(NoTls).unwrap());
    let missing = CharacterId::new(StableId::from_parts(IdKind::Character, 0, 0)).unwrap();
    let foreign = store.delete_character(other, id).unwrap_err();
    let absent = store.delete_character(other, missing).unwrap_err();
    assert_eq!(foreign.to_string(), absent.to_string());
    assert!(matches!(foreign, ApiError::NotFound(_)));
    assert!(store.select_character(owner, id).is_ok());

    let name = store.select_character(owner, id).unwrap().name;
    let (first, second) = std::thread::scope(|scope| {
        let run = || {
            let store = PostgresCharacterStore::new(config.connect(NoTls).unwrap());
            store.delete_character(owner, id)
        };
        let first = scope.spawn(run);
        let second = scope.spawn(run);
        (first.join().unwrap(), second.join().unwrap())
    });
    first.unwrap();
    second.unwrap();
    let before: String = db.query_one(
        "SELECT deleted_at::text FROM lynx.characters WHERE character_id = $1 AND status = 'deleted'",
        &[&id.to_string()],
    ).unwrap().get(0);
    drop(store);
    let reopened = PostgresCharacterStore::new(config.connect(NoTls).unwrap());
    reopened.delete_character(owner, id).unwrap();
    reopened.delete_character(owner, deleted).unwrap();
    reopened.delete_character(owner, suspended).unwrap();
    let after: String = db
        .query_one(
            "SELECT deleted_at::text FROM lynx.characters WHERE character_id = $1",
            &[&id.to_string()],
        )
        .unwrap()
        .get(0);
    assert_eq!(before, after);
    assert!(matches!(
        reopened.select_character(owner, id),
        Err(ApiError::NotFound(_))
    ));
    assert!(
        !reopened
            .characters(owner)
            .unwrap()
            .iter()
            .any(|c| c.id == id || c.id == suspended)
    );
    let audits: i64 = db.query_one(
        "SELECT count(*) FROM lynx.audit_events WHERE target_id = $1 AND action = 'character.delete' AND outcome = 'succeeded'",
        &[&id.to_string()],
    ).unwrap().get(0);
    assert_eq!(audits, 1);
    let audits: i64 = db.query_one(
        "SELECT count(*) FROM lynx.audit_events WHERE target_id = $1 AND action = 'character.delete'",
        &[&deleted.to_string()],
    ).unwrap().get(0);
    assert_eq!(audits, 0);
    let replacement = reopened
        .create_character(owner, "reuse-name", &CreateCharacter { name })
        .unwrap();
    assert_ne!(replacement.id, id);
}

fn check_creation(
    config: &Config,
    db: &mut postgres::Client,
    owner: &AccountId,
    other: &AccountId,
    nonce: u128,
) {
    let reopened = PostgresCharacterStore::new(config.connect(NoTls).unwrap());
    let request = CreateCharacter {
        name: format!("c{nonce}"),
    };
    let created = reopened
        .create_character(owner, "create-1", &request)
        .unwrap();
    assert_eq!(&created.account_id, owner);
    assert_eq!(
        reopened
            .create_character(owner, "create-1", &request)
            .unwrap(),
        created
    );
    assert!(matches!(
        reopened.create_character(
            owner,
            "create-1",
            &CreateCharacter {
                name: "Different".into()
            }
        ),
        Err(ApiError::Conflict(_))
    ));
    assert!(matches!(
        reopened.select_character(other, created.id),
        Err(ApiError::NotFound(_))
    ));
    assert!(matches!(
        reopened.create_character(other, "create-1", &request),
        Err(ApiError::Conflict(_))
    ));
    drop(reopened);
    let reopened = PostgresCharacterStore::new(config.connect(NoTls).unwrap());
    assert_eq!(
        reopened
            .create_character(owner, "create-1", &request)
            .unwrap(),
        created
    );
    assert_eq!(
        reopened.select_character(owner, created.id).unwrap(),
        created
    );
    let parallel_request = CreateCharacter {
        name: format!("p{nonce}"),
    };
    let (first, second) = std::thread::scope(|scope| {
        let run = || {
            let store = PostgresCharacterStore::new(config.connect(NoTls).unwrap());
            store
                .create_character(owner, "concurrent", &parallel_request)
                .unwrap()
        };
        let first = scope.spawn(run);
        let second = scope.spawn(run);
        (first.join().unwrap(), second.join().unwrap())
    });
    assert_eq!(first, second);
    for n in 0..4 {
        reopened
            .create_character(
                owner,
                &format!("limit-{n}"),
                &CreateCharacter {
                    name: format!("l{n}{nonce}"),
                },
            )
            .unwrap();
    }
    assert!(matches!(
        reopened.create_character(
            owner,
            "over-limit",
            &CreateCharacter {
                name: format!("x{nonce}")
            }
        ),
        Err(ApiError::Conflict(_))
    ));
    assert_eq!(reopened.characters(owner).unwrap().len(), 8);
    let audits: i64 = db.query_one("SELECT count(*) FROM lynx.audit_events WHERE target_id = $1 AND action = 'character.create'", &[&created.id.to_string()]).unwrap().get(0);
    assert_eq!(audits, 1);
}
