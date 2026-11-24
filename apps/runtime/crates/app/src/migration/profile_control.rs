pub fn migrate_profile(source_id: &str, account: &str) -> Result<Value> {
    migrate_profile_inner(source_id, account, None, None)
}

fn migrate_profile_inner(source_id: &str, account: &str,
    control: Option<&respire_protocol::migration_control::HostMigrationOperation>,
    material: Option<&HostMigrationMaterial>) -> Result<Value> {
    crate::service::require_profile_change_host()?;
    anyhow::ensure!(!account.is_empty() && account != "main" && safe_name(account) == account,
        "migration needs a new account name containing letters, digits, '-' or '_'");
    let home = crate::service::home_dir()?;
    let lock_root = home.join(".rsrs-migration-lock");
    private_directory(&lock_root)?;
    let _lock = crate::lock::LibraryLock::acquire(&lock_root, Duration::from_secs(30))?;
    migrate_profile_locked(&home, source_id, account, control, material)
}

// The caller holds the migration lock for both admission and recovery.
fn migrate_profile_locked(home: &Path, source_id: &str, account: &str,
    control: Option<&respire_protocol::migration_control::HostMigrationOperation>,
    material: Option<&HostMigrationMaterial>) -> Result<Value> {
    if let Some(original) = control {
        if intent::load(home, original)?.is_some_and(|record| record.phase == intent::Phase::RejectedSchema) {
            return Err(SchemaRejected.into());
        }
    }
    let mut profiles = legacy_profiles(home)?;
    let selected = profiles.iter().position(|profile| profile.identity == source_id)
        .ok_or_else(|| anyhow!("legacy source is no longer available; list sources again"))?;
    let source_receipt = profiles[selected].source.join(SOURCE_RECEIPT);
    if let Ok(metadata) = std::fs::symlink_metadata(&source_receipt) {
        anyhow::ensure!(!metadata.file_type().is_symlink(), "source migration marker is a symbolic link; nothing was migrated");
        let receipt: Value = serde_json::from_slice(&std::fs::read(&source_receipt)?)?;
        if receipt["complete"] == true && receipt["source_identity"].as_str() == Some(source_id) {
            return Ok(json!({"state":"already_migrated","dir":receipt["destination"]}));
        }
    }
    if let Some(destination) = migrated_destination(source_id)? {
        return Ok(json!({"state":"already_migrated","dir":destination}));
    }
    let accounts = crate::service::accounts_root();
    if accounts.exists() {
        anyhow::ensure!(!std::fs::symlink_metadata(&accounts)?.file_type().is_symlink(),
            "account root is a symbolic link; no external data was written");
    }
    profiles[selected].destination = accounts.join(account);
    anyhow::ensure!(!profiles[selected].destination.exists(), "migration account already exists; nothing was overwritten");
    let staging_root = home.join(".rsrs-migration-staging");
    private_directory(&staging_root)?;
    intent::reject_conflicting_admission(home, control, source_id, account)?;
    let mut admitted = match control {
        Some(original) => Some(intent::admit_or_load(home, original, &profiles[selected])?),
        None => None,
    };
    let stage = match admitted.as_mut() {
        Some(record) => {
            anyhow::ensure!(matches!(record.phase, intent::Phase::Copying | intent::Phase::AwaitingCredentials),
                "migration has passed its replay-safe boundary; retain the original operation and staging data");
            if record.phase == intent::Phase::AwaitingCredentials {
                intent::verify_snapshot(record)?;
            }
            if record.phase == intent::Phase::Copying {
                // Never adopt or overwrite a partial snapshot. Retain it for inspection.
                record.stage = staging_root.join(uuid::Uuid::new_v4().to_string());
                intent::save(home, record)?;
            }
            record.stage.clone()
        }
        None => staging_root.join(uuid::Uuid::new_v4().to_string()),
    };
    let resume = admitted.as_ref().is_some_and(|record| record.phase == intent::Phase::AwaitingCredentials);
    if admitted.as_ref().is_none_or(|record| matches!(record.phase, intent::Phase::Copying | intent::Phase::AwaitingCredentials)) {
        let snapshot = snapshot_profile_with_material(&profiles[selected], &stage, &profiles, true,
            control.is_some(), material, resume, |needs_custody| {
            if let Some(record) = admitted.as_mut() {
                intent::verify_source(record, &profiles[selected])?;
                if record.phase == intent::Phase::AwaitingCredentials {
                    intent::verify_snapshot(record)?;
                }
                if needs_custody {
                    if record.phase == intent::Phase::Copying {
                        record.snapshot_sha256 = intent::snapshot_digest(&record.stage)?;
                        record.phase = intent::Phase::AwaitingCredentials;
                        intent::save(home, record)?;
                    }
                } else {
                    record.phase = intent::Phase::Transforming;
                    intent::save(home, record)?;
                }
            }
            Ok(())
        });
        if let Err(error) = snapshot {
            if error.downcast_ref::<InvalidSchema>().is_some() {
                if let Some(record) = admitted.as_mut() {
                    // Only the exact copied snapshot was checked, under the same migration lock.
                    // A failed durable write remains uncertain and must not become a rejection.
                    intent::verify_source(record, &profiles[selected])?;
                    anyhow::ensure!(record.phase == intent::Phase::Copying,
                        "schema rejection crossed the migration side-effect boundary");
                    record.phase = intent::Phase::RejectedSchema;
                    intent::save(home, record)?;
                    return Err(SchemaRejected.into());
                }
            }
            return Err(error);
        }
        if let Some(control) = control {
            let path = stage.join(RECEIPT);
            let mut receipt: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
            receipt["control"] = serde_json::to_value(control)?;
            receipt["control_result"] = json!({"state":"migrated","account":account,
                "dir":profiles[selected].destination,"source":profiles[selected].source,
                "original_preserved":true});
            write_json(&path, &receipt)?;
            if let Some(record) = admitted.as_mut() {
                record.phase = intent::Phase::Ready;
                intent::save(home, record)?;
            }
        }
    }
    if let Some(control) = control {
        anyhow::ensure!(migration_control_receipt(control, &stage.join(RECEIPT))?.is_some(),
            "ready migration snapshot has no matching completion receipt");
    }
    private_directory(&accounts)?;
    // rename must never replace an existing empty directory either.
    std::fs::create_dir(&profiles[selected].destination)
        .context("migration destination changed; nothing was overwritten")?;
    std::fs::remove_dir(&profiles[selected].destination)?;
    std::fs::rename(&stage, &profiles[selected].destination)?;
    write_json(&source_receipt,
        &json!({"schema":1,"complete":true,"source_identity":source_id,
            "destination":profiles[selected].destination,"original_preserved":true,
            "snapshot_at":chrono::Utc::now().to_rfc3339()}))?;
    Ok(json!({"state":"migrated","account":account,"dir":profiles[selected].destination,
        "source":profiles[selected].source,"original_preserved":true}))
}

/// The caller holds exact Paused root/profile ownership; no auth journal is extended.
pub fn control_migrate_profile(
    _ownership: &crate::login_transaction::AuthOwnership,
    original: &respire_protocol::migration_control::HostMigrationOperation,
    query_only: bool,
    material: Option<&HostMigrationMaterial>,
) -> Result<respire_protocol::migration_control::HostMigrationResult> {
    use respire_protocol::migration_control::{HostMigrationOutcome as Outcome,
        HostMigrationResult, HOST_MIGRATION_SCHEMA_V};
    let canonical_id = uuid::Uuid::parse_str(&original.operation_id)?;
    anyhow::ensure!(original.schema_version == HOST_MIGRATION_SCHEMA_V
        && canonical_id.get_version_num() == 4 && canonical_id.to_string() == original.operation_id
        && original.key == original.operation_id
        && original.source_id.len() == 64 && original.source_id.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        && !original.account.is_empty() && original.account != "main" && safe_name(&original.account) == original.account
        && original.fingerprint == hex::encode(Sha256::digest(original.fingerprint_bytes()?)),
        "invalid original migration binding");
    anyhow::ensure!(original.installation_root == crate::service::main_data_dir().canonicalize()?
        && original.profile == crate::service::data_dir().canonicalize()?
        && crate::service::accounts_root().parent().context("migration account root has no parent")?.canonicalize()?
            == original.installation_root, "migration selection changed");
    crate::login_transaction::verify_auth_identity(&original.installation_root, &original.profile, &original.identity)?;
    let result = (|| -> Result<_> { Ok(if query_only {
        let home = crate::service::home_dir()?;
        let lock_root = home.join(".rsrs-migration-lock");
        private_directory(&lock_root)?;
        let _lock = crate::lock::LibraryLock::acquire(&lock_root, Duration::from_secs(30))?;
        match migration_control_result(original)? {
            Some(details) => (Outcome::Completed, details),
            None => match intent::load(&home, original)? {
                Some(record) if matches!(record.phase, intent::Phase::Copying | intent::Phase::AwaitingCredentials) => {
                    crate::service::require_profile_change_host()?;
                    crate::service::ensure_writable()?;
                    anyhow::ensure!(!crate::service::off_mode(), "migration recovery is unavailable while memory is off");
                    migrate_profile_locked(&home, &original.source_id, &original.account, Some(original), material)?;
                    let details = migration_control_result(original)?
                        .context("migration recovery did not publish the original operation")?;
                    (Outcome::Completed, details)
                }
                Some(record) if record.phase == intent::Phase::RejectedSchema => return Err(SchemaRejected.into()),
                _ => (Outcome::Unknown, Value::Null),
            },
        }
    } else {
        crate::service::ensure_writable()?;
        anyhow::ensure!(!crate::service::off_mode(), "migration is unavailable while memory is off");
        let details = migrate_profile_inner(&original.source_id, &original.account, Some(original), material)?;
        let outcome = if details["state"] == "already_migrated" { Outcome::AlreadyMigrated } else { Outcome::Completed };
        (outcome, details)
    }) })();
    crate::login_transaction::verify_auth_identity(&original.installation_root, &original.profile, &original.identity)?;
    let (outcome, details) = match result {
        Ok(result) => result,
        Err(error) => match error.downcast_ref::<host_material::CredentialsRequired>() {
            Some(required) => (Outcome::CredentialsRequired, required.0.clone()),
            None => return Err(error),
        },
    };
    Ok(HostMigrationResult { operation_id: original.operation_id.clone(), fingerprint: original.fingerprint.clone(),
        identity: original.identity.clone(), outcome, details })
}

fn migration_control_result(original: &respire_protocol::migration_control::HostMigrationOperation) -> Result<Option<Value>> {
    let path = crate::service::accounts_root().join(&original.account).join(RECEIPT);
    migration_control_receipt(original, &path)
}

fn migration_control_receipt(original: &respire_protocol::migration_control::HostMigrationOperation, path: &Path) -> Result<Option<Value>> {
    use std::io::Read;
    let destination = crate::service::accounts_root().join(&original.account);
    for ancestor in path.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                anyhow::ensure!(!metadata.file_type().is_symlink(), "migration result path is a symbolic link");
                #[cfg(windows)] {
                    use std::os::windows::fs::MetadataExt;
                    anyhow::ensure!(metadata.file_attributes() & 0x400 == 0, "migration result path is a reparse point");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(metadata.is_file() && metadata.len() <= 64 * 1024, "invalid migration completion marker");
    let mut bytes = Vec::new();
    std::fs::File::open(&path)?.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= 64 * 1024, "migration completion marker exceeds its bound");
    let receipt: Value = serde_json::from_slice(&bytes)?;
    let Some(control) = receipt.get("control") else { return Ok(None); };
    let control: respire_protocol::migration_control::HostMigrationOperation = serde_json::from_value(control.clone())?;
    if control != *original { return Ok(None); }
    anyhow::ensure!(receipt["schema"] == 2 && receipt["complete"] == true
        && receipt["source_identity"].as_str() == Some(original.source_id.as_str())
        && receipt["destination"] == json!(destination)
        && receipt["namespace_migrated"] == true && receipt["original_preserved"] == true
        && receipt["source"].as_str().is_some_and(|source| Path::new(source).is_absolute()),
        "migration completion marker disagrees with the original operation");
    let expected = json!({"state":"migrated","account":original.account,"dir":destination,
        "source":receipt["source"],"original_preserved":true});
    anyhow::ensure!(receipt["control_result"] == expected, "migration result disagrees with its published marker");
    Ok(Some(expected))
}

