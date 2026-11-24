/// A query, rollback or continuation cannot turn an unsent journal into a new
/// operation. Once the journal says sent, the only initial exchange is a query.
fn require_unsent_admission(
    action: &Action<'_>,
    original: &Original,
    identity: &HostAuthIdentity,
) -> Result<()> {
    anyhow::ensure!(
        !original.sent && matches!(action, Action::Reset { resume: None, .. }),
        "key operation was not sent; explicit queries cannot admit it"
    );
    anyhow::ensure!(
        identity == &original.source_identity,
        "key source identity changed before admission"
    );
    Ok(())
}

fn verify_archived_custody(root: &Path, source: &Path, operation_id: &str,
    expected: &HostKeyInput) -> Result<ResultEnvelope> {
    let (original, _) = read_original(&archive_path(root, operation_id))?
        .context("completed key custody archive missing")?;
    anyhow::ensure!(original.schema_version == 1 && original.root == root
        && original.profile == source && original.operation_id == operation_id
        && original.key == operation_id && original.sent && !original.rolled_back
        && original.candidate.is_some()
        && matches!(&original.operation, HostKeyOperation::Fivekeys { addr, user, use_super: true, .. }
            if addr.trim().is_empty() && user.is_empty()), "completed recovery custody binding mismatch");
    // Registration may now own the source identity and pass slot. Neither belongs to this archive.
    let actual = material::load(&original, None, false)?;
    anyhow::ensure!(serde_json::to_value(&actual)? == serde_json::to_value(expected)?,
        "recovery custody material mismatch");
    Ok(ResultEnvelope::new("key-custody", respire_commands::output::Status::Ok,
        serde_json::json!({"verified":true,"operation_id":operation_id,
            "installation_root":root,"source_profile":source}), Vec::new()))
}

fn archive_path(root: &Path, operation_id: &str) -> PathBuf {
    root.join("runtime/autostart/key-results")
        .join(format!("{operation_id}.json"))
}

fn account(identity: &HostAuthIdentity, profile: &Path) -> String {
    match identity {
        HostAuthIdentity::Bound { account_id, .. } => account_id.clone(),
        _ => crate::launch::hash(profile.display().to_string().as_bytes()),
    }
}

fn target_identity_matches(receipt: &HostKeyResult) -> bool {
    let HostKeyOperation::Fivekeys { addr, user, .. } = &receipt.operation else {
        return false;
    };
    if user.is_empty() {
        addr.trim().is_empty()
            && receipt.account_id.is_none()
            && matches!(&receipt.identity, HostAuthIdentity::Unbound { operation_id, session_sha256 }
                if operation_id == &receipt.operation_id && Some(session_sha256) == receipt.session_sha256.as_ref())
    } else {
        let account = crate::launch::hash(user.as_bytes());
        receipt.account_id.as_ref() == Some(&account)
            && matches!(&receipt.identity, HostAuthIdentity::Bound { account_id, session_sha256 }
                if account_id == &account && Some(session_sha256) == receipt.session_sha256.as_ref())
    }
}

/// Key lifecycle checks do not weaken the authenticated-account receipt contract.
pub(crate) fn validate_applied_target(
    receipt: &HostKeyResult,
    operation_id: &str,
    source: &Path,
) -> Result<PathBuf> {
    anyhow::ensure!(
        canonical(operation_id)
            && receipt.operation_id == operation_id
            && receipt.source_profile == source
            && receipt.super_password.is_none()
            && receipt.required_inputs.is_empty()
            && target_identity_matches(receipt)
            && if receipt.verification_required {
                receipt.outcome == HostKeyOutcome::Pending
            } else {
                receipt.outcome == HostKeyOutcome::Committed
            },
        "fivekeys applied receipt identity mismatch"
    );
    let target = receipt
        .target_profile
        .as_ref()
        .context("fivekeys receipt target missing")?;
    let (_, selected) = respire_client_host::config::control_roots()?;
    anyhow::ensure!(
        target.is_absolute()
            && target.canonicalize()? == *target
            && selected.canonicalize()? == *target
            && crate::identity(target)? == receipt.identity,
        "fivekeys applied target changed"
    );
    Ok(target.clone())
}

fn unchanged(
    host: &dyn HostLifecycle,
    original: &Original,
    receipt: &HostKeyResult,
    image: &VerifiedRuntime,
) -> Result<()> {
    let (root, profile) = respire_client_host::config::control_roots()?;
    let current = host.verified_runtime()?;
    current.revalidate()?;
    let selected = if matches!(original.operation, HostKeyOperation::Fivekeys { .. })
        && receipt.outcome == HostKeyOutcome::Committed
    {
        receipt
            .target_profile
            .as_ref()
            .context("fivekeys target missing")?
    } else {
        &original.profile
    };
    anyhow::ensure!(
        root.canonicalize()? == original.root
            && profile.canonicalize()? == *selected
            && current.path() == image.path()
            && current.version() == image.version()
            && current.sha256().eq_ignore_ascii_case(image.sha256())
            && current.port() == image.port()
            && crate::identity(selected)? == receipt.identity,
        "key operation installation, profile, image or identity changed"
    );
    Ok(())
}

/// The former model-control owner verifier, without any asset resolver. The host
/// adapter independently verifies registration and performs the actual drain.
fn verify_owner(
    host: &dyn HostLifecycle,
    health: &respire_commands::net_rpc::Health,
    root: &Path,
    profile: &Path,
    image: &VerifiedRuntime,
    expected: &HostAuthIdentity,
) -> Result<()> {
    respire_commands::net_rpc::request_scope(health)?;
    let registration = host.inspect_registration(root)?;
    let managed_id = match &registration {
        RegistrationMode::Managed { coordinator_id } => Some(coordinator_id.as_str()),
        _ => None,
    };
    let process = respire_commands::net_rpc::runtime_process_identity(health.pid)?;
    let birth = crate::launch::process_birth(health.pid)?
        .context("key owner exited during verification")?;
    let prefix = if cfg!(windows) {
        "windows-filetime-"
    } else if cfg!(target_os = "linux") {
        "linux-starttime-"
    } else {
        "darwin-lstart-"
    };
    anyhow::ensure!(
        health.managed_id.as_deref() == managed_id
            && health.account_id == account(expected, profile)
            && Path::new(&health.installation_root).canonicalize()? == root
            && health.installation_id == crate::launch::hash(root.display().to_string().as_bytes())
            && Path::new(&health.data_dir).canonicalize()? == profile
            && Path::new(&health.exe).canonicalize()? == image.path()
            && Path::new(&health.image_path).canonicalize()? == image.path()
            && Path::new(&process.executable).canonicalize()? == image.path()
            && health.image_sha256.eq_ignore_ascii_case(image.sha256())
            && health.runtime_version == image.version()
            && health.process_start_id == format!("{prefix}{birth}")
            && (process.started_at == birth || process.started_at == health.process_start_id)
            && !health.inference_stalled(),
        "key owner differs from the installed Runtime, account, profile or registration"
    );
    anyhow::ensure!(
        respire_commands::net_rpc::runtime_process_identity(health.pid)? == process,
        "key owner changed during verification"
    );
    Ok(())
}
