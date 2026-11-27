//! Synthetic persisted session and legacy TEXT SQLite compatibility.
use super::*;
use crate::{
    memory::{crypto, MemoryEngine, StoredMemory},
    transport::{local::LocalStore, MemoryTransport},
};
use rusqlite::{params, Connection};
use serde_json::json;

const LEGACY_RECORD: &str =
    include_str!("../fixtures/legacy-memory-v1/legacy_stored_memory.json");
const LEGACY_PLAINTEXT: &str =
    include_str!("../fixtures/legacy-memory-v1/legacy_payload_v2.json");
const SALT: &str = "000102030405060708090a0b0c0d0e0f";
const URK: [u8; 32] = [42; 32];
const LOGIN: &str = "synthetic-original-login";
const ACCOUNT_SECRET: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const SUPER: &str = "synthetic-legacy-super";

fn exercise(vault_version: u8) -> Result<()> {
    let isolate = crate::test_lock::Isolate::new()?;
    let record: StoredMemory = serde_json::from_str(LEGACY_RECORD)?;
    let session = session_file()?;
    assert!(
        session.starts_with(isolate.path()),
        "escaped synthetic data root"
    );

    // The two session envelopes wrap the same fixed URK. The historical
    // memory row is frozen independently of the current writer.
    let kek = match vault_version {
        1 => crypto::derive_kek(LOGIN, ACCOUNT_SECRET, SALT)?,
        2 => crypto::derive_super_kek(SUPER, SALT)?,
        _ => anyhow::bail!("unexpected fixture vault version"),
    };
    let (nonce, wrapped) = crypto::wrap_key(&URK, &kek)?;
    let mut body = json!({
        "user": record.user,
        "vault_version": vault_version,
        "kdf_salt": SALT,
        "wrapped_urk": wrapped,
        "urk_nonce": nonce,
    });
    if vault_version == 1 {
        body["secret"] = json!(ACCOUNT_SECRET);
    }
    assert_eq!(write_session_json(&body)?, session);
    let before_session = std::fs::read(&session)?;
    let saved = read_session_json()?;
    let super_arg = (vault_version == 2).then_some(SUPER);
    let keys = unlock_session_keys(&saved, LOGIN, super_arg, &record.user)?;
    assert_eq!(keys.urk, URK);
    if vault_version == 1 {
        assert!(unlock_session_keys(&saved, "wrong-login", None, &record.user).is_err());
        let mut wrong_secret = saved.clone();
        wrong_secret["secret"] =
            json!("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff");
        assert!(unlock_session_keys(&wrong_secret, LOGIN, None, &record.user).is_err());
    } else {
        assert!(unlock_session_keys(&saved, LOGIN, Some("wrong-super"), &record.user).is_err());
        let other_login = unlock_session_keys(&saved, "not-the-login", Some(SUPER), &record.user)?;
        assert_eq!(other_login.urk, URK);
    }

    let database = isolate.path().join("legacy-synthetic.db");
    {
        let sqlite = Connection::open(&database)?;
        sqlite.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY, user TEXT NOT NULL, ciphertext TEXT NOT NULL,
                nonce TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
                title TEXT NOT NULL, embedding_enc TEXT NOT NULL
            );",
        )?;
        sqlite.execute(
            "INSERT INTO memories VALUES (?1,?2,?3,?4,?5,?5,?6,?7)",
            params![
                record.id,
                record.user,
                record.ciphertext,
                record.nonce,
                record.updated_at,
                "Legacy fixture",
                record.embedding_enc,
            ],
        )?;
    }
    {
        let store = LocalStore::open(&database)?;
        let current = store.all(true)?;
        assert_eq!(current.len(), 1);
        let row = &current[0];
        assert_eq!((&row.id, &row.user), (&record.id, &record.user));
        assert_eq!(
            (&row.ciphertext, &row.nonce, &row.embedding_enc),
            (&record.ciphertext, &record.nonce, &record.embedding_enc)
        );
        assert_eq!(
            keys.decrypt_content(&row.ciphertext, &row.nonce)?,
            LEGACY_PLAINTEXT
        );
        let entry = MemoryEngine::open(&keys, row)?;
        assert_eq!(entry.title, "Legacy fixture");
        assert_eq!(entry.content, "Frozen synthetic historical format sample.");
    }
    // Inspect the persisted SQLite row after closing the initialized store.
    let sqlite =
        Connection::open_with_flags(&database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let retained: (String, String, String, String, String) = sqlite.query_row(
        "SELECT id,ciphertext,nonce,embedding_enc,updated_at FROM memories",
        [],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )?;
    assert_eq!(
        retained,
        (
            record.id,
            record.ciphertext,
            record.nonce,
            record.embedding_enc,
            record.updated_at,
        )
    );
    assert_eq!(std::fs::read(session)?, before_session);
    Ok(())
}

#[test]
fn v1_stored_login_and_account_secret_open_frozen_text_sqlite() -> Result<()> {
    exercise(1)
}

#[test]
fn v2_stored_session_and_explicit_super_open_frozen_text_sqlite() -> Result<()> {
    exercise(2)
}
