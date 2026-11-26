use super::*;
use std::{
    io::{self, Cursor},
    sync::atomic::{AtomicBool, Ordering},
};

pub(in crate::runtime_http::v1::operations) const TEST: &str =
    "runtime_http::v1::operations::model_download::tests::worker_publication_and_cancel_use_live_owner";

#[test]
fn worker_publication_and_cancel_use_live_owner() -> Result<()> {
    super::super::tests::production_router_fixture(TEST)
}

type GateHolder = (
    std::sync::mpsc::Sender<()>,
    std::thread::JoinHandle<Result<()>>,
);
static PUBLISH_GATE: std::sync::Mutex<Option<GateHolder>> = std::sync::Mutex::new(None);

static RELEASE_BODY: AtomicBool = AtomicBool::new(false);

// Exercise streaming and cancellation over a real, private HTTP connection.
fn slow_loopback(
    output: &mut std::fs::File,
    resource: &Resource,
    progress: &dyn Fn(u64) -> Result<()>,
) -> Result<()> {
    use std::net::{Ipv4Addr, TcpListener, TcpStream};
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let mut client = TcpStream::connect_timeout(&listener.local_addr()?, Duration::from_secs(2))?;
    let (mut server, _) = listener.accept()?;
    client.set_read_timeout(Some(Duration::from_secs(7)))?;
    server.set_write_timeout(Some(Duration::from_secs(2)))?;
    server.set_read_timeout(Some(Duration::from_secs(2)))?;
    let request = b"GET /fixture HTTP/1.0\r\n\r\n";
    let header = b"HTTP/1.0 200 OK\r\nContent-Length: 8192\r\n\r\n";
    client.write_all(request)?;
    std::thread::scope(|scope| -> Result<()> {
        let writer = scope.spawn(move || -> io::Result<()> {
            let mut received = [0; b"GET /fixture HTTP/1.0\r\n\r\n".len()];
            server.read_exact(&mut received)?;
            assert_eq!(&received, request);
            server.write_all(header)?;
            server.write_all(&[7; 1024])?;
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !RELEASE_BODY.load(Ordering::Acquire) {
                if std::time::Instant::now() >= deadline {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "cancel not received"));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            // The next read unblocks; copy_verified checks cancellation before publication.
            server.write_all(&[7; 7168])?;
            Ok(())
        });
        let mut received = [0; b"HTTP/1.0 200 OK\r\nContent-Length: 8192\r\n\r\n".len()];
        client.read_exact(&mut received)?;
        assert_eq!(&received, header);
        let result = copy_verified(client, output, resource, progress);
        writer
            .join()
            .map_err(|_| anyhow::anyhow!("fixture writer panicked"))??;
        result
    })
}

// Synthetic byte transport only; production always uses the HTTPS transfer function.
fn synthetic_transfer(
    _: &str,
    output: &mut std::fs::File,
    resource: &Resource,
    progress: &dyn Fn(u64) -> Result<()>,
) -> Result<()> {
    if resource.path.file_name().and_then(|n| n.to_str()) == Some("gate.onnx") {
        let (ready, started) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || -> Result<()> {
            let gate = crate::rpc_worker::shared_exclusive();
            let _held = gate.acquire(true);
            ready.send(())?;
            released.recv_timeout(Duration::from_secs(5))?;
            Ok(())
        });
        started.recv_timeout(Duration::from_secs(5))?;
        *PUBLISH_GATE
            .lock()
            .map_err(|_| anyhow::anyhow!("test gate poisoned"))? = Some((release, holder));
    }
    if resource.path.file_name().and_then(|n| n.to_str()) == Some("held.onnx") {
        loop {
            progress(0)?;
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    if resource.path.file_name().and_then(|n| n.to_str()) == Some("slow.onnx") {
        return slow_loopback(output, resource, progress);
    }
    if resource.path.file_name().and_then(|n| n.to_str()) == Some("corrupt.onnx") {
        return copy_verified(Cursor::new(b"invalid"), output, resource, progress);
    }
    copy_verified(
        Cursor::new(b"synthetic pinned artifact"),
        output,
        resource,
        progress,
    )
}

pub(in crate::runtime_http::v1::operations) async fn check_worker(
    app: &axum::Router,
) -> Result<()> {
    use super::super::tests::{send, BOUND, TOKEN};
    use axum::{
        body::{to_bytes, Body},
        http::Request as HttpRequest,
    };
    use serde_json::{json, Value};
    let bytes = b"synthetic pinned artifact";
    let root = respire_app::service::data_dir().join("synthetic-download");
    let accept = |resources| async move {
        let request = HttpRequest::builder()
            .uri("/v1/models/downloads")
            .header("Host", BOUND.to_string())
            .header("Authorization", format!("Bearer {TOKEN}"))
            .body(Body::empty())?;
        let (parts, _) = request.into_parts();
        let scope = transport::authorize(&parts, BOUND, BOUND)
            .await
            .map_err(|e| anyhow::anyhow!("authorize: {e:?}"))?;
        let reply = rest_dispatch::submit(scope, rest_dispatch::Access::Write, move |ctx| {
            launch(
                ctx.scope.clone(),
                resources,
                origins(None).map_err(|_| transport::invalid())?,
                synthetic_transfer,
            )
        })
        .await
        .map_err(|e| anyhow::anyhow!("launch: {e:?}"))?;
        assert_eq!(reply.status(), StatusCode::ACCEPTED);
        let receipt: Value = serde_json::from_slice(&to_bytes(reply.into_body(), 8192).await?)?;
        Ok::<_, anyhow::Error>(format!(
            "/v1/operations/{}",
            receipt["operation_id"].as_str().context("id missing")?
        ))
    };
    let wait = |path: String, state: &'static str| async move {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let reply = send(app, "GET", &path, Some(TOKEN), None).await?;
                assert_eq!(reply.status(), StatusCode::OK);
                let snapshot: Value =
                    serde_json::from_slice(&to_bytes(reply.into_body(), 8192).await?)?;
                let transferring = state == "transferring"
                    && snapshot["state"] == "running"
                    && snapshot["progress_percent"]
                        .as_u64()
                        .is_some_and(|value| value > 0 && value < 90);
                if snapshot["state"] == state || transferring {
                    return Ok::<_, anyhow::Error>(snapshot);
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await?
    };
    let target = resource(root.join("weights.onnx"), bytes);
    let complete = accept(vec![target.clone()]).await?;
    let snapshot = wait(complete.clone(), "completed").await?;
    assert_eq!(snapshot["progress_percent"], 100);
    assert_eq!(std::fs::read(&target.path)?, bytes);
    let events = send(app, "GET", &format!("{complete}/events"), Some(TOKEN), None).await?;
    let terminal = to_bytes(events.into_body(), 8192).await?;
    assert!(std::str::from_utf8(&terminal)?.contains("event: completed"));
    let mut gated = resource(root.join("gate.onnx"), bytes);
    gated.download = gated
        .download
        .map(|s| s.replace("weights.onnx", "gate.onnx"));
    let waiting = accept(vec![gated.clone()]).await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if PUBLISH_GATE
                .lock()
                .map_err(|_| anyhow::anyhow!("test gate poisoned"))?
                .is_some()
            {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await??;
    tokio::time::sleep(Duration::from_millis(25)).await;
    wait(waiting.clone(), "running").await?;
    assert!(
        !gated.path.exists(),
        "publication crossed a held foreground fence"
    );
    let (release, holder) = PUBLISH_GATE
        .lock()
        .map_err(|_| anyhow::anyhow!("test gate poisoned"))?
        .take()
        .context("test gate missing")?;
    release.send(())?;
    holder
        .join()
        .map_err(|_| anyhow::anyhow!("test gate holder panicked"))??;
    wait(waiting, "completed").await?;
    assert_eq!(std::fs::read(&gated.path)?, bytes);
    let mut corrupt = resource(root.join("corrupt.onnx"), bytes);
    corrupt.download = corrupt
        .download
        .map(|s| s.replace("weights.onnx", "corrupt.onnx"));
    let failed = accept(vec![corrupt.clone()]).await?;
    let snapshot = wait(failed, "failed").await?;
    assert_eq!(snapshot["error"]["code"], "model_mirror_fallback_exhausted");
    assert!(!corrupt.path.exists());
    let mut slow = resource(root.join("slow.onnx"), &[7; 8192]);
    slow.download = slow
        .download
        .map(|s| s.replace("weights.onnx", "slow.onnx"));
    let mut previous = None;
    for _ in 0..2 {
        RELEASE_BODY.store(false, Ordering::Release);
        let cancelled = accept(vec![slow.clone()]).await?;
        wait(cancelled.clone(), "transferring").await?;
        assert_eq!(
            send(
                app,
                "POST",
                "/v1/models/switch",
                Some(TOKEN),
                Some(json!({"model_id":"m3"}))
            )
            .await?
            .status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            send(app, "DELETE", &cancelled, None, None).await?.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            send(app, "DELETE", &cancelled, Some(TOKEN), None)
                .await?
                .status(),
            StatusCode::ACCEPTED
        );
        RELEASE_BODY.store(true, Ordering::Release);
        wait(cancelled.clone(), "cancelled").await?;
        assert!(!slow.path.exists());
        tokio::time::timeout(Duration::from_secs(5), async {
            while crate::rpc_serve::model_switch_running() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await?;
        assert_ne!(previous.as_ref(), Some(&cancelled));
        previous = Some(cancelled);
        let mut remaining = std::fs::read_dir(&root)?
            .map(|entry| entry.map(|file| file.path()))
            .collect::<io::Result<Vec<_>>>()?;
        remaining.sort();
        assert_eq!(remaining, [gated.path.clone(), target.path.clone()]);
        assert_eq!(std::fs::read(&target.path)?, bytes);
        assert_eq!(std::fs::read(&gated.path)?, bytes);
    }
    let mut held = resource(root.join("held.onnx"), bytes);
    held.download = held.download.map(|s| s.replace("weights.onnx", "held.onnx"));
    let revoked = accept(vec![held.clone()]).await?;
    wait(revoked, "running").await?;
    respire_app::auth::clear_runtime_session()?;
    tokio::time::timeout(Duration::from_secs(5), async {
        while crate::rpc_serve::model_switch_running() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await?;
    assert!(!held.path.exists());
    assert_eq!(
        std::fs::read_dir(&root)?.count(),
        2,
        "cancelled staging must be removed"
    );
    Ok(())
}

fn resource(path: PathBuf, bytes: &[u8]) -> Resource {
    Resource {
        path,
        size: bytes.len() as u64,
        sha256: format!("{:x}", Sha256::digest(bytes)),
        download: Some(format!(
            "https://huggingface.co/fixture/model/resolve/{}/weights.onnx",
            "a".repeat(40)
        )),
    }
}

#[test]
fn pinned_sources_and_staging_preserve_existing_assets() -> Result<()> {
    let _serial = crate::TEST_ENV_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("test lock poisoned"))?;
    let root = tempfile::tempdir()?;
    let bytes = b"synthetic pinned artifact";
    let target = resource(root.path().join("weights.onnx"), bytes);
    validate_resource(&target)?;
    use super::super::super::host_config::ModelNetwork;
    let configured = ModelNetwork::parse(&serde_json::json!({
        "model_mirror": "https://hf-mirror.net", "proxy_url": "http://127.0.0.1:3128"
    }))?;
    assert_eq!(configured.origins(None)?, vec!["https://hf-mirror.net"]);
    assert_eq!(configured.origins(Some("https://huggingface.co"))?, vec!["https://huggingface.co"]);
    configured.agent()?;
    for config in [
        serde_json::json!({"model_mirror": null}),
        serde_json::json!({"model_mirror": "https://untrusted.invalid"}),
        serde_json::json!({"proxy_url": 42}),
        serde_json::json!({"proxy_url": "http://user:secret@localhost:3128"}),
        serde_json::json!({"proxy_url": "http://localhost:3128/path"}),
        serde_json::json!({"proxy_url": "http://localhost:3128?token=secret"}),
        serde_json::json!({"proxy_url": "https://localhost:3128"}),
        serde_json::json!({"proxy_url": "http://[::1]:3128"}),
    ] {
        assert!(ModelNetwork::parse(&config).is_err());
    }
    assert_eq!(origins(None)?.len(), 3);
    assert_eq!(origins(Some("https://huggingface.co"))?.len(), 1);
    for mirror in [
        "",
        "http://127.0.0.1",
        "https://huggingface.co/",
        "https://huggingface.co@localhost",
        "https://other.invalid",
    ] {
        assert!(origins(Some(mirror)).is_err());
    }
    for source in ["https://other.invalid/model", "https://huggingface.co/a/b/resolve/main/weights.onnx",
        "https://huggingface.co/a/b/resolve/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/%2e%2e/weights.onnx"] {
        let mut invalid = target.clone();
        invalid.download = Some(source.into());
        assert!(validate_resource(&invalid).is_err());
    }
    assert!(!valid(&target, &|| Ok(()))?);
    let mut temp = tempfile::NamedTempFile::new_in(root.path())?;
    copy_verified(Cursor::new(bytes), temp.as_file_mut(), &target, &|_| Ok(()))?;
    temp.as_file().sync_all()?;
    assert!(!target.path.exists());
    temp.persist_noclobber(&target.path).map_err(|e| e.error)?;
    assert!(valid(&target, &|| Ok(()))?);
    let mut second = tempfile::NamedTempFile::new_in(root.path())?;
    second.write_all(b"other bytes")?;
    assert!(second.persist_noclobber(&target.path).is_err());
    assert_eq!(std::fs::read(&target.path)?, bytes);
    let stage_path;
    {
        let temp = tempfile::NamedTempFile::new_in(root.path())?;
        stage_path = temp.path().to_owned();
    }
    assert!(!stage_path.exists());
    assert!(check_path(&root.path().join("../escape")).is_err());
    #[cfg(unix)]
    {
        let linked = root.path().join("linked");
        std::os::unix::fs::symlink(&target.path, &linked)?;
        assert!(check_path(&linked).is_err());
    }
    Ok(())
}

#[test]
fn transfer_rejects_corruption_truncation_overflow_and_cancellation() -> Result<()> {
    let agent = super::super::super::host_config::ModelNetwork::parse(&serde_json::json!({}))?.agent()?;
    let error = response(&agent, "https://huggingface.co/unused", &|| {
        Err(model_progress::OperationStopped::Cancelled.into())
    })
    .err()
    .context("cancelled request connected")?;
    assert!(matches!(
        error.downcast_ref::<model_progress::OperationStopped>(),
        Some(model_progress::OperationStopped::Cancelled)
    ));
    let root = tempfile::tempdir()?;
    let bytes = b"verified";
    let target = resource(root.path().join("weights.onnx"), bytes);
    let mut output = Vec::new();
    copy_verified(Cursor::new(bytes), &mut output, &target, &|_| Ok(()))?;
    assert_eq!(output, bytes);
    for body in [&b"wrong!!!"[..], &b"short"[..], &b"verified-too-long"[..]] {
        let error = copy_verified(Cursor::new(body), &mut Vec::new(), &target, &|_| Ok(()))
            .err()
            .context("invalid transfer succeeded")?;
        assert!(error.is::<SourceFailure>());
    }
    let error = copy_verified(Cursor::new(bytes), &mut Vec::new(), &target, &|_| {
        Err(model_progress::OperationStopped::Cancelled.into())
    })
    .err()
    .context("cancel ignored")?;
    assert!(error.is::<model_progress::OperationStopped>());
    struct BrokenDisk;
    impl Write for BrokenDisk {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("synthetic disk failure"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let error = copy_verified(Cursor::new(bytes), &mut BrokenDisk, &target, &|_| Ok(()))
        .err()
        .context("disk failure ignored")?;
    assert!(error.is::<io::Error>());
    assert!(!error.is::<SourceFailure>());
    Ok(())
}

pub(in crate::runtime_http::v1::operations) async fn check_routes(
    app: &axum::Router,
) -> Result<()> {
    use super::super::tests::{send, TOKEN};
    use serde_json::json;
    let path = "/v1/models/downloads";
    assert_eq!(
        send(app, "POST", path, None, Some(json!({"model_id":"m3"})))
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    // Every authenticated request below must fail before any network access or publication.
    for body in [
        json!({"model_id":"unknown"}),
        json!({"model_id":"m3", "engine":"gpu"}),
        json!({"model_id":"m3", "mirror":"http://127.0.0.1"}),
        json!({"model_id":"m3", "destination":"outside"}),
    ] {
        assert_eq!(
            send(app, "POST", path, Some(TOKEN), Some(body))
                .await?
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    Ok(())
}
