use super::{body, exchange, router};
use crate::network::{Listener, Peer};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub(crate) struct LegacyBodyObserver(Arc<Mutex<Vec<(String, Vec<u8>)>>>);

impl LegacyBodyObserver {
    pub(crate) fn record(&self, path: &str, bytes: &[u8]) {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push((path.to_owned(), bytes.to_vec()));
    }
}

#[test]
fn tcp_aliases_collect_the_exact_large_utf8_frame_before_dispatch() -> anyhow::Result<()> {
    let fragment = "\u{6d4b}\u{8bd5}\u{1f4be} \"quoted\"\\literal\r\n";
    let tail = "final-sentinel:\"\u{5b8c}\u{6574}\u{1f9ea}\"\r\n";
    let content = format!(
        "{}\r\n{tail}",
        serde_json::json!({"body": {"nested": [{"example": fragment.repeat(4900)}]}})
    );
    // Rust string lengths are UTF-8 bytes, not Windows command-line units.
    assert!(content.len() > 100 * 1024);
    assert!(content.len() > content.chars().count());

    // A neutral method and deliberately invalid ID ensure no business dispatch,
    // even if another unit test has already created the request queue.
    let request_body = serde_json::json!({
        "v": crate::rpc_worker::PROTOCOL_V,
        "id": "invalid synthetic request id",
        "method": "runtime.status",
        "args": [content],
    })
    .to_string();
    assert!(request_body.len() > 100 * 1024);

    let observer = LegacyBodyObserver::default();
    let paths = ["/rpc", "/api/rpc"];
    let replies = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let bound = listener.local_addr()?;
            let app = router(bound).layer(axum::Extension(observer.clone()));
            let server = tokio::spawn(async move {
                axum::serve(
                    Listener(listener),
                    app.into_make_service_with_connect_info::<Peer>(),
                )
                .await
            });

            let replies = async {
                let mut replies = Vec::new();
                for path in paths {
                    let request = format!(
                        "POST {path} HTTP/1.1\r\nHost: {bound}\r\nOrigin: http://{bound}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{request_body}",
                        request_body.len()
                    );
                    replies.push(body(&exchange(bound, request).await?)?);
                }
                Ok::<_, anyhow::Error>(replies)
            }
            .await;
            server.abort();
            let _ = server.await;
            replies
        })?;

    assert_eq!(
        replies[0], replies[1],
        "aliases must defer and reject identically"
    );
    let (status, reply) = &replies[0];
    match *status {
        400 => assert_eq!(reply["error"], "runtime worker is not running"),
        200 => {
            assert_eq!(reply["ok"], false);
            assert_eq!(reply["code"], "invalid_request_id");
        }
        other => anyhow::bail!("unexpected legacy transport status {other}"),
    }

    let entries = observer.0.lock().unwrap_or_else(|error| error.into_inner());
    assert_eq!(
        entries.len(),
        paths.len(),
        "each alias must reach the dispatch boundary once"
    );
    for ((recorded_path, bytes), expected_path) in entries.iter().zip(paths) {
        assert_eq!(recorded_path, expected_path);
        anyhow::ensure!(
            bytes.as_slice() == request_body.as_bytes(),
            "{expected_path}: received {} bytes but the exact {}-byte frame did not match",
            bytes.len(),
            request_body.len()
        );
        anyhow::ensure!(
            serde_json::from_slice::<serde_json::Value>(bytes)?["args"][0]
                .as_str()
                .is_some_and(|s| s == content && s.ends_with(tail)),
            "{expected_path}: UTF-8 content or final sentinel did not roundtrip"
        );
    }
    eprintln!(
        "Verified both legacy aliases over loopback TCP: {} frame bytes, {} UTF-8 content bytes",
        request_body.len(),
        content.len()
    );
    Ok(())
}
