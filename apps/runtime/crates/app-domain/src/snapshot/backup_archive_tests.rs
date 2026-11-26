use super::*;
use std::io::Cursor;

fn manifest(account: &str, db: &[u8]) -> Manifest {
    Manifest {
        schema_version: 1,
        id: "123456789abcdef0123456789abcdef0".into(),
        created_at: "2026-11-25T00:00:00Z".into(),
        note: "test archive".into(),
        database_sha256: format!("{:x}", Sha256::digest(db)),
        bytes: db.len() as u64,
        entries: 1,
        source_library: "same-toolchain test source".into(),
        account_fingerprint: format!("{:x}", Sha256::digest(account.as_bytes())),
        sqlite_user_version: 1,
        database_schema_sha256: "test schema".into(),
        sdk_version: "1.0".into(),
        sdk_abi: 1,
        model: "test model".into(),
        witness_head: None,
        witness_count: None,
        witness_intact: None,
        witness_problem: None,
    }
}

fn archive(db: &[u8], keys: &crate::SessionKeys) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    seal_to_stage(keys, "alice", manifest("alice", db), db, &mut payload, || Ok(()))?;
    Ok(payload)
}

#[test]
fn streams_multichunk_database_in_both_existing_vault_namespaces() -> Result<()> {
    let db = vec![21; 3 * 64 * 1024 + 1];
    let current = crate::SessionKeys::from_urk([7; 32])?;
    let legacy = crate::SessionKeys::from_legacy_urk([7; 32])?;
    for (writer, reader) in [(&current, &legacy), (&legacy, &current)] {
        let payload = archive(&db, writer)?;
        let mut output = Vec::new();
        let mut checkpoints = 0;
        let restored = open_to_stage(reader, "alice", Cursor::new(payload), &mut output, || {
            checkpoints += 1;
            Ok(())
        })?;
        assert_eq!(restored.bytes, db.len() as u64);
        assert_eq!(output, db);
        assert!(checkpoints >= 5);
    }
    Ok(())
}

#[test]
fn different_key_account_or_altered_manifest_never_writes_db_bytes() -> Result<()> {
    let keys = crate::SessionKeys::from_urk([7; 32])?;
    let other = crate::SessionKeys::from_urk([8; 32])?;
    let payload = archive(b"database bytes", &keys)?;
    let mut output = Vec::new();
    assert!(open_to_stage(&other, "alice", payload.as_slice(), &mut output, || Ok(())).is_err());
    assert!(open_to_stage(&keys, "bob", payload.as_slice(), &mut output, || Ok(())).is_err());

    let header_size = u32::from_be_bytes(payload[8..12].try_into()?) as usize;
    let mut header: Header = serde_json::from_slice(&payload[12..12 + header_size])?;
    header.manifest.entries += 1;
    let altered = serde_json::to_vec(&header)?;
    let archive = [
        MAGIC.as_slice(),
        &(altered.len() as u32).to_be_bytes(),
        altered.as_slice(),
        &payload[12 + header_size..],
    ].concat();
    assert!(open_to_stage(&keys, "alice", archive.as_slice(), &mut output, || Ok(())).is_err());
    assert!(output.is_empty());
    Ok(())
}

#[test]
fn rejects_truncated_modified_trailing_oversized_and_cancelled_archives() -> Result<()> {
    let keys = crate::SessionKeys::from_urk([7; 32])?;
    let payload = archive(b"database bytes", &keys)?;
    let mut altered = payload.clone();
    if let Some(last) = altered.last_mut() {
        *last ^= 1;
    }
    let mut trailing = payload.clone();
    trailing.push(0);
    let mut oversized = payload.clone();
    oversized[8..12].copy_from_slice(&(MAX_HEADER as u32 + 1).to_be_bytes());
    for invalid in [&payload[..payload.len() - 1], &altered, &trailing, &oversized] {
        assert!(open_to_stage(&keys, "alice", invalid, std::io::sink(), || Ok(())).is_err());
    }

    let mut written = Vec::new();
    let mut checks = 0;
    assert!(open_to_stage(&keys, "alice", payload.as_slice(), &mut written, || {
        checks += 1;
        anyhow::ensure!(checks == 1, "cancelled");
        Ok(())
    }).is_err());
    assert!(written.is_empty());
    Ok(())
}

#[test]
fn native_snapshot_envelope_preserves_committed_wal_and_excludes_grants() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let profile = directory.path();
    let keys = crate::SessionKeys::from_urk([7; 32])?;
    let live = rusqlite::Connection::open(profile.join("rsrs.db"))?;
    live.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE memories(id TEXT PRIMARY KEY, ciphertext TEXT, nonce TEXT);
         CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
         CREATE TABLE access_grants(token_hash TEXT);
         INSERT INTO access_grants VALUES ('private-grant-does-not-travel');",
    )?;
    let (nonce, ciphertext) = keys.encrypt_content("fixture plaintext")?;
    live.execute(
        "INSERT INTO memories VALUES ('fixture', ?1, ?2)",
        rusqlite::params![ciphertext, nonce],
    )?;
    let id = super::super::create(profile, "alice", "fixture", &keys)?.id;
    let lock = crate::lock::LibraryLock::acquire(profile, std::time::Duration::ZERO)?;
    let mut archive = Vec::new();
    seal_verified_snapshot(profile, &lock, &keys, "alice", &id, &mut archive, || Ok(()))?;
    assert!(!archive
        .windows(b"private-grant-does-not-travel".len())
        .any(|window| window == b"private-grant-does-not-travel"));
    let imported = profile.join("detached-test.db");
    let restored = open_to_stage(
        &keys, "alice", archive.as_slice(), File::create(&imported)?, || Ok(()),
    )?;
    assert_eq!(restored.id, id);
    let separate = rusqlite::Connection::open(&imported)?;
    let (ciphertext, nonce): (String, String) = separate.query_row(
        "SELECT ciphertext, nonce FROM memories WHERE id='fixture'",
        [], |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(keys.decrypt_content(&ciphertext, &nonce)?, "fixture plaintext");
    assert_eq!(separate.query_row("SELECT count(*) FROM access_grants", [], |row| row.get::<_, u64>(0))?, 0);
    assert_eq!(live.query_row("SELECT count(*) FROM access_grants", [], |row| row.get::<_, u64>(0))?, 1);
    Ok(())
}


#[cfg(target_os = "linux")]
#[test]
fn authenticated_upload_enters_exact_native_custody_once() -> Result<()> {
    use respire_spawn::owner_credential::ArchiveOperation;
    use std::{fs, os::unix::fs::PermissionsExt};

    let owner = tempfile::tempdir()?;
    let root = owner.path().canonicalize()?;
    let root = root.as_path();
    fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
    let selected = root.join("rsrs.db");
    let unrelated = root.join("other.db");
    fs::write(&selected, "active library remains untouched")?;
    fs::write(&unrelated, "unrelated fixture library")?;
    let db = vec![63; 2 * 64 * 1024 + 1];
    let keys = crate::SessionKeys::from_urk([7; 32])?;
    let mut document = manifest("alice", &db);
    let path = selected.to_str().context("fixture path must be UTF-8")?;
    document.source_library = format!("{:x}", Sha256::digest(path.as_bytes()));
    let mut wire = Vec::new();
    seal_to_stage(&keys, "alice", document, db.as_slice(), &mut wire, || Ok(()))?;

    let operation = ArchiveOperation::from_v4_bytes(*uuid::Uuid::new_v4().as_bytes())?;
    let stage = |wire: &[u8], target: &Path| {
        let mut bytes = wire;
        BackupArchiveHeader::read(&mut bytes)?.authenticate(&keys, "alice")?
            .stage_for_restore(bytes, root, target, operation, || Ok(()))
    };

    let mut altered = wire.clone();
    if let Some(last) = altered.last_mut() { *last ^= 1; }
    assert!(stage(&wire, &unrelated).is_err());
    assert!(stage(&altered, &selected).is_err());
    assert!(stage(&wire[..wire.len() - 1], &selected).is_err());
    assert_eq!(fs::read_dir(root)?.count(), 2, "no partial upload is retained");

    let pending = stage(&wire, &selected)?;
    let handoff = pending.handoff();
    let ready = pending.publish(|| Ok(()))?;
    let actual = ready.consume(|reader| {
        let mut received = Vec::new();
        reader.read_to_end(&mut received)?;
        Ok(received)
    })?;
    assert_eq!(actual, wire);
    handoff.discard(root)?;
    assert_eq!(fs::read_dir(root)?.count(), 2);
    assert_eq!(fs::read(&selected)?, b"active library remains untouched");
    Ok(())
}
