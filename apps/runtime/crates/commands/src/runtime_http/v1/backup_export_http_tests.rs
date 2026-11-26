use super::*;
use anyhow::{Context, Result};
use axum::{extract::ConnectInfo, routing, Router};
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
        let mut child = Command::new(std::env::current_exe()?)
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, "1")
            .env(FINISHED, &witness)
            .env("RSRS_CORE_TEST_MODE", "1")
            .env("RSRS_DATA_DIR", directory.path().canonicalize()?)
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
            let base = crate::runtime_http::router(bound);
            let app = base.merge(Router::new().route(EXPORT, routing::get(
                move |ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>,
                      request: Request| get(bound, peer, request),
            )));
            #[cfg(target_os = "linux")]
            let app = {
                let app = app.merge(super::super::operations::archive_claim_fixture(bound)).merge(Router::new().route("/v1/backups/import", routing::post(
                    move |ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>,
                          request: Request| super::super::operations::backup_import::post(
                              bound, peer, request,
                          ),
                )));
                app
            };
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
    let parent = save("main", "isolated parent fixture memory")?;
    let created = post("/v1/spaces").send_json(json!({"name": "isolated backup fixture"}))?;
    ensure!(created.status() == 201, "device space creation failed");
    let create: Value = created.into_json()?;
    let space = create["id"].as_str().context("fixture device space ID absent")?;
    let child = save(space, "isolated child fixture memory")?;

    let keys = respire_app::SessionKeys::from_urk(urk)?;
    for (space, expected) in [("main", parent), (space, child)] {
        let response = get(&format!("{EXPORT}?space_id={space}"))
            .set("Authorization", &format!("Bearer {TOKEN}")).call()?;
        assert_eq!(response.status(), 200);
        assert_eq!(response.header("content-type"), Some("application/octet-stream"));
        assert_eq!(response.header("cache-control"), Some("no-store"));
        let mut wire = Vec::new();
        response.into_reader().take(8 * 1024 * 1024 + 1).read_to_end(&mut wire)?;
        ensure!(wire.len() <= 8 * 1024 * 1024, "fixture archive exceeded its byte budget");
        #[cfg(target_os = "linux")]
        {
            let upload = |payload: &[u8]| post("/v1/backups/import?space_id=main")
                .set("Content-Type", "application/octet-stream").send_bytes(payload);
            let reject = |payload: &[u8]| -> Result<()> {
                ensure!(matches!(upload(payload), Err(ureq::Error::Status(400, _))), "invalid upload accepted");
                ensure!(backup_stage_count(root)? == 0, "rejected upload reached confirmation worker");
                Ok(())
            };
            if space == "main" {
                reject(b"invalid envelope")?;
                reject(&wire[..wire.len()-1])?;
                let mut bad_seal = wire.clone();
                let field = br#""sealed_manifest_digest":""#;
                let seal = bad_seal.windows(field.len()).position(|bytes| bytes == field)
                    .context("fixture seal missing")? + field.len();
                bad_seal[seal] = if bad_seal[seal] == b'A' { b'B' } else { b'A' };
                let header_size = u32::from_be_bytes(wire[8..12].try_into()?) as usize;
                reject(&bad_seal[..12 + header_size])?;
                {
                    use std::{io::Write, net::{Shutdown, TcpStream}};
                    let mut stream = TcpStream::connect(bound)?;
                    stream.set_read_timeout(Some(WAIT))?;
                    stream.set_write_timeout(Some(WAIT))?;
                    write!(stream, "POST /v1/backups/import HTTP/1.1\r\nHost: {bound}\r\nAuthorization: Bearer {TOKEN}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", wire.len())?;
                    stream.write_all(&wire[..wire.len()/2])?;
                    stream.shutdown(Shutdown::Write)?;
                    let mut reply = Vec::new();
                    stream.take(8192).read_to_end(&mut reply)?;
                    ensure!(!reply.starts_with(b"HTTP/1.1 202"), "aborted upload accepted");
                    ensure!(backup_stage_count(root)? == 0, "aborted upload reached worker");
                }
                let mut corrupt = wire.clone();
                *corrupt.last_mut().context("empty archive")? ^= 1;
                reject(&corrupt)?;
                let mut trailing = wire.clone();
                trailing.push(0);
                reject(&trailing)?;
                ensure!(wire.len() > 2 * 1024 * 1024, "fixture must cross the old buffer limit");
                // An unsized reader makes ureq send HTTP chunked framing. Split
                // even the magic across reads, then use odd-sized payload blocks.
                let accepted = post("/v1/backups/import?space_id=main")
                    .set("Content-Type", "application/octet-stream")
                    .send(SplitArchive { rest: &wire, first: true })?;
                assert_eq!(accepted.status(), 202);
                let receipt: Value = accepted.into_json()?;
                ensure!(matches!(upload(&wire), Err(ureq::Error::Status(503, _))),
                    "full pending custody admitted another upload");
                assert_eq!(backup_stage_count(root)?, 1);
                let id = receipt["operation_id"].as_str().context("missing operation")?;
                let uuid = uuid::Uuid::parse_str(id)?;
                assert_eq!(receipt, json!({"operation_id": id, "status": "pending"}));
                let status: Value = get(&format!("/v1/operations/{id}"))
                    .set("Authorization", &format!("Bearer {TOKEN}")).call()?.into_json()?;
                assert_eq!(status["state"], "pending");
                let claimed = claim_over_loopback(&agent, &base, &service::database_path(root)?.canonicalize()?, uuid, &wire)?;
                use respire_spawn::owner_credential::{ArchiveOperation,BackupArchiveHandoff,ReadyBackupArchive};
                let mut intent = [0; 32];
                hex::decode_to_slice(claimed["intent_sha256"].as_str().context("missing claimed intent")?, &mut intent)?;
                let handoff = BackupArchiveHandoff::from_approved_intent(ArchiveOperation::from_v4_bytes(*uuid.as_bytes())?, intent);
                let ready = ReadyBackupArchive::open(root, handoff)?;
                ready.consume(|input| {
                    let mut staged = Vec::new();
                    input.read_to_end(&mut staged)?;
                    assert_eq!(staged, wire);
                    Ok(())
                })?;
                handoff.discard(root)?;
            } else {
                reject(&wire)?;
                let accepted = post(&format!("/v1/backups/import?space_id={space}"))
                    .set("Content-Type", "application/octet-stream").send_bytes(&wire)?;
                assert_eq!(accepted.status(), 202);
                let receipt: Value = accepted.into_json()?;
                let id = receipt["operation_id"].as_str().context("missing child operation")?;
                assert_eq!(backup_stage_count(root)?, 1);
                let uuid = uuid::Uuid::parse_str(id)?;
                let parent = service::database_path(root)?.canonicalize()?;
                ensure!(claim_over_loopback(&agent, &base, &parent, uuid, &wire).is_err(), "child accepted parent claim");
                let child = service::main_data_dir().join(".respire-local-spaces-v1").join(space).join("onememory.db").canonicalize()?;
                let claimed = claim_over_loopback(&agent, &base, &child, uuid, &wire)?;
                use respire_spawn::owner_credential::{ArchiveOperation, BackupArchiveHandoff};
                let mut intent = [0; 32];
                hex::decode_to_slice(claimed["intent_sha256"].as_str().context("missing child intent")?, &mut intent)?;
                BackupArchiveHandoff::from_approved_intent(ArchiveOperation::from_v4_bytes(*uuid.as_bytes())?, intent).discard(root)?;
                assert_eq!(backup_stage_count(root)?, 0);
            }
        }
        let output = tempfile::NamedTempFile::new_in(root)?;
        open_to_stage(&keys, "backup-fixture", wire.as_slice(), output.as_file(), || Ok(()))?;
        let db = rusqlite::Connection::open(output.path())?;
        let mut rows = db.prepare("SELECT id FROM memories WHERE deleted=0")?;
        let ids = rows.query_map([], |row| row.get(0))?.collect::<rusqlite::Result<BTreeSet<String>>>()?;
        assert_eq!(ids, BTreeSet::from([expected]));
    }
    assert!(backup_stage_count(root)? == 0);
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

#[cfg(target_os = "linux")]
struct SplitArchive<'a> {
    rest: &'a [u8],
    first: bool,
}

#[cfg(target_os = "linux")]
impl Read for SplitArchive<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let limit = if std::mem::take(&mut self.first) { 3 } else { 8191 };
        let size = out.len().min(limit);
        self.rest.read(&mut out[..size])
    }
}

#[cfg(target_os="linux")]
fn claim_over_loopback(agent:&ureq::Agent, base:&str, target:&Path, id:uuid::Uuid, wire:&[u8]) -> Result<Value> {
    use respire_spawn::owner_credential::{archive_proof::{self,ArchiveClaim}, read_owner_credential};
    let health:Value=agent.get(&format!("{base}/api/health")).call()?.into_json()?;
    let instance=uuid::Uuid::parse_str(health["instance_id"].as_str().context("fixture instance")?)?;
    let claim=ArchiveClaim{query_only:false,operation:*id.as_bytes(), archive:Sha256::digest(wire).into(),
        target:Sha256::digest(target.to_str().context("canonical target")?.as_bytes()).into(),
        instance:*instance.as_bytes(),generation:health["generation"].as_u64().context("fixture generation")?};
    let record=read_owner_credential(&crate::rpc_worker::runtime_dir(), &instance.to_string())?;
    let proof=archive_proof::issue(record.as_bytes(),claim,super::super::owner_control::wall_ms().context("clock")?).context("fixture proof")?;
    let frame=json!({"schema_version":1,"operation_id":id.to_string(),"archive_sha256":hex::encode(claim.archive),"target_sha256":hex::encode(claim.target)});
    let receipt:Value=agent.post(&format!("{base}/api/runtime/private-backup/claim"))
        .set("x-respire-archive-proof",&proof).send_json(frame)?.into_json()?;
    assert_eq!(receipt["state"],"claimed");
    assert_eq!(receipt["operation_id"],id.to_string());
    Ok(receipt)
}
