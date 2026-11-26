use super::*;
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::{TcpListener, TcpStream}};

const PATH: &str = "/v1/backups/import?space_id=main";
const TCP_LIMIT: Duration = Duration::from_secs(60);

pub(super) async fn check(app: Router, root: &Path) -> Result<()> {
    {
        let db = rusqlite::Connection::open(service::database_path(root)?)?;
        db.busy_timeout(WAIT)?;
        db.execute_batch(
            "CREATE TABLE upload_fixture(payload BLOB NOT NULL);
             INSERT INTO upload_fixture VALUES (zeroblob(33554432));",
        )?;
    }
    let response = request(&app, "GET", EXPORT, Some(TOKEN)).await?;
    assert_eq!(response.status(), StatusCode::OK);
    let archive = tokio::time::timeout(TCP_LIMIT, to_bytes(response.into_body(), 64 * 1024 * 1024)).await??;
    assert!(archive.len() > 32 * 1024 * 1024);
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let bound = listener.local_addr()?;
    let base = crate::runtime_http::router(bound);
    assert_eq!(request(&app, "POST", PATH, Some(TOKEN)).await?.status(), StatusCode::NOT_FOUND);
    let app = base.route("/v1/backups/import", routing::post(
        move |ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>, request: Request<Body>| {
            crate::runtime_http::v1::backup_import::stage(bound, peer, request)
        },
    ));
    let task = tokio::spawn(async move {
        axum::serve(crate::network::Listener(listener), app.into_make_service_with_connect_info::<crate::network::Peer>()).await
    });
    let result = tokio::time::timeout(TCP_LIMIT, transfer_cases(bound, root, &archive)).await;
    task.abort();
    let _ = task.await;
    result?
}

async fn transfer_cases(bound: SocketAddr, root: &Path, archive: &[u8]) -> Result<()> {
    let mut complete = begin(bound, archive.len()).await?;
    complete.write_all(archive).await?;
    let mut response = Vec::new();
    complete.read_to_end(&mut response).await?;
    assert!(response.starts_with(b"HTTP/1.1 200 "));
    assert!(!has_stage(root, false)?);

    // Send a valid header and enough data for the worker to have completed
    // at least one write before the client drops the incomplete request.
    let mut interrupted = begin(bound, archive.len()).await?;
    interrupted.write_all(&archive[..256 * 1024]).await?;
    tokio::time::timeout(WAIT, async {
        loop {
            if has_stage(root, true)? {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await??;
    drop(interrupted);
    tokio::time::timeout(WAIT, async {
        loop {
            if !has_stage(root, false)? {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await??;
    let db = rusqlite::Connection::open(service::database_path(root)?)?;
    assert_eq!(db.query_row("SELECT length(payload) FROM upload_fixture", [], |row| row.get::<_, u64>(0))?, 32 * 1024 * 1024);
    Ok(())
}

async fn begin(bound: SocketAddr, bytes: usize) -> Result<TcpStream> {
    let mut stream = TcpStream::connect(bound).await?;
    stream.write_all(format!(
        "POST {PATH} HTTP/1.1\r\nHost: {bound}\r\nAuthorization: Bearer {TOKEN}\r\nContent-Type: application/octet-stream\r\nContent-Length: {bytes}\r\nConnection: close\r\n\r\n",
    ).as_bytes()).await?;
    Ok(stream)
}

fn has_stage(root: &Path, require_data: bool) -> Result<bool> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with(".backup-import-") {
            match entry.metadata() {
                Ok(metadata) if !require_data || metadata.len() > 0 => return Ok(true),
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(false)
}
