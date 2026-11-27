use super::*;

pub(crate) fn cleanup_paused() -> Result<bool> {
    let _takeover = crate::runtime_policy::takeover_lock()?;
    let root = respire_client_host::paths::main_data_dir().canonicalize()?;
    cleanup_orphan(&root)
}

pub(super) fn cleanup_orphan(root: &Path) -> Result<bool> {
    let Some(bytes) = marker_bytes(root)? else { return Ok(false); };
    let value = parse_marker(&bytes)?;
    if value.phase != Phase::Paused || controller_alive(&value)? {
        return Ok(false);
    }
    anyhow::ensure!(
        value.expected_profile.is_none() && value.snapshot.is_none() && value.agent_config.is_none(),
        "Paused maintenance has an operation binding; use its explicit recovery command"
    );
    super::super::supervise::offline_keygen::reject_generic_recovery(root)?;
    if let Some(mut config) = read_registration(root)? {
        owned(&config)?;
        anyhow::ensure!(config.installation == root && config.id == value.id,
            "Paused maintenance belongs to another Runtime registration");
        if config.maintenance {
            config.maintenance = false;
            save(&mut config)?;
        }
    } else {
        anyhow::ensure!(value.id == format!("host-{}", hash(root.display().to_string().as_bytes())),
            "Paused maintenance belongs to another installation");
    }
    anyhow::ensure!(marker_bytes(root)?.as_deref() == Some(bytes.as_slice()),
        "Paused maintenance changed during cleanup");
    let path = marker_path(root);
    fs::remove_file(&path)?;
    respire_spawn::durable::sync_directory(path.parent().context("maintenance parent missing")?)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doctor_clears_only_unbound_orphaned_pause() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().canonicalize()?;
        let path = marker_path(&root);
        let mut value = Marker {
            schema: 1,
            id: format!("host-{}", hash(root.display().to_string().as_bytes())),
            token: uuid::Uuid::new_v4().to_string(),
            phase: Phase::Paused,
            expected_profile: None,
            expected_version: env!("CARGO_PKG_VERSION").into(),
            owner_pid: std::process::id(),
            owner_birth: manager::process_birth(std::process::id())?.context("test process missing")?,
            snapshot: None,
            agent_config: None,
        };
        fs::create_dir_all(path.parent().context("test marker parent missing")?)?;
        fs::write(&path, serde_json::to_vec(&value)?)?;
        assert!(!cleanup_orphan(&root)?);
        value.owner_birth.push_str("-orphaned");
        value.phase = Phase::Starting;
        fs::write(&path, serde_json::to_vec(&value)?)?;
        assert!(!cleanup_orphan(&root)?);
        value.phase = Phase::Paused;
        value.expected_profile = Some(root.clone());
        fs::write(&path, serde_json::to_vec(&value)?)?;
        assert!(cleanup_orphan(&root).is_err());
        assert!(path.try_exists()?);
        value.expected_profile = None;
        fs::write(&path, serde_json::to_vec(&value)?)?;
        assert!(cleanup_orphan(&root)?);
        assert!(!path.try_exists()?);
        assert!(!cleanup_orphan(&root)?);
        Ok(())
    }
}
