//! Linux installation-owner backup control. Request frames travel on inherited
//! pipes. The supervisor retains the takeover lock across all owned children.
use anyhow::{ensure, Context, Result};
use respire_app::{auth, lock::LibraryLock, snapshot};
use respire_commands::{
    backup_restart,
    net_rpc::{self, Health},
};
use respire_protocol::{credentials::RuntimeUnlock, HostControlLaunchGrant as Launch};
use respire_spawn::owner_credential::{
    archive_proof::{self, ArchiveClaim},
    backup_grant::{self, Receiver, Scope},
    read_owner_credential, ArchiveTarget,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{BufRead, Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Open {
    controller_pid: u32,
    controller_birth: String,
    session_sha256: String,
    super_password: Option<String>,
    login_password: Option<String>,
    legacy_secret: Option<String>,
    resume_operation: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Confirm {
    action: String,
    #[serde(deserialize_with = "parse_uuid")]
    operation_id: Uuid,
    archive_sha256: String,
    target_sha256: String,
    target_identity_sha256: String,
    #[serde(deserialize_with = "parse_uuid")]
    instance_id: Uuid,
    space: String,
}

fn parse_uuid<'de, D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Uuid, D::Error> {
    let raw = String::deserialize(deserializer)?;
    Uuid::parse_str(&raw)
        .ok()
        .filter(|id| id.get_version_num() == 4 && id.to_string() == raw)
        .ok_or_else(|| serde::de::Error::custom("canonical version 4 UUID required"))
}

#[derive(Serialize, Deserialize)]
enum ChildLaunch {
    Stage(RuntimeUnlock),
    Reopen(RuntimeUnlock),
}

fn read<T: DeserializeOwned>(reader: &mut impl BufRead) -> Result<T> {
    let mut bytes = Vec::new();
    reader.take(16385).read_until(b'\n', &mut bytes)?;
    ensure!(
        bytes.len() <= 16384 && bytes.last() == Some(&b'\n'),
        "missing or oversized private backup frame"
    );
    serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid private backup frame"))
}
fn emit(value: Value) -> Result<()> {
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, &value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}
fn sha(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn digest(s: &str) -> Result<[u8; 32]> {
    ensure!(
        s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid SHA-256"
    );
    let mut out = [0; 32];
    for (n, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[n * 2..n * 2 + 2], 16)?;
    }
    Ok(out)
}
impl Open {
    fn verify(&self) -> Result<()> {
        respire_spawn::verify_control_parent(self.controller_pid)?;
        ensure!(
            super::startup_identity::process_birth(self.controller_pid)?.as_deref()
                == Some(&self.controller_birth)
                && fs::metadata(format!("/proc/{}", self.controller_pid))?.uid()
                    == fs::metadata("/proc/self")?.uid(),
            "backup controller owner changed"
        );
        ensure!(
            self.super_password
                .as_ref()
                .or(self.login_password.as_ref())
                .or(self.legacy_secret.as_ref())
                .is_some_and(|v| !v.is_empty()),
            "explicit Vault factor required"
        );
        Ok(())
    }
    fn unlock(&self, g: &Launch) -> RuntimeUnlock {
        RuntimeUnlock {
            schema_version: 1,
            profile: g.profile.clone(),
            session_sha256: self.session_sha256.clone(),
            instance_id: g.instance_id.clone(),
            generation: respire_commands::rpc_worker::generation_on_wire(g.sync_generation),
            super_password: self.super_password.clone(),
            login_password: self.login_password.clone(),
            legacy_secret: self.legacy_secret.clone(),
            provider_credentials: Vec::new(),
        }
    }
}

pub(super) fn unlock_child(grant: &Launch) -> Result<()> {
    respire_spawn::verify_control_parent(grant.supervisor_pid)?;
    let (unlock, stage) = match read(&mut std::io::stdin().lock())? {
        ChildLaunch::Stage(unlock) => (unlock, true),
        ChildLaunch::Reopen(unlock) => (unlock, false),
    };
    ensure!(
        unlock.instance_id == grant.instance_id
            && unlock.profile == grant.profile
            && unlock.generation == respire_commands::rpc_worker::generation_on_wire(grant.sync_generation),
        "backup credential scope differs from launch"
    );
    auth::supply_runtime_session(unlock)?;
    respire_spawn::watch_control_parent()?;
    if stage {
        respire_commands::backup_restart::enable_private_backup_control(grant)?;
    }
    Ok(())
}

mod serving;
use serving::rotate;

fn locks(root: &Path) -> Result<(LibraryLock, LibraryLock)> {
    Ok((
        LibraryLock::acquire(&root.join("runtime"), Duration::ZERO)?,
        LibraryLock::acquire(root, Duration::ZERO)?,
    ))
}

pub(super) fn run() -> Result<i32> {
    respire_commands::runtime_policy::require_host("backup supervisor")?;
    let mut input = std::io::stdin().lock();
    let open: Open = read(&mut input)?;
    open.verify()?;
    let port = net_rpc::rpc_port();
    let root = respire_app::service::main_data_dir().canonicalize()?;
    ensure!(
        root == respire_app::service::data_dir().canonicalize()?,
        "backup supervisor requires the installation profile"
    );
    let takeover = respire_commands::runtime_policy::takeover_lock()?;
    let (installation, library) = locks(&root)?;
    let initial = super::startup_identity::backup_launch(port)?;
    if open.resume_operation.is_none() { snapshot::ensure_no_incomplete_restore(&root)?; }
    auth::supply_runtime_session(open.unlock(&initial))?;
    let (keys, user) = auth::load_local_session_with_owner()?;
    let account = user.context("Vault owner missing")?;
    let fence_path = root.join("runtime/autostart/launch/backup-restart");
    if let Some(raw) = &open.resume_operation {
        let id = Uuid::parse_str(raw)?;
        ensure!(id.get_version_num()==4 && id.to_string()==*raw,"invalid operation UUID");
        let fence = super::startup_identity::read_private_file(&fence_path)?;
        respire_commands::runtime_http::BackupAdmission::resume_release(&root,&account,&keys,id,&library,fence.as_deref())?;
        if fence.is_some() { fs::remove_file(&fence_path)?; respire_spawn::durable::sync_directory(fence_path.parent().context("reservation directory")?)?; }
        emit(json!({"operation_id":raw,"kind":"backup_import","state":"completed","progress_percent":100}))?;
        return Ok(0);
    }
    let mut child = rotate(&open, None, &takeover, installation, library, initial)?;
    let agent = ureq::AgentBuilder::new()
        .try_proxy_from_env(false)
        .redirects(0)
        .timeout(Duration::from_secs(15))
        .build();
    let token =
        super::startup_identity::read_private_file(&net_rpc::token_path())?.context("missing local token")?;
    let token = std::str::from_utf8(&token)?.trim();
    ensure!(!token.is_empty(), "empty local token");
    // V1 health initializes the native owner credential for this unlocked instance.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        child.verify()?;
        let native: Value =
            agent.get(&format!("{}/v1/health", net_rpc::rpc_base_url())).call()?.into_json()?;
        ensure!(native["instance_id"] == child.launch.instance_id, "native owner instance changed");
        if native["capabilities"].as_array().is_some_and(|v| v.iter().any(|flag| flag == "auth.handshake.v1"))
        {
            break;
        }
        ensure!(Instant::now() < deadline, "native backup owner is unavailable");
        std::thread::sleep(Duration::from_millis(25));
    }
    let pin = ArchiveTarget::pin(respire_app::service::database_path(&root)?.canonicalize()?)?;
    emit(json!({"state":"ready","instance_id":child.health.instance_id,"space":"main",
        "target_sha256":hex(&sha(pin.path().as_os_str().as_encoded_bytes())),"target_identity_sha256":hex(&pin.identity_digest()?)}))?;
    let (confirm, grant) = loop {
        if input.fill_buf()?.is_empty() {
            backup_restart::release(&root, &child.launch, &mut child.child, &takeover)?;
            child.drain(&agent, token)?;
            return Ok(0);
        }
        let confirm: Confirm = match read(&mut input) {
            Ok(frame) => frame,
            Err(error) => {
                backup_restart::release(&root, &child.launch, &mut child.child, &takeover)?;
                child.drain(&agent, token)?;
                return Err(error);
            }
        };
        open.verify()?;
        if let Some(grant) = child.claim(&agent, &confirm, &pin, &account)? {
            break (confirm, grant);
        }
        emit(json!({"operation_id":confirm.operation_id.to_string(),"state":"denied"}))?;
    };
    let epoch = Uuid::from_bytes(grant.vault_epoch());
    let op = snapshot::SnapshotOperation {
        operation_id: confirm.operation_id.to_string(),
        request_fingerprint: grant.fingerprint(),
    };
    let admission = respire_commands::runtime_http::BackupAdmission::retain(&grant)?;
    child.drain(&agent, token)?;
    let (installation, library) = locks(&root)?;
    let authorize = |current: &Launch| -> Result<()> {
        open.verify()?;
        backup_restart::require_held(&root, current, &takeover)?;
        let session = super::startup_identity::read_private_file(&auth::session_file()?)?
            .context("Vault session missing")?;
        ensure!(hex(&sha(&session)) == open.session_sha256, "Vault or takeover changed");
        Ok(())
    };
    let installed = snapshot::backup_archive::restore_approved_archive_locked(
        &root,
        &root,
        &pin,
        &account,
        epoch,
        &keys,
        None,
        &op,
        &library,
        grant,
        || authorize(&child.launch),
    )?
    .context("missing install receipt")?;
    ensure!(
        installed.phase == "installed" && installed.receipt.operation.as_ref() == Some(&op),
        "install scope changed"
    );
    let restored = ArchiveTarget::pin(pin.path().to_owned())?;
    let mut next = rotate(
        &open,
        Some(&child.launch),
        &takeover,
        installation,
        library,
        super::startup_identity::backup_launch(port)?,
    )?;
    let witness =
        admission.observe_parent(&child.health, &next.health, &restored, &installed.receipt.restored)?;
    next.drain(&agent, token)?;
    let (installation, library) = locks(&root)?;
    authorize(&next.launch)?;
    snapshot::commit_restore(&root, &installed.receipt)?;
    let fence = serde_json::to_vec(&next.launch)?;
    admission.publish(&root, &account, &restored, "main", &witness, &installed.receipt.restored, &library, &keys, &fence)?;
    respire_commands::runtime_http::BackupAdmission::resume_release(&root, &account, &keys, confirm.operation_id, &library, Some(&fence))?;
    let mut serving = rotate(
        &open,
        Some(&next.launch),
        &takeover,
        installation,
        library,
        super::startup_identity::backup_launch(port)?,
    )?;
    serving.verify()?;
    let terminal: Value = agent
        .get(&format!("{}/v1/operations/{}", net_rpc::rpc_base_url(), confirm.operation_id))
        .set("Authorization", &format!("Bearer {token}"))
        .call()?
        .into_json()?;
    ensure!(
        terminal["operation_id"] == confirm.operation_id.to_string() && terminal["state"] == "completed",
        "original operation not complete"
    );
    backup_restart::release(&root, &serving.launch, &mut serving.child, &takeover)?;
    emit(terminal)?;
    drop(takeover);
    // The caller keeps its input pipe open while using the reopened Runtime.
    while !input.fill_buf()?.is_empty() {
        let Ok(replay) = read::<Confirm>(&mut input) else { break };
        emit(json!({"operation_id":replay.operation_id.to_string(),"state":"denied"}))?;
    }
    serving.drain(&agent, token)?;
    Ok(0)
}
