//! One-shot launch grants. Public instance IDs cannot replay consumed grants.
use anyhow::{Context, Result};
use respire_protocol::HostControlLaunchGrant as LaunchGrant;
#[cfg(test)]
use respire_protocol::HostControlMaintenanceBinding as MaintenanceBinding;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

static STARTUP: OnceLock<Startup> = OnceLock::new();

#[derive(Clone, Debug, PartialEq, Eq)]
struct LaunchContext {
    root: PathBuf,
    profile: PathBuf,
    account_id: String,
    sync_generation: usize,
    image_path: PathBuf,
    image_sha256: String,
    port: u16,
}

struct Startup {
    context: LaunchContext,
    grant: Option<LaunchGrant>,
    private_control: bool,
    auth_control: bool,
    paused_control: bool,
    unbound_identity: Option<respire_protocol::HostAuthIdentity>,
    migrated_identity: Option<[String; 2]>,
}

pub(crate) fn authorize(supervised: bool, boot_token: Option<&str>, port: u16) -> Result<()> {
    anyhow::ensure!(!respire_app::login_transaction::auth_recovery_pending()?,
        "interrupted auth control must be recovered before serving");
    anyhow::ensure!(
        STARTUP.get().is_none(),
        "runtime startup was authorized twice"
    );
    fs::create_dir_all(respire_app::service::main_data_dir())?;
    fs::create_dir_all(respire_app::service::data_dir())?;
    let context = current_context(port)?;
    let migrated_identity = respire_client_host::identity::read_migrated_local_session(&context.profile, hash)?
        .map(|(_, identity)| identity);
    let unbound_identity = serving_unbound_identity(&context.profile)?;
    if let Some(identity) = unbound_identity.as_ref() {
        respire_app::login_transaction::verify_locked_auth_identity(&context.root, &context.profile, identity)?;
    }
    let (grant, instance_id) = if let Some(token) = boot_token {
        let nonce = token
            .strip_prefix("launch-v1:")
            .context("unsupported runtime launch token")?;
        validate_nonce(nonce)?;
        let directory = context
            .root
            .join("runtime")
            .join("autostart")
            .join("launch");
        let path = directory.join(format!("launch-{nonce}.json"));
        let bytes =
            read_private_file(&path)?.context("runtime launch grant is missing or consumed")?;
        let grant: LaunchGrant = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid runtime launch grant JSON"))?;
        validate_grant(&grant, &context, supervised, nonce, now_ms()?)?;
        verify_authority(Some(&grant), &context)?;
        claim(&directory, nonce, &bytes)?;
        fs::remove_file(&path).context("consumed runtime launch grant could not be removed")?;
        sync_directory(&directory)?;
        let instance_id = grant.instance_id.clone();
        (Some(grant), instance_id)
    } else {
        anyhow::ensure!(
            !supervised,
            "supervised startup requires a one-time host launch grant"
        );
        verify_authority(None, &context)?;
        (None, uuid::Uuid::new_v4().to_string())
    };
    respire_commands::rpc_serve::set_launch_instance_id(&instance_id)?;
    STARTUP
        .set(Startup { context, grant, private_control: false, auth_control: false, paused_control: false, unbound_identity, migrated_identity })
        .map_err(|_| anyhow::anyhow!("runtime startup identity was installed twice"))?;
    Ok(())
}

pub(crate) fn managed_id() -> Option<&'static str> {
    STARTUP.get()?.grant.as_ref()?.managed_id.as_deref()
}
mod authority;
mod control;
use authority::{validate_grant, verify_authority, verify_authority_for_mode, verify_offline_read_authority};
#[cfg(test)]
use authority::{validate_control, validate_control_for_mode};
#[cfg(test)]
use control::{offline_read_context, verify_completion_marker, validate_key_result_context, validate_identity_write_context, validate_control_binding};
pub(crate) use control::{
    authorize_control,
    authorize_offline_read,
    verify_offline_read_binding,
    authorize_auth_control,
    verify_login_recovery_status,
    authorize_account_completion,
    verify_account_completion_authority,
    verify_core_config_maintenance,
    consumed_control_grant,
    authorize_snapshot_control,
    authorize_model_control,
    authorize_metadata_control,
    verify_metadata_control_binding,
    verify_keygen_control_binding,
    verify_auth_control_binding,
    verify_registration_control_binding,
    acquire_auth_ownership,
    verify_key_control_result,
    verify_identity_write_result,
    verify_control_binding,
    verify_memory_revision_binding,
};

pub(crate) fn verify_managed_boot() -> Result<()> {
    verify_managed_boot_inner(true)
}

fn verify_managed_boot_inner(check_login_recovery: bool) -> Result<()> {
    let startup = STARTUP
        .get()
        .context("runtime startup authorization is missing")?;
    if check_login_recovery && !startup.auth_control {
        anyhow::ensure!(!respire_app::login_transaction::auth_recovery_pending()?,
            "interrupted auth control must be recovered before opening the store");
    }
    anyhow::ensure!(
        current_context(startup.context.port)? == startup.context,
        "runtime launch context changed before opening the store"
    );
    if !startup.auth_control {
        anyhow::ensure!(
            respire_client_host::identity::read_migrated_local_session(&startup.context.profile, hash)?
                .map(|(_, identity)| identity)
                == startup.migrated_identity,
            "migrated local session changed before opening the store"
        );
        anyhow::ensure!(
            serving_unbound_identity(&startup.context.profile)? == startup.unbound_identity,
            "offline serving session changed before opening the store"
        );
        if let Some(identity) = startup.unbound_identity.as_ref() {
            respire_app::login_transaction::verify_locked_auth_identity(&startup.context.root, &startup.context.profile, identity)?;
        }
    }
    if startup.private_control {
        verify_authority_for_mode(startup.grant.as_ref(), &startup.context, startup.paused_control)
    } else {
        verify_authority(startup.grant.as_ref(), &startup.context)
    }
}

/// Verify the exact inherited parent and installation without consuming a
/// serving or Paused grant. This cannot authorize any other host operation.
pub(crate) fn verify_cold_keygen_launch(
    grant: &LaunchGrant,
    binding: &respire_protocol::ControlBinding,
) -> Result<()> {
    respire_commands::runtime_policy::require_host("private cold keygen")?;
    respire_spawn::verify_control_parent(grant.supervisor_pid)?;
    respire_app::service::validate_client_config()?;
    let context = current_context(respire_commands::net_rpc::rpc_port())?;
    anyhow::ensure!(
        grant.maintenance.is_none() && grant.managed_id.is_none()
            && context.account_id == hash(context.profile.display().to_string().as_bytes()),
        "cold keygen requires an unbound, unmanaged profile"
    );
    validate_grant(grant, &context, false, &grant.nonce, now_ms()?)?;
    control::validate_control_binding(binding, &context, grant, None)?;
    // An old host may have published its journal before the Paused marker.
    // Do not inspect or implicitly recover any legacy transaction here.
    let control = context.root.join("runtime").join("autostart");
    for name in ["offline-keygen.json", "maintenance.json", "account-active.json",
        "key-control.json", "space-client2.json"]
    {
        match fs::symlink_metadata(control.join(name)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
            Ok(_) => anyhow::bail!("another host control operation must be recovered first"),
        }
    }
    verify_authority_for_mode(Some(grant), &context, false)
}


fn serving_unbound_identity(profile: &Path) -> Result<Option<respire_protocol::HostAuthIdentity>> {
    use respire_client_host::identity::{auth_session_identity, AuthSessionIdentity};
    let Some(session) = respire_client_host::identity::read_session_json(&profile.join("session.json"))? else {
        return Ok(None);
    };
    // Legacy sessions retain their existing migration and credential path.
    // A new transaction claim must never fall back to legacy authorization.
    if session.get("auth_operation_id").is_none() {
        return Ok(None);
    }
    match auth_session_identity(profile, hash)? {
        AuthSessionIdentity::Unbound { operation_id, session_sha256 } =>
            Ok(Some(respire_protocol::HostAuthIdentity::Unbound { operation_id, session_sha256 })),
        AuthSessionIdentity::Bound { .. } => Ok(None),
        AuthSessionIdentity::Absent => anyhow::bail!("offline serving session disappeared during authorization"),
    }
}

fn current_context(port: u16) -> Result<LaunchContext> {
    let root = respire_app::service::main_data_dir().canonicalize()?;
    let profile = respire_app::service::data_dir().canonicalize()?;
    let account = respire_app::service::session_user_of_dir(&profile);
    let account_source = if account.is_empty() {
        profile.display().to_string()
    } else {
        account
    };
    let image_path = std::env::current_exe()?.canonicalize()?;
    Ok(LaunchContext {
        root,
        profile,
        account_id: hash(account_source.as_bytes()),
        sync_generation: respire_commands::rpc_worker::sync_generation(),
        image_sha256: hash_file(&image_path)?,
        image_path,
        port,
    })
}

fn validate_nonce(nonce: &str) -> Result<()> {
    let uuid = uuid::Uuid::parse_str(nonce)
        .map_err(|_| anyhow::anyhow!("invalid runtime launch nonce"))?;
    anyhow::ensure!(
        uuid.get_version_num() == 4 && uuid.to_string() == nonce,
        "runtime launch nonce must be a canonical random UUID"
    );
    Ok(())
}

/// Match the CLI launch producer's birth format without inspecting command lines.
pub(crate) fn process_birth(pid: u32) -> Result<Option<String>> {
    anyhow::ensure!(pid > 0, "invalid launch owner PID");
    #[cfg(windows)]
    {
        return Ok(respire_spawn::process_birth(pid)?.map(|birth| birth.to_string()));
    }
    #[cfg(target_os = "linux")]
    {
        let raw = match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        return linux_process_birth(&raw);
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let output = std::process::Command::new("/bin/ps")
            .args(["-p", &pid.to_string(), "-o", "lstart="])
            .env("LC_ALL", "C")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()?;
        if output.status.code() == Some(1) && output.stdout.is_empty() {
            return Ok(None);
        }
        anyhow::ensure!(output.status.success(), "launch owner birth lookup failed");
        let birth = String::from_utf8(output.stdout)?;
        let birth = birth.trim();
        anyhow::ensure!(!birth.is_empty(), "launch owner birth is empty");
        return Ok(Some(birth.to_owned()));
    }
    #[cfg(not(any(windows, unix)))]
    anyhow::bail!("launch owner identity is unsupported on this platform")
}

#[cfg(any(target_os = "linux", test))]
fn linux_process_birth(raw: &str) -> Result<Option<String>> {
    let fields = raw
        .rsplit_once(')')
        .context("invalid launch owner process identity")?
        .1;
    if matches!(fields.split_whitespace().next(), Some("Z" | "X")) {
        return Ok(None);
    }
    let birth = fields
        .split_whitespace()
        .nth(19)
        .context("launch owner birth is missing")?;
    let ticks = birth.parse::<u64>().context("invalid launch owner birth")?;
    anyhow::ensure!(ticks > 0, "launch owner birth cannot be zero");
    Ok(Some(ticks.to_string()))
}

fn claim(directory: &Path, nonce: &str, grant: &[u8]) -> Result<()> {
    let path = directory.join(format!("launch-{nonce}.claimed"));
    let birth = process_birth(std::process::id())?
        .context("runtime claimant process identity is unavailable")?;
    let verified: LaunchGrant = serde_json::from_slice(grant)?;
    let registration = read_private_file(
        &directory.parent().context("launch directory has no parent")?.join("registration.json"),
    )?;
    anyhow::ensure!(
        verified.registration_sha256 == registration.as_deref().map(hash),
        "registration changed before the launch claim was persisted"
    );
    let registration_json = registration.map(String::from_utf8).transpose()?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .context("runtime launch grant was already claimed")?;
    // Retain the claim on any later failure; a failed startup never revives a token.
    file.write_all(&serde_json::to_vec(&json!({
        "schema_version": 2,
        "protocol_version": respire_commands::rpc_worker::PROTOCOL_V,
        "state": "consumed",
        "grant_sha256": hash(grant),
        "grant_json": std::str::from_utf8(grant)?,
        "registration_json": registration_json,
        "pid": std::process::id(), "process_start_id": birth,
    }))?)?;
    file.sync_all()?;
    sync_directory(directory)
}

fn parse_control(bytes: &[u8]) -> Result<Value> {
    serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("invalid runtime startup authorization JSON"))
}

pub(crate) fn read_private_file(path: &Path) -> Result<Option<Vec<u8>>> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                anyhow::ensure!(
                    !metadata.file_type().is_symlink(),
                    "runtime authorization path contains a symlink"
                );
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    anyhow::ensure!(
                        metadata.file_attributes() & 0x400 == 0,
                        "runtime authorization path contains a reparse point"
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(
        metadata.is_file() && metadata.len() <= 64 * 1024,
        "runtime startup authorization must be a bounded regular file"
    );
    let file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() <= 64 * 1024,
        "runtime startup authorization is too large"
    );
    Ok(Some(bytes))
}

fn sync_directory(directory: &Path) -> Result<()> {
    #[cfg(unix)]
    fs::File::open(directory)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = directory;
    Ok(())
}

fn now_ms() -> Result<u64> {
    u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())
        .context("runtime launch clock overflow")
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn hash_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}


#[cfg(test)]
mod tests;
