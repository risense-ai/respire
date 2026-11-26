//! Exercise the private confirmation ABI against the production serving binary.
#![cfg(target_os = "linux")]
use anyhow::{ensure, Context, Result};
use respire_spawn::owner_credential::{
    archive_proof::{self, ArchiveClaim},
    read_owner_credential, ArchiveTarget,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    os::unix::fs::PermissionsExt,
    process::{Child, Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

struct Fixture(std::path::PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Owned(Child);
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn production_runtime_confirms_only_exact_native_archive() -> Result<()> {
    let root = Fixture(
        std::env::temp_dir()
            .canonicalize()?
            .join(format!("backup-{}", Uuid::new_v4())),
    );
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(&root.0)?;
    let profile = &root.0;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(profile.join("runtime"))?;
    let token_path = profile.join("runtime/token");
    fs::write(&token_path, Uuid::new_v4().simple().to_string())?;
    fs::set_permissions(token_path, fs::Permissions::from_mode(0o600))?;
    let (password, urk, salt, wrapped, nonce) = respire_app::auth::new_vault_v4()?;
    let session = serde_json::to_vec(
        &json!({"user":"backup-fixture","vault_version":4,"kdf_salt":salt,
        "urk_nonce":nonce,"wrapped_urk":wrapped}),
    )?;
    fs::write(profile.join("session.json"), &session)?;
    fs::set_permissions(
        profile.join("session.json"),
        fs::Permissions::from_mode(0o600),
    )?;
    let reservation = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = reservation.local_addr()?.port();
    drop(reservation);
    let launch = || -> Result<Owned> {
        Ok(Owned(
            Command::new(env!("CARGO_BIN_EXE_rsrs-runtime"))
                .env_clear()
                .env("HOME", profile)
                .env("RSRS_DATA_DIR", profile)
                .env("RSRS_RPC_PORT", port.to_string())
                .env(
                    "LD_LIBRARY_PATH",
                    std::env::var_os("LD_LIBRARY_PATH").unwrap_or_default(),
                )
                .args(["serve", "--host", "127.0.0.1"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .spawn()?,
        ))
    };
    std::env::set_var("RSRS_DATA_DIR", profile);
    std::env::set_var("RSRS_RPC_PORT", port.to_string());
    let base = format!("http://127.0.0.1:{port}");
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .try_proxy_from_env(false)
        .timeout(Duration::from_secs(15))
        .build();
    let ready = |child: &mut Owned| -> Result<Value> {
        let until = Instant::now() + Duration::from_secs(30);
        let health: Value = loop {
            ensure!(
                child.0.try_wait()?.is_none() && Instant::now() < until,
                "Runtime startup failed"
            );
            if let Ok(r) = agent.get(&format!("{base}/api/health")).call() {
                let health: Value = r.into_json()?;
                if health["startup_complete"] == true {
                    break health;
                }
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        ensure!(health["pid"] == child.0.id(), "wrong serving process");
        Ok(health)
    };
    let mut child = launch()?;
    let health = ready(&mut child)?;
    let instance: Uuid = health["instance_id"]
        .as_str()
        .context("instance")?
        .parse()?;
    let generation = health["generation"].as_u64().context("generation")?;
    let token = fs::read_to_string(profile.join("runtime/token"))?;
    let bearer = format!("Bearer {}", token.trim());
    agent.post(&format!("{base}/api/runtime/unlock")).set("Authorization",&bearer).send_json(json!({
        "schema_version":1,"profile":profile,"instance_id":instance.to_string(),"generation":generation,
        "session_sha256":hex(&Sha256::digest(&session)),"super_password":password,"provider_credentials":[] }))?;
    let mut wire = Vec::new();
    agent
        .get(&format!("{base}/v1/backups/export?space_id=main"))
        .set("Authorization", &bearer)
        .call()?
        .into_reader()
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut wire)?;
    ensure!(wire.len() <= 8 * 1024 * 1024, "oversized fixture");
    let response = agent
        .post(&format!("{base}/v1/backups/import?space_id=main"))
        .set("Authorization", &bearer)
        .set("Content-Type", "application/octet-stream")
        .send_bytes(&wire)?;
    ensure!(response.status() == 202, "upload did not stage");
    let accepted: Value = response.into_json()?;
    let id: Uuid = accepted["operation_id"]
        .as_str()
        .context("operation")?
        .parse()?;
    let pin = ArchiveTarget::pin(respire_app::service::database_path(profile)?.canonicalize()?)?;
    let claim = ArchiveClaim {
        operation: *id.as_bytes(),
        archive: Sha256::digest(&wire).into(),
        target: Sha256::digest(pin.path().as_os_str().as_encoded_bytes()).into(),
        target_identity: pin.identity_digest()?,
        instance: *instance.as_bytes(),
        generation,
        inspect_only: false,
        query_only: false,
    };
    let native_owner = |instance: Uuid| -> Result<respire_spawn::owner_credential::OwnerRecord> {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            let health: Value = agent
                .get(&format!("{base}/v1/health"))
                .call()?
                .into_json()?;
            ensure!(
                health["instance_id"] == instance.to_string(),
                "owner instance changed"
            );
            if health["capabilities"]
                .as_array()
                .is_some_and(|flags| flags.iter().any(|v| v == "auth.handshake.v1"))
            {
                return read_owner_credential(&profile.join("runtime"), &instance.to_string())
                    .context("Runtime native owner credential");
            }
            ensure!(Instant::now() < until, "native owner did not become ready");
            std::thread::sleep(Duration::from_millis(30));
        }
    };
    let owner = native_owner(instance)?;
    use respire_spawn::owner_credential::backup_grant::{self, Receiver, Scope};
    let stat = fs::read_to_string("/proc/self/stat")?;
    let birth: u64 = stat
        .rsplit_once(')')
        .context("stat")?
        .1
        .split_whitespace()
        .nth(19)
        .context("birth")?
        .parse()?;
    let scope = Scope {
        operation: claim.operation,
        archive: claim.archive,
        target: claim.target,
        target_identity: claim.target_identity,
        instance: claim.instance,
        generation,
        account: Sha256::digest(b"backup-fixture").into(),
        profile: Sha256::digest(profile.as_os_str().as_encoded_bytes()).into(),
        supervisor_pid: std::process::id(),
        supervisor_birth: birth,
    };
    let receiver = Receiver::arm(&owner, scope)?;
    let challenge = receiver.challenge();
    let delivered = std::cell::RefCell::new(Value::Null);
    let request = |claim: ArchiveClaim,
                   public: bool,
                   supervisor_birth: u64,
                   corrupt: bool|
     -> Result<u16> {
        let signed = Scope {
            archive: claim.archive,
            target: claim.target,
            target_identity: claim.target_identity,
            operation: claim.operation,
            supervisor_birth,
            ..scope
        };
        let mut tag = backup_grant::sign_request(&owner, &challenge, &signed)?;
        if corrupt {
            tag[0] ^= 1;
        }
        let proof = archive_proof::issue(
            owner.as_bytes(),
            claim,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)?
                .as_millis()
                .try_into()?,
        )
        .context("proof")?;
        let mut req = agent
            .post(&format!("{base}/api/runtime/private-backup/claim"))
            .set("x-respire-archive-proof", &proof);
        if public {
            req = req.set("Authorization", &bearer);
        }
        match req.send_json(json!({"schema_version":3,"handoff":{"supervisor_pid":scope.supervisor_pid,"supervisor_birth":supervisor_birth,"challenge":challenge,"tag":tag},"operation_id":id.to_string(),"archive_sha256":hex(&claim.archive),
            "target_sha256":hex(&claim.target),"target_identity_sha256":hex(&claim.target_identity)})) {
            Ok(response) => {
                let value: Value = response.into_json()?;
                ensure!(value["operation_id"]==id.to_string() && value["state"]=="claimed","incorrect admission receipt");
                *delivered.borrow_mut() = value;
                Ok(200)
            }
            Err(ureq::Error::Status(code,_)) => Ok(code),
            Err(error) => Err(error.into()),
        }
    };
    ensure!(
        request(claim, false, birth, true)? == 401
            && request(claim, false, birth + 1, false)? == 401,
        "invalid handoff signature or supervisor birth accepted"
    );
    let send = |claim, public| request(claim, public, birth, false);
    ensure!(
        send(claim, true)? == 401
            && send(
                ArchiveClaim {
                    archive: [0; 32],
                    ..claim
                },
                false
            )? == 401,
        "invalid proof tuple accepted"
    );
    ensure!(
        send(claim, false)? == 200 && send(claim, false)? == 409,
        "claim was not one-use"
    );
    let status: Value = agent
        .get(&format!("{base}/v1/operations/{id}"))
        .set("Authorization", &bearer)
        .call()?
        .into_json()?;
    ensure!(
        status["state"] == "running",
        "claim receipt falsely reported terminal state"
    );
    let issued = delivered.borrow().clone();
    let raw = issued["intent_sha256"].as_str().context("intent")?;
    let mut intent = [0u8; 32];
    for (i, byte) in intent.iter_mut().enumerate() {
        *byte = u8::from_str_radix(raw.get(i * 2..i * 2 + 2).context("intent digest")?, 16)?;
    }
    let epoch: Uuid = issued["vault_epoch"].as_str().context("epoch")?.parse()?;
    let grant = receiver.accept(
        intent,
        *epoch.as_bytes(),
        &serde_json::from_value(issued["handoff"]["grant"].clone())?,
    )?;
    use respire_app::snapshot::{
        backup_archive::{open_to_stage, restore_approved_archive_locked, BackupVaultIdentity},
        SnapshotOperation,
    };
    let vault = BackupVaultIdentity::from_attestation(
        &owner,
        &challenge[..16].try_into()?,
        &serde_json::from_value::<Vec<u8>>(issued["handoff"]["vault"].clone())?,
        &serde_json::from_value(issued["handoff"]["vault_tag"].clone())?,
    )?;
    vault.check(&grant)?;
    let op = SnapshotOperation {
        operation_id: id.to_string(),
        request_fingerprint: grant.fingerprint(),
    };
    let admission = respire_commands::runtime_http::BackupAdmission::retain(&grant)?;
    let drain = |child: &mut Owned, health: &Value| -> Result<()> {
        let instance = health["instance_id"].as_str().context("drain instance")?;
        let generation = health["generation"].as_u64().context("drain generation")?;
        let stop: Value = agent
        .post(&format!("{base}/api/runtime/stop"))
        .set("X-Respire-Lifecycle", "coordinator-v1")
        .set("Authorization", &bearer)
        .send_json(
            json!({"installation_id":health["installation_id"],"instance_id":instance.to_string(),
        "account_id":health["account_id"],"generation":generation}),
        )?
        .into_json()?;
        ensure!(
            stop["ok"] == true && stop["state"] == "accepted",
            "stop refused"
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        while child.0.try_wait()?.is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        ensure!(
            child.0.try_wait()?.is_some_and(|s| s.success()),
            "Runtime did not drain"
        );
        let receipt: Value = serde_json::from_slice(&fs::read(
            profile
                .join("runtime")
                .join(format!("stop-{instance}.json")),
        )?)?;
        ensure!(
            receipt["operation_id"] == stop["operation_id"]
                && receipt["instance_id"] == health["instance_id"]
                && receipt["pid"] == health["pid"]
                && receipt["state"] == "drained"
                && receipt["unfinished"].as_array().is_some_and(Vec::is_empty),
            "invalid final drain receipt"
        );
        Ok(())
    };
    drain(&mut child, &health)?;
    let keys = respire_app::SessionKeys::from_urk(urk)?;
    let manifest = open_to_stage(
        &keys,
        "backup-fixture",
        wire.as_slice(),
        std::io::sink(),
        || Ok(()),
    )?;
    let lock = respire_app::lock::LibraryLock::acquire(profile, Duration::ZERO)?;
    let installed = restore_approved_archive_locked(
        profile,
        profile,
        &pin,
        &vault,
        &keys,
        &manifest.id,
        &op,
        &lock,
        grant,
        || Ok(()),
    )?
    .context("installation receipt")?;
    ensure!(
        installed.phase == "installed" && installed.receipt.operation.as_ref() == Some(&op),
        "offline install scope mismatch"
    );
    drop(lock);
    let restored = ArchiveTarget::pin(pin.path().to_owned())?;
    let mut next = launch()?;
    let fresh = ready(&mut next)?;
    ensure!(
        fresh["instance_id"] != health["instance_id"]
            && fresh["image_sha256"] == health["image_sha256"],
        "restart identity mismatch"
    );
    agent.post(&format!("{base}/api/runtime/unlock")).set("Authorization",&bearer).send_json(json!({
        "schema_version":1,"profile":profile,"instance_id":fresh["instance_id"],"generation":fresh["generation"],
        "session_sha256":hex(&Sha256::digest(&session)),"super_password":password,"provider_credentials":[] }))?;
    let reopened: Value = agent
        .get(&format!("{base}/api/health"))
        .call()?
        .into_json()?;
    ensure!(
        reopened["instance_id"] == fresh["instance_id"],
        "restart changed during unlock"
    );
    let restart = admission.observe_parent(
        &serde_json::from_value(health.clone())?,
        &serde_json::from_value(reopened.clone())?,
        &restored,
        &manifest.id,
    )?;
    drain(&mut next, &reopened)?;
    let lock = respire_app::lock::LibraryLock::acquire(profile, Duration::ZERO)?;
    respire_app::snapshot::complete_restore(profile, &installed.receipt)?;
    admission.publish(
        profile,
        "backup-fixture",
        &restored,
        "main",
        &restart,
        &manifest.id,
        &lock,
    )?;
    use respire_spawn::owner_credential::{ArchiveOperation, BackupArchiveHandoff};
    BackupArchiveHandoff::from_approved_intent(
        ArchiveOperation::from_v4_bytes(*id.as_bytes())?,
        intent,
    )
    .discard(profile)?;
    drop(lock);
    let mut lookup = launch()?;
    let after = ready(&mut lookup)?;
    agent.post(&format!("{base}/api/runtime/unlock")).set("Authorization",&bearer).send_json(json!({
        "schema_version":1,"profile":profile,"instance_id":after["instance_id"],"generation":after["generation"],
        "session_sha256":hex(&Sha256::digest(&session)),"super_password":password,"provider_credentials":[] }))?;
    let _fresh_owner = native_owner(
        after["instance_id"]
            .as_str()
            .context("lookup instance")?
            .parse()?,
    )?;
    ensure!(
        send(claim, false)? == 401,
        "stale native confirmation accepted"
    );
    let status: Value = agent
        .get(&format!("{base}/v1/operations/{id}"))
        .set("Authorization", &bearer)
        .call()?
        .into_json()?;
    ensure!(
        status["operation_id"] == id.to_string() && status["state"] == "completed",
        "wrong terminal operation"
    );
    drain(&mut lookup, &after)?;
    Ok(())
}
