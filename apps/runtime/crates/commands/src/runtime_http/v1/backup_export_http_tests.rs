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
        response.into_reader().take(2 * 1024 * 1024 + 1).read_to_end(&mut wire)?;
        ensure!(wire.len() <= 2 * 1024 * 1024, "fixture archive exceeded its byte budget");
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
        if entry?.file_name().to_string_lossy().starts_with(".backup-export-") {
            count += 1;
        }
    }
    Ok(count)
}
