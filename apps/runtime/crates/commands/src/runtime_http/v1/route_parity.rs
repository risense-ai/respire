//! Test-only witness of calls made by the actual v1 router constructor.
use super::registration::RouteSpec;
use crate::network::{Listener, Peer};
use serde_json::{json, Value};
use std::{cell::RefCell, net::SocketAddr, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

thread_local! {
    static ACTIVE: RefCell<Option<Vec<RouteSpec>>> = const { RefCell::new(None) };
}

pub(super) fn record(spec: RouteSpec) {
    ACTIVE.with(|active| {
        if let Some(found) = active.borrow_mut().as_mut() {
            found.push(spec);
        }
    });
}

#[test]
fn export_registered_routes() -> anyhow::Result<()> {
    super::operations::check_receipts()?;
    let bound = SocketAddr::from(([127, 0, 0, 1], 32100));
    ACTIVE.with(|active| {
        assert!(active.replace(Some(Vec::new())).is_none());
        let _actual_router = super::router(bound);
        let actual = active
            .replace(None)
            .ok_or_else(|| anyhow::anyhow!("capture missing"))?;
        for (method, path, operation_id) in [
            ("GET", "/v1/operations/{operationId}", "getOperation"),
            ("DELETE", "/v1/operations/{operationId}", "cancelOperation"),
            ("GET", "/v1/operations/{operationId}/events", "streamOperationEvents"),
            ("POST", "/v1/sync/trigger", "triggerSync"),
            ("POST", "/v1/memories/recall", "recallMemories"),
            ("GET", "/v1/organize/suggestions", "getOrganizeSuggestions"),
            ("POST", "/v1/organize/split", "splitOrganizeNode"),
            ("GET", "/v1/backups/export", "exportBackup"),
            ("POST", "/v1/backups/import", "importBackup"),
        ] {
            assert!(actual.iter().any(|spec| {
                spec.method.as_str() == method && spec.path == path && spec.operation_id == operation_id
            }), "missing {method} {path}");
        }
        let rows: Vec<_> = actual
            .iter()
            .map(|spec| {
                json!({
                    "method": spec.method.as_str(),
                    "path": spec.path,
                    "operation_id": spec.operation_id,
                })
            })
            .collect();
        assert!(!rows.is_empty(), "no registrations observed");
        println!("\nRESPIRE_V1_ROUTES_JSON={}", serde_json::to_string(&rows)?);
        Ok(())
    })
}

async fn read_fixture(
    bound: SocketAddr,
    method: &str,
    path: &str,
) -> anyhow::Result<(u16, String, Value)> {
    tokio::time::timeout(Duration::from_secs(4), async {
        let mut client = tokio::net::TcpStream::connect(bound).await?;
        let request =
            format!("{method} {path} HTTP/1.1\r\nHost: {bound}\r\nConnection: close\r\n\r\n");
        client.write_all(request.as_bytes()).await?;
        let mut wire = Vec::new();
        client.read_to_end(&mut wire).await?;
        let wire = std::str::from_utf8(&wire)?;
        let (head, body) = wire
            .split_once("\r\n\r\n")
            .ok_or_else(|| anyhow::anyhow!("missing HTTP head"))?;
        let mut lines = head.lines();
        let status = lines
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .ok_or_else(|| anyhow::anyhow!("missing HTTP status"))?
            .parse()?;
        let media = lines
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            .map(|(_, value)| value.trim().to_owned())
            .ok_or_else(|| anyhow::anyhow!("missing HTTP media type"))?;
        Ok::<_, anyhow::Error>((status, media, serde_json::from_str(body)?))
    })
    .await?
}

fn check_problem(result: (u16, String, Value), expected: u16, code: &str) {
    let (status, media, problem) = result;
    assert_eq!(status, expected);
    assert_eq!(media, "application/problem+json");
    assert_eq!(problem["status"], expected);
    assert_eq!(problem["type"], "about:blank");
    assert_eq!(problem["code"], code);
    for field in ["title", "detail"] {
        assert!(problem[field]
            .as_str()
            .is_some_and(|value| !value.is_empty()));
    }
    assert!(!problem.to_string().contains("parity-fixture-never-reflect"));
}

#[test]
fn real_router_health_and_problem_wire_shapes() -> anyhow::Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let bound = listener.local_addr()?;
            let server = tokio::spawn(async move {
                axum::serve(
                    Listener(listener),
                    crate::runtime_http::router(bound)
                        .into_make_service_with_connect_info::<Peer>(),
                )
                .await
            });
            let (status, media, health) = read_fixture(bound, "GET", "/v1/health").await?;
            assert_eq!(status, 200);
            assert_eq!(media, "application/json");
            assert!(matches!(
                health["status"].as_str(),
                Some("starting" | "ready" | "degraded")
            ));
            assert!(health["capabilities"].is_array());
            assert_eq!(health["capabilities"], json!([]));
            assert!(health["instance_id"]
                .as_str()
                .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok()));
            check_problem(
                read_fixture(bound, "POST", "/v1/health").await?,
                405,
                "method_not_allowed",
            );
            check_problem(
                read_fixture(bound, "GET", "/v1/parity-fixture-never-reflect").await?,
                404,
                "route_not_found",
            );
            check_problem(
                read_fixture(bound, "DELETE", "/v1/memories/fixture-memory").await?,
                401,
                "unauthorized",
            );
            check_problem(
                read_fixture(bound, "POST", "/v1/memories/fixture-memory").await?,
                405,
                "method_not_allowed",
            );
            for (method, path) in [
                ("GET", "/v1/operations/fixture-id"),
                ("DELETE", "/v1/operations/fixture-id"),
                ("GET", "/v1/operations/fixture-id/events"),
                ("POST", "/v1/sync/trigger"),
                ("GET", "/v1/models/catalog"),
            ] {
                check_problem(read_fixture(bound, method, path).await?, 401, "unauthorized");
            }
            server.abort();
            Ok(())
        })
}

#[test]
fn health_catalog_flag_tracks_ready_verified_dispatch_without_broad_claims() {
    use super::health_capabilities;
    assert!(health_capabilities("starting", true, true).is_empty());
    assert!(health_capabilities("degraded", true, true).is_empty());
    assert!(health_capabilities("ready", false, false).is_empty());
    assert_eq!(
        health_capabilities("ready", true, false),
        ["auth.handshake.v1"]
    );
    assert_eq!(
        health_capabilities("ready", false, true),
        ["openapi.v1", "backups.v1", "sse.operations.v1", "model.catalog.v2"]
    );
    let ready = health_capabilities("ready", true, true);
    assert_eq!(
        ready,
        ["openapi.v1", "backups.v1", "sse.operations.v1", "model.catalog.v2", "auth.handshake.v1"]
    );
    for withheld in [
        "host.transactions.v1",
        "model.active_assets",
        "model.managed_download",
    ] {
        assert!(!ready.contains(&withheld));
    }
}
