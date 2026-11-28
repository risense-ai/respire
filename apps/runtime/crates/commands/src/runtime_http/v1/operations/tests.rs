use anyhow::{Context, Result};
use axum::{
    body::{to_bytes, Body},
    extract::ConnectInfo,
    http::{header, Request, StatusCode},
    response::Response,
    Router,
};
use futures_util::StreamExt;
use respire_app::{auth, service};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{fs, net::SocketAddr, process::Command, thread, time::Duration};
use tower::ServiceExt;

mod main_create_tests;
#[path = "sse_cursor_tests.rs"]
mod sse_cursor_tests;
use sse_cursor_tests::events;

#[path = "model_switch_tests.rs"]
mod model_switch_tests;
#[path = "route_fixture_helpers.rs"]
mod route_fixture_helpers;
use route_fixture_helpers::check_light_status_and_legacy_aliases;
#[path = "host_config_tests.rs"]
mod host_config_tests;
#[path = "space_members_tests.rs"]
mod space_members_tests;

const TEST: &str = "runtime_http::v1::operations::tests::scoped_sync_sse_on_production_router";
const DEEPEN_TEST: &str = "runtime_http::v1::operations::tests::scoped_deepen_on_production_router";
const SPACE_TEST: &str = "runtime_http::v1::operations::tests::device_local_space_patch_on_production_router";
const CHILD: &str = "RSRS_CARD_010_SSE_CHILD";
pub(super) const TOKEN: &str = "fixture-runtime-bearer";
const NEXT_TOKEN: &str = "fixture-rotated-bearer";
const WAIT: Duration = Duration::from_secs(5);
pub(super) const BOUND: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 32100);

#[test]
fn scoped_sync_sse_on_production_router() -> Result<()> {
    production_router_fixture(TEST)
}

#[test]
fn scoped_deepen_on_production_router() -> Result<()> {
    production_router_fixture(DEEPEN_TEST)
}

#[test]
fn device_local_space_patch_on_production_router() -> Result<()> {
    production_router_fixture(SPACE_TEST)
}

pub(super) fn production_router_fixture(test: &'static str) -> Result<()> {
    if std::env::var_os(CHILD).is_none() {
        let directory = tempfile::tempdir()?;
        let fixture_root = space_members_tests::fixture_root(directory.path(), matches!(test, TEST | SPACE_TEST))?;
        let fixture_root = route_fixture_helpers::owner_fixture_root(fixture_root, test == SPACE_TEST)?;
        if !matches!(test, TEST | SPACE_TEST) {
            let profile = fixture_root.join("accounts").join("outline-fixture");
            fs::create_dir_all(&profile)?;
            fs::write(fixture_root.join("client.json"), serde_json::to_vec(&json!({"data_dir": profile}))?)?;
        }
        let result = Command::new(std::env::current_exe()?)
            .args(["--exact", test, "--nocapture"])
            .env(CHILD, "1")
            .env("RSRS_CORE_TEST_MODE", "1")
            .env("RSRS_DATA_DIR", &fixture_root)
            .env_remove("RSRS_DEFAULT_DATA_DIR")
            .env_remove("RSRS_RPC_TOKEN")
            .env_remove("RSRS_ADDR")
            .env_remove("RSRS_TOKEN")
            .env_remove("RSRS_LANG")
            .output()?;
        anyhow::ensure!(
            result.status.success(),
            "SSE child failed\n{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr),
        );
        assert!(String::from_utf8_lossy(&result.stdout).contains("1 passed"));
        return Ok(());
    }

    let root = service::data_dir();
    let token = crate::net_rpc::token_path();
    fs::create_dir_all(token.parent().context("fixture token parent")?)?;
    fs::write(&token, TOKEN)?;
    let (password, urk, salt, wrapped, nonce) = auth::new_vault_v4()?;
    let session_data = space_members_tests::fixture_session(test, salt, nonce, wrapped);
    let session = auth::write_session_json(&session_data)?;
    let unlock = || -> Result<()> {
        auth::supply_runtime_session(respire_protocol::credentials::RuntimeUnlock {
            schema_version: respire_protocol::credentials::CREDENTIAL_SCHEMA_V,
            profile: root.canonicalize()?,
            session_sha256: format!("{:x}", Sha256::digest(fs::read(&session)?)),
            instance_id: "fixture".into(),
            generation: 1,
            super_password: Some(password.clone()),
            login_password: None,
            legacy_secret: None,
            provider_credentials: Vec::new(),
        })
    };
    unlock()?;
    let (stop, stopped) = std::sync::mpsc::channel::<()>();
    let (ready, started) = std::sync::mpsc::channel();
    let profile = root.clone();
    let dispatcher = thread::spawn(move || -> Result<()> {
        let lock = respire_app::lock::LibraryLock::acquire(&profile, WAIT)?;
        if test != TEST {
            respire_app::transport::local::LocalStore::open(&profile.join("onememory.db"))?;
        }
        let (store, _) = service::open_runtime_store(&lock)?;
        if test == DEEPEN_TEST {
            seed_deepen(&store, urk)?;
        }
        if test == model_switch_tests::TEST {
            model_switch_tests::seed(&store, urk)?;
        }
        let mut dispatch =
            crate::runtime_http::start_owner_dispatch().context("fixture dispatcher")?;
        ready.send(())?;
        while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
            stopped.recv_timeout(Duration::from_millis(5))
        {
            dispatch.tick(Some(&store), &lock);
        }
        if test == DEEPEN_TEST {
            let foreign = store.active_memory_exact("foreign-child")?.context("foreign fixture child")?;
            assert_eq!(foreign.local_parent_id, OUTLINE);
        }
        if test == model_switch_tests::TEST {
            model_switch_tests::check_store(&store)?;
        }
        Ok(())
    });
    started.recv_timeout(WAIT)?;
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let app = crate::runtime_http::router(BOUND);
            if matches!(test, space_members_tests::INVITE_TEST | main_create_tests::CREATE_TEST) {
                crate::rpc_serve::mark_http_fixture_ready();
                return match test { main_create_tests::CREATE_TEST => main_create_tests::check_main_create_abandonment(&app).await, _ => space_members_tests::loopback().await };
            }
            if test == SPACE_TEST {
                crate::rpc_serve::mark_http_fixture_ready();
                let pending = send(&app, "POST", "/v1/spaces/main/members", Some(TOKEN),
                    Some(json!({"identifier":"tablet","role":"viewer"}))).await?;
                assert_eq!(pending.status(), StatusCode::CONFLICT);
                return space_members_tests::device_space_tests::check(&app, &root, true).await;
            }
            if test == super::organize_split::tests::TEST {
                crate::rpc_serve::mark_http_fixture_ready();
                return super::organize_split::tests::check(&app, &token).await;
            }
            if test == DEEPEN_TEST || test == host_config_tests::TEST {
                crate::rpc_serve::mark_http_fixture_ready();
                if test == host_config_tests::TEST { return host_config_tests::check(&app).await; }
                space_members_tests::check(&app).await?;
                return check_deepen_routes(&app, &token).await;
            }
            if test == model_switch_tests::TEST {
                crate::rpc_serve::mark_http_fixture_ready();
                return model_switch_tests::check_routes(&app).await;
            }
            if test == super::model_download::tests::TEST {
                crate::rpc_serve::mark_http_fixture_ready();
                return super::model_download::tests::check_worker(&app).await;
            }
            let health = json_response(
                request(&app, "GET", "/v1/health", None).await?,
                StatusCode::OK,
            )
            .await?;
            assert_eq!(health["status"], "starting");
            assert_eq!(health["capabilities"], json!([]));
            let legacy_health = json_response(
                request(&app, "GET", "/api/health", None).await?,
                StatusCode::OK,
            )
            .await?;
            assert_eq!(legacy_health["startup_complete"], false);
            assert_eq!(legacy_health["instance_id"], health["instance_id"]);
            crate::rpc_serve::mark_http_fixture_ready();
            let health = json_response(
                request(&app, "GET", "/v1/health", None).await?,
                StatusCode::OK,
            )
            .await?;
            assert_eq!(health["status"], "ready");
            assert!(health["capabilities"]
                .as_array()
                .context("health capabilities")?
                .contains(&json!("sse.operations.v1")));
            check_resource_routes(&app).await?;
            check_light_status_and_legacy_aliases(&app).await?;
            check_organize_suggestions(&app).await?;

            let completed = trigger(&app, TOKEN).await?;
            let mut stream = events(&app, &completed, TOKEN)
                .await?
                .into_body()
                .into_data_stream();
            loop {
                let frame = tokio::time::timeout(WAIT, stream.next())
                    .await?
                    .context("stream ended before terminal failure")??;
                if std::str::from_utf8(&frame)?.contains("event: failed") {
                    assert!(std::str::from_utf8(&frame)?.contains("\"state\":\"failed\""));
                    break;
                }
            }
            assert!(tokio::time::timeout(WAIT, stream.next()).await?.is_none());
            let status = json_response(
                request(&app, "GET", &completed, Some(TOKEN)).await?,
                StatusCode::OK,
            )
            .await?;
            assert_eq!(status["state"], "failed");

            let _hold = crate::rpc_serve::hold_sync_fixture()?;
            let pending = trigger(&app, TOKEN).await?;
            let mut cancelled = events(&app, &pending, TOKEN)
                .await?
                .into_body()
                .into_data_stream();
            let progress = tokio::time::timeout(WAIT, cancelled.next())
                .await?
                .context("pending event")??;
            assert!(std::str::from_utf8(&progress)?.contains("\"state\":\"pending\""));
            let status = json_response(
                request(&app, "DELETE", &pending, Some(TOKEN)).await?,
                StatusCode::ACCEPTED,
            )
            .await?;
            assert_eq!(status["state"], "cancelled");
            let terminal = tokio::time::timeout(WAIT, cancelled.next())
                .await?
                .context("cancel event")??;
            sse_cursor_tests::check_reconnect(&app, &pending, &progress, &terminal, &status)
                .await?;
            assert!(tokio::time::timeout(WAIT, cancelled.next())
                .await?
                .is_none());

            let pending = trigger(&app, TOKEN).await?;
            let mut revoked = events(&app, &pending, TOKEN)
                .await?
                .into_body()
                .into_data_stream();
            assert!(tokio::time::timeout(WAIT, revoked.next())
                .await?
                .context("pending event")?
                .is_ok());
            fs::write(&token, NEXT_TOKEN)?;
            assert_eq!(request(&app, "GET", "/v1/spaces/main/members", Some(TOKEN)).await?.status(), StatusCode::UNAUTHORIZED);
            assert!(tokio::time::timeout(WAIT, revoked.next()).await?.is_none());
            assert_eq!(
                request(&app, "GET", &pending, Some(TOKEN)).await?.status(),
                StatusCode::UNAUTHORIZED
            );
            assert_eq!(
                request(&app, "GET", &pending, Some(NEXT_TOKEN))
                    .await?
                    .status(),
                StatusCode::NOT_FOUND
            );

            let pending = trigger(&app, NEXT_TOKEN).await?;
            let mut revoked = events(&app, &pending, NEXT_TOKEN)
                .await?
                .into_body()
                .into_data_stream();
            assert!(tokio::time::timeout(WAIT, revoked.next())
                .await?
                .context("pending event")?
                .is_ok());
            auth::clear_runtime_session()?;
            assert_eq!(request(&app, "GET", "/v1/spaces/main/members", Some(NEXT_TOKEN)).await?.status(), StatusCode::SERVICE_UNAVAILABLE);
            let account = request(&app, "GET", "/v1/host/account", Some(NEXT_TOKEN)).await?;
            assert_eq!(
                json_response(account, StatusCode::OK).await?,
                json!({"signed_in":true, "account_id":"sse-fixture", "transaction_state":"none"})
            );
            unlock()?;
            assert!(tokio::time::timeout(WAIT, revoked.next()).await?.is_none());
            assert_eq!(
                request(&app, "GET", &pending, Some(NEXT_TOKEN))
                    .await?
                    .status(),
                StatusCode::NOT_FOUND
            );

            let pending = trigger(&app, NEXT_TOKEN).await?;
            let mut revoked = events(&app, &pending, NEXT_TOKEN)
                .await?
                .into_body()
                .into_data_stream();
            assert!(tokio::time::timeout(WAIT, revoked.next())
                .await?
                .context("pending event")?
                .is_ok());
            crate::rpc_serve::advance_generation(crate::rpc_worker::sync_generation())?;
            assert!(tokio::time::timeout(WAIT, revoked.next()).await?.is_none());
            assert_eq!(
                request(&app, "GET", &pending, Some(NEXT_TOKEN))
                    .await?
                    .status(),
                StatusCode::NOT_FOUND
            );
            Ok(())
        });
    drop(stop);
    let joined = dispatcher
        .join()
        .map_err(|_| anyhow::anyhow!("fixture dispatcher panicked"));
    result.and_then(|()| joined?)
}

async fn request(app: &Router, method: &str, path: &str, token: Option<&str>) -> Result<Response> {
    send(app, method, path, token, None).await
}

pub(super) async fn send(
    app: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> Result<Response> {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, BOUND.to_string());
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let mut request = match body {
        Some(value) => request
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&value)?))?,
        None => request.body(Body::empty())?,
    };
    request
        .extensions_mut()
        .insert(ConnectInfo(crate::network::Peer(BOUND)));
    Ok(tokio::time::timeout(WAIT, app.clone().oneshot(request)).await??)
}

async fn json_response(response: Response, status: StatusCode) -> Result<Value> {
    assert_eq!(response.status(), status);
    Ok(serde_json::from_slice(
        &to_bytes(response.into_body(), 256 * 1024).await?,
    )?)
}

async fn check_resource_routes(app: &Router) -> Result<()> {
    space_members_tests::check(app).await?;
    host_config_tests::check(app).await?;
    super::model_download::tests::check_routes(app).await?;
    let response = request(app, "GET", "/v1/models/catalog", Some(TOKEN)).await?;
    let catalog = json_response(response, StatusCode::OK).await?;
    assert_eq!(catalog["active_model_id"], "granite-311m-r2");
    assert_eq!(
        request(app, "GET", "/v1/models/active-assets", Some(TOKEN))
            .await?
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );

    let recall = |token, request| send(app, "POST", "/v1/memories/recall", token, Some(request));
    let query = json!({"query":"orchid fixture keyword", "top_k":1});
    assert_eq!(recall(None, query.clone()).await?.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(recall(Some(TOKEN), json!({"query":"x","top_k":101})).await?.status(), StatusCode::BAD_REQUEST);
    assert_eq!(recall(Some(TOKEN), json!({"query":"x","space_id":"other"})).await?.status(), StatusCode::NOT_FOUND);
    assert_eq!(json_response(recall(Some(TOKEN), query.clone()).await?, StatusCode::OK).await?, json!({"hits":[]}));

    let content = format!("orchid fixture keyword {}", "\u{1f680}".repeat(26_000));
    let response = send(
        app,
        "POST",
        "/v1/memories",
        Some(TOKEN),
        Some(json!({"space_id":"main","title":"original","content":content})),
    )
    .await?;
    let created = json_response(response, StatusCode::CREATED).await?;
    let id = created["id"].as_str().context("memory ID")?;
    let path = format!("/v1/memories/{id}");
    let response = send(
        app,
        "PATCH",
        &path,
        Some(TOKEN),
        Some(json!({"title":"edited","tags":["rest"]})),
    )
    .await?;
    let edited = json_response(response, StatusCode::OK).await?;
    assert_eq!(edited["id"], id);
    assert_eq!(edited["content"], content);
    assert_eq!(edited["title"], "edited");
    assert_eq!(edited["tags"], json!(["rest"]));
    let stored = json_response(
        request(app, "GET", &path, Some(TOKEN)).await?,
        StatusCode::OK,
    )
    .await?;
    assert_eq!(stored, edited);
    let ranked = json_response(recall(Some(TOKEN), query.clone()).await?, StatusCode::OK).await?;
    assert_eq!(ranked["hits"][0]["memory"], edited);
    assert!(ranked["hits"][0]["score"].is_number());
    assert_eq!(
        request(app, "DELETE", &path, Some(TOKEN)).await?.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(app, "GET", &path, Some(TOKEN)).await?.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(json_response(recall(Some(TOKEN), query).await?, StatusCode::OK).await?, json!({"hits":[]}));

    let add_diary = |content| {
        send(
            app,
            "POST",
            "/v1/diary",
            Some(TOKEN),
            Some(json!({"date":"2026-01-02","content":content})),
        )
    };
    let first = json_response(add_diary("first  \n\n").await?, StatusCode::OK).await?;
    let second = json_response(add_diary("second").await?, StatusCode::OK).await?;
    assert_eq!(first["id"], second["id"]);
    assert_eq!(second["content"], "first  \n\nsecond");
    let listing = request(
        app,
        "GET",
        "/v1/diary?space_id=main&date=2026-01-02",
        Some(TOKEN),
    )
    .await?;
    assert_eq!(
        json_response(listing, StatusCode::OK).await?["entries"],
        json!([second])
    );
    Ok(())
}

async fn check_organize_suggestions(app: &Router) -> Result<()> {
    let path = "/v1/organize/suggestions";
    assert_eq!(
        request(app, "GET", path, None).await?.status(),
        StatusCode::UNAUTHORIZED
    );
    for (query, status) in [
        ("top=0", StatusCode::BAD_REQUEST),
        ("top=201", StatusCode::BAD_REQUEST),
        ("top=1&top=2", StatusCode::BAD_REQUEST),
        ("space_id=other", StatusCode::NOT_FOUND),
    ] {
        let response = request(app, "GET", &format!("{path}?{query}"), Some(TOKEN)).await?;
        assert_eq!(response.status(), status);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/problem+json");
    }
    assert_eq!(
        json_response(request(app, "GET", path, Some(TOKEN)).await?, StatusCode::OK).await?,
        json!({"suggestions": []})
    );

    let create = |body| send(app, "POST", "/v1/memories", Some(TOKEN), Some(body));
    let root = json_response(create(json!({"content": "Fixture tree root"})).await?, StatusCode::CREATED).await?;
    let id = root["id"].as_str().context("fixture root ID")?;
    let mut last_child = Value::Null;
    for index in 0..15 {
        last_child = json_response(
            create(json!({"content": format!("Fixture tree child {index}"), "parent_id": id})).await?,
            StatusCode::CREATED,
        ).await?;
    }
    let response = request(app, "GET", &format!("{path}?space_id=main&top=1"), Some(TOKEN)).await?;
    assert_eq!(
        json_response(response, StatusCode::OK).await?,
        json!({"suggestions": [{
            "kind": "deepen",
            "target_id": id,
            "summary": "Consider grouping the 15 direct children of this root."
        }]})
    );
    let child_id = last_child["id"].as_str().context("fixture child ID")?;
    let child_path = format!("/v1/memories/{child_id}");
    assert_eq!(
        json_response(request(app, "GET", &child_path, Some(TOKEN)).await?, StatusCode::OK).await?,
        last_child
    );
    assert_eq!(
        request(app, "DELETE", &child_path, Some(TOKEN)).await?.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        json_response(request(app, "GET", path, Some(TOKEN)).await?, StatusCode::OK).await?,
        json!({"suggestions": []})
    );
    Ok(())
}

const OUTLINE: &str = "fixture-outline-root";

fn seed_deepen(store: &respire_app::transport::local::LocalStore, urk: [u8; 32]) -> Result<()> {
    use respire_app::{memory::search::HashingEmbedder, MemoryEngine, MemoryTransport, SessionKeys};
    let keys = SessionKeys::from_urk(urk)?;
    let embedder = HashingEmbedder::default();
    let put = |id: &str, owner: &str, parent: &str, text: &str| -> Result<()> {
        let mut entry = super::super::writes::new_entry(owner.to_owned(), text.to_owned(), text.to_owned());
        entry.id = id.to_owned();
        entry.parent_id = parent.to_owned();
        assert!(store.put(&MemoryEngine::seal(&keys, &embedder, &entry, owner)?)?);
        Ok(())
    };
    put(OUTLINE, "sse-fixture", "", "Owner outline")?;
    for (cluster, text) in ["whales ocean sonar migration", "ledger invoice tax receipts"].into_iter().enumerate() {
        for index in 0..8 {
            put(&format!("group-{cluster}-{index}"), "sse-fixture", OUTLINE, text)?;
        }
    }
    put("foreign-root", "other-owner", "", "Private outline")?;
    put("foreign-child", "other-owner", OUTLINE, "whales ocean sonar migration")?;
    Ok(())
}

async fn check_deepen_routes(app: &Router, token_path: &std::path::Path) -> Result<()> {
    let plan_path = "/v1/organize/deepen/plan";
    let apply_path = "/v1/organize/deepen/apply";
    let post = |path, token, body| send(app, "POST", path, token, Some(body));
    let input = json!({"parent_id":OUTLINE,"space_id":"outline-fixture"});
    assert_eq!(post(plan_path, None, input.clone()).await?.status(), StatusCode::UNAUTHORIZED);
    let plan = |body| post(plan_path, Some(TOKEN), body);
    for id in ["foreign-root", "fixture-outline"] {
        assert_eq!(plan(json!({"parent_id":id})).await?.status(), StatusCode::NOT_FOUND);
    }
    assert_eq!(plan(json!({"auto":true,"space_id":"main"})).await?.status(), StatusCode::NOT_FOUND);
    assert_eq!(plan(json!({"parent_id":"group-0-0"})).await?.status(), StatusCode::CONFLICT);
    let scan = json_response(plan(json!({})).await?, StatusCode::OK).await?;
    assert_eq!(scan["moves"].as_array().context("default scan moves")?.len(), 16);
    let preview = json_response(plan(input.clone()).await?, StatusCode::OK).await?;
    assert_eq!(preview["moves"].as_array().context("preview moves")?.len(), 16);
    fs::write(token_path, NEXT_TOKEN)?;
    let apply = |preview: &Value| post(apply_path, Some(NEXT_TOKEN), json!({"plan_id":preview["plan_id"]}));
    assert_eq!(apply(&preview).await?.status(), StatusCode::NOT_FOUND);

    let stale = json_response(post(plan_path, Some(NEXT_TOKEN), input).await?, StatusCode::OK).await?;
    json_response(
        send(app, "POST", "/v1/diary", Some(NEXT_TOKEN), Some(json!({"date":"2026-01-02","content":"Changes revision"}))).await?,
        StatusCode::OK,
    ).await?;
    assert_eq!(apply(&stale).await?.status(), StatusCode::CONFLICT);

    let preview = json_response(post(plan_path, Some(NEXT_TOKEN), json!({"auto":true,"parent_id":"group-0-0"})).await?, StatusCode::OK).await?;
    let moves = preview["moves"].as_array().context("preview moves")?;
    assert_eq!(moves.len(), 16);
    let settings = service::data_dir().join("agent.json");
    fs::write(&settings, br#"{"readonly":true,"readonly_team":true}"#)?;
    assert_eq!(apply(&preview).await?.status(), StatusCode::FORBIDDEN);
    fs::write(settings, b"{}")?;
    let receipt = json_response(apply(&preview).await?, StatusCode::ACCEPTED).await?;
    let operation = format!("/v1/operations/{}", receipt["operation_id"].as_str().context("deepen operation ID")?);
    let response = events(app, &operation, NEXT_TOKEN).await?;
    let stream = tokio::time::timeout(WAIT, to_bytes(response.into_body(), 8192)).await??;
    assert!(std::str::from_utf8(&stream)?.contains("event: completed"));
    assert_eq!(apply(&preview).await?.status(), StatusCode::NOT_FOUND);

    let mut parents = std::collections::HashSet::new();
    for item in moves {
        let id = item["memory_id"].as_str().context("moved memory ID")?;
        assert!(id.starts_with("group-"));
        let child = request(app, "GET", &format!("/v1/memories/{id}"), Some(NEXT_TOKEN)).await?;
        assert_eq!(json_response(child, StatusCode::OK).await?["parent_id"], item["new_parent_id"]);
        parents.insert(item["new_parent_id"].as_str().context("new parent ID")?);
    }
    assert_eq!(parents.len(), 2);
    for parent in parents {
        let row = request(app, "GET", &format!("/v1/memories/{parent}"), Some(NEXT_TOKEN)).await?;
        assert_eq!(json_response(row, StatusCode::OK).await?["parent_id"], OUTLINE);
    }
    let status = request(app, "GET", "/v1/status/light", Some(NEXT_TOKEN)).await?;
    assert_eq!(json_response(status, StatusCode::OK).await?["indexed_count"], 19);
    Ok(())
}

async fn trigger(app: &Router, token: &str) -> Result<String> {
    let response = request(app, "POST", "/v1/sync/trigger", Some(token)).await?;
    let operation = json_response(response, StatusCode::ACCEPTED).await?;
    let id = operation["operation_id"].as_str().context("operation ID")?;
    uuid::Uuid::parse_str(id)?;
    Ok(format!("/v1/operations/{id}"))
}
