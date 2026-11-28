// ── small helpers ──

pub fn open_store() -> Result<LocalStore> {
    validate_client_config()?;
    crate::snapshot::ensure_no_incomplete_restore(&data_dir())?;
    open_store_with_profile().map(|(_, store)| store)
}

/// Return the identity captured for this exact connection, not a later config read.
pub fn open_store_with_profile() -> Result<(PathBuf, LocalStore)> {
    validate_client_config()?;
    let root = data_dir();
    crate::snapshot::ensure_no_incomplete_restore(&root)?;
    check_runtime_profile(&root)?;
    respire_core_sdk::set_index_root(&root)?;
    let db = database_path(&root)?;
    let store = if RUNTIME_PROFILE.get().is_some() {
        LocalStore::open_existing(&db)
    } else {
        open_or_initialize_models(&db)
    }?;
    respire_core_sdk::profiles::set_active(store.active_selection()?);
    crate::embedding::install_transport()?;
    Ok((root, store))
}

/// Bootstrap under actual runtime library ownership without opening business access.
pub fn open_runtime_store(ownership:&crate::lock::LibraryLock)->Result<(LocalStore,Option<String>)> {
    let root=data_dir();check_runtime_profile(&root)?;
    anyhow::ensure!(ownership.protects(&root)?,"runtime bootstrap requires matching library ownership");
    let expected=crate::snapshot::runtime_bootstrap_target(&root)?;
    respire_core_sdk::set_index_root(&root)?;
    let store=open_or_initialize_models(&database_path(&root)?)?;
    store.clear_pending_selection(None)?;
    if let Some(id)=expected.as_ref() {
        store.verify_sqlite_integrity()?;
        anyhow::ensure!(store.snapshot_quarantine_id()?.as_ref()==Some(id),"opened library does not match the restored snapshot transaction");
    }
    respire_core_sdk::profiles::set_active(store.active_selection()?);
    crate::embedding::install_transport()?;
    Ok((store,expected))
}

/// Initialize the runtime's owned library after normal service is restored.
/// Disabled startup deliberately leaves a new database uncreated.
pub fn initialize_runtime_store() -> Result<LocalStore> {
    validate_client_config()?;
    anyhow::ensure!(!service_paused(), "database initialization requires normal service");
    let root = data_dir();
    crate::snapshot::ensure_no_incomplete_restore(&root)?;
    check_runtime_profile(&root)?;
    respire_core_sdk::set_index_root(&root)?;
    let store = open_or_initialize_models(&database_path(&root)?)?;
    store.clear_pending_selection(None)?;
    Ok(store)
}

fn open_or_initialize_models(path: &Path) -> Result<LocalStore> {
    LocalStore::open_with_model_default(path, || {
        if respire_core_sdk::onnx::has_legacy_settings()? { return Ok(None); }
        let mut selection = crate::embedding::default_selection()?;
        selection.generation = respire_core_sdk::profiles::generation_key_for_profile(&selection)?;
        Ok(Some(selection))
    })
}

/// New libraries use rsrs.db; opening an existing library never renames it.
pub fn database_path(root: &Path) -> Result<PathBuf> {
    let current = root.join("rsrs.db");
    let legacy = root.join("onememory.db");
    anyhow::ensure!(!(current.exists() && legacy.exists()),
        "both rsrs.db and the legacy database exist; select the intended library explicitly");
    Ok(if current.exists() || !legacy.exists() { current } else { legacy })
}

/// Read the selected source as one committed snapshot, without creating plaintext files.
/// Sunk from the client-side login_transaction module (WP-2c): the engine-side service
/// tests and the login flow both read this committed snapshot; the client module
/// re-exports the call through `crate::service::local_snapshot`.
pub fn local_snapshot(source_secret: Option<&str>) -> Result<String> {
    let session = crate::auth::read_session_json()?;
    let user = session["user"].as_str().unwrap_or("local");
    let alias = session["keyring_account"].as_str().unwrap_or(user);
    let version = session["vault_version"].as_i64().unwrap_or(1);
    let password = if version == 1 {
        match source_secret.or_else(|| session["pass"].as_str()) {
            Some(value) => value.to_owned(),
            None => match session["keyring_backend"].as_str() {
                Some(backend) => crate::keystore::read_imported_v1_password(alias, backend)?,
                None => crate::keystore::load_login_pass(alias).ok_or_else(|| anyhow::Error::new(
                    crate::auth::SessionCredentialRequired("original source password required")))?,
            },
        }
    } else { String::new() };
    let keys = crate::auth::unlock_session_keys(&session, &password,
        if version == 1 { None } else { source_secret }, alias)?;
    local_snapshot_with_keys(&keys)
}

/// Read the selected committed database with explicitly verified host material.
pub fn local_snapshot_with_keys(keys: &crate::memory::SessionKeys) -> Result<String> {
    let directory = crate::service::data_dir();
    let path = crate::service::database_path(&directory)?;
    anyhow::ensure!(path.is_file(), "selected source has no local library; nothing was migrated");
    let database = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut statement = database.prepare("SELECT id,ciphertext,nonce,COALESCE(parent_id,'') FROM memories WHERE COALESCE(deleted,0)=0 ORDER BY id")?;
    let mut rows = statement.query([])?;
    let mut entries = Vec::new();
    while let Some(row) = rows.next()? {
        let mut stored = crate::memory::model::StoredMemory::new_pending(row.get(0)?, String::new());
        stored.ciphertext = row.get(1)?;
        stored.nonce = row.get(2)?;
        stored.local_parent_id = row.get(3)?;
        entries.push(crate::memory::MemoryEngine::open(&keys, &stored)
            .context("source record cannot be decrypted; original library was preserved")?);
    }
    Ok(serde_json::to_string(&entries)?)
}

