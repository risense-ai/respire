pub(crate) fn begin_maintenance_locked() -> Result<Maintenance> {
    anyhow::ensure!(
        !ACTIVE.with(|v| v.borrow().is_some()),
        "host maintenance already has an active controller"
    );
    let (root, _) = respire_client_host::config::control_roots()?;
    fs::create_dir_all(&root)?;
    let root = root.canonicalize()?;
    match read_registration(&root)? {
        Some(config) if config.enabled && !config.manually_stopped => {
            let guard = Maintenance::begin(config, RECOVERY.with(|v| v.get()))?;
            reset_attempts(
                guard
                    .config
                    .as_ref()
                    .context("maintenance registration missing")?,
            )?;
            Ok(guard)
        }
        config => Maintenance::begin_local(root, config, RECOVERY.with(|v| v.get())),
    }
}

/// Prepare one exact Paused controller, durably recording its original operation
/// through `admit` before either marker or managed-registration state is changed.
pub(crate) fn begin_offline_keygen_maintenance(
    root: &Path, profile: &Path, admit: impl FnOnce(&str, &str, &str) -> Result<()>,
) -> Result<Maintenance> {
    anyhow::ensure!(current_boot_token().is_none(), "offline keygen controller already active");
    let (selected_root, selected_profile) = respire_client_host::config::control_roots()?;
    anyhow::ensure!(selected_root.canonicalize()? == root && selected_profile.canonicalize()? == profile
        && marker(root)?.is_none(), "offline keygen selection changed or maintenance already exists");
    let registration = read_registration(root)?;
    if let Some(config) = registration.as_ref() {
        owned(config)?;
        anyhow::ensure!(config.installation == root, "offline keygen registration changed");
        ensure_control_endpoint(config)?;
        let inspected = manager::inspect(config)?;
        anyhow::ensure!(!inspected.registered || config.artifact_hashes.contains_key("service"),
            "offline keygen registration is not owned");
    }
    let id = registration.as_ref().map(|config| config.id.clone())
        .unwrap_or_else(|| format!("host-{}", hash(root.display().to_string().as_bytes())));
    let value = Marker { schema: 1, id, token: uuid::Uuid::new_v4().to_string(), phase: Phase::Paused,
        expected_profile: Some(profile.to_path_buf()), expected_version: env!("CARGO_PKG_VERSION").into(),
        owner_pid: std::process::id(), owner_birth: manager::process_birth(std::process::id())?
            .context("offline keygen controller birth unavailable")?, snapshot: None, agent_config: None };
    let bytes = serde_json::to_vec(&value)?;
    let digest = hash(&bytes);
    admit(&value.token, &value.id, &digest)?;
    private_write(&marker_path(root), &bytes, None)?;
    let mut config = registration.filter(|config| config.enabled && !config.manually_stopped);
    if let Some(config) = config.as_mut() { config.maintenance = true; save(config)?; reset_attempts(config)?; }
    ACTIVE.with(|slot| *slot.borrow_mut() = Some(value.token.clone()));
    Ok(Maintenance { installation: Some(root.to_path_buf()), config, marker: Some(value),
        marker_digest: Some(digest), completed: false })
}

/// Reattach only the original identity-write controller, retaining its stable token.
pub(crate) fn resume_identity_write_maintenance(
    root: &Path, profile: &Path, token: &str, coordinator: &str,
) -> Result<Maintenance> {
    super::supervise::offline_keygen::reject_generic_recovery(root)?;
    resume_key_maintenance(root, profile, profile, token, coordinator)
}

/// A key receipt can move selection before its private response reaches CLI.
/// Keep the original token/controller checks, allowing only source or selection.
pub(crate) fn verify_offline_keygen_finished(root: &Path) -> Result<()> {
    anyhow::ensure!(marker_bytes(root)?.is_none() && current_boot_token().is_none(),
        "offline keygen completed marker is still present; no spawn attempted");
    Ok(())
}

pub(crate) fn verify_offline_keygen_marker(root: &Path, profile: &Path, token: &str,
    coordinator: &str, digest: &str) -> Result<()> {
    let bytes = marker_bytes(root)?.context("offline keygen marker missing; no replacement created")?;
    let actual = marker(root)?.context("offline keygen marker missing")?;
    anyhow::ensure!(hash(&bytes) == digest && actual.phase == Phase::Paused
        && actual.token == token && actual.id == coordinator
        && actual.expected_profile.as_deref() == Some(profile)
        && actual.expected_version == env!("CARGO_PKG_VERSION")
        && actual.snapshot.is_none() && actual.agent_config.is_none()
        && (!controller_alive(&actual)? || actual.owner_pid == std::process::id()),
        "offline keygen original Paused marker changed");
    Ok(())
}

/// Prepared and confirmed RolledBack originals may rebind before any new effect
/// claim. Journal the exact old/target marker pair before replacing the marker.
/// Only Prepared permits an absent initial publication; rollback never does.
pub(crate) fn resume_offline_keygen_prelaunch_maintenance(root: &Path, profile: &Path,
    token: &str, coordinator: &str, digest: &str, pending: Option<&str>, allow_absent: bool,
    admit: impl FnOnce(Option<&str>, &str) -> Result<()>) -> Result<Maintenance> {
    let observed=marker_bytes(root)?.as_deref().map(hash);
    if let Some(actual)=observed.as_deref() {
        anyhow::ensure!(actual == digest || pending == Some(actual), "offline keygen marker differs from durable rebind intent");
        verify_offline_keygen_marker(root, profile, token, coordinator, actual)?;
    } else {
        anyhow::ensure!(allow_absent, "offline keygen rollback marker missing; no replacement created");
    }
    resume_key_maintenance_admitted(root, profile, profile, token, coordinator, false, |previous,target| {
        anyhow::ensure!(previous == observed.as_deref(), "offline keygen marker changed before rebind admission");
        admit(previous,target)
    })
}

#[cfg(test)]
std::thread_local! {
    static FAIL_KEYGEN_REBIND_AFTER_MARKER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
#[cfg(test)]
pub(crate) fn fail_keygen_rebind_after_marker_for_test() {
    FAIL_KEYGEN_REBIND_AFTER_MARKER.with(|fail| fail.set(true));
}

pub(crate) fn resume_key_maintenance(
    root: &Path, profile: &Path, source: &Path, token: &str, coordinator: &str,
) -> Result<Maintenance> {
    resume_key_maintenance_inner(root, profile, source, token, coordinator, false)
}

pub(crate) fn pending_agent_config(root: &Path, query: bool) -> Result<Option<(respire_protocol::config_control::AgentConfigMaintenance, String, String)>> {
    let Some(marker) = marker(root)? else { return Ok(None); };
    let Some(record) = marker.agent_config else {
        anyhow::ensure!(query, "finish existing maintenance before agent config");
        return Ok(None);
    };
    Ok(Some((serde_json::from_value(record)?, marker.token, marker.id)))
}

pub(crate) fn resume_agent_config_maintenance(root: &Path, profile: &Path, token: &str, coordinator: &str) -> Result<Maintenance> {
    super::supervise::offline_keygen::reject_generic_recovery(root)?;
    resume_key_maintenance_inner(root, profile, profile, token, coordinator, true)
}

fn resume_key_maintenance_inner(root: &Path, profile: &Path, source: &Path, token: &str, coordinator: &str,
    agent_config: bool) -> Result<Maintenance> {
    resume_key_maintenance_admitted(root, profile, source, token, coordinator, agent_config, |_,_| Ok(()))
}

fn resume_key_maintenance_admitted(root: &Path, profile: &Path, source: &Path, token: &str, coordinator: &str,
    agent_config: bool, admit: impl FnOnce(Option<&str>, &str) -> Result<()>) -> Result<Maintenance> {
    anyhow::ensure!(current_boot_token().is_none(), "host maintenance already has a controller");
    let (selected_root, selected_profile) = respire_client_host::config::control_roots()?;
    anyhow::ensure!(selected_root.canonicalize()? == root && selected_profile.canonicalize()? == profile,
        "identity recovery selection changed");
    let previous = marker_bytes(root)?;
    let mut registration = read_registration(root)?;
    if let Some(config) = registration.as_ref() {
        owned(config)?;
        anyhow::ensure!(config.installation == root && config.id == coordinator, "identity recovery registration changed");
        ensure_control_endpoint(config)?;
    } else {
        anyhow::ensure!(coordinator == format!("host-{}", hash(root.display().to_string().as_bytes())),
            "identity recovery coordinator changed");
    }
    let previous_marker = marker(root)?;
    if let Some(value) = previous_marker.as_ref() {
        anyhow::ensure!(value.token == token && value.id == coordinator && value.snapshot.is_none()
            && value.agent_config.is_some() == agent_config
            && matches!(value.expected_profile.as_deref(), Some(expected) if expected == profile || expected == source)
            && value.expected_version == env!("CARGO_PKG_VERSION")
            && (!controller_alive(value)? || value.owner_pid == std::process::id()),
            "identity recovery does not own the existing maintenance marker");
    }
    let value = Marker { schema: 1, id: coordinator.to_owned(), token: token.to_owned(), phase: Phase::Paused,
        expected_profile: Some(profile.to_path_buf()), expected_version: env!("CARGO_PKG_VERSION").into(),
        owner_pid: std::process::id(), owner_birth: manager::process_birth(std::process::id())?
            .context("identity recovery controller birth unavailable")?, snapshot: None,
        agent_config: previous_marker.and_then(|value| value.agent_config) };
    let bytes = serde_json::to_vec(&value)?;
    admit(previous.as_deref().map(hash).as_deref(), &hash(&bytes))?;
    private_write(&marker_path(root), &bytes, previous.as_ref().map(|bytes| hash(bytes)).as_deref())?;
    #[cfg(test)]
    anyhow::ensure!(!FAIL_KEYGEN_REBIND_AFTER_MARKER.with(|fail| fail.replace(false)),
        "synthetic registration-save failure after keygen marker publication");
    let mut config = registration.take().filter(|config| config.enabled && !config.manually_stopped);
    if let Some(config) = config.as_mut() { config.maintenance = true; save(config)?; }
    ACTIVE.with(|slot| *slot.borrow_mut() = Some(token.to_owned()));
    Ok(Maintenance { installation: Some(root.to_path_buf()), config, marker: Some(value),
        marker_digest: Some(hash(&bytes)), completed: false })
}

/// Explicit host restart is the recovery operation for an interrupted transition.
/// The caller holds the same installation takeover lock as profile changes.
pub(crate) fn restart_maintenance_locked() -> Result<Maintenance> {
    let Some(config) = read_registration(&respire_client_host::paths::main_data_dir())? else {
        return with_explicit_recovery(begin_maintenance_locked);
    };
    let mut guard = Maintenance::begin(config, true)?;
    let config = guard
        .config
        .as_mut()
        .context("restart registration missing")?;
    config.manually_stopped = false;
    save(config)?;
    reset_attempts(config)?;
    Ok(guard)
}

/// Legacy detached mode may be used after an explicitly disabled registration.
/// It still needs the active controller's phase/token before opening the store.
pub(crate) fn authorize_detached_boot_locked() -> Result<()> {
    let Some(token) = current_boot_token() else {
        return Ok(());
    };
    let (root, profile) = respire_client_host::config::control_roots()?;
    let Some(mut value) = marker(&root)? else {
        bail!("active maintenance marker missing");
    };
    anyhow::ensure!(value.token == token, "maintenance owner changed");
    let path = marker_path(&root);
    let previous = fs::read(&path)?;
    value.phase = Phase::Starting;
    value.expected_profile = Some(profile.canonicalize()?);
    private_write(&path, &serde_json::to_vec(&value)?, Some(&hash(&previous)))
}

/// Export only an authorization binding; never forward the reusable maintenance token.
fn verified_account_marker(actual: &Marker, held: &Marker, target: &Path, bytes: &[u8], digest: &str) -> Result<()> {
    anyhow::ensure!(actual.phase == Phase::Verified && held.phase == Phase::Verified
        && actual.id == held.id && actual.token == held.token
        && actual.owner_pid == std::process::id() && actual.owner_birth == held.owner_birth
        && actual.expected_profile.as_deref() == Some(target)
        && current_boot_token().as_deref() == Some(held.token.as_str())
        && hash(bytes) == digest,
        "account completion requires this controller's exact Verified marker");
    Ok(())
}

/// Export only an authorization binding; never forward the reusable maintenance token.
pub(super) fn launch_maintenance_binding(profile: &Path, managed_id: Option<&str>) -> Result<Option<Value>> {
    maintenance_binding(profile, managed_id, false)
}

fn maintenance_binding(profile: &Path, managed_id: Option<&str>, auth: bool) -> Result<Option<Value>> {
    let root = respire_client_host::paths::main_data_dir();
    let Some(value) = marker(&root)? else {
        return Ok(None);
    };
    let bytes = fs::read(marker_path(&root))?;
    let observed: Marker = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid runtime maintenance authorization"))?;
    let managed_authorized = if let Some(id) = managed_id {
        let config = read_registration(&root)?.context("managed launch registration is missing")?;
        owned(&config)?;
        anyhow::ensure!(
            std::env::current_exe()?.canonicalize()? == config.executable.canonicalize()?,
            "managed launch controller differs from the registered CLI"
        );
        config.id == id && value.id == id && config.enabled && !config.manually_stopped
    } else {
        false
    };
    anyhow::ensure!(
        serde_json::to_vec(&observed)? == serde_json::to_vec(&value)?
            && (managed_authorized || current_boot_token().as_deref() == Some(value.token.as_str()))
            && if auth { value.phase == Phase::Paused }
                else { matches!(value.phase, Phase::Starting | Phase::Verified) }
            && value.expected_profile.as_deref() == Some(profile)
            && value.expected_version == env!("CARGO_PKG_VERSION")
            && controller_alive(&value)?,
        "runtime maintenance does not authorize the selected launch"
    );
    Ok(Some(json!({
        "marker_sha256": hash(&bytes),
        "coordinator_id": value.id,
        "controller_pid": value.owner_pid,
        "controller_birth": value.owner_birth,
        "issuer_cli_version": value.expected_version,
    })))
}

pub(crate) fn background_paused() -> bool {
    // Malformed/unreadable control state also fences background mutations.
    !matches!(marker(&respire_client_host::paths::main_data_dir()), Ok(None))
}

pub(crate) fn with_explicit_recovery<T>(action: impl FnOnce() -> Result<T>) -> Result<T> {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            RECOVERY.with(|v| v.set(self.0));
        }
    }
    let _reset = Reset(RECOVERY.with(|v| v.replace(true)));
    action()
}
