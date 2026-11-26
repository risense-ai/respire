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
            match root.to_str().and_then(|p| p.strip_prefix(r"\\?\")) {
                Some(local) => std::path::PathBuf::from(local),
                None => root,
            }
        };
        #[cfg(not(windows))]
        let data_dir = base_dir;
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
    auth::supply_runtime_session(respire_protocol::credentials::RuntimeUnlock {
        schema_version: respire_protocol::credentials::CREDENTIAL_SCHEMA_V,
        profile: root.clone(),
        session_sha256: format!("{:x}", Sha256::digest(fs::read(session)?)),
        instance_id: "fixture".into(),
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
            let handle = tokio::runtime::Handle::current();
            let result = tokio::task::spawn_blocking(move || verify_http(bound, &root, urk, handle)).await;
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

fn verify_http(bound: SocketAddr, root: &Path, urk: [u8; 32], handle: tokio::runtime::Handle) -> Result<()> {
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

    use super::super::operations::backup_import::{confirm_local_restore, LocalRestoreConfirmation};
    let main_db = service::database_path(root)?.canonicalize()?;
    let child_db = root
        .join(".respire-local-spaces-v1")
        .join(space)
        .join("onememory.db")
        .canonicalize()?;
    let child_digest: [u8; 32] = Sha256::digest(&child_wire).into();
    let main_digest: [u8; 32] = Sha256::digest(&main_wire).into();

    let accepted_child = post(&format!("/v1/backups/import?space_id={space}"))
        .set("Content-Type", "application/octet-stream")
        .send_bytes(&child_wire)?;
    assert_eq!(accepted_child.status(), 202);
    let receipt_child: Value = accepted_child.into_json()?;
    assert_eq!(receipt_child["status"], "pending");
    let child_op = receipt_child["operation_id"].as_str().context("child restore operation_id missing")?;
    let child_uuid = uuid::Uuid::parse_str(child_op)?;
    assert_eq!(backup_stage_count(root)?, 1);

    // 202 acceptance stages only; database writes must wait for explicit local confirmation.
    thread::sleep(Duration::from_millis(100));
    let pending_status: Value = get(&format!("/v1/operations/{child_op}"))
        .set("Authorization", &format!("Bearer {TOKEN}")).call()?.into_json()?;
    assert_eq!(pending_status["state"], "pending");
    assert_eq!(list_ids(space)?, BTreeSet::from([child.clone(), child_extra.clone()]));

    let mut wrong_digest = child_digest;
    wrong_digest[0] ^= 1;
    for bad in [
        LocalRestoreConfirmation { operation_id: uuid::Uuid::new_v4(), archive_sha256: child_digest, target: child_db.clone() },
        LocalRestoreConfirmation { operation_id: child_uuid, archive_sha256: wrong_digest, target: child_db.clone() },
        LocalRestoreConfirmation { operation_id: child_uuid, archive_sha256: child_digest, target: main_db.clone() },
    ] {
        assert!(handle.block_on(confirm_local_restore(bad)).is_err());
    }
    assert_eq!(list_ids(space)?, BTreeSet::from([child.clone(), child_extra]));
    assert_eq!(backup_stage_count(root)?, 1);

    // Bounded pending queue holds up to 2 pending imports and releases on operation cancel.
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
    let cancelled = agent.delete(&format!("{base}/v1/operations/{cancel_op}"))
        .set("Authorization", &format!("Bearer {TOKEN}")).call()?;
    assert_eq!(cancelled.status(), 202);
    let cancel_json: Value = cancelled.into_json()?;
    assert_eq!(cancel_json["state"], "cancelled");
    assert_eq!(backup_stage_count(root)?, 1);
    assert!(handle.block_on(confirm_local_restore(LocalRestoreConfirmation {
        operation_id: cancel_uuid, archive_sha256: main_digest, target: main_db.clone(),
    })).is_err());

    handle.block_on(confirm_local_restore(LocalRestoreConfirmation {
        operation_id: child_uuid, archive_sha256: child_digest, target: child_db.clone(),
    }))?;
    wait_completed(child_op)?;
    assert_eq!(list_ids(space)?, BTreeSet::from([child.clone()]));
    assert_eq!(list_ids("main")?, BTreeSet::from([parent.clone(), parent_extra.clone()]));
    assert_eq!(backup_stage_count(root)?, 0);
    assert!(handle.block_on(confirm_local_restore(LocalRestoreConfirmation {
        operation_id: child_uuid, archive_sha256: child_digest, target: child_db,
    })).is_err());

    let accepted_main = post("/v1/backups/import?space_id=main")
        .set("Content-Type", "application/octet-stream")
        .send(SplitArchive { rest: &main_wire, first: true })?;
    assert_eq!(accepted_main.status(), 202);
    let receipt_main: Value = accepted_main.into_json()?;
    assert_eq!(receipt_main["status"], "pending");
    let main_op = receipt_main["operation_id"].as_str().context("main restore operation_id missing")?;
    let main_uuid = uuid::Uuid::parse_str(main_op)?;
    assert_eq!(list_ids("main")?, BTreeSet::from([parent.clone(), parent_extra]));
    handle.block_on(confirm_local_restore(LocalRestoreConfirmation {
        operation_id: main_uuid, archive_sha256: main_digest, target: main_db,
    }))?;
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
