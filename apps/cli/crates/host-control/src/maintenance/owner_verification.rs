impl Drop for Maintenance {
    fn drop(&mut self) {
        self.clear_active();
    }
}

pub(crate) fn current_boot_token() -> Option<String> {
    ACTIVE.with(|slot| slot.borrow().clone())
}

/// Durable journal writes are internal to the shared coordinators and inherit
/// the marker writer's private mode, bound, CAS and namespace sync semantics.
pub(crate) fn write_recovery(path: &Path, bytes: &[u8], expected: Option<&str>) -> Result<()> {
    let (root, _) = selected_roots()?;
    anyhow::ensure!(
        path.starts_with(root.join("runtime/autostart"))
            && !path
                .components()
                .any(|part| part == std::path::Component::ParentDir),
        "recovery journal escapes this installation's control directory"
    );
    private_write(path, bytes, expected)
}

pub(crate) fn marker_path(root: &Path) -> PathBuf {
    root.join("runtime/autostart/maintenance.json")
}

fn selected_roots() -> Result<(PathBuf, PathBuf)> {
    let (root, profile) = respire_client_host::config::control_roots()?;
    Ok((root.canonicalize()?, profile.canonicalize()?))
}

fn verify_selection(root: &Path, profile: &Path) -> Result<()> {
    let (actual_root, actual_profile) = selected_roots()?;
    anyhow::ensure!(
        actual_root == root && actual_profile == profile,
        "maintenance installation or selected profile changed"
    );
    Ok(())
}

fn parse_marker(bytes: &[u8]) -> Result<Marker> {
    let value: Marker =
        serde_json::from_slice(bytes).context("invalid runtime maintenance marker")?;
    anyhow::ensure!(
        value.schema == 1
            && uuid::Uuid::parse_str(&value.token).is_ok()
            && value.owner_pid > 0
            && !value.owner_birth.is_empty()
            && !value.id.is_empty()
            && !value.expected_version.is_empty(),
        "invalid runtime maintenance marker"
    );
    Ok(value)
}

fn controller_alive(value: &Marker) -> Result<bool> {
    Ok(process_birth(value.owner_pid)?.as_deref() == Some(value.owner_birth.as_str()))
}

fn ensure_exact_owner(actual: &Marker, held: &Marker, bytes: &[u8], digest: &str) -> Result<()> {
    anyhow::ensure!(
        actual == held
            && actual.owner_pid == std::process::id()
            && current_boot_token().as_deref() == Some(held.token.as_str())
            && hash(bytes) == digest
            && controller_alive(actual)?,
        "maintenance owner, phase, process birth or exact marker digest changed"
    );
    Ok(())
}

fn ensure_purpose(purpose: LaunchPurpose, phase: Phase) -> Result<()> {
    anyhow::ensure!(
        match purpose {
            LaunchPurpose::Account => phase == Phase::Paused,
            LaunchPurpose::AccountComplete => phase == Phase::Verified,
            LaunchPurpose::Serving => matches!(phase, Phase::Starting | Phase::Verified),
        },
        "maintenance phase does not authorize this launch role"
    );
    Ok(())
}

fn validate_registration_mode(root: &Path, mode: &RegistrationMode) -> Result<()> {
    let path = root.join("runtime/autostart/registration.json");
    if matches!(mode, RegistrationMode::Unregistered) {
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
            Ok(_) => anyhow::bail!("existing registration cannot be treated as unregistered"),
        }
    } else {
        let bytes = private_io::bounded_bytes(&path, 64 * 1024)?
            .context("registered maintenance owner disappeared")?;
        let value: Value = serde_json::from_slice(&bytes)?;
        anyhow::ensure!(
            value["id"].as_str() == Some(mode.coordinator_id(root).as_str())
                && value["installation"].as_str().map(Path::new) == Some(root),
            "observed registration does not match its maintenance owner"
        );
    }
    Ok(())
}

fn ensure_registration(
    host: &dyn HostLifecycle,
    root: &Path,
    expected: &RegistrationMode,
) -> Result<()> {
    let actual = host.inspect_registration(root)?;
    validate_registration_mode(root, &actual)?;
    anyhow::ensure!(
        &actual == expected,
        "maintenance startup registration changed"
    );
    Ok(())
}

fn ensure_no_owner(host: &dyn HostLifecycle) -> Result<()> {
    anyhow::ensure!(
        host.probe_runtime()?.is_none() && !host.endpoint_path().try_exists()?,
        "account control blocked: runtime owner or locator has no completed drain proof"
    );
    Ok(())
}

fn verify_health_roots(health: &Health, root: &Path, profile: &Path) -> Result<()> {
    anyhow::ensure!(
        health.pid > 0
            && !health.data_dir.is_empty()
            && !health.installation_root.is_empty()
            && Path::new(&health.data_dir).canonicalize()? == profile
            && Path::new(&health.installation_root).canonicalize()? == root
            && health.installation_id == hash(root.display().to_string().as_bytes()),
        "runtime owner roots or installation identity do not match maintenance"
    );
    Ok(())
}

/// Match the observed old image, never the selected replacement image. An
/// upgrade may legitimately drain a different Runtime version and executable.
fn verify_observed_process(
    health: &Health,
    process: &respire_commands::net_rpc::RuntimeProcessIdentity,
    image_sha256: &str,
) -> Result<()> {
    let executable = Path::new(&process.executable).canonicalize()?;
    anyhow::ensure!(
        health.pid > 0
            && health.v == crate::PROTOCOL_V
            && !health.instance_id.is_empty()
            && !health.account_id.is_empty()
            && health.generation > 0
            && !health.runtime_version.is_empty()
            && health.image_sha256.len() == 64
            && health
                .image_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            && Path::new(&health.image_path).canonicalize()? == executable
            && Path::new(&health.exe).canonicalize()? == executable
            && process_start_matches(&process.started_at, &health.process_start_id)
            && health.image_sha256.eq_ignore_ascii_case(image_sha256),
        "observed Runtime process, image digest or typed stop scope changed"
    );
    Ok(())
}

fn process_start_matches(birth: &str, start_id: &str) -> bool {
    let prefix = if cfg!(windows) {
        "windows-filetime-"
    } else if cfg!(target_os = "linux") {
        "linux-starttime-"
    } else {
        "darwin-lstart-"
    };
    !birth.is_empty() && start_id.strip_prefix(prefix) == Some(birth)
}

fn ensure_applied(receipt: &HostAuthAccountResult) -> Result<()> {
    anyhow::ensure!(
        matches!(
            receipt.outcome,
            HostAuthAccountOutcome::AwaitingVerification | HostAuthAccountOutcome::Committed
        ),
        "account verification requires an applied candidate receipt"
    );
    Ok(())
}

fn receipt_target(receipt: &HostAuthAccountResult) -> Result<PathBuf> {
    let target = receipt
        .target_profile
        .as_deref()
        .context("account target is missing")?;
    respire_client_host::config::normalize_control_path(target)?
        .canonicalize()
        .map_err(Into::into)
}
