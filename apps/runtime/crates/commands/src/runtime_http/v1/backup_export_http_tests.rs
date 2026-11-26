use super::*;
use anyhow::{Context, Result};
use respire_app::{auth, service, snapshot::backup_archive::open_to_stage};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fs, io::Read, path::Path, process::Command, thread};

const TEST: &str = "runtime_http::v1::backup_export::tests::scoped_archive_over_loopback";
const CHILD: &str = "RSRS_BACKUP_EXPORT_SOCKET_FIXTURE";
const FINISHED: &str = "RSRS_BACKUP_EXPORT_FIXTURE_FINISHED";
const WAIT: Duration = Duration::from_secs(10);
const TOKEN: &str = "synthetic-export-bearer";
const EXPORT: &str = "/v1/backups/export";

#[test]
fn scoped_archive_over_loopback() -> Result<()> {
    if std::env::var_os(CHILD).is_none() {
        let directory = tempfile::tempdir()?;
        let witness = directory.path().join("backup-fixture-completed");
        let base_dir = directory.path().canonicalize()?;
        #[cfg(windows)]
        let data_dir = {
            let root = respire_spawn::owner_credential::LocalSpaceNamespace::make_fixture_installation(&base_dir)?;
            respire_spawn::owner_credential::LocalSpaceNamespace::make_fixture_runtime_parent(&root)?;
            match root.to_str().and_then(|p| p.strip_prefix(r"\\?\")) {
                Some(local) => std::path::PathBuf::from(local),
                None => root,
            }
        };
        #[cfg(not(windows))]
        let data_dir = {
            fs::create_dir_all(base_dir.join("runtime"))?;
            base_dir
        };
        let mut child = Command::new(std::env::current_exe()?)
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, "1")
            .env(FINISHED, &witness)
            .env("RSRS_CORE_TEST_MODE", "1")
            .env("RSRS_DATA_DIR", &data_dir)
            .env_remove("RSRS_DEFAULT_DATA_DIR")
            .env_remove("RSRS_RPC_TOKEN")
            .env_remove("RSRS_ADDR")
            .env_remove("RSRS_TOKEN")
            .spawn()?;
        let deadline = Instant::now() + 4 * WAIT;
        loop {
            if let Some(status) = child.try_wait()? {
                ensure!(status.success(), "backup socket fixture failed: {status}");
                ensure!(fs::read(&witness)? == b"verified", "backup socket fixture did not run");
                return Ok(());
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!("backup socket fixture exceeded its deadline");
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    let root = service::data_dir().canonicalize()?;
    let token = crate::net_rpc::token_path();
    fs::create_dir_all(token.parent().context("missing fixture token parent")?)?;
    fs::write(&token, TOKEN)?;
    let (password, urk, salt, wrapped, nonce) = auth::new_vault_v4()?;
    let session = auth::write_session_json(&json!({
        "user": "backup-fixture", "vault_version": 4, "kdf_salt": salt,
        "urk_nonce": nonce, "wrapped_urk": wrapped,
    }))?;
    let instance_id = uuid::Uuid::new_v4();
    auth::supply_runtime_session(respire_protocol::credentials::RuntimeUnlock {
        schema_version: respire_protocol::credentials::CREDENTIAL_SCHEMA_V,
        profile: root.clone(),
        session_sha256: format!("{:x}", Sha256::digest(fs::read(session)?)),
        instance_id: instance_id.to_string(),
        generation: 1,
        super_password: Some(password),
        login_password: None,
        legacy_secret: None,
        provider_credentials: Vec::new(),
    })?;

    let (stop, stopped) = std::sync::mpsc::channel::<()>();
    let (ready, started) = std::sync::mpsc::channel();
    let profile = root.clone();
    let dispatch = thread::spawn(move || -> Result<()> {
        let lock = respire_app::lock::LibraryLock::acquire(&profile, WAIT)?;
        respire_app::transport::local::LocalStore::open(&profile.join("onememory.db"))?;
        rusqlite::Connection::open(profile.join("onememory.db"))?.execute_batch(
            "CREATE TABLE fixture_payload(data BLOB); INSERT INTO fixture_payload VALUES(zeroblob(2300000));",
        )?;
        let (store, _) = service::open_runtime_store(&lock)?;
        let mut dispatcher = crate::runtime_http::start_owner_dispatch().context("fixture dispatcher unavailable")?;
        ready.send(())?;
        while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
            stopped.recv_timeout(Duration::from_millis(5))
        {
            dispatcher.tick(Some(&store), &lock);
        }
        Ok(())
    });
    let run = (|| -> Result<()> {
        started.recv_timeout(WAIT)?;
        crate::rpc_serve::mark_http_fixture_ready();
        tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let bound = listener.local_addr()?;
            let app = crate::runtime_http::router(bound);
            let server = tokio::spawn(async move {
                axum::serve(
                    crate::network::Listener(listener),
                    app.into_make_service_with_connect_info::<crate::network::Peer>(),
                ).await
            });
            let result = tokio::task::spawn_blocking(move || verify_http(bound, &root, urk)).await;
            server.abort();
            let _ = server.await;
            result?
        })
    })();
    drop(stop);
    dispatch.join().map_err(|_| anyhow::anyhow!("fixture dispatcher panicked"))??;
    run?;
    let witness = std::env::var_os(FINISHED).context("backup fixture witness missing")?;
    fs::write(witness, b"verified")?;
    Ok(())
}

fn verify_http(bound: SocketAddr, root: &Path, urk: [u8; 32]) -> Result<()> {
    let base = format!("http://{bound}");
    let agent = ureq::AgentBuilder::new().try_proxy_from_env(false).timeout(WAIT).build();
    let get = |name: &str| agent.get(&format!("{base}{name}"));
    let post = |name: &str| agent.post(&format!("{base}{name}")).set("Authorization", &format!("Bearer {TOKEN}"));
    for (request, status) in [
        (get(EXPORT), 401),
        (get(EXPORT).set("Authorization", "Bearer stale-fixture"), 401),
        (get(EXPORT).set("Authorization", &format!("Bearer {TOKEN}")).set("Origin", "http://untrusted.invalid"), 403),
        (get(&format!("{EXPORT}?path=disallowed")).set("Authorization", &format!("Bearer {TOKEN}")), 400),
        (get(&format!("{EXPORT}?space_id=unknown-fixture")).set("Authorization", &format!("Bearer {TOKEN}")), 404),
        (agent.post(&format!("{base}/v1/backups/import")), 401),
        (post("/v1/backups/import").set("Origin", "http://untrusted.invalid"), 403),
        (post("/v1/backups/import?path=disallowed").set("Content-Type", "application/octet-stream"), 400),
        (post("/v1/backups/import?space_id=unknown-fixture").set("Content-Type", "application/octet-stream"), 404),
        (post("/v1/backups/import").set("Content-Type", "application/json"), 400),
    ] {
        match request.call() {
            Err(ureq::Error::Status(observed, _)) if observed == status => (),
            _ => anyhow::bail!("unexpected backup admission status; expected {status}"),
        }
    }
    assert!(backup_stage_count(root)? == 0);

    let save = |space: &str, content: &str| -> Result<String> {
        let response = post("/v1/memories").send_json(json!({"space_id": space, "content": content}))?;
        ensure!(response.status() == 201, "memory creation failed");
        let response: Value = response.into_json()?;
        Ok(response["id"].as_str().context("fixture memory ID absent")?.to_owned())
    };
    let list_ids = |space: &str| -> Result<BTreeSet<String>> {
        let response: Value = get(&format!("/v1/memories?space_id={space}"))
            .set("Authorization", &format!("Bearer {TOKEN}"))
            .call()?
            .into_json()?;
        let items = response["items"].as_array().context("memory items array missing")?;
        items.iter().map(|entry| entry["id"].as_str().map(str::to_owned).context("memory id missing")).collect()
    };
    let wait_completed = |operation_id: &str| -> Result<()> {
        let deadline = Instant::now() + WAIT;
        loop {
            let status: Value = get(&format!("/v1/operations/{operation_id}"))
                .set("Authorization", &format!("Bearer {TOKEN}"))
                .call()?
                .into_json()?;
            match status["state"].as_str() {
                Some("completed") => return Ok(()),
                Some("pending" | "running") if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
                other => anyhow::bail!("backup import operation did not complete cleanly: {other:?} ({status})"),
            }
        }
    };

    let parent = save("main", "isolated parent fixture memory")?;
    let created = post("/v1/spaces").send_json(json!({"name": "isolated backup fixture"}))?;
    ensure!(created.status() == 201, "device space creation failed");
    let create: Value = created.into_json()?;
    let space = create["id"].as_str().context("fixture device space ID absent")?;
    let child = save(space, "isolated child fixture memory")?;

    let keys = respire_app::SessionKeys::from_urk(urk)?;
    let mut main_wire = Vec::new();
    let mut child_wire = Vec::new();
    for (target_space, expected, slot) in [("main", &parent, &mut main_wire), (space, &child, &mut child_wire)] {
        let response = get(&format!("{EXPORT}?space_id={target_space}"))
            .set("Authorization", &format!("Bearer {TOKEN}")).call()?;
        assert_eq!(response.status(), 200);
        assert_eq!(response.header("content-type"), Some("application/octet-stream"));
        assert_eq!(response.header("cache-control"), Some("no-store"));
        response.into_reader().take(8 * 1024 * 1024 + 1).read_to_end(slot)?;
        ensure!(slot.len() <= 8 * 1024 * 1024, "fixture archive exceeded its byte budget");
        if target_space == "main" {
            ensure!(slot.len() > 2 * 1024 * 1024, "fixture must cross the old buffer limit");
        }
        let output = tempfile::NamedTempFile::new_in(root)?;
        open_to_stage(&keys, "backup-fixture", slot.as_slice(), output.as_file(), || Ok(()))?;
        let db = rusqlite::Connection::open(output.path())?;
        let mut rows = db.prepare("SELECT id FROM memories WHERE deleted=0")?;
        let ids = rows.query_map([], |row| row.get(0))?.collect::<rusqlite::Result<BTreeSet<String>>>()?;
        assert_eq!(ids, BTreeSet::from([expected.clone()]));
    }

    let upload_main = |payload: &[u8]| post("/v1/backups/import?space_id=main")
        .set("Content-Type", "application/octet-stream").send_bytes(payload);
    let reject_main = |payload: &[u8]| -> Result<()> {
        ensure!(matches!(upload_main(payload), Err(ureq::Error::Status(400, _))), "invalid upload accepted");
        Ok(())
    };
    reject_main(b"invalid envelope")?;
    reject_main(&main_wire[..main_wire.len() - 1])?;
    let mut bad_seal = main_wire.clone();
    let field = br#""sealed_manifest_digest":""#;
    let seal = bad_seal.windows(field.len()).position(|bytes| bytes == field)
        .context("fixture seal missing")? + field.len();
    bad_seal[seal] = if bad_seal[seal] == b'A' { b'B' } else { b'A' };
    let header_size = u32::from_be_bytes(main_wire[8..12].try_into()?) as usize;
    reject_main(&bad_seal[..12 + header_size])?;
    {
        use std::{io::Write, net::{Shutdown, TcpStream}};
        let mut stream = TcpStream::connect(bound)?;
        stream.set_read_timeout(Some(WAIT))?;
        stream.set_write_timeout(Some(WAIT))?;
        write!(stream, "POST /v1/backups/import HTTP/1.1\r\nHost: {bound}\r\nAuthorization: Bearer {TOKEN}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", main_wire.len())?;
        stream.write_all(&main_wire[..main_wire.len() / 2])?;
        stream.shutdown(Shutdown::Write)?;
        let mut reply = Vec::new();
        stream.take(8192).read_to_end(&mut reply)?;
        ensure!(!reply.starts_with(b"HTTP/1.1 202"), "aborted upload accepted");
    }
    let mut corrupt = main_wire.clone();
    *corrupt.last_mut().context("empty archive")? ^= 1;
    reject_main(&corrupt)?;
    let mut trailing = main_wire.clone();
    trailing.push(0);
    reject_main(&trailing)?;
    reject_main(&child_wire)?;
    ensure!(
        matches!(
            post(&format!("/v1/backups/import?space_id={space}"))
                .set("Content-Type", "application/octet-stream")
                .send_bytes(&main_wire[..12 + header_size]),
            Err(ureq::Error::Status(400, _))
        ),
        "main backup must not restore into a device space"
    );

    let parent_extra = save("main", "post-export parent memory to discard")?;
    let child_extra = save(space, "post-export child memory to discard")?;
    assert_eq!(list_ids("main")?, BTreeSet::from([parent.clone(), parent_extra.clone()]));
    assert_eq!(list_ids(space)?, BTreeSet::from([child.clone(), child_extra.clone()]));

    let owner_deadline = Instant::now() + WAIT;
    while !super::super::owner_control::capable() {
        ensure!(Instant::now() < owner_deadline, "native owner credential not ready");
        thread::sleep(Duration::from_millis(10));
    }
    let health: Value = get("/v1/health").call()?.into_json()?;
    assert_eq!(health["status"], "ready");
    let health_instance = health["instance_id"].as_str().context("serving instance ID")?;
    let (live_instance, live_generation) = crate::rpc_serve::with_verified_owner_scope(
        |_, _, instance, generation, _| (instance, generation),
    )
    .context("live owner scope missing")?;
    assert_eq!(live_instance.to_string(), health_instance);
    let record = respire_spawn::owner_credential::read_owner_credential(
        &crate::rpc_worker::runtime_dir(),
        &live_instance.to_string(),
    )?;
    let main_db = service::database_path(root)?.canonicalize()?;
    let child_db = root.join(".respire-local-spaces-v1").join(space).join("onememory.db").canonicalize()?;
    let target_hash = |path: &Path| -> Result<[u8; 32]> {
        Ok(Sha256::digest(path.to_str().context("canonical path UTF-8")?.as_bytes()).into())
    };
    let child_target: [u8; 32] = target_hash(&child_db)?;
    let main_target: [u8; 32] = target_hash(&main_db)?;
    let child_digest: [u8; 32] = Sha256::digest(&child_wire).into();
    let main_digest: [u8; 32] = Sha256::digest(&main_wire).into();
    let sign_confirm = |op: uuid::Uuid, archive: [u8; 32], target: [u8; 32]| -> Result<String> {
        let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis() as u64;
        let claim = respire_spawn::owner_credential::backup_proof::BackupRestoreClaim {
            operation_id: *op.as_bytes(),
            archive_sha256: archive,
            target_sha256: target,
            instance: *live_instance.as_bytes(),
            generation: live_generation,
        };
        let proof = respire_spawn::owner_credential::backup_proof::issue(record.as_bytes(), claim, now_ms)
            .context("backup confirmation proof issue failed")?;
        Ok(proof.as_str().to_owned())
    };
    let call_confirm = |op: uuid::Uuid, archive: [u8; 32], target: [u8; 32], proof: &str| {
        agent.post(&format!("{base}/api/runtime/private-backup/confirm"))
            .set("x-respire-backup-proof", proof)
            .send_json(json!({
                "schema_version": 1,
                "operation_id": op.to_string(),
                "archive_sha256": hex::encode(archive),
                "target_sha256": hex::encode(target),
            }))
    };

    let accepted_child = post(&format!("/v1/backups/import?space_id={space}"))
        .set("Content-Type", "application/octet-stream")
        .send_bytes(&child_wire)?;
    assert_eq!(accepted_child.status(), 202);
    let receipt_child: Value = accepted_child.into_json()?;
    assert_eq!(receipt_child["status"], "pending");
    let child_op = receipt_child["operation_id"].as_str().context("child restore operation_id missing")?;
    let child_uuid = uuid::Uuid::parse_str(child_op)?;
    assert_eq!(backup_stage_count(root)?, 1);

    // 202 stages only; confirm HTTP bearer, foreign origins, and mismatched local claims are refused.
    thread::sleep(Duration::from_millis(100));
    let pending_status: Value = get(&format!("/v1/operations/{child_op}"))
        .set("Authorization", &format!("Bearer {TOKEN}")).call()?.into_json()?;
    assert_eq!(pending_status["state"], "pending");
    assert_eq!(list_ids(space)?, BTreeSet::from([child.clone(), child_extra.clone()]));
    let child_proof_for_rejects = sign_confirm(child_uuid, child_digest, child_target)?;
    for (req, expected_status) in [
        (agent.post(&format!("{base}/api/runtime/private-backup/confirm")).set("Authorization", &format!("Bearer {TOKEN}")), 401),
        (agent.post(&format!("{base}/api/runtime/private-backup/confirm")).set("x-respire-model-proof", &format!("rhm1_{}", "a".repeat(144))), 401),
        (agent.post(&format!("{base}/api/runtime/private-backup/confirm")).set("Origin", "http://untrusted.invalid").set("x-respire-backup-proof", &child_proof_for_rejects), 403),
        (agent.post(&format!("{base}/api/runtime/private-backup/confirm")).set("Cookie", "ambient=1").set("x-respire-backup-proof", &child_proof_for_rejects), 403),
    ] {
        ensure!(matches!(req.send_json(json!({
            "schema_version": 1, "operation_id": child_op,
            "archive_sha256": hex::encode(child_digest), "target_sha256": hex::encode(child_target),
        })), Err(ureq::Error::Status(observed, _)) if observed == expected_status));
    }
    let mut wrong_digest = child_digest;
    wrong_digest[0] ^= 1;
    let unknown_uuid = uuid::Uuid::new_v4();
    for (bad_op, bad_archive, bad_target, expected) in [
        (unknown_uuid, child_digest, child_target, 404),
        (child_uuid, wrong_digest, child_target, 409),
        (child_uuid, child_digest, main_target, 409),
    ] {
        let proof = sign_confirm(bad_op, bad_archive, bad_target)?;
        ensure!(matches!(call_confirm(bad_op, bad_archive, bad_target, &proof), Err(ureq::Error::Status(observed, _)) if observed == expected));
    }
    assert_eq!(list_ids(space)?, BTreeSet::from([child.clone(), child_extra]));
    assert_eq!(backup_stage_count(root)?, 1);

    // Bounded queue (max 2) + concurrent cancel/status/reap race must never deadlock or leak stages.
    let cancel_accepted = post("/v1/backups/import?space_id=main")
        .set("Content-Type", "application/octet-stream")
        .send_bytes(&main_wire)?;
    assert_eq!(cancel_accepted.status(), 202);
    let cancel_receipt: Value = cancel_accepted.into_json()?;
    let cancel_op = cancel_receipt["operation_id"].as_str().context("cancel operation_id missing")?;
    let cancel_uuid = uuid::Uuid::parse_str(cancel_op)?;
    assert_eq!(backup_stage_count(root)?, 2);
    ensure!(
        matches!(upload_main(&main_wire[..12 + header_size]), Err(ureq::Error::Status(503, _))),
        "pending custody limit should reject a third upload with 503"
    );
    let mut racers = Vec::new();
    for step in 0..6 {
        let cancel_url = format!("{base}/v1/operations/{cancel_op}");
        racers.push(thread::spawn(move || -> Result<()> {
            let local = ureq::AgentBuilder::new().try_proxy_from_env(false).timeout(WAIT).build();
            if step % 3 == 0 {
                let reply = local.delete(&cancel_url).set("Authorization", &format!("Bearer {TOKEN}")).call()?;
                ensure!(reply.status() == 202, "unexpected cancel status");
            } else if step % 3 == 1 {
                let _ = local.get(&cancel_url).set("Authorization", &format!("Bearer {TOKEN}")).call()?;
            } else {
                super::super::operations::backup_import::reap_stale();
            }
            Ok(())
        }));
    }
    for racer in racers {
        racer.join().map_err(|_| anyhow::anyhow!("concurrent cancel racer panicked"))??;
    }
    let cancelled_state: Value = get(&format!("/v1/operations/{cancel_op}"))
        .set("Authorization", &format!("Bearer {TOKEN}")).call()?.into_json()?;
    assert_eq!(cancelled_state["state"], "cancelled");
    assert_eq!(backup_stage_count(root)?, 1);
    let cancel_proof = sign_confirm(cancel_uuid, main_digest, main_target)?;
    ensure!(matches!(call_confirm(cancel_uuid, main_digest, main_target, &cancel_proof), Err(ureq::Error::Status(404, _))));

    let child_confirm_proof = sign_confirm(child_uuid, child_digest, child_target)?;
    let confirmed_child = call_confirm(child_uuid, child_digest, child_target, &child_confirm_proof)?;
    assert_eq!(confirmed_child.status(), 200);
    let confirmed_child_body: Value = confirmed_child.into_json()?;
    assert_eq!(confirmed_child_body, json!({"operation_id": child_op, "status": "completed"}));
    wait_completed(child_op)?;
    assert_eq!(list_ids(space)?, BTreeSet::from([child.clone()]));
    assert_eq!(list_ids("main")?, BTreeSet::from([parent.clone(), parent_extra.clone()]));
    assert_eq!(backup_stage_count(root)?, 0);
    ensure!(matches!(call_confirm(child_uuid, child_digest, child_target, &child_confirm_proof), Err(ureq::Error::Status(401, _))));

    let accepted_main = post("/v1/backups/import?space_id=main")
        .set("Content-Type", "application/octet-stream")
        .send(SplitArchive { rest: &main_wire, first: true })?;
    assert_eq!(accepted_main.status(), 202);
    let receipt_main: Value = accepted_main.into_json()?;
    assert_eq!(receipt_main["status"], "pending");
    let main_op = receipt_main["operation_id"].as_str().context("main restore operation_id missing")?;
    let main_uuid = uuid::Uuid::parse_str(main_op)?;
    assert_eq!(list_ids("main")?, BTreeSet::from([parent.clone(), parent_extra]));
    let main_confirm_proof = sign_confirm(main_uuid, main_digest, main_target)?;
    let confirmed_main = call_confirm(main_uuid, main_digest, main_target, &main_confirm_proof)?;
    assert_eq!(confirmed_main.status(), 200);
    wait_completed(main_op)?;
    assert_eq!(list_ids("main")?, BTreeSet::from([parent]));
    assert_eq!(list_ids(space)?, BTreeSet::from([child]));
    assert_eq!(backup_stage_count(root)?, 0);
    Ok(())
}

fn backup_stage_count(root: &Path) -> Result<usize> {
    let mut count = 0;
    for entry in fs::read_dir(root)? {
        let name = entry?.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(".backup-export-") || name.starts_with(".backup-archive-") {
            count += 1;
        }
    }
    Ok(count)
}

struct SplitArchive<'a> {
    rest: &'a [u8],
    first: bool,
}

impl Read for SplitArchive<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let limit = if std::mem::take(&mut self.first) { 3 } else { 8191 };
        let size = out.len().min(limit);
        self.rest.read(&mut out[..size])
    }
}
