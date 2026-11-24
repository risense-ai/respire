use super::router;
use crate::network::{Listener, Peer};
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn exchange(bound: SocketAddr, request: String) -> anyhow::Result<String> {
    let result = tokio::time::timeout(std::time::Duration::from_secs(4), async {
        let mut socket = tokio::net::TcpStream::connect(bound).await?;
        socket.write_all(request.as_bytes()).await?;
        let mut response = Vec::new();
        socket.read_to_end(&mut response).await?;
        Ok::<_, anyhow::Error>(String::from_utf8(response)?)
    })
    .await??;
    Ok(result)
}

fn post(path: &str, host: &str, origin: &str) -> String {
    format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nContent-Length: 1\r\nConnection: close\r\n\r\n{{"
    )
}

fn body(response: &str) -> anyhow::Result<(u16, serde_json::Value)> {
    let (header, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| anyhow::anyhow!("missing response body"))?;
    let status = header
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("missing status"))?
        .parse()?;
    Ok((status, serde_json::from_str(body)?))
}

#[test]
fn compatibility_alias_uses_same_deferred_and_admission_path() -> anyhow::Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let bound = listener.local_addr()?;
            let server = tokio::spawn(async move {
                axum::serve(
                    Listener(listener),
                    router(bound).into_make_service_with_connect_info::<Peer>(),
                )
                .await
            });
            let host = bound.to_string();
            let origin = format!("http://{bound}");
            for (host, origin, code, error) in [
                (host.as_str(), origin.as_str(), 400, None),
                (
                    "other.invalid",
                    origin.as_str(),
                    403,
                    Some("forbidden host"),
                ),
                (
                    host.as_str(),
                    "https://other.invalid",
                    403,
                    Some("forbidden origin"),
                ),
            ] {
                let actual = body(&exchange(bound, post("/rpc", host, origin)).await?)?;
                let previous = body(&exchange(bound, post("/api/rpc", host, origin)).await?)?;
                assert_eq!(actual, previous);
                assert_eq!(actual.0, code);
                if let Some(error) = error {
                    assert_eq!(actual.1["error"], error);
                }
            }

            let v1 = exchange(bound, post("/v1/unknown", &host, &origin)).await?;
            let (status, problem) = body(&v1)?;
            assert_eq!(status, 404);
            assert!(v1.contains("content-type: application/problem+json"));
            assert_eq!(problem["code"], "route_not_found");

            let denied = exchange(bound, post("/v1/unknown", "other.invalid", &origin)).await?;
            let (status, problem) = body(&denied)?;
            assert_eq!(status, 403);
            assert_eq!(problem["code"], "forbidden_host");
            let get = |path| {
                format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nConnection: close\r\n\r\n"
        )
            };
            let (_, old_health) = body(&exchange(bound, get("/api/health")).await?)?;
            let (status, health) = body(&exchange(bound, get("/v1/health")).await?)?;
            assert_eq!(status, 200);
            assert_eq!(old_health["server"], "respire");
            let instance = old_health["instance_id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing legacy instance"))?;
            assert!(uuid::Uuid::parse_str(instance).is_ok());
            let expected = match (
                old_health["startup_complete"].as_bool(),
                old_health["state"].as_str(),
            ) {
                (Some(false), _) => "starting",
                (Some(true), Some("ready")) => "ready",
                (Some(true), _) => "degraded",
                _ => anyhow::bail!("legacy health did not report startup state"),
            };
            assert_eq!(
                health,
                serde_json::json!({
                    "status": expected,
                    "instance_id": instance,
                    "capabilities": [],
                })
            );

            let (status, problem) =
                body(&exchange(bound, post("/v1/health", &host, &origin)).await?)?;
            assert_eq!(status, 405);
            assert_eq!(problem["code"], "method_not_allowed");
            let unmounted = format!(
            "POST /v1/auth/handshake HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nAuthorization: Bearer fixture\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
        );
        let (status, problem) = body(&exchange(bound, unmounted).await?)?;
        assert_eq!(status, 404);
        assert_eq!(problem["code"], "route_not_found");
        assert!(problem.get("access_token").is_none());

        let duplicate = format!(
            "POST /v1/auth/handshake HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nOrigin: https://other.invalid\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
        );
        let (status, problem) = body(&exchange(bound, duplicate).await?)?;
        assert_eq!(status, 403);
        assert_eq!(problem["code"], "forbidden_origin");
        server.abort();
            Ok(())
        })
}

mod large_alias;
pub(super) use large_alias::LegacyBodyObserver;
