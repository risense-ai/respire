//! Reuse the native auth journal for a new, empty offline library.
use super::*;
use respire_protocol::HostAuthKeygenMaterial;
use std::fs;

/// Called only while the native helper owns startup, serving and auth exclusion.
/// This function cannot rotate keys or attach to a previously configured account.
impl AuthOwnership {
    pub fn cold_keygen(
        &self,
        operation: &str,
        key: &str,
        query_only: bool,
        material: &HostAuthKeygenMaterial,
    ) -> Result<HostAuthResult> {
        canonical_operation_id(operation)?;
        let fingerprint = digest(&serde_json::to_vec(&json!({
            "schema_version": respire_protocol::HOST_AUTH_SCHEMA_V,
            "method": "control.auth.cold_keygen",
            "root": self.root, "profile": self.profile,
            "slot": material.keyring_account, "backend": material.keyring_backend,
            "salt": material.kdf_salt, "wrapped": material.wrapped_urk, "nonce": material.urk_nonce,
        }))?);
        let journal = load_journal()?;
        let matched = journal.operations.iter().find(|record| {
            record.operation_id == operation
                || (record.root == self.root && record.idempotency_key == key)
        });
        if let Some(record) = matched {
            ensure!(
                record.operation_id == operation
                    && record.idempotency_key == key
                    && record.root == self.root
                    && record.profile == self.profile
                    && record.original_sha256.is_none()
                    && record.alias == material.keyring_account
                    && record.fingerprint == fingerprint
                    && record.account.is_none()
                    && record.identity.is_none()
                    && record.key_operation.is_none()
                    && record.space_operation.is_none()
                    && record.profile_remove.is_none(),
                "offline keygen receipt scope mismatch"
            );
        }
        if let Some(active) = journal.active.as_ref() {
            // Adjacent auth operations are never recovered through a cold keygen.
            ensure!(
                query_only
                    && matched.is_some()
                    && active.operation_id.as_deref() == Some(operation),
                "query the original offline operation"
            );
            let record = matched.context("original cold keygen receipt is missing")?;
            if record.candidate.is_some() {
                verify_host_keygen_candidate(record, Some(material))?;
            } else {
                // Even an Admitted record without a published candidate pins
                // the original wrapped key through its nonsecret fingerprint.
                crate::memory::SessionKeys::unlock_v4(
                    &material.super_password,
                    &material.kdf_salt,
                    &material.wrapped_urk,
                    &material.urk_nonce,
                )
                .context("original offline keygen factor is required")?;
            }
            let identity = self.cold_keygen_identity(operation)?;
            self.recover_keygen(&identity, Some(material))?;
        }
        if matched.is_none() {
            ensure!(!query_only, "offline keygen has no durable receipt");
            for database in ["rsrs.db", "onememory.db"] {
                for suffix in ["", "-wal", "-shm", "-journal"] {
                    match fs::symlink_metadata(self.profile.join(format!("{database}{suffix}"))) {
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                        Ok(_) => anyhow::bail!("offline keygen requires an empty library"),
                    }
                }
            }
            ensure!(
                matches!(
                    self.cold_keygen_identity(operation)?,
                    HostAuthIdentity::Absent
                ),
                "offline keygen cannot replace an existing session"
            );
        }
        let identity = self.cold_keygen_identity(operation)?;
        let mut result = control_keygen_with_fingerprint(
            self,
            &identity,
            operation,
            key,
            false,
            query_only || matched.is_some(),
            Some(material),
            fingerprint,
        )?;
        // The host already owns the factor. Only return the verified receipt.
        result.super_password = None;
        Ok(result)
    }

    fn cold_keygen_identity(&self, operation: &str) -> Result<HostAuthIdentity> {
        let Some(bytes) = read_original(&self.profile)? else {
            return Ok(HostAuthIdentity::Absent);
        };
        let value: Value = serde_json::from_slice(&bytes)?;
        ensure!(
            value.get("user").is_none() && value["auth_operation_id"].as_str() == Some(operation),
            "offline keygen cannot claim an existing session"
        );
        Ok(HostAuthIdentity::Unbound {
            operation_id: operation.to_owned(),
            session_sha256: digest(&bytes),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cold_keygen_preserves_legacy_sidecars_and_returns_exact_receipt_without_factor() -> Result<()>
    {
        let isolated = crate::test_lock::Isolate::new()?;
        let _credentials = keygen_existing_protocol::memory_keyring();
        let root = isolated.path().canonicalize()?;
        let owner = AuthOwnership::acquire(&root, &root)?;
        let op = uuid::Uuid::new_v4().to_string();
        let foreign = uuid::Uuid::new_v4().to_string();
        let material = keygen_existing_protocol::host_material()?;
        let sidecar = root.join("onememory.db-wal");
        fs::write(&sidecar, b"synthetic legacy sidecar")?;
        assert!(owner.cold_keygen(&op, &op, false, &material).is_err());
        assert_eq!(fs::read(&sidecar)?, b"synthetic legacy sidecar");
        assert!(load_journal()?.operations.is_empty());
        fs::remove_file(sidecar)?;
        assert!(owner.cold_keygen(&op, &op, true, &material).is_err());
        let committed = owner.cold_keygen(&op, &op, false, &material)?;
        assert!(
            committed.outcome == HostAuthOutcome::Committed && committed.super_password.is_none()
        );
        let original = fs::read(root.join("session.json"))?;
        let journal = serde_json::to_vec(&load_journal()?)?;
        let replay = owner.cold_keygen(&op, &op, true, &material)?;
        assert_eq!(replay.session_sha256, committed.session_sha256);
        assert!(replay.super_password.is_none());
        assert!(owner
            .cold_keygen(&foreign, &foreign, true, &material)
            .is_err());
        assert!(owner.cold_keygen(&op, &foreign, true, &material).is_err());
        assert_eq!(fs::read(root.join("session.json"))?, original);
        assert_eq!(serde_json::to_vec(&load_journal()?)?, journal);
        Ok(())
    }
    #[test]
    fn cold_keygen_does_not_retire_an_unrelated_active_record() -> Result<()> {
        let isolated = crate::test_lock::Isolate::new()?;
        let _credentials = keygen_existing_protocol::memory_keyring();
        let root = isolated.path().canonicalize()?;
        let owner = AuthOwnership::acquire(&root, &root)?;
        let op = uuid::Uuid::new_v4().to_string();
        let foreign = uuid::Uuid::new_v4().to_string();
        let material = keygen_existing_protocol::host_material()?;
        let committed = owner.cold_keygen(&op, &op, false, &material)?;
        assert!(committed.outcome == HostAuthOutcome::Committed);

        let path = recovery_path();
        let mut journal = load_journal()?;
        journal.active = Some(Recovery {
            directory: root.clone(),
            alias: "synthetic-other-operation".into(),
            operation_id: Some(foreign.clone()),
            retired: true,
            ..Recovery::default()
        });
        save_journal(&mut journal)?;
        let journal_bytes = fs::read(&path)?;
        let session_path = root.join("session.json");
        let session_bytes = fs::read(&session_path)?;

        for (id, key, query_only) in [
            (op.as_str(), op.as_str(), true),
            (op.as_str(), op.as_str(), false),
            (foreign.as_str(), foreign.as_str(), true),
        ] {
            assert!(owner.cold_keygen(id, key, query_only, &material).is_err());
            assert_eq!(fs::read(&path)?, journal_bytes);
            assert_eq!(fs::read(&session_path)?, session_bytes);
        }
        Ok(())
    }

    #[test]
    fn cold_keygen_reconciles_only_its_prepared_session() -> Result<()> {
        let isolated = crate::test_lock::Isolate::new()?;
        let _credentials = keygen_existing_protocol::memory_keyring();
        let root = isolated.path().canonicalize()?;
        let owner = AuthOwnership::acquire(&root, &root)?;
        let op = uuid::Uuid::new_v4().to_string();
        let mut material = keygen_existing_protocol::host_material()?;
        let committed = owner.cold_keygen(&op, &op, false, &material)?;
        let key = committed
            .session_sha256
            .context("missing synthetic digest")?;
        let file = root.join("session.json");
        let bytes = fs::read(&file)?;
        // Recreate the durable boundary after the candidate write but before
        // publishing its terminal receipt, using only the synthetic operation.
        let mut journal = load_journal()?;
        journal.operations[0].phase = AuthPhase::Prepared;
        journal.active = Some(Recovery {
            directory: root.clone(),
            alias: material.keyring_account.clone(),
            directory_existed: true,
            operation_id: Some(op.clone()),
            ..Recovery::default()
        });
        save_journal(&mut journal)?;
        let recoverable = fs::read(recovery_path())?;
        let wrong = std::mem::replace(
            &mut material.super_password,
            "synthetic wrong factor".into(),
        );
        fs::remove_file(&file)?;
        assert!(owner.cold_keygen(&op, &op, true, &material).is_err());
        assert_eq!(fs::read(recovery_path())?, recoverable);
        assert!(!file.exists());
        fs::write(&file, &bytes)?;
        assert!(owner.cold_keygen(&op, &op, true, &material).is_err());
        assert_eq!(fs::read(recovery_path())?, recoverable);
        assert_eq!(fs::read(&file)?, bytes);
        material.super_password = wrong;
        let result = owner.cold_keygen(&op, &op, true, &material)?;
        assert!(result.outcome == HostAuthOutcome::Committed);
        assert_eq!(result.session_sha256.as_deref(), Some(key.as_str()));
        assert!(result.super_password.is_none());
        assert_eq!(fs::read(file)?, bytes);
        assert!(load_journal()?.active.is_none());
        Ok(())
    }
}
