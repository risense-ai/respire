//! Scoped private-control startup and operation binding.
use super::*;


/// An owned inherited channel is the capability, not the serialized identity.
/// Do not consume a serving grant file or create runtime state for a read probe.
pub(crate) fn authorize_control(grant: LaunchGrant) -> Result<()> {
    authorize_control_inner(grant, false, false)
}

/// A fixed read may inspect an absent profile, but cannot create or serve it.
pub(crate) fn authorize_offline_read(grant: LaunchGrant,
    kind: respire_protocol::metadata_control::HostOfflineRead) -> Result<()> {
    anyhow::ensure!(STARTUP.get().is_none(), "offline read grant was already consumed");
    respire_commands::runtime_policy::require_host("offline configuration read")?;
    respire_spawn::verify_control_parent(grant.supervisor_pid)?;
    respire_app::service::validate_client_config()?;
    if kind != respire_protocol::metadata_control::HostOfflineRead::SnapshotPreflight {
        anyhow::ensure!(!respire_app::login_transaction::auth_recovery_pending()?,
            "finish authentication recovery before an offline configuration read");
    }
    let context = offline_read_context(grant.port, kind)?;
    validate_grant(&grant, &context, grant.managed_id.is_some(), &grant.nonce, now_ms()?)?;
    verify_offline_read_authority(&grant, &context, kind)?;
    STARTUP.set(Startup { context, grant: Some(grant), private_control: true, auth_control: false, paused_control: false, unbound_identity: None, migrated_identity: None })
        .map_err(|_| anyhow::anyhow!("offline read grant was already consumed"))?;
    Ok(())
}

pub(crate) fn verify_offline_read_binding(binding: &respire_protocol::ControlBinding,
    kind: respire_protocol::metadata_control::HostOfflineRead) -> Result<()> {
    let startup = STARTUP.get().context("offline read grant is missing")?;
    let grant = startup.grant.as_ref().context("offline read grant is missing")?;
    respire_spawn::verify_control_parent(grant.supervisor_pid)?;
    let current = offline_read_context(startup.context.port, kind)?;
    anyhow::ensure!(current == startup.context, "offline read selection or installation changed");
    let account = if kind == respire_protocol::metadata_control::HostOfflineRead::SnapshotPreflight { None }
        else { respire_protocol::metadata_control::offline_read_user(&current.profile)?
            .map(|user| hash(user.as_bytes())) };
    validate_control_binding(binding, &current, grant, account.as_deref())?;
    if kind != respire_protocol::metadata_control::HostOfflineRead::SnapshotPreflight {
        anyhow::ensure!(!respire_app::login_transaction::auth_recovery_pending()?,
            "authentication recovery interrupted the offline read");
    }
    verify_offline_read_authority(grant, &current, kind)
}

pub(super) fn offline_read_context(port: u16, kind: respire_protocol::metadata_control::HostOfflineRead) -> Result<LaunchContext> {
    use respire_protocol::metadata_control::metadata_status_path;
    let root = metadata_status_path(&respire_app::service::main_data_dir())?;
    let profile = metadata_status_path(&respire_app::service::data_dir())?;
    let account_id = if kind == respire_protocol::metadata_control::HostOfflineRead::SnapshotPreflight { None }
        else { respire_protocol::metadata_control::offline_read_user(&profile)?
            .map(|user| hash(user.as_bytes())) }
        .unwrap_or_else(|| hash(profile.display().to_string().as_bytes()));
    let image_path = std::env::current_exe()?.canonicalize()?;
    Ok(LaunchContext { root, profile, account_id,
        sync_generation: respire_commands::rpc_worker::sync_generation(),
        image_sha256: hash_file(&image_path)?, image_path, port })
}

pub(crate) fn authorize_auth_control(grant: LaunchGrant) -> Result<()> {
    anyhow::ensure!(grant.maintenance.is_some(), "auth control requires a paused maintenance owner");
    authorize_control_inner(grant, true, true)
}

/// This fixed read reports only journal state and cannot claim a maintenance owner.
pub(crate) fn verify_login_recovery_status(
    grant: &LaunchGrant,
    params: &respire_protocol::host_control::HostLoginRecoveryParams,
) -> Result<()> {
    anyhow::ensure!(STARTUP.get().is_none() && grant.maintenance.is_none(),
        "recovery status cannot inherit a serving or maintenance authorization");
    respire_commands::runtime_policy::require_host("login recovery status")?;
    respire_spawn::verify_control_parent(grant.supervisor_pid)?;
    respire_app::service::validate_client_config()?;
    let context = current_context(grant.port)?;
    validate_grant(grant, &context, grant.managed_id.is_some(), &grant.nonce, now_ms()?)?;
    let registration = read_private_file(&context.root.join("runtime/autostart/registration.json"))?;
    anyhow::ensure!(grant.registration_sha256 == registration.as_deref().map(hash)
        && process_birth(grant.supervisor_pid)?.as_deref() == Some(grant.supervisor_birth.as_str()),
        "recovery status registration or parent changed");
    let account = match &params.identity {
        respire_protocol::HostAuthIdentity::Bound { account_id, .. } => Some(account_id.as_str()),
        _ => None,
    };
    validate_control_binding(&params.binding, &context, grant, account)?;
    respire_app::login_transaction::verify_locked_auth_identity(&context.root, &context.profile, &params.identity)
}

pub(crate) fn authorize_account_completion(grant: LaunchGrant) -> Result<()> {
    anyhow::ensure!(grant.maintenance.is_some(), "account completion requires a verified maintenance owner");
    authorize_control_inner(grant, true, false)?;
    verify_account_completion_authority()
}

pub(crate) fn verify_account_completion_authority() -> Result<()> {
    let startup = STARTUP.get().context("account completion grant is missing")?;
    anyhow::ensure!(startup.auth_control && !startup.paused_control,
        "account completion cannot use an offline auth grant");
    verify_managed_boot()?;
    let grant = startup.grant.as_ref().context("account completion grant is missing")?;
    let binding = grant.maintenance.as_ref().context("account completion maintenance is missing")?;
    let bytes = read_private_file(&startup.context.root.join("runtime/autostart/maintenance.json"))?
        .context("account completion maintenance disappeared")?;
    verify_completion_marker(&bytes, &binding.marker_sha256)
}

pub(super) fn verify_completion_marker(bytes: &[u8], expected_sha256: &str) -> Result<()> {
    let marker = parse_control(bytes)?;
    anyhow::ensure!(hash(bytes) == expected_sha256 && marker["phase"] == "Verified",
        "account completion requires the exact verified maintenance receipt");
    Ok(())
}

/// Public query configuration keeps the host's exact Paused maintenance authority.
pub(crate) fn verify_core_config_maintenance(
    binding: Option<&respire_protocol::HostControlMaintenanceBinding>,
) -> Result<()> {
    let roots = respire_app::service::resolved_roots()?;
    let marker = read_private_file(&roots.installation_root.join("runtime/autostart/maintenance.json"))?;
    match (marker, binding) {
        (None, None) => Ok(()),
        (Some(bytes), Some(binding)) => {
            let marker = parse_control(&bytes)?;
            anyhow::ensure!(hash(&bytes) == binding.marker_sha256
                && marker["schema"] == 1 && marker["phase"] == "Paused"
                && marker["id"] == binding.coordinator_id && !binding.coordinator_id.is_empty()
                && marker["owner_pid"] == binding.controller_pid
                && marker["owner_birth"] == binding.controller_birth
                && marker["expected_version"] == binding.issuer_cli_version
                && !binding.issuer_cli_version.is_empty() && !binding.controller_birth.is_empty()
                && marker["expected_profile"].as_str().map(Path::new) == Some(roots.account_data_root.canonicalize()?.as_path())
                && process_birth(binding.controller_pid)?.as_deref() == Some(binding.controller_birth.as_str()),
                "core configuration requires the exact live Paused maintenance owner");
            Ok(())
        }
        _ => anyhow::bail!("core configuration maintenance binding is missing or stale"),
    }
}

pub(crate) fn consumed_control_grant() -> Result<&'static LaunchGrant> {
    STARTUP.get().and_then(|startup| startup.grant.as_ref())
        .context("runtime control grant is missing")
}

pub(crate) fn authorize_snapshot_control(grant: LaunchGrant, finalize: bool) -> Result<()> {
    anyhow::ensure!(grant.maintenance.is_some(), "snapshot control requires maintenance ownership");
    anyhow::ensure!(!respire_app::login_transaction::auth_recovery_pending()?,
        "interrupted auth control must be recovered before snapshot maintenance");
    authorize_control_inner(grant, false, !finalize)
}

pub(crate) fn authorize_model_control(grant: LaunchGrant) -> Result<()> {
    anyhow::ensure!(grant.maintenance.is_some(), "model preparation requires a paused maintenance owner");
    anyhow::ensure!(!respire_app::login_transaction::auth_recovery_pending()?,
        "interrupted auth control must be recovered before model preparation");
    authorize_control_inner(grant, false, true)?;
    anyhow::ensure!(!respire_app::service::off_mode(), "model preparation is unavailable while memory is off");
    respire_app::service::ensure_writable()?;
    respire_app::snapshot::ensure_no_incomplete_restore(&respire_app::service::data_dir())
}

pub(crate) fn authorize_metadata_control(grant: LaunchGrant) -> Result<()> {
    anyhow::ensure!(grant.maintenance.is_some(), "metadata requires Paused maintenance ownership");
    anyhow::ensure!(!respire_app::login_transaction::auth_recovery_pending()?,
        "finish authentication recovery before metadata maintenance");
    // Local-only sessions have no cloud user. Their exact bytes are checked by
    // verify_metadata_control_binding rather than admitted as a business account.
    authorize_control_inner(grant, true, true)?;
    anyhow::ensure!(!respire_app::service::off_mode(), "restore normal service before changing metadata");
    respire_app::service::ensure_writable()?;
    respire_app::snapshot::ensure_no_incomplete_restore(&respire_app::service::data_dir())
}

pub(crate) fn verify_metadata_control_binding(binding: &respire_protocol::ControlBinding,
    identity: &respire_protocol::metadata_control::HostMetadataIdentity) -> Result<()> {
    let startup = STARTUP.get().context("metadata grant is missing")?;
    anyhow::ensure!(startup.auth_control && startup.paused_control, "metadata requires a Paused grant");
    let grant = startup.grant.as_ref().context("metadata grant is missing")?;
    respire_spawn::verify_control_parent(grant.supervisor_pid)?;
    anyhow::ensure!(current_context(startup.context.port)? == startup.context
        && respire_protocol::metadata_control::metadata_identity(&startup.context.profile, hash)? == *identity,
        "metadata selection or session identity changed");
    validate_control_binding(binding, &startup.context, grant, identity.account_id.as_deref())?;
    anyhow::ensure!(!respire_app::login_transaction::auth_recovery_pending()?,
        "authentication recovery interrupted metadata maintenance");
    verify_authority_for_mode(Some(grant), &startup.context, true)
}

fn authorize_control_inner(grant: LaunchGrant, auth: bool, paused: bool) -> Result<()> {
    anyhow::ensure!(STARTUP.get().is_none(), "runtime control grant was already consumed");
    respire_commands::runtime_policy::require_host("offline host control")?;
    respire_spawn::verify_control_parent(grant.supervisor_pid)?;
    respire_app::service::validate_client_config()?;
    let resolved_profile = respire_app::service::data_dir();
    let context = current_context(grant.port)?;
    anyhow::ensure!(
        resolved_profile.canonicalize()? == context.profile,
        "runtime control profile changed during authorization"
    );
    validate_grant(
        &grant,
        &context,
        grant.managed_id.is_some(),
        &grant.nonce,
        now_ms()?,
    )?;
    verify_authority_for_mode(Some(&grant), &context, paused)?;
    // Strict parsing must precede accepting the legacy account/path hash.
    if !auth { control_account(&context.profile)?; }
    // Pin the exact resolved spelling used by memory_revision before it opens DB.
    if !auth { respire_app::service::install_runtime_profile(resolved_profile); }
    let unbound_identity = if auth { None } else { serving_unbound_identity(&context.profile)? };
    let migrated_identity = if auth { None } else {
        respire_client_host::identity::read_migrated_local_session(&context.profile, hash)?
            .map(|(_, identity)| identity)
    };
    if let Some(identity) = unbound_identity.as_ref() {
        respire_app::login_transaction::verify_locked_auth_identity(&context.root, &context.profile, identity)?;
    }
    STARTUP.set(Startup { context, grant: Some(grant), private_control: true, auth_control: auth, paused_control: paused, unbound_identity, migrated_identity })
        .map_err(|_| anyhow::anyhow!("runtime control grant was already consumed"))?;
    Ok(())
}

/// Keygen authenticates its candidate using explicit host material, never a credential broker.
pub(crate) fn verify_keygen_control_binding(
    binding: &respire_protocol::ControlBinding,
    identity: &respire_protocol::HostAuthIdentity,
) -> Result<()> {
    let startup = STARTUP.get().context("runtime control grant is not consumed")?;
    let grant = startup.grant.as_ref().context("runtime control grant is missing")?;
    respire_spawn::verify_control_parent(grant.supervisor_pid)?;
    respire_app::login_transaction::verify_locked_auth_identity(&startup.context.root, &startup.context.profile, identity)?;
    let account = match identity {
        respire_protocol::HostAuthIdentity::Bound { account_id, .. } => Some(account_id.as_str()),
        _ => None,
    };
    validate_control_binding(binding, &startup.context, grant, account)?;
    verify_managed_boot()
}

pub(crate) fn verify_auth_control_binding(
    binding: &respire_protocol::ControlBinding,
    identity: &respire_protocol::HostAuthIdentity,
) -> Result<()> {
    let startup = STARTUP.get().context("runtime control grant is not consumed")?;
    let grant = startup.grant.as_ref().context("runtime control grant is missing")?;
    respire_spawn::verify_control_parent(grant.supervisor_pid)?;
    respire_app::login_transaction::verify_locked_auth_identity(&startup.context.root, &startup.context.profile, identity)?;
    let account = match identity {
        respire_protocol::HostAuthIdentity::Bound { account_id, .. } => Some(account_id.as_str()),
        _ => None,
    };
    validate_control_binding(binding, &startup.context, grant, account)?;
    verify_managed_boot()
}

/// Host-owned registration verifies the source receipt without reopening credential custody.
pub(crate) fn verify_registration_control_binding(
    binding: &respire_protocol::ControlBinding,
    identity: &respire_protocol::HostAuthIdentity,
) -> Result<()> {
    let startup = STARTUP.get().context("runtime control grant is not consumed")?;
    let grant = startup.grant.as_ref().context("runtime control grant is missing")?;
    respire_spawn::verify_control_parent(grant.supervisor_pid)?;
    respire_app::login_transaction::verify_locked_auth_identity(&startup.context.root, &startup.context.profile, identity)?;
    let account = match identity {
        respire_protocol::HostAuthIdentity::Bound { account_id, .. } => Some(account_id.as_str()),
        _ => None,
    };
    validate_control_binding(binding, &startup.context, grant, account)?;
    verify_managed_boot()
}

pub(crate) fn acquire_auth_ownership(
    binding: &respire_protocol::ControlBinding,
    identity: &respire_protocol::HostAuthIdentity,
) -> Result<respire_app::login_transaction::AuthOwnership> {
    let startup = STARTUP.get().context("runtime control grant is not consumed")?;
    let grant = startup.grant.as_ref().context("runtime control grant is missing")?;
    let account = match identity {
        respire_protocol::HostAuthIdentity::Bound { account_id, .. } => Some(account_id.as_str()),
        _ => None,
    };
    validate_control_binding(binding, &startup.context, grant, account)?;
    let ownership = respire_app::login_transaction::AuthOwnership::acquire(&startup.context.root, &startup.context.profile)?;
    respire_spawn::verify_control_parent(grant.supervisor_pid)?;
    verify_managed_boot()?;
    Ok(ownership)
}

/// Called only after the app has matched this result to its durable key receipt.
/// The launch grant stays bound to the initial context; only fivekeys may select
/// the receipt's exact target, and no other launch field is allowed to change.
pub(crate) fn verify_key_control_result(
    binding: &respire_protocol::ControlBinding,
    launched_identity: &respire_protocol::HostAuthIdentity,
    result: &respire_protocol::key_control::HostKeyResult,
) -> Result<()> {
    use respire_protocol::HostAuthIdentity;
    let startup = STARTUP.get().context("key control grant is missing")?;
    anyhow::ensure!(startup.auth_control, "key control requires an auth grant");
    let grant = startup.grant.as_ref().context("key control grant is missing")?;
    let account = match launched_identity { HostAuthIdentity::Bound { account_id, .. } => Some(account_id.as_str()), _ => None };
    validate_control_binding(binding, &startup.context, grant, account)?;
    respire_spawn::verify_control_parent(grant.supervisor_pid)?;
    verify_authority_for_mode(Some(grant), &startup.context, startup.paused_control)?;
    let current = current_context(startup.context.port)?;
    validate_key_result_context(&startup.context, launched_identity, &current, result)?;
    respire_app::login_transaction::verify_locked_auth_identity(&current.root, &current.profile, &result.identity)
}

pub(super) fn validate_key_result_context(original: &LaunchContext, launched_identity: &respire_protocol::HostAuthIdentity,
    current: &LaunchContext, result: &respire_protocol::key_control::HostKeyResult) -> Result<()> {
    use respire_protocol::HostAuthIdentity;
    use respire_protocol::key_control::{HostKeyOperation, HostKeyOutcome};
    let mut expected = original.clone();
    match &result.operation {
        HostKeyOperation::SuperReset { .. } => {
            anyhow::ensure!(result.source_profile == original.profile
                && result.target_profile.as_ref().is_none_or(|target| *target == original.profile)
                && !result.verification_required, "super-reset changed its profile");
            anyhow::ensure!(matches!((launched_identity, &result.identity),
                (HostAuthIdentity::Bound { account_id: before, .. }, HostAuthIdentity::Bound { account_id: after, .. })
                    if before == after), "super-reset changed its account");
        }
        HostKeyOperation::Fivekeys { addr, user, .. } => {
            if result.verification_required || result.outcome == HostKeyOutcome::Committed {
                let target = result.target_profile.as_ref().context("fivekeys target missing")?;
                if user.is_empty() {
                    anyhow::ensure!(addr.trim().is_empty() && result.account_id.is_none()
                        && matches!(&result.identity, HostAuthIdentity::Unbound { operation_id, session_sha256 }
                            if operation_id == &result.operation_id && Some(session_sha256) == result.session_sha256.as_ref()),
                        "offline recovery identity differs from its intent");
                } else {
                    let account = hash(user.as_bytes());
                    anyhow::ensure!(result.account_id.as_ref() == Some(&account)
                        && matches!(&result.identity, HostAuthIdentity::Bound { account_id, session_sha256 }
                            if account_id == &account && Some(session_sha256) == result.session_sha256.as_ref()),
                        "fivekeys target identity differs from its intent");
                }
                expected.profile = target.clone();
            } else if result.outcome == HostKeyOutcome::RolledBack {
                expected.profile = result.source_profile.clone();
            } else {
                anyhow::ensure!(result.identity == *launched_identity, "unpublished fivekeys changed its identity");
            }
            expected.account_id = match &result.identity {
                HostAuthIdentity::Bound { account_id, .. } => account_id.clone(),
                _ => hash(expected.profile.display().to_string().as_bytes()),
            };
        }
    }
    anyhow::ensure!(*current == expected, "key control changed fields outside its owned target");
    Ok(())
}

/// An owned identity transaction may remove its session; the serving fence stays strict.
pub(crate) fn verify_identity_write_result(
    binding: &respire_protocol::ControlBinding,
    launched_identity: &respire_protocol::HostAuthIdentity,
    result: &respire_protocol::identity_write_control::HostIdentityWriteResult,
) -> Result<()> {
    let startup = STARTUP.get().context("identity write grant is missing")?;
    anyhow::ensure!(startup.auth_control && startup.paused_control,
        "identity write requires a Paused private control grant");
    let grant = startup.grant.as_ref().context("identity write grant is missing")?;
    let original_account = match launched_identity {
        respire_protocol::HostAuthIdentity::Bound { account_id, .. } => Some(account_id.as_str()),
        _ => None,
    };
    validate_control_binding(binding, &startup.context, grant, original_account)?;
    respire_spawn::verify_control_parent(grant.supervisor_pid)?;
    verify_authority_for_mode(Some(grant), &startup.context, true)?;
    respire_app::login_transaction::verify_locked_auth_identity(
        &startup.context.root, &startup.context.profile, &result.identity,
    )?;
    validate_identity_write_context(&startup.context, launched_identity,
        &current_context(startup.context.port)?, result)
}

pub(super) fn validate_identity_write_context(
    original: &LaunchContext,
    launched_identity: &respire_protocol::HostAuthIdentity,
    current: &LaunchContext,
    result: &respire_protocol::identity_write_control::HostIdentityWriteResult,
) -> Result<()> {
    use respire_protocol::HostAuthIdentity;
    use respire_protocol::identity_write_control::{HostIdentityWriteOperation, HostIdentityWriteOutcome};
    anyhow::ensure!(result.path == original.profile.join("session.json"),
        "identity write receipt path differs from the owned profile");
    match (launched_identity, &result.identity) {
        (_, HostAuthIdentity::Absent) => anyhow::ensure!(
            matches!(launched_identity, HostAuthIdentity::Absent)
                || matches!(&result.operation, HostIdentityWriteOperation::Logout { full: true }),
            "this identity operation cannot remove its session"),
        (HostAuthIdentity::Bound { account_id: before, .. }, HostAuthIdentity::Bound { account_id: after, .. }) =>
            anyhow::ensure!(before == after, "identity write cannot change the account"),
        (HostAuthIdentity::Unbound { operation_id: before, .. }, HostAuthIdentity::Unbound { operation_id: after, .. }) =>
            anyhow::ensure!(before == after, "identity write cannot replace the unbound operation"),
        _ => anyhow::bail!("identity write created a different session identity"),
    }
    let session_sha256 = match &result.identity {
        HostAuthIdentity::Absent => None,
        HostAuthIdentity::Bound { session_sha256, .. }
        | HostAuthIdentity::Unbound { session_sha256, .. } => Some(session_sha256.as_str()),
    };
    anyhow::ensure!(result.session_sha256.as_deref() == session_sha256,
        "identity write receipt session digest mismatch");
    if result.outcome == HostIdentityWriteOutcome::Committed
        && matches!(&result.operation, HostIdentityWriteOperation::Logout { full: true }) {
        anyhow::ensure!(matches!(&result.identity, HostAuthIdentity::Absent),
            "completed full logout still has a session");
    }
    let mut expected = original.clone();
    expected.account_id = match &result.identity {
        respire_protocol::HostAuthIdentity::Bound { account_id, .. } => account_id.clone(),
        _ => hash(original.profile.display().to_string().as_bytes()),
    };
    anyhow::ensure!(*current == expected,
        "identity write changed more than its owned account identity");
    Ok(())
}

pub(crate) fn verify_control_binding(binding: &respire_protocol::ControlBinding) -> Result<()> {
    verify_control_binding_inner(binding, true)
}

/// Revision reads must not inspect or recover an interrupted login journal.
pub(crate) fn verify_memory_revision_binding(binding: &respire_protocol::ControlBinding) -> Result<()> {
    verify_control_binding_inner(binding, false)
}

fn verify_control_binding_inner(
    binding: &respire_protocol::ControlBinding, check_login_recovery: bool,
) -> Result<()> {
    let startup = STARTUP.get().context("runtime control grant is not consumed")?;
    let grant = startup.grant.as_ref().context("runtime control grant is missing")?;
    respire_spawn::verify_control_parent(grant.supervisor_pid)?;
    let account = control_account(&startup.context.profile)?;
    validate_control_binding(binding, &startup.context, grant, account.as_deref())?;
    verify_managed_boot_inner(check_login_recovery)
}

pub(super) fn validate_control_binding(
    binding: &respire_protocol::ControlBinding,
    context: &LaunchContext,
    grant: &LaunchGrant,
    account_id: Option<&str>,
) -> Result<()> {
    anyhow::ensure!(
        binding.installation_root == context.root
            && binding.profile == context.profile
            && binding.launch_id == grant.instance_id
            && binding.expected_account_id.as_deref() == account_id,
        "runtime control binding does not match the consumed grant or account"
    );
    Ok(())
}

fn control_account(profile: &Path) -> Result<Option<String>> {
    let Some(bytes) = read_private_file(&profile.join("session.json"))? else {
        return Ok(None);
    };
    let session: Value = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid control session JSON"))?;
    anyhow::ensure!(session.is_object(), "control session must be an object");
    match session.get("user") {
        None => Ok(None),
        Some(Value::String(user)) if user.is_empty() => Ok(None),
        Some(Value::String(user)) => Ok(Some(hash(user.as_bytes()))),
        _ => anyhow::bail!("control session user must be a string"),
    }
}
