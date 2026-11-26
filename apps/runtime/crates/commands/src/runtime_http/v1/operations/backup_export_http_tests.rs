use super::*;
use axum::routing;
use respire_app::snapshot::backup_archive::open_to_stage;
use std::{collections::BTreeSet, path::Path};

#[path = "backup_import_http_tests.rs"]
mod import;

const EXPORT: &str = "/v1/backups/export";

#[test]
fn native_backup_stream_fixture() -> Result<()> {
    production_router_fixture(BACKUP_TEST)
}

async fn save(app: &Router, space: &str, content: &str) -> Result<String> {
    let response = send(app, "POST", "/v1/memories", Some(TOKEN), Some(json!({
        "space_id": space, "content": content,
    }))).await?;
    let result = json_response(response, StatusCode::CREATED).await?;
    Ok(result["id"].as_str().context("native memory ID missing")?.to_owned())
}

async fn read_archive(
    app: &Router,
    path: &str,
    root: &Path,
    keys: &respire_app::SessionKeys,
) -> Result<BTreeSet<String>> {
    let response = request(app, "GET", path, Some(TOKEN)).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/octet-stream");
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let bytes = tokio::time::timeout(WAIT, to_bytes(response.into_body(), 1024 * 1024)).await??;
    let db_path = root.join(format!("fixture-{}.db", uuid::Uuid::new_v4().simple()));
    let mut out = fs::File::create(&db_path)?;
    open_to_stage(keys, "sse-fixture", bytes.as_ref(), &mut out, || Ok(()))?;
    drop(out);
    let db = rusqlite::Connection::open(&db_path)?;
    let mut stmt = db.prepare("SELECT id FROM memories WHERE deleted=0")?;
    let rows = stmt.query_map([], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub(super) async fn check(base: Router, root: &Path, token: &Path, urk: [u8; 32]) -> Result<()> {
    // The binding is fixture-only. All authority, memory, SQLite, and native
    // staging functions below are the real Runtime implementations.
    assert_eq!(request(&base, "GET", EXPORT, Some(TOKEN)).await?.status(), StatusCode::NOT_FOUND);
    let app = base.route(EXPORT, routing::get(
        |ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>, request: Request<Body>| {
            crate::runtime_http::v1::backup_export::get(BOUND, peer, request)
        },
    ));
    assert_eq!(request(&app, "GET", EXPORT, None).await?.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(request(&app, "GET", &format!("{EXPORT}?path=unsupported"), Some(TOKEN)).await?.status(), StatusCode::BAD_REQUEST);
    assert_eq!(request(&app, "GET", &format!("{EXPORT}?space_id=unknown"), Some(TOKEN)).await?.status(), StatusCode::NOT_FOUND);

    let parent = save(&app, "main", "parent backup fixture marker").await?;
    let created = send(&app, "POST", "/v1/spaces", Some(TOKEN), Some(json!({"name":"backup child fixture"}))).await?;
    let space = json_response(created, StatusCode::CREATED).await?;
    let space_id = space["id"].as_str().context("device-local space ID missing")?;
    let child = save(&app, space_id, "child backup fixture marker").await?;
    let endpoint = format!("{EXPORT}?space_id={space_id}");
    let keys = respire_app::SessionKeys::from_urk(urk)?;
    assert_eq!(read_archive(&app, EXPORT, root, &keys).await?, BTreeSet::from([parent.clone()]));
    assert_eq!(read_archive(&app, &endpoint, root, &keys).await?, BTreeSet::from([child.clone()]));

    let child_route = format!("/v1/spaces/{space_id}");
    assert_eq!(send(&app, "PATCH", &child_route, Some(TOKEN), Some(json!({"active":true}))).await?.status(), StatusCode::OK);
    assert_eq!(read_archive(&app, EXPORT, root, &keys).await?, BTreeSet::from([child]));
    assert_eq!(read_archive(&app, &format!("{EXPORT}?space_id=main"), root, &keys).await?, BTreeSet::from([parent]));
    assert_eq!(send(&app, "PATCH", "/v1/spaces/main", Some(TOKEN), Some(json!({"active":true}))).await?.status(), StatusCode::OK);

    let mut revoked = request(&app, "GET", &endpoint, Some(TOKEN)).await?.into_body().into_data_stream();
    assert!(tokio::time::timeout(WAIT, revoked.next()).await?.context("archive header missing")??.starts_with(b"RSRSBKP"));
    fs::write(token, NEXT_TOKEN)?;
    let frame = tokio::time::timeout(WAIT, revoked.next()).await?.context("revoked stream falsely ended cleanly")?;
    assert!(frame.is_err());
    assert!(tokio::time::timeout(WAIT, revoked.next()).await?.is_none());
    fs::write(token, TOKEN)?;

    let mut retired = request(&app, "GET", &endpoint, Some(TOKEN)).await?.into_body().into_data_stream();
    assert!(tokio::time::timeout(WAIT, retired.next()).await?.context("archive header missing")??.starts_with(b"RSRSBKP"));
    assert_eq!(account_stage_count(root)?, 1, "child output belongs to the verified owner root");
    assert_eq!(send(&app, "DELETE", &child_route, Some(TOKEN), None).await?.status(), StatusCode::NO_CONTENT);
    let frame = tokio::time::timeout(WAIT, retired.next()).await?.context("retired-space stream falsely ended cleanly")?;
    assert!(frame.is_err());
    assert!(tokio::time::timeout(WAIT, retired.next()).await?.is_none());
    assert_eq!(request(&app, "GET", &endpoint, Some(TOKEN)).await?.status(), StatusCode::NOT_FOUND);
    assert_eq!(account_stage_count(root)?, 0, "consumer release cleans only its private output");
    import::check(app, root).await
}

fn account_stage_count(profile: &Path) -> Result<usize> {
    let mut count = 0;
    for entry in fs::read_dir(profile)? {
        if entry?.file_name().to_string_lossy().starts_with(".backup-export-") {
            count += 1;
        }
    }
    Ok(count)
}
