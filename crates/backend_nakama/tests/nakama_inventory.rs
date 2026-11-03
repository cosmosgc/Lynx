//! Authenticated inventory contracts against PostgreSQL with a local Nakama
//! HTTP identity stub, or an optionally configured test Nakama instance.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use backend_contract_tests::{SessionInventoryContract, assert_inventory_contract};
use backend_nakama::{
    NakamaClient, NakamaConfig, NakamaPostgresBackend, PostgresCharacterStore,
    PostgresFactionCalendarStore, PostgresIdentityStore, PostgresInventoryStore,
};
use engine_core::{IdKind, Locale, StableId, id::DefinitionId, inventory::ItemStack};
use game_api::{
    AccountId, AuthService, CharacterId, CharacterService, CreateCharacter, DeviceLogin,
    InventorySlot, InventoryStore, SessionToken,
};
use postgres::{Config, NoTls};

#[test]
#[allow(clippy::too_many_lines)] // One isolated fixture covers identity, ownership, and cleanup end to end.
fn authenticated_inventory_reads_are_account_scoped() -> Result<(), Box<dyn std::error::Error>> {
    let Some(config) = postgres_config(false)? else {
        return Ok(());
    };
    let mut db = config.connect(NoTls)?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let owner = AccountId::new(format!("pst-nakama-inv-{nonce}"))?;
    let other = AccountId::new(format!("pst-nakama-other-{nonce}"))?;
    let character = CharacterId::new(StableId::from_parts(IdKind::Character, 0, nonce))?;
    let definition = DefinitionId::new("item.nakama_contract_test")?;

    for account in [&owner, &other] {
        db.execute(
            "INSERT INTO lynx.accounts (account_id, nakama_user_id) VALUES ($1, md5($1)::uuid)",
            &[&account.as_str()],
        )?;
    }
    let character_id = character.to_string();
    let owner_id = owner.as_str().to_owned();
    let character_name = format!("inventory{nonce}");
    db.execute(
        "INSERT INTO lynx.characters (character_id, account_id, name) VALUES ($1, $2, $3)",
        &[&character_id, &owner_id, &character_name],
    )?;
    let owner_nakama_id: String = db
        .query_one(
            "SELECT nakama_user_id::text FROM lynx.accounts WHERE account_id = $1",
            &[&owner.as_str()],
        )?
        .get(0);
    let other_nakama_id: String = db
        .query_one(
            "SELECT nakama_user_id::text FROM lynx.accounts WHERE account_id = $1",
            &[&other.as_str()],
        )?
        .get(0);

    let inventory = PostgresInventoryStore::new(config.connect(NoTls)?);
    let slots = vec![InventorySlot {
        slot: 2,
        stack: ItemStack::new(definition, 4),
    }];
    inventory.replace_inventory(character, &slots)?;

    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let nakama_ids = [owner_nakama_id, other_nakama_id];
    let stub = thread::spawn(move || {
        for expected_id in nakama_ids {
            let (mut stream, _) = listener.accept().expect("accept Nakama request");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("set request timeout");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = stream.read(&mut buffer).expect("read request");
                assert_ne!(count, 0, "request has HTTP headers");
                request.extend_from_slice(&buffer[..count]);
            }
            let request = String::from_utf8(request).expect("request headers are UTF-8");
            assert!(request.starts_with("GET /v2/account "));
            let user = serde_json::json!({ "user": { "id": expected_id } });
            let body = user.to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write account response");
        }
    });

    let backend = NakamaPostgresBackend::new(
        NakamaClient::new(NakamaConfig::new(format!("http://{address}")))?,
        PostgresIdentityStore::new(config.connect(NoTls)?),
        PostgresCharacterStore::new(config.connect(NoTls)?),
        inventory,
        PostgresFactionCalendarStore::new(config.connect(NoTls)?),
    );
    let owner_token = SessionToken::new("owner-session-token")?;
    let other_token = SessionToken::new("other-session-token")?;
    assert_inventory_contract(
        &SessionInventoryContract { backend: &backend },
        &owner_token,
        &other_token,
        character,
        &slots,
    );
    stub.join().expect("Nakama HTTP stub completed");

    db.execute(
        "DELETE FROM lynx.inventory_slots WHERE owner_id = $1",
        &[&character.to_string()],
    )?;
    db.execute(
        "DELETE FROM lynx.inventory_revisions WHERE owner_id = $1",
        &[&character.to_string()],
    )?;
    db.execute(
        "DELETE FROM lynx.characters WHERE character_id = $1",
        &[&character.to_string()],
    )?;
    db.execute(
        "DELETE FROM lynx.accounts WHERE account_id IN ($1, $2)",
        &[&owner.as_str(), &other.as_str()],
    )?;
    Ok(())
}

#[test]
#[allow(clippy::too_many_lines)] // End-to-end configured-instance contract plus isolated DB cleanup.
fn configured_nakama_inventory_contract() -> Result<(), Box<dyn std::error::Error>> {
    let Some(base_url) = std::env::var("LYNX_NAKAMA_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        if std::env::var_os("LYNX_REQUIRE_NAKAMA_TEST").is_some() {
            return Err("LYNX_NAKAMA_URL is required for configured Nakama tests".into());
        }
        eprintln!("skipping configured Nakama inventory test: LYNX_NAKAMA_URL is not set");
        return Ok(());
    };
    let Some(owner_device_id) = std::env::var("LYNX_NAKAMA_TEST_OWNER_DEVICE_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return missing_nakama_test_configuration("LYNX_NAKAMA_TEST_OWNER_DEVICE_ID");
    };
    let Some(other_device_id) = std::env::var("LYNX_NAKAMA_TEST_OTHER_DEVICE_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return missing_nakama_test_configuration("LYNX_NAKAMA_TEST_OTHER_DEVICE_ID");
    };
    if owner_device_id == other_device_id {
        return Err("Nakama contract test device IDs must be distinct".into());
    }
    let Some(config) = postgres_config(true)? else {
        return Ok(());
    };
    let mut db = config.connect(NoTls)?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let mut nakama_config = NakamaConfig::new(base_url);
    nakama_config.http_key = std::env::var("LYNX_NAKAMA_HTTP_KEY").ok();
    let backend = NakamaPostgresBackend::new(
        NakamaClient::new(nakama_config)?,
        PostgresIdentityStore::new(config.connect(NoTls)?),
        PostgresCharacterStore::new(config.connect(NoTls)?),
        PostgresInventoryStore::new(config.connect(NoTls)?),
        PostgresFactionCalendarStore::new(config.connect(NoTls)?),
    );

    let owner = backend.authenticate_device(&DeviceLogin {
        device_id: owner_device_id,
        username: None,
        create: false,
        locale: Locale::default(),
    })?;
    let other = backend.authenticate_device(&DeviceLogin {
        device_id: other_device_id,
        username: None,
        create: false,
        locale: Locale::default(),
    })?;
    let character = backend.create_character(
        &owner.token,
        &CreateCharacter {
            name: format!("PST010{nonce}"),
        },
    )?;
    let character_id = character.id;
    let slots = vec![InventorySlot {
        slot: 1,
        stack: ItemStack::new(DefinitionId::new("item.nakama_contract_test")?, 2),
    }];
    PostgresInventoryStore::new(config.connect(NoTls)?).replace_inventory(character_id, &slots)?;

    assert_inventory_contract(
        &SessionInventoryContract { backend: &backend },
        &owner.token,
        &other.token,
        character_id,
        &slots,
    );

    let character_id_text = character_id.to_string();
    db.execute(
        "DELETE FROM lynx.inventory_slots WHERE owner_id = $1",
        &[&character_id_text],
    )?;
    db.execute(
        "DELETE FROM lynx.inventory_revisions WHERE owner_id = $1",
        &[&character_id_text],
    )?;
    db.execute(
        "DELETE FROM lynx.characters WHERE character_id = $1",
        &[&character_id_text],
    )?;
    Ok(())
}

fn missing_nakama_test_configuration(variable: &str) -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("LYNX_REQUIRE_NAKAMA_TEST").is_some() {
        return Err(format!("{variable} is required for configured Nakama tests").into());
    }
    eprintln!("skipping configured Nakama inventory test: {variable} is not set");
    Ok(())
}

fn postgres_config(require: bool) -> Result<Option<Config>, Box<dyn std::error::Error>> {
    let required = ["LYNX_PG_DATABASE", "LYNX_PG_USER", "LYNX_PG_PASSWORD"];
    if required.iter().any(|key| std::env::var_os(key).is_none()) {
        if require || std::env::var_os("LYNX_REQUIRE_POSTGRES_TEST").is_some() {
            return Err("PostgreSQL configuration missing".into());
        }
        eprintln!("skipping Nakama inventory test: database configuration missing");
        return Ok(None);
    }
    let mut config = Config::new();
    config
        .host(&std::env::var("LYNX_PG_HOST").unwrap_or_else(|_| "127.0.0.1".into()))
        .port(
            std::env::var("LYNX_PG_PORT")
                .unwrap_or_else(|_| "5432".into())
                .parse()?,
        )
        .dbname(&std::env::var("LYNX_PG_DATABASE")?)
        .user(&std::env::var("LYNX_PG_USER")?)
        .password(&std::env::var("LYNX_PG_PASSWORD")?);
    Ok(Some(config))
}
