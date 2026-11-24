fn prepare_grant(
    binary: &Path,
    root: &Path,
    profile: &Path,
    managed_id: Option<&str>,
    port: u16,
) -> Result<(serde_json::Value, String)> {
    let maintenance = lifecycle::launch_maintenance_binding(profile, managed_id)?;
    let account_id = hash(account_identity_source(profile)?.as_bytes());
    prepare_grant_for_identity(binary, root, profile, managed_id, port, &account_id, maintenance)
}

pub(crate) fn prepare_grant_for_identity(
    binary: &Path,
    root: &Path,
    profile: &Path,
    managed_id: Option<&str>,
    port: u16,
    account_id: &str,
    maintenance: Option<serde_json::Value>,
) -> Result<(serde_json::Value, String)> {
    let registration = read_registration(root)?;
    if let Some(config) = &registration {
        lifecycle::owned(config)?;
        if managed_id.is_some() {
            verify_registered_supervisor(&std::env::current_exe()?, &config.executable)?;
        }
        anyhow::ensure!(
            !config.maintenance || maintenance.is_some(),
            "registration maintenance lacks an active launch controller"
        );
        match managed_id {
            Some(id) => anyhow::ensure!(
                config.id == id && config.enabled && !config.manually_stopped,
                "managed launch does not match an enabled registration"
            ),
            None => anyhow::ensure!(
                !config.enabled || maintenance.is_some(),
                "registered runtime requires its host lifecycle coordinator"
            ),
        }
    } else {
        anyhow::ensure!(
            managed_id.is_none(),
            "managed launch registration is missing"
        );
    }
    build_grant(binary, root, profile, managed_id, port, account_id, maintenance, registration.as_ref())
}

/// Fixed reads may inspect a stopped installation without authorizing a serving launch.
pub(super) fn prepare_offline_read_grant(
    binary: &Path, root: &Path, profile: &Path, port: u16, account_id: &str,
    kind: respire_protocol::metadata_control::HostOfflineRead,
) -> Result<serde_json::Value> {
    use respire_protocol::metadata_control::HostOfflineRead;
    match kind {
        HostOfflineRead::MetadataStatus | HostOfflineRead::AgentConfigRead | HostOfflineRead::SnapshotPreflight => {}
    }
    let registration = read_registration(root)?;
    let managed_id = registration.as_ref()
        .filter(|config| config.enabled && !config.manually_stopped)
        .map(|config| config.id.as_str());
    if let Some(config) = registration.as_ref() {
        lifecycle::owned(config)?;
        anyhow::ensure!(!config.maintenance,
            "offline read cannot replace an active maintenance controller");
        if managed_id.is_some() {
            verify_registered_supervisor(&std::env::current_exe()?, &config.executable)?;
        }
    }
    let (grant, _) = build_grant(binary, root, profile, managed_id, port, account_id, None, registration.as_ref())?;
    Ok(grant)
}

/// A status read cannot start serving or replace an interrupted maintenance marker.
pub(super) fn prepare_login_recovery_status_grant(
    binary: &Path, root: &Path, profile: &Path, port: u16, account_id: &str,
) -> Result<serde_json::Value> {
    let registration = read_registration(root)?;
    if let Some(config) = registration.as_ref() {
        lifecycle::owned(config)?;
    }
    let (grant, _) = build_grant(binary, root, profile, None, port, account_id, None, registration.as_ref())?;
    Ok(grant)
}

fn build_grant(
    binary: &Path, root: &Path, profile: &Path, managed_id: Option<&str>,
    port: u16, account_id: &str, maintenance: Option<serde_json::Value>,
    registration: Option<&Registration>,
) -> Result<(serde_json::Value, String)> {
    let supervisor_pid = std::process::id();
    let supervisor_birth = super::super::manager::process_birth(supervisor_pid)?
        .context("launch supervisor process identity is unavailable")?;
    let nonce = uuid::Uuid::new_v4().to_string();
    let issued_at_ms = u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())
        .context("launch clock overflow")?;
    let expires_at_ms = issued_at_ms
        .checked_add(60_000)
        .context("launch expiry overflow")?;
    let grant = json!({
        "schema_version": 1,
        "protocol_version": respire_commands::rpc_client::PROTOCOL_V,
        "nonce": nonce,
        "instance_id": nonce,
        "installation_root": root,
        "installation_id": hash(root.display().to_string().as_bytes()),
        "profile": profile,
        "account_id": account_id,
        // Phase one starts a fresh worker; this is not a persistent owner generation.
        "sync_generation": 0,
        "image_path": binary,
        "image_sha256": hash_file(binary)?,
        "supervisor_pid": supervisor_pid,
        "supervisor_birth": supervisor_birth,
        "managed_id": managed_id,
        "registration_sha256": registration.as_ref().and_then(|config| config.digest.as_deref()),
        "maintenance": maintenance,
        "port": port,
        "issued_at_ms": issued_at_ms,
        "expires_at_ms": expires_at_ms,
    });
    Ok((grant, nonce))
}

/// One private read against an authenticated installed image; never starts serving.
pub(super) fn control_memory_revision() -> Result<ResultEnvelope> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let binary = crate::runtime_fetch::managed_runtime_path()?
        .context("host control requires an installed managed Runtime")?
        .canonicalize()?;
    let manifest = crate::runtime_fetch::read_manifest()?
        .context("host control Runtime manifest is missing")?;
    anyhow::ensure!(
        hash_file(&binary)?.eq_ignore_ascii_case(&manifest.sha256),
        "host control Runtime image does not match the managed manifest"
    );
    let (root, profile) = respire_client_host::config::control_roots()?;
    let root = root.canonicalize()?;
    let profile = profile.canonicalize()?;
    let registration = read_registration(&root)?;
    let managed_id = registration
        .as_ref()
        .filter(|config| config.enabled && !config.manually_stopped)
        .map(|config| config.id.as_str());
    let (grant, _) = prepare_grant(
        &binary,
        &root,
        &profile,
        managed_id,
        crate::net_rpc::rpc_port(),
    )?;
    let grant: HostControlLaunchGrant = serde_json::from_value(grant)?;
    anyhow::ensure!(
        grant.image_path == binary && grant.image_sha256.eq_ignore_ascii_case(&manifest.sha256),
        "host control image changed while preparing authorization"
    );
    let expected_account_id = respire_client_host::identity::host_session_user_of_dir(&profile, hash)?
        .map(|user| hash(user.as_bytes()));
    anyhow::ensure!(
        grant.account_id
            == expected_account_id
                .clone()
                .unwrap_or_else(|| hash(profile.display().to_string().as_bytes())),
        "host control account changed while preparing authorization"
    );
    let params = HostControlParams {
        schema_version: HOST_CONTROL_SCHEMA_V,
        binding: ControlBinding {
            installation_root: root,
            profile: profile.clone(),
            launch_id: grant.instance_id.clone(),
            expected_account_id,
        },
    };
    let request = RpcRequest {
        v: respire_commands::rpc_client::PROTOCOL_V,
        id: uuid::Uuid::new_v4().to_string(),
        method: HostControlOperation::MemoryRevision.method().to_owned(),
        args: vec![serde_json::to_string(&params)?],
        scope: None,
        operation_id: None,
        idempotency_key: None,
        parent_operation_id: None,
        queue_wait_ms: None,
        lifecycle: None,
    };
    exchange_control(&binary, &grant, &request, deadline, |response, pid| {
        validate_control_response(response, &request, &profile, pid)
    })
}
