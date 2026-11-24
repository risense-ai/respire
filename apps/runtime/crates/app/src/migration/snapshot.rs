fn snapshot_profile(profile: &Profile, stage: &Path, profiles: &[Profile], current_namespace: bool) -> Result<()> {
    snapshot_profile_checkpoint(profile, stage, profiles, current_namespace, || Ok(()))
}

fn snapshot_profile_checkpoint(profile: &Profile, stage: &Path, profiles: &[Profile], current_namespace: bool,
    before_transform: impl FnOnce() -> Result<()>) -> Result<()> {
    snapshot_profile_with_material(profile, stage, profiles, current_namespace, false, None, false,
        |_| before_transform())
}

fn snapshot_profile_with_material(profile: &Profile, stage: &Path, profiles: &[Profile], current_namespace: bool,
    host_control: bool, material: Option<&HostMigrationMaterial>, resume: bool,
    before_transform: impl FnOnce(bool) -> Result<()>) -> Result<()> {
    // Reading a held SQLite library lock detects an active original writer without
    // stopping it or modifying its lock file.
    let old_lock = profile.source.join("lock.db");
    let mut legacy_active = false;
    if old_lock.is_file() {
        let connection = Connection::open_with_flags(old_lock, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.busy_timeout(Duration::from_millis(100))?;
        match connection.query_row("PRAGMA schema_version", [], |row| row.get::<_, i64>(0)) {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(error, _))
                if matches!(
                    error.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                ) =>
            {
                legacy_active = true;
                eprintln!("legacy runtime is active: migration captures committed data at the snapshot; use rsrs after upgrading; later unsynced old writes remain in the original library");
            }
            Err(error) => return Err(error.into()),
        }
    }
    let session_path = profile.source.join("session.json");
    let original_session = if session_path.is_file() {
        Some(std::fs::read(&session_path)?)
    } else {
        None
    };
    if !resume { copy_tree(&profile.source, stage, true)?; }
    let database = crate::service::database_path(stage)?;
    validate_schema(&database)?;
    let mut prepared = if host_control {
        match original_session.as_ref() {
            Some(bytes) => Some(host_material::prepare(profile, bytes, &database, material)?),
            None => {
                anyhow::ensure!(material.is_none_or(|value| value.candidates.is_empty() && value.custody.is_none()),
                    "host material has no matching source session");
                anyhow::ensure!(encrypted_count(&database)? == 0,
                    "legacy library has ciphertext but no session; original data was preserved");
                validate_live_memories(&database, None)?;
                None
            }
        }
    } else { None };
    let needs_custody = prepared.as_ref().and_then(|plan| plan.selected).is_some();
    before_transform(needs_custody)?;
    if let Some(index) = prepared.as_ref().and_then(|plan| plan.selected) {
        let candidate = material.and_then(|value| value.candidates.get(index))
            .context("selected host migration material disappeared")?;
        return Err(host_material::CredentialsRequired(json!({"state":"credentials_required",
            "session_sha256":candidate.session_sha256,"candidate_index":index})).into());
    }
    if database.is_file() {
        private_file(&database)?;
    }
    let mut keyring_backend = None;
    let mut keyring_account = None;
    let mut credential_references = Vec::new();
    if let Some(bytes) = original_session.as_ref() {
        let mut session: Value = serde_json::from_slice(bytes)
            .context("legacy session is invalid; source was preserved")?;
        let recovered = if host_control {
            let plan = prepared.take().context("verified host migration plan missing")?;
            session = plan.session;
            credential_references = plan.references;
            plan.keys
        } else {
            migrate_credentials(profile, stage, &database, &mut session, &mut credential_references)?
        };
        if current_namespace {
            if let Some(keys) = recovered.as_ref() {
                migrate_namespace_database(&database, keys)?;
                let alias = session["keyring_account"].as_str().context("migrated credential alias is missing")?;
                let (salt, wrapped, nonce) = if host_control {
                    let candidate = material.and_then(|value| value.candidates.first())
                        .context("final host migration factors missing")?;
                    host_material::rewrap(candidate, keys)?
                } else { rewrap_namespace(&session, alias, keys)? };
                session["kdf_salt"] = json!(salt);
                session["wrapped_urk"] = json!(wrapped);
                session["urk_nonce"] = json!(nonce);
            }
            session["crypto_namespace"] = json!(crate::memory::crypto::RSRS_PREFIX);
            if let Some(keys) = recovered.as_ref() {
                if host_control {
                    let candidate = material.and_then(|value| value.candidates.first())
                        .context("final host migration factors missing")?;
                    let restored = host_material::unlock(&session, candidate)?;
                    anyhow::ensure!(restored.urk == keys.urk, "migration changed the original data key");
                } else { verify_local_session(&session)?; }
            }
        }
        keyring_backend = session.get("keyring_backend").cloned();
        keyring_account = session.get("keyring_account").cloned();
        rewrite_address(&mut session);
        write_json(&stage.join("session.json"), &session)?;
    } else if encrypted_count(&database)? > 0 {
        bail!("legacy library has ciphertext but no session; recover the original keys before migration");
    } else {
        validate_live_memories(&database, None)?;
    }
    for name in ["client.json", "agent.json"] {
        let path = stage.join(name);
        if path.is_file() {
            let mut config: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
            rewrite_address(&mut config);
            if let Some(value) = config["data_dir"]
                .as_str()
                .filter(|value| !value.is_empty())
            {
                let old = crate::service::expand_tilde(value);
                if old
                    .components()
                    .any(|component| matches!(component, std::path::Component::ParentDir))
                {
                    bail!("legacy configuration contains a parent-directory path; originals were preserved");
                }
                // Windows paths captured by the host may use the extended prefix.
                let comparable = |path: &Path| -> Result<PathBuf> {
                    if cfg!(windows) {
                        crate::service::canonical_profile_path(path)
                            .context("cannot resolve legacy configuration path; originals were preserved")
                    } else {
                        Ok(path.to_path_buf())
                    }
                };
                let old = comparable(&old)?;
                let mut found = None;
                for candidate in profiles {
                    let source = comparable(&candidate.source)?;
                    let depth = source.components().count();
                    if old.starts_with(&source) && found.as_ref().is_none_or(|(_, _, best)| depth >= *best) {
                        found = Some((candidate, source, depth));
                    }
                }
                if let Some((found, source, _)) = found {
                    let suffix = old.strip_prefix(&source)?;
                    let destination = if suffix.as_os_str().is_empty() {
                        found.destination.clone()
                    } else {
                        found.destination.join(suffix)
                    };
                    config["data_dir"] = json!(destination);
                } else {
                    bail!("legacy configuration uses an external data directory; originals were preserved; set RSRS_DATA_DIR explicitly to open it");
                }
            }
            write_json(&path, &config)?;
        }
    }
    if let Some(bytes) = original_session {
        if std::fs::read(session_path)? != bytes {
            bail!("legacy identity changed during migration; retry after closing the old runtime");
        }
    }
    if current_namespace && database.is_file() && database.file_name().is_some_and(|name| name != "rsrs.db") {
        std::fs::rename(&database, stage.join("rsrs.db"))?;
    }
    write_json(
        &stage.join(RECEIPT),
        &json!({"schema":2,"complete":true,"source_identity":profile.identity,
        "source":profile.source,"destination":profile.destination,
            "api_default":crate::service::DEFAULT_SERVER_ADDR,"original_preserved":true,
            "keyring_backend":keyring_backend,"keyring_account":keyring_account,
            "credential_references":credential_references,
        "snapshot_at":chrono::Utc::now().to_rfc3339(),"legacy_runtime_active":legacy_active,
        "snapshot_only":true,"namespace_migrated":current_namespace}),
    )?;
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path, root: bool) -> Result<()> {
    private_directory(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        let text = name.to_string_lossy();
        // Session factors stay in memory until the host has checkpointed their slots.
        if excluded(&text) || text == RECEIPT || root && matches!(text.as_ref(), "accounts" | "session.json") {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            bail!("legacy library contains a symbolic link; no external data was copied");
        }
        let target = destination.join(&name);
        if kind.is_dir() {
            copy_tree(&entry.path(), &target, false)?;
        } else if kind.is_file() {
            if text.ends_with(".db") || text.ends_with(".sqlite") || text.ends_with(".sqlite3") {
                snapshot_database(&entry.path(), &target)?;
            } else {
                std::fs::copy(entry.path(), &target)?;
                private_file(&target)?;
            }
        }
    }
    Ok(())
}

fn snapshot_database(source: &Path, destination: &Path) -> Result<()> {
    let source = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    source.busy_timeout(Duration::from_millis(200))?;
    let mut destination = Connection::open(destination)?;
    let backup = Backup::new(&source, &mut destination)?;
    let began = Instant::now();
    loop {
        match backup.step(256)? {
            StepResult::Done => break,
            _ if began.elapsed() >= Duration::from_secs(60) => {
                bail!("legacy SQLite snapshot remained busy; migration was not published")
            }
            _ => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    drop(backup);
    let result: String = destination.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if result != "ok" {
        bail!("legacy SQLite snapshot failed integrity validation");
    }
    if let Some(path) = destination.path() {
        private_file(Path::new(path))?;
    }
    Ok(())
}

fn encrypted_count(path: &Path) -> Result<usize> {
    if !path.is_file() {
        return Ok(0);
    }
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let has_table: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='memories')",
        [],
        |row| row.get(0),
    )?;
    if !has_table {
        return Ok(0);
    }
    let count: i64 = db.query_row(
        "SELECT count(*) FROM memories WHERE ciphertext <> ''",
        [],
        |row| row.get(0),
    )?;
    Ok(usize::try_from(count)?)
}

/// Verify live payloads before credential import or a completion receipt. This
/// reads the snapshot without upgrading it; deleted rows remain opaque bytes.
fn validate_live_memories(path: &Path, keys: Option<&crate::memory::SessionKeys>) -> Result<()> {
    if !path.is_file() {
        return Ok(());
    }
    let database = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let columns = database
        .prepare("PRAGMA table_info(memories)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if columns.is_empty() {
        return Ok(());
    }
    // Older compatible schemas acquire deleted=0 only on normal store open.
    // A NULL marker is not evidence that a memory was deleted.
    let query = if columns.iter().any(|column| column == "deleted") {
        "SELECT ciphertext, nonce FROM memories WHERE COALESCE(deleted, 0) = 0"
    } else {
        "SELECT ciphertext, nonce FROM memories"
    };
    let mut statement = database.prepare(query)?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let keys = keys.ok_or_else(|| anyhow!(
            "legacy live ciphertext requires usable session keys; migration was not published"
        ))?;
        let mut stored = crate::memory::model::StoredMemory::new_pending(String::new(), String::new());
        stored.ciphertext = row.get(0).context("legacy live ciphertext is missing or invalid; migration was not published")?;
        stored.nonce = row.get(1).context("legacy live ciphertext nonce is missing or invalid; migration was not published")?;
        // Reuse normal supported payload decoding, including legacy defaults.
        // Do not include decrypted content or identifiers in a failure report.
        crate::memory::MemoryEngine::open(keys, &stored).map_err(|_| anyhow!(
            "legacy live ciphertext is unreadable with the recovered account key; migration was not published; original rows were preserved"
        ))?;
    }
    Ok(())
}

fn validate_schema(path: &Path) -> Result<()> {
    if !path.is_file() {
        return Ok(());
    }
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let columns = db
        .prepare("PRAGMA table_info(memories)")?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if columns.is_empty() {
        return Ok(());
    }
    let compatible_id = columns.iter().any(|(name, kind, primary)| {
        name == "id" && kind.eq_ignore_ascii_case("TEXT") && *primary == 1
    }) && columns
        .iter()
        .filter(|(_, _, primary)| *primary > 0)
        .count()
        == 1;
    let required = ["user", "ciphertext", "nonce", "created_at", "updated_at"];
    if !compatible_id
        || columns.iter().any(|(name, _, _)| name == "tag_hashes")
        || required
            .iter()
            .any(|field| !columns.iter().any(|(name, _, _)| name == field))
    {
        return Err(InvalidSchema.into());
    }
    Ok(())
}

