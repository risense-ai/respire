//! Exercise native confirmation and durable restoration through the shipped supervisor.
#![cfg(target_os = "linux")]
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
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
fn shipped_supervisor_applies_exact_pending_archive_and_reopens_original_operation() -> Result<()> {
    exercise(false)
}
#[test]
fn exact_release_marker_resumes_without_replacing_the_database() -> Result<()> {
    exercise(true)
}
fn exercise(interrupt: bool) -> Result<()> {
    let root = Fixture(std::env::temp_dir().canonicalize()?.join(format!("backup-{}", Uuid::new_v4())));
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(&root.0)?;
    let profile = &root.0;
    fs::DirBuilder::new().mode(0o700).create(profile.join("runtime"))?;
    let token_path = profile.join("runtime/token");
    fs::write(&token_path, Uuid::new_v4().simple().to_string())?;
    fs::set_permissions(token_path, fs::Permissions::from_mode(0o600))?;
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new().create_new(true).write(true).mode(0o600).open(profile.join("rsrs.db"))?;
    let (password, _urk, salt, wrapped, nonce) = respire_app::auth::new_vault_v4()?;
    let session = serde_json::to_vec(&json!({"user":"backup-fixture","vault_version":4,"kdf_salt":salt,
        "urk_nonce":nonce,"wrapped_urk":wrapped}))?;
    fs::write(profile.join("session.json"), &session)?;
    fs::set_permissions(profile.join("session.json"), fs::Permissions::from_mode(0o600))?;
    let reservation = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = reservation.local_addr()?.port();
    drop(reservation);
    // Workspace tests unify the commands crate's synthetic keyring feature.
    // A separate profile keeps both the ordinary image and workspace test binaries intact.
    let built = Command::new(env!("CARGO"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([
            "build",
            "--locked",
            "--offline",
            "--package",
            "respire_runtime",
            "--bin",
            "rsrs-runtime",
            "--profile",
            "backup-proof",
            "--config",
            r#"profile.backup-proof.inherits="dev""#,
            "--config",
            "profile.backup-proof.debug=0",
            "--message-format=json-render-diagnostics",
        ])
        .output()?;
    ensure!(
        built.status.success(),
        "ordinary Runtime build failed: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    let executable = std::str::from_utf8(&built.stdout)?
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find_map(|event| {
            (event["target"]["name"] == "rsrs-runtime")
                .then(|| event["executable"].as_str().map(std::path::PathBuf::from))
                .flatten()
        })
        .context("ordinary Runtime executable missing")?;
    use std::io::{BufRead, BufReader, Read, Write};
    let launch = || -> Result<Owned> { Ok(Owned(
        Command::new(&executable)
            .env_clear()
            .env("HOME", profile)
            .env("RSRS_DATA_DIR", profile)
            .env("RSRS_RPC_PORT", port.to_string())
            .env("LD_LIBRARY_PATH", std::env::var_os("LD_LIBRARY_PATH").unwrap_or_default())
            .arg("--backup-supervisor")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?,
    )) };
    let mut parent = launch()?;
    let mut requests = parent.0.stdin.take().context("supervisor input")?;
    let mut replies = BufReader::new(parent.0.stdout.take().context("supervisor output")?);
    let mut send = |v: Value| -> Result<()> {
        serde_json::to_writer(&mut requests, &v)?;
        requests.write_all(b"\n")?;
        requests.flush()?;
        Ok(())
    };
    let mut receive = || -> Result<Value> {
        let mut line = String::new();
        ensure!(replies.read_line(&mut line)? > 0, "supervisor closed without receipt");
        Ok(serde_json::from_str(&line)?)
    };
    let birth = fs::read_to_string("/proc/self/stat")?
        .rsplit_once(')')
        .context("stat")?
        .1
        .split_whitespace()
        .nth(19)
        .context("birth")?
        .to_owned();
    send(
        json!({"controller_pid":std::process::id(),"controller_birth":birth,"session_sha256":hex(&Sha256::digest(&session)),
        "super_password":password}),
    )?;
    let ready = receive()?;
    ensure!(ready["state"] == "ready", "missing supervisor readiness");
    let base = format!("http://127.0.0.1:{port}");
    let agent = ureq::AgentBuilder::new()
        .try_proxy_from_env(false)
        .redirects(0)
        .timeout(Duration::from_secs(20))
        .build();
    let bearer = format!("Bearer {}", fs::read_to_string(profile.join("runtime/token"))?.trim());
    let get = |path: &str| agent.get(&format!("{base}{path}")).set("Authorization", &bearer);
    let post = |path: &str| agent.post(&format!("{base}{path}")).set("Authorization", &bearer);
    let rival = Command::new(&executable)
        .env_clear()
        .env("HOME", profile)
        .env("RSRS_DATA_DIR", profile)
        .env("RSRS_RPC_PORT", port.to_string())
        .env("LD_LIBRARY_PATH", std::env::var_os("LD_LIBRARY_PATH").unwrap_or_default())
        .args(["serve", "--host", "127.0.0.1"])
        .output()?;
    ensure!(
        !rival.status.success()
            && String::from_utf8_lossy(&rival.stderr).contains("reserved for an exact replacement"),
        "ordinary rival passed backup reservation"
    );
    let save = |s: &str| -> Result<String> {
        let r: Value = post("/v1/memories").send_json(json!({"space_id":"main","content":s}))?.into_json()?;
        Ok(r["id"].as_str().context("memory id")?.to_owned())
    };
    let preserved = save("restore original synthetic entry")?;
    let mut archive = Vec::new();
    get("/api/runtime/private-backup/export?space_id=main")
        .call()?
        .into_reader()
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut archive)?;
    ensure!(archive.len() <= 8 * 1024 * 1024, "oversized fixture");
    let removed = save("synthetic entry created after export")?;
    let upload = || -> Result<Value> {
        let r = post("/api/runtime/private-backup/import?space_id=main")
            .set("Content-Type", "application/octet-stream")
            .send_bytes(&archive)?;
        ensure!(r.status() == 202, "stage failed");
        Ok(r.into_json()?)
    };
    let cancelled = upload()?["operation_id"].as_str().context("cancelled UUID")?.to_owned();
    agent.delete(&format!("{base}/v1/operations/{cancelled}")).set("Authorization", &bearer).call()?;
    let id = upload()?["operation_id"].as_str().context("UUID")?.to_owned();
    let exact = json!({"action":"restore","operation_id":id,"archive_sha256":hex(&Sha256::digest(&archive)),
        "target_sha256":ready["target_sha256"],"target_identity_sha256":ready["target_identity_sha256"],
        "instance_id":ready["instance_id"],"space":"main"});
    for (field, value) in [
        ("action", json!("delete")),
        ("archive_sha256", json!("0".repeat(64))),
        ("target_identity_sha256", json!("0".repeat(64))),
        ("operation_id", json!(cancelled)),
    ] {
        let mut wrong = exact.clone();
        wrong[field] = value;
        send(wrong)?;
        ensure!(receive()?["state"] == "denied", "wrong confirmation admitted");
        let poll: Value = get(&format!("/v1/operations/{id}")).call()?.into_json()?;
        ensure!(poll["state"] == "pending", "denial mutated job");
    }
    ensure!(
        matches!(get("/v1/backups/export").call(), Err(ureq::Error::Status(404, _))),
        "public export mounted"
    );
    ensure!(
        matches!(post("/v1/backups/import").call(), Err(ureq::Error::Status(404, _))),
        "public import mounted"
    );
    ensure!(
        matches!(
            post("/api/runtime/private-backup/claim").send_json(exact.clone()),
            Err(ureq::Error::Status(401, _))
        ),
        "public bearer conferred native claim authority"
    );
    send(exact.clone())?;
    if interrupt {
        let prefix = format!(".backup-archive-{}.", Uuid::parse_str(&id)?.simple());
        let deadline = Instant::now()+Duration::from_secs(15);
        while !profile.join(format!("{prefix}archive")).is_file() {
            ensure!(Instant::now()<deadline, "claim did not publish custody");
            std::thread::sleep(Duration::from_millis(1));
        }
        let obstacle=profile.join(format!("{prefix}pending"));
        fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&obstacle)?;
        ensure!(!parent.0.wait()?.success(), "expected exact cleanup to refuse the substituted pending file");
        fs::remove_file(obstacle)?;
        let live=respire_app::service::database_path(profile)?;
        use std::os::unix::fs::MetadataExt;
        let fingerprint=|| -> Result<_> { Ok((fs::metadata(&live)?.ino(),Sha256::digest(fs::read(&live)?))) };
        let before=fingerprint()?;
        for attempt in [Uuid::new_v4().to_string(),id.clone(),id.clone()] {
            let mut retry=launch()?;
            let mut input=retry.0.stdin.take().context("retry pipe")?;
            writeln!(input,"{}",json!({"controller_pid":std::process::id(),"controller_birth":birth,
                "session_sha256":hex(&Sha256::digest(&session)),"super_password":password,"resume_operation":attempt}))?;
            drop(input);
            let mut reply=String::new();
            retry.0.stdout.take().context("retry result")?.read_to_string(&mut reply)?;
            let success=retry.0.wait()?.success();
            let fence=profile.join("runtime/autostart/launch/backup-restart");
            if attempt!=id { ensure!(!success && reply.is_empty() && fence.exists(),"wrong UUID released reservation"); }
            else {
                let result:Value=serde_json::from_str(&reply)?;
                ensure!(success && result["state"]=="completed" && result["operation_id"]==id && !fence.exists(),"release not completed");
            }
            ensure!(fingerprint()?==before,"retry changed SQLite");
        }
        return Ok(());
    }
    let terminal = receive()?;
    ensure!(terminal["operation_id"] == id && terminal["state"] == "completed", "wrong terminal scope");
    ensure!(
        matches!(
            post("/api/runtime/private-backup/import?space_id=main").call(),
            Err(ureq::Error::Status(404, _))
        ),
        "final serving child accepted new custody"
    );
    let lookup: Value = get(&format!("/v1/operations/{id}")).call()?.into_json()?;
    ensure!(lookup == terminal, "operation lookup mismatch");
    let health: Value = get("/api/health").call()?.into_json()?;
    ensure!(health["instance_id"] != ready["instance_id"], "old Runtime survived");
    send(exact)?;
    ensure!(receive()?["state"] == "denied", "completed operation accepted a second confirmation");
    let unchanged: Value = get("/api/health").call()?.into_json()?;
    ensure!(unchanged["instance_id"] == health["instance_id"], "replay disturbed the reopened Runtime");
    drop(requests);
    let until = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = parent.0.try_wait()? {
            ensure!(status.success(), "supervisor failed");
            break;
        }
        ensure!(Instant::now() < until, "supervisor did not exit");
        std::thread::sleep(Duration::from_millis(25));
    }
    let db = rusqlite::Connection::open_with_flags(
        respire_app::service::database_path(profile)?,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let has = |id: &str| {
        db.query_row("SELECT count(*) FROM memories WHERE id=?1 AND deleted=0", [id], |r| r.get::<_, u64>(0))
    };
    ensure!(has(&preserved)? == 1 && has(&removed)? == 0, "restored SQLite content mismatch");
    drop(db);
    // A consumed reservation left by this actual departed supervisor needs reconciliation.
    let launch_dir = profile.join("runtime/autostart/launch");
    let receipt: Value = serde_json::from_slice(&fs::read(
        launch_dir.join(format!("launch-{}.claimed", ready["instance_id"].as_str().context("instance")?)),
    )?)?;
    let old: respire_protocol::HostControlLaunchGrant =
        serde_json::from_str(receipt["grant_json"].as_str().context("consumed grant")?)?;
    let fence = launch_dir.join("backup-restart");
    let mut fence_file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&fence)?;
    fence_file.write_all(&serde_json::to_vec(&old)?)?;
    let takeover =
        respire_app::lock::LibraryLock::acquire(&profile.join("runtime/takeover"), Duration::ZERO)?;
    let installation = respire_app::lock::LibraryLock::acquire(&profile.join("runtime"), Duration::ZERO)?;
    let library = respire_app::lock::LibraryLock::acquire(profile, Duration::ZERO)?;
    ensure!(
        respire_commands::backup_restart::recover_stale(profile, profile, &takeover, &installation, &library)
            .is_err(),
        "consumed grant was recovered without reconciliation"
    );
    Ok(())
}
