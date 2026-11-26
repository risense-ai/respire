use super::*;
use respire_spawn::owner_credential::{
    archive_proof::{self, ArchiveClaim},
    backup_grant::{self, Receiver, Scope},
    read_owner_credential, ArchiveOperation, BackupArchiveHandoff, ReadyBackupArchive,
};
use serde::{Deserialize, Serialize};
use std::{io::Write, os::unix::fs::MetadataExt, path::PathBuf, process::Stdio, sync::Mutex};

type Selection = ([u8; 32], [u8; 32]);
static TARGET: Mutex<Option<Selection>> = Mutex::new(None);

pub(in crate::runtime_http::v1::backup_export) fn remember_selection(
    store: &respire_app::transport::local::LocalStore,
) -> Result<()> {
    let pin = store.pin_backup_target()?;
    super::lifecycle_fixture::remember(&pin)?;
    super::offline_fixture::remember_pin(store)?;
    *TARGET
        .lock()
        .map_err(|_| anyhow::anyhow!("selection lock"))? = Some((
        Sha256::digest(
            pin.path()
                .to_str()
                .context("selected SQLite target")?
                .as_bytes(),
        )
        .into(),
        pin.identity_digest()?,
    ));
    Ok(())
}

pub(super) fn target() -> Result<Selection> {
    TARGET
        .lock()
        .map_err(|_| anyhow::anyhow!("selection lock"))?
        .take()
        .context("missing independent export selection")
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Frame {
    operation: String,
    instance: String,
    generation: u64,
    port: u16,
    target: [u8; 32],
    identity: [u8; 32],
    account: [u8; 32],
    wire: [u8; 32],
    issued: u64,
}
fn frame(
    agent: &ureq::Agent,
    base: &str,
    target: Selection,
    op: uuid::Uuid,
    wire: &[u8],
) -> Result<Frame> {
    let health: Value = agent
        .get(&format!("{base}/api/health"))
        .call()?
        .into_json()?;
    let address: SocketAddr = base.strip_prefix("http://").context("scheme")?.parse()?;
    ensure!(
        address.ip() == std::net::Ipv4Addr::LOCALHOST,
        "fixture must be local"
    );
    Ok(Frame {
        operation: op.to_string(),
        instance: health["instance_id"]
            .as_str()
            .context("instance")?
            .parse()?,
        generation: health["generation"].as_u64().context("generation")?,
        port: address.port(),
        target: target.0,
        identity: target.1,
        account: decode(&health["account_id"])?,
        wire: Sha256::digest(wire).into(),
        issued: now()?,
    })
}
fn now() -> Result<u64> {
    super::super::super::owner_control::wall_ms().context("clock")
}

const PREFIX: &str = "runtime_http::v1::backup_export::tests::process_fixture::";
fn launch(exe: &Path, mode: &str) -> Result<std::process::Child> {
    Ok(Command::new(exe)
        .args([
            "--ignored",
            "--exact",
            &format!("{PREFIX}{mode}"),
            "--nocapture",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?)
}
fn exchange(mut child: std::process::Child, frame: &impl Serialize) -> Result<Value> {
    child
        .stdin
        .take()
        .context("inherited input")?
        .write_all(&serde_json::to_vec(frame)?)?;
    let out = child.wait_with_output()?;
    ensure!(
        out.status.success(),
        "child failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::str::from_utf8(&out.stdout)?;
    let line = text
        .lines()
        .find(|line| line.starts_with('{'))
        .context("missing subprocess result")?;
    Ok(serde_json::from_str(line)?)
}
fn grant_path() -> PathBuf {
    crate::rpc_worker::runtime_dir().join("fixture-archive-supervisor.json")
}
fn image(pid: u32) -> Result<(u64, u64)> {
    let m = fs::metadata(format!("/proc/{pid}/exe"))?;
    Ok((m.dev(), m.ino()))
}
fn birth(pid: u32) -> Result<String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    Ok(stat
        .rsplit_once(')')
        .and_then(|(_, r)| r.split_whitespace().nth(19))
        .context("process birth")?
        .to_owned())
}
fn invoke(f: &Frame, wrong_parent: bool, other: bool) -> Result<Value> {
    let current = std::env::current_exe()?;
    let supervisor = launch(&current, "supervisor")?;
    let pid = if wrong_parent {
        std::process::id()
    } else {
        supervisor.id()
    };
    let grant = json!({"pid":pid,"birth":birth(pid)?,"image":image(pid)?,"frame":f,"other":other});
    let dir = crate::rpc_worker::runtime_dir();
    let mut stage = tempfile::NamedTempFile::new_in(dir)?;
    stage.write_all(&serde_json::to_vec(&grant)?)?;
    stage.persist_noclobber(grant_path())?;
    let result = exchange(supervisor, f);
    fs::remove_file(grant_path())?;
    result
}
fn input<T: serde::de::DeserializeOwned>() -> Result<T> {
    let mut bytes = Vec::new();
    std::io::stdin().lock().take(2049).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 2048, "oversize input");
    Ok(serde_json::from_slice(&bytes)?)
}

#[test]
#[ignore = "subprocess entry point"]
fn supervisor() -> Result<()> {
    let f: Frame = input()?;
    let grant: Value = serde_json::from_slice(&fs::read(grant_path())?)?;
    let me = std::env::current_exe()?;
    let other = if grant["other"] == true {
        let dir = tempfile::tempdir_in(me.parent().context("executable directory")?)?;
        fs::copy(&me, dir.path().join("fixture-helper"))?;
        Some(dir)
    } else {
        None
    };
    let executable = other
        .as_ref()
        .map(|d| d.path().join("fixture-helper"))
        .unwrap_or(me);
    let pid = std::process::id();
    let scope = offline_scope(&f, pid)?;
    let owner = read_owner_credential(&crate::rpc_worker::runtime_dir(), &f.instance)?;
    let receiver = Receiver::arm(&owner, scope)?;
    let mut result = exchange(
        launch(&executable, "native_helper")?,
        &(&f, receiver.challenge()),
    )?;
    if result["state"] == "claimed" {
        let intent = decode(&result["intent_sha256"])?;
        let op = ArchiveOperation::from_v4_bytes(scope.operation)?;
        ReadyBackupArchive::open(
            &service::data_dir(),
            BackupArchiveHandoff::from_approved_intent(op, intent),
        )?
        .require_archive_digest(scope.archive)?;
        let tag = result
            .as_object_mut()
            .context("receipt")?
            .remove("grant_tag")
            .context("missing grant")?;
        let verified = receiver.accept(intent, epoch(&result)?, &decode(&tag)?)?;
        // The authenticated helper has exited, but the admitted Runtime is still serving.
        // An exit from another child of the same image cannot advance this grant.
        let health: crate::net_rpc::Health = serde_json::from_slice(&fs::read(crate::rpc_worker::endpoint_path())?)?;
        let mut unrelated = Command::new(std::env::current_exe()?).arg("--list")
            .stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
        ensure!(unrelated.wait()?.success(), "same-image probe failed");
        let refused = super::drain_fixture::verify_child_exit(&mut unrelated, &health,
            uuid::Uuid::new_v4(), &verified, || anyhow::bail!("unexpected stop receipt read"));
        ensure!(refused.is_err_and(|error| error.to_string() == "owned child differs from admitted backup instance"),
            "unrelated exit advanced the grant");
        result["grant_fingerprint"] = json!(verified.fingerprint());
    }
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

#[test]
#[ignore = "subprocess entry point"]
fn native_helper() -> Result<()> {
    let (f, challenge): (Frame, [u8; 32]) = input()?;
    let result = match native(&f, &challenge) {
        Ok(receipt) => receipt,
        Err(error) => json!({"refused":error.to_string()}),
    };
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}
fn epoch(receipt: &Value) -> Result<[u8; 16]> {
    Ok(
        *uuid::Uuid::parse_str(receipt["vault_epoch"].as_str().context("missing epoch")?)?
            .as_bytes(),
    )
}
fn decode(value: &Value) -> Result<[u8; 32]> {
    let mut bytes = [0; 32];
    hex::decode_to_slice(value.as_str().context("missing digest")?, &mut bytes)?;
    Ok(bytes)
}
fn offline_scope(f: &Frame, parent: u32) -> Result<Scope> {
    Ok(Scope {
        operation: *uuid::Uuid::parse_str(&f.operation)?.as_bytes(),
        archive: f.wire,
        target: f.target,
        target_identity: f.identity,
        account: f.account,
        profile: Sha256::digest(
            service::data_dir()
                .canonicalize()?
                .as_os_str()
                .as_encoded_bytes(),
        )
        .into(),
        instance: *uuid::Uuid::parse_str(&f.instance)?.as_bytes(),
        generation: f.generation,
        supervisor_pid: parent,
        supervisor_birth: birth(parent)?.parse()?,
    })
}
fn native(f: &Frame, challenge: &[u8; 32]) -> Result<Value> {
    let grant_bytes = fs::read(grant_path())?;
    let grant: Value = serde_json::from_slice(&grant_bytes)?;
    ensure!(
        grant["frame"] == serde_json::to_value(f)?,
        "unapproved tuple"
    );
    let parent = u32::try_from(grant["pid"].as_u64().context("supervisor")?)?;
    respire_spawn::verify_control_parent(parent).context("wrong control parent")?;
    ensure!(
        grant["birth"] == birth(parent)? && grant["image"] == json!(image(parent)?),
        "supervisor changed"
    );

    let endpoint = crate::rpc_worker::endpoint_path();
    let pinned = fs::read(&endpoint)?;
    let record: Value = serde_json::from_slice(&pinned)?;
    let base = format!("http://127.0.0.1:{}", f.port);
    let agent = ureq::AgentBuilder::new()
        .try_proxy_from_env(false)
        .redirects(0)
        .timeout(WAIT)
        .build();
    let health: Value = agent
        .get(&format!("{base}/api/health"))
        .call()?
        .into_json()?;
    let observed: crate::net_rpc::Health = serde_json::from_value(health.clone())?;
    for k in [
        "pid",
        "process_start_id",
        "instance_id",
        "generation",
        "url",
        "installation_root",
        "data_dir",
        "account_id",
    ] {
        ensure!(
            !record[k].is_null() && health[k] == record[k],
            "endpoint mismatch"
        );
    }
    ensure!(
        crate::net_rpc::is_our_health(&health)
            && health["store_ready"] == true
            && record["url"] == base
            && record["instance_id"] == f.instance.to_string()
            && record["generation"] == f.generation
            && decode(&health["account_id"])? == f.account
            && Path::new(&observed.data_dir).canonicalize()?
                == service::data_dir().canonicalize()?
            && Path::new(&observed.installation_root).canonicalize()?
                == service::main_data_dir().canonicalize()?
            && crate::net_rpc::instance_is_running(&observed)?
            && crate::net_rpc::pid_listening_on(f.port) == Some(observed.pid),
        "unverified listener"
    );
    ensure!(
        image(observed.pid)? == image(std::process::id())?,
        "different runtime image"
    );
    let credential =
        read_owner_credential(&crate::rpc_worker::runtime_dir(), &f.instance.to_string())?;
    let claim = ArchiveClaim {
        query_only: false,
        inspect_only: false,
        operation: *uuid::Uuid::parse_str(&f.operation)?.as_bytes(),
        instance: *uuid::Uuid::parse_str(&f.instance)?.as_bytes(),
        generation: f.generation,
        archive: f.wire,
        target: f.target,
        target_identity: f.identity,
    };
    ensure!(
        fs::read(&endpoint)? == pinned && fs::read(grant_path())? == grant_bytes,
        "authority changed"
    );
    let inspection = ArchiveClaim { inspect_only: true, target_identity: [0;32], ..claim };
    let lookup = archive_proof::issue(credential.as_bytes(), inspection, f.issued).context("owner inspection proof")?;
    let prepared = agent.post(&format!("{base}/api/runtime/private-backup/claim"))
        .set("x-respire-archive-proof", &lookup).send_json(json!({"schema_version":2,"inspect_only":true,
            "operation_id":f.operation,"archive_sha256":hex::encode(f.wire),"target_sha256":hex::encode(f.target),
            "target_identity_sha256":hex::encode(inspection.target_identity)}));
    let prepared_target: Value = match prepared {
        Ok(reply) => reply.into_json()?,
        Err(ureq::Error::Status(code, _)) => return Ok(json!({"refused_status":code})),
        Err(error) => return Err(error.into()),
    };
    ensure!(prepared_target["state"]=="pending" && prepared_target["operation_id"]==f.operation
        && prepared_target["archive_sha256"]==hex::encode(f.wire) && prepared_target["target_sha256"]==hex::encode(f.target)
        && <[u8;32]>::from(Sha256::digest(prepared_target["target"].as_str().context("canonical target")?.as_bytes()))==f.target,
        "unverified pending native selection");
    ensure!(decode(&prepared_target["target_identity_sha256"])? == f.identity, "pending target inode differs");
    let tag =
        archive_proof::issue(credential.as_bytes(), claim, f.issued).context("owner proof")?;
    let frame = json!({"schema_version":2,"target_identity_sha256":hex::encode(f.identity),"operation_id":f.operation,"archive_sha256":hex::encode(f.wire),"target_sha256":hex::encode(f.target)});
    let reply = agent
        .post(&format!("{base}/api/runtime/private-backup/claim"))
        .set("x-respire-archive-proof", &tag)
        .send_json(frame);
    let mut result: Value = match reply {
        Ok(response) => response.into_json()?,
        Err(ureq::Error::Status(code, _)) => return Ok(json!({"refused_status":code})),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        result["state"] == "claimed"
            && result["operation_id"] == f.operation.to_string()
            && result["archive_sha256"] == hex::encode(f.wire)
            && result["target_sha256"] == hex::encode(f.target)
            && decode(&result["target_identity_sha256"])? == f.identity
            && fs::read(&endpoint)? == pinned
            && crate::net_rpc::instance_is_running(&observed)?
            && crate::net_rpc::pid_listening_on(f.port) == Some(observed.pid),
        "unconfirmed receipt"
    );
    let scope = offline_scope(f, parent)?;
    let tag = backup_grant::issue(
        &credential,
        challenge,
        &scope,
        &decode(&result["intent_sha256"])?,
        &epoch(&result)?,
    )?;
    result["grant_tag"] = json!(hex::encode(tag));
    Ok(result)
}
pub(super) fn exercise(
    agent: &ureq::Agent,
    base: &str,
    target: Selection,
    id: uuid::Uuid,
    wire: &[u8],
    negatives: bool,
) -> Result<Value> {
    let mut f = frame(agent, base, target, id, wire)?;
    if negatives {
        ensure!(
            invoke(&f, true, false)?["refused"] == "wrong control parent",
            "parent check"
        );
        let other = invoke(&f, false, true)?;
        ensure!(
            other["refused"] == "different runtime image",
            "image check: {other}"
        );
        let mut bad = f.clone();
        bad.target[0] ^= 1;
        ensure!(
            invoke(&bad, false, false)?["refused_status"] == 401,
            "target check"
        );
        bad = f.clone();
        bad.identity[0] ^= 1;
        ensure!(invoke(&bad, false, false)?["refused"] == "pending target inode differs", "target inode check");
        bad = f.clone();
        bad.wire[0] ^= 1;
        ensure!(
            invoke(&bad, false, false)?["refused_status"] == 401,
            "wire digest check"
        );
        bad = f.clone();
        bad.issued = bad.issued.saturating_sub(60000);
        ensure!(
            invoke(&bad, false, false)?["refused_status"] == 401,
            "expiry check"
        );
    }
    f.issued = now()?;
    let receipt = invoke(&f, false, false)?;
    ensure!(receipt["state"] == "claimed", "claim failed: {receipt}");
    ensure!(
        invoke(&f, false, false)?["refused_status"] == 409,
        "replay check"
    );
    eprintln!(
        "native {} receipt {}",
        if negatives { "parent" } else { "child" },
        receipt
    );
    Ok(receipt)
}
pub(super) fn reject_cancelled(
    agent: &ureq::Agent,
    base: &str,
    target: Selection,
    id: uuid::Uuid,
    wire: &[u8],
) -> Result<()> {
    let f = frame(agent, base, target, id, wire)?;
    ensure!(
        invoke(&f, false, false)?["refused_status"] == 404,
        "cancel check"
    );
    Ok(())
}


/// The calling fixture retains both the challenge and its authenticated target until offline.
pub(super) fn claim_for_offline(agent: &ureq::Agent, base: &str, target: Selection,
    id: uuid::Uuid, wire: &[u8]) -> Result<backup_grant::VerifiedGrant> {
    let f = frame(agent, base, target, id, wire)?;
    let pid = std::process::id();
    let scope = offline_scope(&f, pid)?;
    let owner = read_owner_credential(&crate::rpc_worker::runtime_dir(), &f.instance)?;
    let receiver = Receiver::arm(&owner, scope)?;
    let grant = json!({"pid":pid,"birth":birth(pid)?,"image":image(pid)?,"frame":f});
    let mut stage = tempfile::NamedTempFile::new_in(crate::rpc_worker::runtime_dir())?;
    stage.write_all(&serde_json::to_vec(&grant)?)?;
    stage.persist_noclobber(grant_path())?;
    let response = exchange(launch(&std::env::current_exe()?, "native_helper")?, &(&f, receiver.challenge()));
    fs::remove_file(grant_path())?;
    let reply = response?;
    ensure!(reply["state"] == "claimed", "exact archive was not claimed");
    let epoch = epoch(&reply)?;
    Ok(receiver.accept(decode(&reply["intent_sha256"])?, epoch, &decode(&reply["grant_tag"])?)?)
}
