use anyhow::Result;
use respire_spawn::owner_credential::{MigrationCopy, MigrationSource};
use std::{fs, io::Read};

#[test]
fn pinned_export_stage_denies_rename_and_verifies_exact_reader() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let root = respire_spawn::owner_credential::LocalSpaceNamespace::make_fixture_installation(
        &directory.path().canonicalize()?,
    )?;
    let source = MigrationSource::open(&root, false)?;
    let leaf = ".backup-export-test.pending";
    let stage = MigrationCopy::create_for_export(&root, leaf)?;
    fs::write(stage.path(), b"synthetic export")?;
    let sentinel = root.join("independent");
    fs::write(&sentinel, b"keep")?;

    let directory_id = source.identity()?;
    let file_id = stage.source_identity(leaf)?;
    let length = fs::metadata(stage.path())?.len();
    let wrong_id = [file_id[0], file_id[1] ^ 1];
    let wrong_root = [directory_id[0], directory_id[1] ^ 1];
    assert!(fs::rename(stage.path(), root.join("renamed")).is_err());
    assert!(stage.open_verified_peer(leaf, wrong_root, file_id, length).is_err());
    assert!(stage.open_verified_peer(leaf, directory_id, wrong_id, length).is_err());
    assert!(stage.open_verified_peer(leaf, directory_id, file_id, length + 1).is_err());
    assert!(stage.open_verified_peer("independent", directory_id, file_id, length).is_err());

    let mut data = Vec::new();
    stage.open_verified_peer(leaf, directory_id, file_id, length)?.read_to_end(&mut data)?;
    assert_eq!(data, b"synthetic export");
    let link = root.join("additional-link");
    fs::hard_link(&sentinel, &link)?;
    assert!(source.file_identity("independent").is_err());
    fs::remove_file(link)?;
    drop(stage);
    assert!(!root.join(leaf).exists());
    assert_eq!(fs::read(sentinel)?, b"keep");
    Ok(())
}

pub(in crate::runtime_http::v1) const LOOPBACK: &str =
    "runtime_http::v1::operations::tests::route_fixture_helpers::backup_export::windows_tests::scoped_archive_over_loopback";

#[test]
fn scoped_archive_over_loopback() -> Result<()> {
    super::super::super::production_router_fixture(LOOPBACK)
}

pub(in crate::runtime_http::v1) async fn check_loopback(root: &std::path::Path, urk: [u8; 32]) -> Result<()> {
    use axum::{extract::ConnectInfo, routing, Router};
    use crate::network::{Listener, Peer};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let bound = listener.local_addr()?;
    let path = "/v1/backups/export";
    let app = crate::runtime_http::router(bound).merge(Router::new().route(path, routing::get(
        move |ConnectInfo(Peer(peer)): ConnectInfo<Peer>, request| super::get(bound, peer, request),
    )));
    let server = tokio::spawn(async move {
        axum::serve(Listener(listener), app.into_make_service_with_connect_info::<Peer>()).await
    });
    let root = root.to_owned();
    let request = tokio::task::spawn_blocking(move || -> Result<()> {
        let agent = ureq::AgentBuilder::new().try_proxy_from_env(false)
            .timeout(std::time::Duration::from_secs(10)).build();
        let url = format!("http://{bound}{path}");
        assert!(matches!(agent.get(&url).call(), Err(ureq::Error::Status(401, _))));
        let auth = format!("Bearer {}", super::super::super::TOKEN);
        let post = |path: &str| agent.post(&format!("http://{bound}{path}")).set("Authorization", &auth);
        let child: serde_json::Value = post("/v1/spaces")
            .send_json(serde_json::json!({"name": "Windows export fixture"}))?.into_json()?;
        let child = child["id"].as_str().ok_or_else(|| anyhow::anyhow!("missing fixture space"))?;
        let save = |space: &str| -> Result<String> {
            let saved: serde_json::Value = post("/v1/memories")
                .send_json(serde_json::json!({"space_id": space, "content": format!("fixture for {space}")}))?
                .into_json()?;
            Ok(saved["id"].as_str().ok_or_else(|| anyhow::anyhow!("missing fixture memory"))?.to_owned())
        };
        let memories = [("main", save("main")?), (child, save(child)?)];
        let keys = respire_app::SessionKeys::from_urk(urk)?;
        for (space, id) in memories {
            let response = agent.get(&format!("{url}?space_id={space}")).set("Authorization", &auth).call()?;
            assert_eq!(response.status(), 200);
            assert_eq!(response.header("content-type"), Some("application/octet-stream"));
            assert_eq!(response.header("cache-control"), Some("no-store"));
            let mut wire = Vec::new();
            response.into_reader().take(2 * 1024 * 1024 + 1).read_to_end(&mut wire)?;
            anyhow::ensure!(wire.len() <= 2 * 1024 * 1024, "fixture export exceeded its byte budget");
            let output = tempfile::NamedTempFile::new_in(&root)?;
            respire_app::snapshot::backup_archive::open_to_stage(
                &keys, "sse-fixture", wire.as_slice(), output.as_file(), || Ok(()),
            )?;
            let db = rusqlite::Connection::open(output.path())?;
            let mut rows = db.prepare("SELECT id FROM memories WHERE deleted=0")?;
            let found = rows.query_map([], |row| row.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?;
            assert_eq!(found, [id]);
        }
        for entry in fs::read_dir(root)? {
            assert!(!entry?.file_name().to_string_lossy().starts_with(".backup-export-"));
        }
        Ok(())
    }).await;
    server.abort();
    let _ = server.await;
    request?
}
