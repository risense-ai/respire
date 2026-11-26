use super::*;
use respire_app::snapshot::{
    backup_archive::{restore_approved_archive_locked, BackupVaultIdentity},
    SnapshotOperation,
};
use respire_spawn::owner_credential::{read_owner_credential, ArchiveTarget};
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, Stdio},
    sync::{Mutex, OnceLock},
};
use uuid::Uuid;
const CHILD_TEST: &str = "runtime_http::v1::backup_export::tests::lifecycle_fixture::serving_child";
const COORD: &str =
    "runtime_http::v1::backup_export::tests::lifecycle_fixture::continuous_native_restore";
static SELECTED: OnceLock<Mutex<Option<std::path::PathBuf>>> = OnceLock::new();

static ARMED: Mutex<Option<selected_receipt::Request>> = Mutex::new(None);

pub(in crate::runtime_http::v1::backup_export) fn capture_armed(ctx: &rest_dispatch::LibraryContext<'_>) -> V1Result<()> {
    let request = ARMED.lock().map_err(|_| transport::unavailable())?.take();
    if let Some(request) = request {
        let owner = read_owner_credential(&crate::rpc_worker::runtime_dir(), &ctx.scope.instance.to_string())
            .map_err(|_| transport::unavailable())?;
        let witness = selected_receipt::capture(ctx, request, &owner)?;
        println!("SELECTED {}", serde_json::to_string(&witness).map_err(|_| transport::unavailable())?);
    }
    Ok(())
}

pub(super) fn remember(pin: &ArchiveTarget) -> Result<()> {
    if let Some(slot) = SELECTED.get() {
        *slot.lock().map_err(|_| anyhow::anyhow!("selection lock"))? = Some(pin.path().to_owned());
    }
    Ok(())
}

#[test]
#[ignore = "isolated owned Runtime child"]
fn serving_child() -> Result<()> {
    crate::rpc_serve::enable_fixture_shutdown_finalizer(CHILD_TEST)?;
    crate::runtime_http::install_fixture_routes(install_backup_routes)?;
    SELECTED
        .set(Mutex::new(None))
        .map_err(|_| anyhow::anyhow!("duplicate startup"))?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let unlock: respire_protocol::credentials::RuntimeUnlock = serde_json::from_str(&line)?;
    let instance = unlock.instance_id.clone();
    crate::rpc_serve::set_launch_instance_id(&instance)?;
    // The libtest build uses its configured credential reader during store bootstrap.
    // Supply the same synthetic Vault factor received on the private startup pipe.
    std::env::set_var(
        "RSRS_SUPER",
        unlock
            .super_password
            .as_deref()
            .context("synthetic Vault factor")?,
    );
    auth::supply_runtime_session(unlock)?;
    auth::load_local_session().context("synthetic store bootstrap credential")?;
    thread::spawn(move || -> Result<()> {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        if line.is_empty() {
            return Ok(());
        }
        if line.starts_with('{') {
            *ARMED.lock().map_err(|_| anyhow::anyhow!("arm"))? = Some(serde_json::from_str(&line)?);
            println!("ARMED");
            return Ok(());
        }
        let challenge: Uuid = line.trim().parse()?;
        let profile = service::data_dir().canonicalize()?;
        let vault = BackupVaultIdentity::capture(&profile)?;
        let owner = read_owner_credential(&crate::rpc_worker::runtime_dir(), &instance)?;
        let (blob, tag) = vault.attest(&owner, challenge.as_bytes())?;
        let path = SELECTED
            .get()
            .context("startup")?
            .lock()
            .map_err(|_| anyhow::anyhow!("selection lock"))?
            .take()
            .context("selected SQLite")?;
        let (target, identity) = process_fixture::target()?;
        println!(
            "OBS {}",
            json!({"blob":blob,"tag":tag,"path":path,"target":target,"identity":identity})
        );
        Ok(())
    });
    crate::rpc_serve::serve(crate::rpc_serve::RuntimeFlags {
        host: "127.0.0.1".into(),
        port: Some(crate::net_rpc::rpc_port()),
        status: false,
        stop: false,
    })
}

struct Owned(Child);
impl Drop for Owned {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn start(
    agent: &ureq::Agent,
    base: &str,
    password: &str,
    root: &Path,
) -> Result<(Owned, crate::net_rpc::Health)> {
    let instance = Uuid::new_v4().to_string();
    let mut owned = Owned(
        Command::new(std::env::current_exe()?)
            .args(["--ignored", "--exact", CHILD_TEST, "--nocapture"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?,
    );
    let unlock = json!({"schema_version":respire_protocol::credentials::CREDENTIAL_SCHEMA_V,"profile":root,"instance_id":instance,
        "generation":1,"session_sha256":format!("{:x}",Sha256::digest(fs::read(root.join("session.json"))?)),"super_password":password,"provider_credentials":[]});
    writeln!(owned.0.stdin.as_mut().context("input pipe")?, "{unlock}")?;
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline && owned.0.try_wait()?.is_none() {
        let ready = agent
            .get(&format!("{base}/v1/health"))
            .call()
            .ok()
            .and_then(|r| r.into_json::<Value>().ok());
        if !ready.is_some_and(|r| r["status"] == "ready" && r["instance_id"] == instance) {
            thread::sleep(Duration::from_millis(20));
            continue;
        }
        if let Ok(reply) = agent.get(&format!("{base}/api/health")).call() {
            let value: Value = reply.into_json()?;
            if value["store_ready"] == true && value["state"] == "ready" {
                let h: crate::net_rpc::Health = serde_json::from_value(value)?;
                ensure!(
                    h.pid == owned.0.id()
                        && h.instance_id == instance
                        && h.url == base
                        && crate::net_rpc::instance_is_running(&h)?
                        && crate::net_rpc::pid_listening_on(crate::net_rpc::rpc_port())
                            == Some(h.pid)
                        && Path::new(&h.data_dir).canonicalize()? == root
                        && Path::new(&h.image_path).canonicalize()?
                            == std::env::current_exe()?.canonicalize()?,
                    "serving identity mismatch"
                );
                return Ok((owned, h));
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
    anyhow::bail!("fixture Runtime did not become ready")
}
fn stop(agent: &ureq::Agent, child: &mut Owned, h: &crate::net_rpc::Health) -> Result<Uuid> {
    ensure!(
        child.0.id() == h.pid
            && crate::net_rpc::instance_is_running(h)?
            && crate::net_rpc::pid_listening_on(crate::net_rpc::rpc_port()) == Some(h.pid),
        "lost admitted Runtime"
    );
    let response: Value = agent
        .post(&format!("{}/api/runtime/stop", h.url))
        .set("X-Respire-Lifecycle", "coordinator-v1")
        .set("Authorization", &format!("Bearer {TOKEN}"))
        .send_json(
            json!({"installation_id":h.installation_id,"instance_id":h.instance_id,
            "account_id":h.account_id,"generation":h.generation}),
        )?
        .into_json()?;
    ensure!(
        response["ok"] == true && response["state"] == "accepted",
        "stop refused: {response}"
    );
    let op = response["operation_id"]
        .as_str()
        .context("stop id")?
        .parse()?;
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline && child.0.try_wait()?.is_none() {
        thread::sleep(Duration::from_millis(10));
    }
    ensure!(
        child.0.try_wait()?.is_some_and(|s| s.success()),
        "unclean Runtime exit"
    );
    Ok(op)
}

#[test]
fn continuous_native_restore() -> Result<()> {
    if std::env::var_os("RSRS_FIXTURE_COORD_ROOT").is_none() {
        for selection in ["parent", "child"] {
            let dir = tempfile::tempdir()?;
            let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
            let port = listener.local_addr()?.port();
            drop(listener);
            let status = Command::new(std::env::current_exe()?)
                .args(["--exact", COORD, "--nocapture"])
                .env("RSRS_FIXTURE_COORD_ROOT", dir.path())
                .env("RSRS_FIXTURE_SELECTION", selection)
                .env("RSRS_DATA_DIR", dir.path())
                .env("RSRS_CORE_TEST_MODE", "1")
                .env("RSRS_RPC_PORT", port.to_string())
                .env_remove("RSRS_DEFAULT_DATA_DIR")
                .env_remove("RSRS_RPC_TOKEN")
                .env_remove("RSRS_ADDR")
                .env_remove("RSRS_TOKEN")
                .status()?;
            ensure!(
                status.success() && fs::read(dir.path().join("verified"))? == b"verified",
                "{selection} lifecycle failed"
            );
        }
        return Ok(());
    }
    let root = service::data_dir().canonicalize()?;
    ensure!(
        root == Path::new(&std::env::var_os("RSRS_FIXTURE_COORD_ROOT").context("root")?)
            .canonicalize()?,
        "synthetic profile"
    );
    let token = crate::net_rpc::token_path();
    fs::create_dir_all(token.parent().context("token directory")?)?;
    fs::write(token, TOKEN)?;
    let (password, urk, salt, wrapped, nonce) = auth::new_vault_v4()?;
    auth::write_session_json(
        &json!({"user":"backup-fixture","vault_version":4,"kdf_salt":salt,"urk_nonce":nonce,"wrapped_urk":wrapped}),
    )?;
    let agent = ureq::AgentBuilder::new()
        .try_proxy_from_env(false)
        .redirects(0)
        .timeout(WAIT)
        .build();
    let base = format!("http://127.0.0.1:{}", crate::net_rpc::rpc_port());
    let (mut first, health) = start(&agent, &base, &password, &root)?;
    let post = |path: &str| {
        agent
            .post(&format!("{base}{path}"))
            .set("Authorization", &format!("Bearer {TOKEN}"))
    };
    let child = std::env::var("RSRS_FIXTURE_SELECTION")? == "child";
    let space = if child {
        let r: Value = post("/v1/spaces")
            .send_json(json!({"name":"lifecycle child"}))?
            .into_json()?;
        r["id"].as_str().context("space")?.to_owned()
    } else {
        "main".to_owned()
    };
    let save = |text: &str| -> Result<String> {
        let r: Value = post("/v1/memories")
            .send_json(json!({"space_id":space,"content":text}))?
            .into_json()?;
        Ok(r["id"].as_str().context("memory")?.to_owned())
    };
    let original = save("before backup")?;
    let mut wire = Vec::new();
    agent
        .get(&format!("{base}{EXPORT}?space_id={space}"))
        .set("Authorization", &format!("Bearer {TOKEN}"))
        .call()?
        .into_reader()
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut wire)?;
    ensure!(wire.len() <= 8 * 1024 * 1024, "archive too large");
    let later = save("after backup")?;
    let response = post(&format!("/v1/backups/import?space_id={space}"))
        .set("Content-Type", "application/octet-stream")
        .send_bytes(&wire)?;
    ensure!(response.status() == 202, "unexpected import response");
    let accepted: Value = response.into_json()?;
    let id: Uuid = accepted["operation_id"]
        .as_str()
        .context("operation")?
        .parse()?;
    let status = |expected: &str| -> Result<()> {
        let value: Value = agent.get(&format!("{base}/v1/operations/{id}"))
            .set("Authorization", &format!("Bearer {TOKEN}")).call()?.into_json()?;
        ensure!(value["operation_id"] == id.to_string() && value["kind"] == "backup_import"
            && value["state"] == expected, "unexpected backup operation state");
        Ok(())
    };
    status("pending")?;
    let challenge = Uuid::new_v4();
    writeln!(first.0.stdin.as_mut().context("input")?, "{challenge}")?;
    let stdout = first.0.stdout.take().context("output")?;
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let line = BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
            .find_map(|s| s.strip_prefix("OBS ").map(str::to_owned));
        let _ = tx.send(line);
    });
    let obs: Value = serde_json::from_str(&rx.recv_timeout(WAIT)?.context("observation missing")?)?;
    let owner = read_owner_credential(&crate::rpc_worker::runtime_dir(), &health.instance_id)?;
    let blob: Vec<u8> = serde_json::from_value(obs["blob"].clone())?;
    let tag: [u8; 32] = serde_json::from_value(obs["tag"].clone())?;
    ensure!(
        BackupVaultIdentity::from_attestation(&owner, Uuid::new_v4().as_bytes(), &blob, &tag)
            .is_err(),
        "stale observation accepted"
    );
    let vault = BackupVaultIdentity::from_attestation(&owner, challenge.as_bytes(), &blob, &tag)?;
    let pin = ArchiveTarget::pin(serde_json::from_value(obs["path"].clone())?)?;
    let selection = (
        serde_json::from_value(obs["target"].clone())?,
        serde_json::from_value(obs["identity"].clone())?,
    );
    ensure!(pin.identity_digest()? == selection.1, "target changed");
    let grant = process_fixture::claim_for_offline(&agent, &base, selection, id, &wire)?;
    vault.check(&grant)?;
    status("running")?;
    let stop_id = stop(&agent, &mut first, &health)?;
    drain_fixture::verify_child_exit(&mut first.0, &health, stop_id, &grant, || {
        read_receipt(&health)
    })?;
    let keys = respire_app::SessionKeys::from_urk(urk)?;
    let manifest = open_to_stage(&keys, "backup-fixture", &wire[..], std::io::sink(), || {
        Ok(())
    })?;
    let op = SnapshotOperation {
        operation_id: id.to_string(),
        request_fingerprint: grant.fingerprint(),
    };
    let custody = respire_spawn::owner_credential::BackupArchiveHandoff::from_approved_intent(
        respire_spawn::owner_credential::ArchiveOperation::from_v4_bytes(*id.as_bytes())?,
        grant.intent(),
    );
    let selected = pin.path().parent().context("selected root")?;
    let lock = respire_app::lock::LibraryLock::acquire(selected, Duration::ZERO)?;
    let admission = completion::Admitted::retain(&grant)?;
    let installed = restore_approved_archive_locked(
        selected,
        &root,
        &pin,
        &vault,
        &keys,
        &manifest.id,
        &op,
        &lock,
        grant,
        || Ok(()),
    )?
    .context("missing installation")?;
    ensure!(
        installed.phase == "installed" && installed.receipt.operation.as_ref() == Some(&op),
        "operation missing"
    );
    drop(lock);
    let restored_file = ArchiveTarget::pin(pin.path().to_owned())?;
    let (mut next, fresh) = start(&agent, &base, &password, &root)?;
    ensure!(
        fresh.instance_id != health.instance_id
            && fresh.image_sha256 == health.image_sha256
            && fresh.account_id == health.account_id
            && fresh.installation_id == health.installation_id,
        "restart mismatch"
    );
    if child {
        let page: Value = agent
            .get(&format!("{base}/v1/memories?space_id={space}"))
            .set("Authorization", &format!("Bearer {TOKEN}"))
            .call()?
            .into_json()?;
        let items = page["items"].as_array().context("selected child page")?;
        ensure!(
            items.iter().any(|row| row["id"] == original)
                && items.iter().all(|row| row["id"] != later),
            "fresh Runtime did not serve the restored child"
        );
    } else {
        ensure!(
            fresh.snapshot_restore_id.as_deref() == Some(manifest.id.as_str()),
            "fresh Runtime opened the wrong snapshot"
        );
    }
    let db = rusqlite::Connection::open_with_flags(
        pin.path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let exists = |id: &str| -> rusqlite::Result<bool> {
        db.prepare("SELECT 1 FROM memories WHERE id=?1")?
            .exists([id])
    };
    ensure!(
        exists(&original)? && !exists(&later)?,
        "restored rows differ"
    );
    drop(db);
    let restart = if child {
        let request = selected_receipt::Request { instance: *Uuid::parse_str(&fresh.instance_id)?.as_bytes(),
            operation: *id.as_bytes(), challenge: *Uuid::new_v4().as_bytes(), space: space.clone() };
        writeln!(next.0.stdin.as_mut().context("input")?, "{}", serde_json::to_string(&request)?)?;
        let out = next.0.stdout.take().context("output")?;
        let (tx, rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if line == "ARMED" || line.starts_with("SELECTED ") {
                    if tx.send(line).is_err() { break; }
                }
            }
        });
        ensure!(rx.recv_timeout(WAIT)? == "ARMED", "restart challenge not armed");
        agent.get(&format!("{base}{EXPORT}?space_id={space}"))
            .set("Authorization", &format!("Bearer {TOKEN}")).call()?;
        let reply = rx.recv_timeout(WAIT)?;
        let witness: selected_receipt::Witness = serde_json::from_str(reply.strip_prefix("SELECTED ").context("receipt")?)?;
        let observe = |request: &selected_receipt::Request| {
            admission.observe_selected(&health, &fresh, &restored_file, request, &witness)
        };
        let mut wrong = request.clone();
        wrong.space = "different-child".into();
        ensure!(observe(&wrong).is_err(), "wrong selection accepted");
        wrong = request.clone();
        wrong.instance = *Uuid::new_v4().as_bytes();
        ensure!(observe(&wrong).is_err(), "stale instance accepted");
        wrong = request.clone();
        wrong.challenge = *Uuid::new_v4().as_bytes();
        ensure!(observe(&wrong).is_err(), "different challenge accepted");
        ensure!(admission.observe_selected(&health, &health, &restored_file, &request, &witness).is_err(),
            "admitted Runtime accepted as fresh");
        observe(&request)?
    } else {
        admission.observe_parent(&health, &fresh, &restored_file, &manifest.id)?
    };
    let fresh_stop = stop(&agent, &mut next, &fresh)?;
    drain_fixture::verify_final_exit(&mut next.0, &fresh, fresh_stop, || read_receipt(&fresh))?;
    let _lock = respire_app::lock::LibraryLock::acquire(selected, Duration::ZERO)?;
    respire_app::snapshot::complete_restore(selected, &installed.receipt)?;
    let terminal = respire_app::snapshot::operation_status(selected, "backup-fixture", &op)?
        .context("terminal receipt missing")?;
    ensure!(
        terminal.phase == "released"
            && terminal.receipt.operation.as_ref() == Some(&op)
            && !terminal.cleanup_pending,
        "backup operation did not release"
    );
    ensure!(admission.publish(&root, "backup-fixture", &restored_file, &space, &restart,
        "different-snapshot", &_lock).is_err(), "mismatched journal accepted");
    admission.publish(&root, "backup-fixture", &restored_file, &space, &restart,
        &manifest.id, &_lock)?;
    admission.publish(&root, "backup-fixture", &restored_file, &space, &restart,
        &manifest.id, &_lock)?;
    custody.discard(&root)?;
    drop(_lock);
    let (mut lookup_runtime, lookup_health) = start(&agent, &base, &password, &root)?;
    status("completed")?;
    let final_stop = stop(&agent, &mut lookup_runtime, &lookup_health)?;
    drain_fixture::verify_final_exit(&mut lookup_runtime.0, &lookup_health, final_stop, || read_receipt(&lookup_health))?;
    ensure!(
        backup_stage_count(&root)? == 1,
        "expected only the durable terminal receipt"
    );
    eprintln!(
        "verified {}",
        json!({"selection":if child {"child"}else{"parent"},"operation":id.to_string(),
        "admitted_instance":health.instance_id,"stop_operation":stop_id.to_string(),"fresh_instance":fresh.instance_id,
        "snapshot":manifest.id,"phase":terminal.phase})
    );
    fs::write(root.join("verified"), b"verified")?;
    Ok(())
}

fn read_receipt(h: &crate::net_rpc::Health) -> Result<respire_protocol::RuntimeStopReceipt> {
    use std::os::unix::fs::MetadataExt;
    let path = service::main_data_dir()
        .join("runtime")
        .join(format!("stop-{}.json", Uuid::parse_str(&h.instance_id)?));
    let entry = fs::symlink_metadata(&path)?;
    ensure!(
        entry.is_file() && entry.len() <= 65536 && entry.mode() & 0o077 == 0,
        "invalid private receipt"
    );
    let file = fs::File::open(&path)?;
    let meta = file.metadata()?;
    ensure!(
        (entry.dev(), entry.ino()) == (meta.dev(), meta.ino()),
        "receipt was replaced"
    );
    let mut bytes = Vec::new();
    file.take(65537).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 65536, "receipt too large");
    Ok(serde_json::from_slice(&bytes)?)
}
