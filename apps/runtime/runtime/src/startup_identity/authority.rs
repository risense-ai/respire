//! Grant validation and managed authorization evidence.
use super::*;

pub(super) fn validate_grant(
    grant: &LaunchGrant,
    context: &LaunchContext,
    supervised: bool,
    nonce: &str,
    now: u64,
) -> Result<()> {
    validate_nonce(nonce)?;
    anyhow::ensure!(
        grant.schema_version == 1
            && grant.protocol_version == respire_commands::rpc_worker::PROTOCOL_V
            && grant.nonce == nonce
            && grant.instance_id == nonce,
        "runtime launch grant schema, protocol or nonce mismatch"
    );
    anyhow::ensure!(
        grant.installation_root == context.root
            && grant.installation_id == hash(context.root.display().to_string().as_bytes())
            && grant.profile == context.profile
            && grant.account_id == context.account_id
            && grant.sync_generation == context.sync_generation
            && grant.image_path == context.image_path
            && grant.image_sha256 == context.image_sha256
            && grant.port != 0
            && grant.port == context.port,
        "runtime launch grant does not match this installation, account, generation or image"
    );
    anyhow::ensure!(
        supervised == grant.managed_id.is_some()
            && grant.managed_id.as_ref().is_none_or(|id| !id.is_empty())
            && grant.issued_at_ms <= now
            && now <= grant.expires_at_ms
            && grant.expires_at_ms > grant.issued_at_ms
            && grant.expires_at_ms - grant.issued_at_ms <= 60_000,
        "runtime launch grant mode or expiry is invalid"
    );
    Ok(())
}

pub(super) fn verify_authority(grant: Option<&LaunchGrant>, context: &LaunchContext) -> Result<()> {
    verify_control_authority(grant, context, false, None, false)
}

pub(super) fn verify_authority_for_mode(grant: Option<&LaunchGrant>, context: &LaunchContext, auth: bool) -> Result<()> {
    verify_control_authority(grant, context, auth, None, true)
}

pub(super) fn verify_offline_read_authority(
    grant: &LaunchGrant, context: &LaunchContext,
    kind: respire_protocol::metadata_control::HostOfflineRead,
) -> Result<()> {
    use respire_protocol::metadata_control::HostOfflineRead;
    match kind {
        HostOfflineRead::MetadataStatus | HostOfflineRead::AgentConfigRead | HostOfflineRead::SnapshotPreflight => {}
    }
    verify_control_authority(Some(grant), context, false, Some(kind), true)
}

fn verify_control_authority(
    grant: Option<&LaunchGrant>, context: &LaunchContext, auth: bool,
    offline_read: Option<respire_protocol::metadata_control::HostOfflineRead>,
    require_live_supervisor: bool,
) -> Result<()> {
    let control = context.root.join("runtime").join("autostart");
    let maintenance_bytes = read_private_file(&control.join("maintenance.json"))?;
    if !require_live_supervisor
        && maintenance_bytes.is_none()
        && grant.is_none_or(|grant| grant.maintenance.is_none())
    {
        // Legacy admission writes this journal before the Paused marker.
        // Do not start listening in the gap between those durable writes.
        require_no_orphaned_keygen(&control)?;
        // Registration belongs to the host. Only transaction authority needs it.
        return Ok(());
    }
    let registration_bytes = read_private_file(&control.join("registration.json"))?;
    let mut registration = registration_bytes
        .as_ref()
        .map(|bytes| parse_control(bytes))
        .transpose()?;
    if let Some(registration) = &mut registration {
        let raw_root = registration["environment"]["RSRS_DATA_DIR"]
            .as_str()
            .context("registered runtime root is missing")?;
        let registered_root = Path::new(raw_root).canonicalize()?;
        anyhow::ensure!(
            registered_root == context.root,
            "registered runtime root belongs to another installation"
        );
        registration["environment"]["RSRS_DATA_DIR"] = json!(registered_root);
    }
    let maintenance = maintenance_bytes
        .as_ref()
        .map(|bytes| parse_control(bytes))
        .transpose()?;
    validate_control_authority(
        grant,
        context,
        registration.as_ref(),
        registration_bytes.as_deref(),
        maintenance.as_ref(),
        maintenance_bytes.as_deref(),
        auth,
        offline_read,
    )?;
    if let Some(grant) = grant {
        if require_live_supervisor {
            anyhow::ensure!(
                grant.supervisor_pid > 0
                    && grant.supervisor_pid != std::process::id()
                    && !grant.supervisor_birth.is_empty()
                    && process_birth(grant.supervisor_pid)?.as_deref()
                        == Some(grant.supervisor_birth.as_str()),
                "runtime control supervisor is missing, exited or its PID was reused"
            );
        }
        if let Some(binding) = &grant.maintenance {
            anyhow::ensure!(
                process_birth(binding.controller_pid)?.as_deref()
                    == Some(binding.controller_birth.as_str()),
                "runtime maintenance controller exited or its PID was reused"
            );
        }
    }
    Ok(())
}

fn require_no_orphaned_keygen(control: &Path) -> Result<()> {
    match fs::symlink_metadata(control.join("offline-keygen.json")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("check offline key-generation recovery fence"),
        Ok(_) => anyhow::bail!("offline key-generation admission must be recovered before serving"),
    }
}

#[cfg(test)]
mod keygen_fence_tests {
    use super::*;

    #[test]
    fn ordinary_admission_refuses_a_keygen_journal_without_reading_or_mutating_it() -> Result<()> {
        struct OwnedScratch(PathBuf);
        impl Drop for OwnedScratch {
            fn drop(&mut self) {
                let _ = fs::remove_file(self.0.join("offline-keygen.json"));
                let _ = fs::remove_dir(&self.0);
            }
        }

        let path =
            std::env::temp_dir().join(format!("respire-keygen-fence-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path)?;
        let scratch = OwnedScratch(path);
        require_no_orphaned_keygen(&scratch.0)?;
        let journal = scratch.0.join("offline-keygen.json");
        let witness = b"synthetic recovery journal";
        fs::write(&journal, witness)?;
        assert!(require_no_orphaned_keygen(&scratch.0).is_err());
        assert_eq!(fs::read(journal)?, witness);
        Ok(())
    }
}

#[cfg(test)]
pub(super) fn validate_control(
    grant: Option<&LaunchGrant>,
    context: &LaunchContext,
    registration: Option<&Value>,
    registration_bytes: Option<&[u8]>,
    maintenance: Option<&Value>,
    maintenance_bytes: Option<&[u8]>,
) -> Result<()> {
    validate_control_for_mode(grant, context, registration, registration_bytes, maintenance, maintenance_bytes, false)
}

pub(super) fn validate_control_for_mode(
    grant: Option<&LaunchGrant>,
    context: &LaunchContext,
    registration: Option<&Value>,
    registration_bytes: Option<&[u8]>,
    maintenance: Option<&Value>,
    maintenance_bytes: Option<&[u8]>,
    auth: bool,
) -> Result<()> {
    validate_control_authority(grant, context, registration, registration_bytes,
        maintenance, maintenance_bytes, auth, None)
}

fn validate_control_authority(
    grant: Option<&LaunchGrant>,
    context: &LaunchContext,
    registration: Option<&Value>,
    registration_bytes: Option<&[u8]>,
    maintenance: Option<&Value>,
    maintenance_bytes: Option<&[u8]>,
    auth: bool,
    offline_read: Option<respire_protocol::metadata_control::HostOfflineRead>,
) -> Result<()> {
    if let Some(grant) = grant {
        anyhow::ensure!(
            grant.registration_sha256 == registration_bytes.map(hash),
            "runtime registration changed after launch authorization"
        );
    }
    if let Some(registration) = registration {
        let owner = registration["owner"]
            .as_str()
            .filter(|v| !v.is_empty())
            .context("runtime registration owner is missing")?;
        let id =
            hash(format!("respire-runtime-v1\0{}\0{owner}", context.root.display()).as_bytes());
        anyhow::ensure!(
            registration["schema"] == 1
                && registration["platform"] == std::env::consts::OS
                && registration["installation"].as_str().map(Path::new)
                    == Some(context.root.as_path())
                && registration["id"].as_str() == Some(&id[..24])
                && registration["environment"]["RSRS_RPC_PORT"]
                    .as_str()
                    .and_then(|v| v.parse::<u16>().ok())
                    == Some(context.port)
                && registration["environment"]["RSRS_DATA_DIR"]
                    .as_str()
                    .map(Path::new)
                    == Some(context.root.as_path()),
            "runtime registration belongs to another installation, platform or port"
        );
        if let Some(managed) = grant.and_then(|grant| grant.managed_id.as_deref()) {
            anyhow::ensure!(
                registration["id"] == managed
                    && registration["enabled"] == true
                    && registration["manually_stopped"] == false,
                "managed runtime launch is disabled or belongs to another registration"
            );
        } else {
            anyhow::ensure!(
                (registration["enabled"] == false && registration["manually_stopped"] == false)
                    || grant.is_some_and(|grant| grant.maintenance.is_some())
                    || (offline_read.is_some() && grant.is_some()
                        && registration["manually_stopped"] == true),
                "registered runtime requires its host lifecycle coordinator"
            );
        }
        anyhow::ensure!(
            registration["maintenance"] == false || maintenance.is_some(),
            "registration maintenance has no matching controller marker"
        );
    } else {
        anyhow::ensure!(
            grant.and_then(|grant| grant.managed_id.as_ref()).is_none(),
            "managed runtime registration disappeared"
        );
    }
    match (
        maintenance,
        grant.and_then(|grant| grant.maintenance.as_ref()),
    ) {
        (None, None) => Ok(()),
        (Some(marker), Some(binding)) => {
            anyhow::ensure!(
                maintenance_bytes.map(hash).as_deref() == Some(binding.marker_sha256.as_str())
                    && marker["schema"] == 1
                    && marker["id"] == binding.coordinator_id
                    && registration
                        .is_none_or(|registration| registration["id"] == binding.coordinator_id)
                    && marker["owner_pid"] == binding.controller_pid
                    && marker["owner_birth"] == binding.controller_birth
                    && marker["expected_version"] == binding.issuer_cli_version
                    && !binding.issuer_cli_version.is_empty()
                    && !binding.controller_birth.is_empty()
                    && binding.controller_pid > 0
                    && marker["expected_profile"].as_str().map(Path::new)
                        == Some(context.profile.as_path())
                    && if auth { marker["phase"] == "Paused" }
                        else { matches!(marker["phase"].as_str(), Some("Starting" | "Verified")) },
                "runtime maintenance does not authorize this launch"
            );
            Ok(())
        }
        _ => anyhow::bail!("runtime maintenance launch binding is missing or stale"),
    }
}
