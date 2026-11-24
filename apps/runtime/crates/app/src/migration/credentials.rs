fn candidate_values(
    profile: &Profile,
    user: &str,
    slot: &str,
    fields: &[&str],
    session: &Value,
) -> Vec<String> {
    let mut values = Vec::new();
    for field in fields {
        if let Some(value) = session[field].as_str().filter(|value| !value.is_empty()) {
            values.push(value.to_owned());
        }
    }
    let account = if let Some(alias) = session["keyring_account"].as_str().filter(|value| !value.is_empty()) {
        alias
    } else if user.trim().is_empty() {
        "local"
    } else {
        user.trim()
    };
    for service in profile.services {
        values.extend(crate::keystore::read_credentials(
            service,
            &format!("{slot}:{account}"),
        ));
    }
    if slot == "super" {
        if let Ok(value) = crate::env::var("RSRS_SUPER") {
            if !value.is_empty() {
                values.push(value);
            }
        }
    }
    let mut seen = BTreeSet::new();
    values.retain(|value| seen.insert(value.clone()));
    values
}

// Keep attempted backend references even if a host write or later transform fails.
fn import_migration_credential(profile: &Profile, stage: &Path, references: &mut Vec<Value>,
    alias: &str, slot: &str, value: &str) -> Result<&'static str> {
    let mut checkpoint = |backend: &str| -> Result<()> {
        anyhow::ensure!(backend != "kernel", "migration requires a persistent host credential backend");
        let reference = json!({"slot":slot,"backend":backend});
        if !references.contains(&reference) { references.push(reference); }
        write_json(&stage.join(RECEIPT), &json!({"schema":2,"complete":false,
            "source_identity":profile.identity,"source":profile.source,"destination":profile.destination,
            "original_preserved":true,"keyring_account":alias,"credential_references":references}))
    };
    let backend = crate::keystore::import_credential_checkpoint(alias, slot, value, &mut checkpoint)?;
    // An already matching host credential does not invoke the pre-write callback.
    checkpoint(backend)?;
    Ok(backend)
}

fn migrate_credentials(profile: &Profile, stage: &Path, database: &Path, session: &mut Value,
    references: &mut Vec<Value>) -> Result<Option<crate::memory::SessionKeys>> {
    let user = session["user"].as_str().unwrap_or("").to_owned();
    let version = session["vault_version"].as_i64().unwrap_or(1);
    anyhow::ensure!((1..=4).contains(&version), "unsupported vault version; original credentials were preserved");
    let passes = candidate_values(profile, &user, "pass", &["pass"], session);
    let supers = candidate_values(profile, &user, "super",
        if version >= 4 { &["secret_key", "super"] } else { &["super"] }, session);
    let legacy_secret = if matches!(version, 1 | 3) {
        if let Some(backend) = session.get("legacy_secret_backend") {
            let backend = backend.as_str().context("invalid legacy secret backend")?;
            anyhow::ensure!(backend != "kernel", "legacy factor requires a persistent host backend");
            let alias = session["keyring_account"].as_str().unwrap_or(&user);
            Some(crate::keystore::read_imported(alias, "legacy-secret", backend)?)
        } else {
            session[if version == 1 { "secret" } else { "secret_key" }]
                .as_str().filter(|value| !value.is_empty()).map(str::to_owned)
        }
    } else { None };
    let (keys, super_pass, legacy_pass) = if session["wrapped_urk"].as_str().filter(|value| !value.is_empty()).is_none() {
        anyhow::ensure!(encrypted_count(database)? == 0,
            "legacy session has no wrapped key; recover the original keys before migration");
        validate_live_memories(database, None)?;
        (None, supers.first().cloned(), passes.first().cloned())
    } else {
        let mut unlocked = None;
        if version >= 2 {
            for super_pass in &supers {
                if let Ok(keys) = crate::auth::unlock_session_keys(session, "", Some(super_pass), &user) {
                    unlocked = Some((Some(keys), Some(super_pass.clone()), None));
                    break;
                }
            }
        } else {
            for pass in &passes {
                if let Ok(keys) = crate::auth::unlock_session_keys(session, pass, None, &user) {
                    unlocked = Some((Some(keys), None, Some(pass.clone())));
                    break;
                }
            }
        }
        let recovered = unlocked.ok_or_else(|| anyhow!("legacy vault could not be unlocked; supply its original recovery key/password; no new vault was created"))?;
        validate_live_memories(database, recovered.0.as_ref())?;
        recovered
    };
    let alias = format!("legacy-{}", &profile.identity[..16]);
    if let Some(value) = super_pass {
        let backend = import_migration_credential(profile, stage, references, &alias, "super", &value)?;
        session["keyring_backend"] = json!(backend);
    }
    if let Some(value) = legacy_pass.as_ref().or_else(|| passes.first()) {
        let backend = import_migration_credential(profile, stage, references, &alias, "pass", value)?;
        if version == 1 { session["keyring_backend"] = json!(backend); }
    }
    session.as_object_mut().context("legacy session must be an object")?.remove("legacy_secret_backend");
    if let Some(value) = legacy_secret {
        let backend = import_migration_credential(profile, stage, references, &alias, "legacy-secret", &value)?;
        session["legacy_secret_backend"] = json!(backend);
    }
    session["keyring_account"] = json!(alias);
    session["vault_version"] = json!(version);
    for field in ["super", "pass", "secret", "secret_key"] {
        session.as_object_mut().context("legacy session must be an object")?.remove(field);
    }
    if let Some(keys) = keys.as_ref() {
        let backend = session["keyring_backend"].as_str().context("migrated credential backend is missing")?;
        let primary = if version == 1 { crate::keystore::read_imported_v1_password(&alias, backend)? }
            else { crate::keystore::read_imported(&alias, "super", backend)? };
        let restored = crate::auth::unlock_session_keys(session,
            if version == 1 { &primary } else { "" },
            if version == 1 { None } else { Some(primary.as_str()) }, &alias)?;
        anyhow::ensure!(restored.urk == keys.urk, "migration host credentials changed the original data key");
    }
    Ok(keys)
}

/// Recalculate internal keys while retaining every original user factor. Older
/// two-factor vaults keep their version and factors; migration is not a reset.
fn rewrap_namespace(session: &Value, alias: &str, keys: &crate::memory::SessionKeys) -> Result<(String, String, String)> {
    use crate::memory::crypto;
    let version = session["vault_version"].as_i64().unwrap_or(1);
    let salt = crypto::random_hex(16);
    let backend = session["keyring_backend"].as_str().context("migrated credential backend is missing")?;
    let primary = if version == 1 { crate::keystore::read_imported_v1_password(alias, backend)? }
        else { crate::keystore::read_imported(alias, "super", backend)? };
    let legacy = || -> Result<String> {
        let backend = session["legacy_secret_backend"].as_str().context("original legacy factor backend is missing")?;
        anyhow::ensure!(backend != "kernel", "legacy factor requires a persistent host backend");
        crate::keystore::read_imported(alias, "legacy-secret", backend)
    };
    let kek = match version {
        1 => crypto::derive_rsrs_password_kek(&primary, &legacy()?, &salt)?,
        2 => crypto::derive_super_kek(&primary, &salt)?,
        3 => crypto::derive_rsrs_vault_kek(&primary, &legacy()?, &salt)?,
        4 => crypto::derive_rsrs_kek(&primary, &salt)?,
        _ => bail!("unsupported vault version; original credentials were preserved"),
    };
    let (nonce, wrapped) = crypto::wrap_key(&keys.urk, &kek)?;
    anyhow::ensure!(crypto::unwrap_key(&wrapped, &nonce, &kek)? == keys.urk,
        "new key wrap did not round-trip; migration was not completed");
    Ok((salt, format!("{}{wrapped}", crypto::RSRS_PREFIX), nonce))
}

/// A new library is a new encrypted projection. The original immutable sync
/// journal remains in the old library; changing its operation bodies would
/// invalidate retries. The new projection queues fresh operations on normal open.
fn migrate_namespace_database(path: &Path, source: &crate::memory::SessionKeys) -> Result<()> {
    if !path.is_file() { return Ok(()); }
    let mut database = Connection::open(path)?;
    crate::transport::local::register_witness_functions(&database)?;
    let tables = database.prepare("SELECT name FROM sqlite_master WHERE type='table'")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<BTreeSet<_>>>()?;
    if !tables.contains("memories") { return Ok(()); }
    let columns = database.prepare("PRAGMA table_info(memories)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<BTreeSet<_>>>()?;
    let query = if columns.contains("embedding_enc") {
        "SELECT id,ciphertext,nonce,embedding_enc FROM memories ORDER BY id"
    } else { "SELECT id,ciphertext,nonce,'' FROM memories ORDER BY id" };
    let rows = database.prepare(query)?.query_map([], |row| Ok((
        row.get::<_, String>(0)?, row.get::<_, String>(1)?,
        row.get::<_, String>(2)?, row.get::<_, String>(3)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let target = crate::memory::SessionKeys::from_urk(source.urk)?;
    let mut converted = Vec::with_capacity(rows.len());
    for (id, ciphertext, nonce, embedding) in rows {
        let plaintext = source.decrypt_content(&ciphertext, &nonce)
            .map_err(|_| anyhow!("a source record cannot be decrypted; full migration was not published; original library and keys were preserved"))?;
        let (new_nonce, new_ciphertext) = target.encrypt_content(&plaintext)?;
        anyhow::ensure!(target.decrypt_content(&new_ciphertext, &new_nonce)? == plaintext,
            "migrated ciphertext did not round-trip; no completion marker was written");
        let embedding = source.migrate_embedding(&embedding)?;
        converted.push((id, ciphertext, new_ciphertext, new_nonce, embedding));
    }
    let transaction = database.transaction()?;
    transaction.execute_batch("DROP TRIGGER IF EXISTS sync_capture_insert; DROP TRIGGER IF EXISTS sync_capture_update;")?;
    for table in ["sync_outbox", "sync_inbox", "sync_remote_heads", "sync_conflict_analysis", "sync_resolution_outbox"] {
        if tables.contains(table) { transaction.execute(&format!("DELETE FROM {table}"), [])?; }
    }
    if tables.contains("meta") {
        transaction.execute("DELETE FROM meta WHERE key IN ('sync_v2_cursor','sync_v2_snapshot_done','sync_v2_snapshot_until','sync_cursor')", [])?;
    }
    for (id, old, ciphertext, nonce, embedding) in converted {
        let sql = format!("UPDATE memories SET ciphertext=?1,nonce=?2{}{} WHERE id=?3",
            if columns.contains("dirty") { ",dirty=1" } else { "" },
            if columns.contains("embedding_enc") { ",embedding_enc=?4" } else { "" });
        if columns.contains("embedding_enc") {
            transaction.execute(&sql, rusqlite::params![ciphertext, nonce, id, embedding])?;
        } else {
            transaction.execute(&sql, rusqlite::params![ciphertext, nonce, id])?;
        }
        if tables.contains("core_artifacts") {
            transaction.execute("UPDATE core_artifacts SET source=?1 WHERE memory_id=?2 AND source=?3",
                rusqlite::params![ciphertext, id, old])?;
        }
    }
    if tables.contains("sync_outbox") && tables.contains("sync_base") {
        // Preserve acknowledged base revisions. Assign new operation IDs to the
        // newly encrypted projection rather than changing immutable old bodies.
        transaction.execute_batch("INSERT INTO sync_outbox (op_id,id,base_rev,user,ciphertext,nonce,updated_at,deleted)
            SELECT lower(hex(randomblob(16))),m.id,b.rev,m.user,m.ciphertext,m.nonce,m.updated_at,m.deleted
            FROM memories m LEFT JOIN sync_base b ON b.id=m.id WHERE m.dirty=1;")?;
    }
    transaction.commit()?;
    database.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")?;
    let integrity: String = database.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    anyhow::ensure!(integrity == "ok", "migrated database failed integrity validation");
    Ok(())
}

fn rewrite_address(value: &mut Value) {
    let Some(address) = value["addr"].as_str() else {
        return;
    };
    let host = address
        .strip_prefix("https://")
        .or_else(|| address.strip_prefix("http://"))
        .and_then(|value| value.split('/').next())
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(
        host.as_str(),
        "1memory.ai"
            | "api.1memory.ai"
            | "memocap.ai"
            | "api.memocap.ai"
            | "respire.ai"
            | "api.respire.ai"
    ) {
        value["addr"] = json!(crate::service::DEFAULT_SERVER_ADDR);
    }
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    use std::io::Write;
    anyhow::ensure!(!std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()),
        "migration metadata must not be a symbolic link");
    let parent = path.parent().context("migration metadata has no parent directory")?;
    let temporary = parent.join(format!(".rsrs-metadata-{}.tmp", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)] {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut file = options.open(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(value)?)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)?;
        #[cfg(unix)] std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() && temporary.is_file() { let _ = std::fs::remove_file(&temporary); }
    result
}

fn private_directory(path: &Path) -> Result<()> {
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            bail!("migration destination is a symbolic link; no external data was written");
        }
    }
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn private_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)?.permissions().mode();
        std::fs::set_permissions(
            path,
            std::fs::Permissions::from_mode(0o600 | (mode & 0o100)),
        )?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

