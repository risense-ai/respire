//! Main-space create admission through the scoped production HTTP router.
use super::*;
use std::future::Future;

pub(super) const CREATE_TEST: &str = "runtime_http::v1::operations::tests::main_create_tests::main_create_abandonment_on_production_router";
#[test]
fn main_create_abandonment_on_production_router() -> Result<()> {
    production_router_fixture(CREATE_TEST)
}

pub(super) async fn check_main_create_abandonment(app: &axum::Router) -> Result<()> {
    let (entered, wait_entered) = std::sync::mpsc::sync_channel(1);
    let (resume, wait_resume) = std::sync::mpsc::channel();
    crate::runtime_http::v1::writes::create_test_admission::install((entered, wait_resume))?;
    let post = send(app, "POST", "/v1/memories", Some(TOKEN),
        Some(json!({"title":"abandoned","content":"abandoned marker"})));
    let entered = tokio::task::spawn_blocking(move || wait_entered.recv_timeout(WAIT));
    let (response, entered) = tokio::join!(post, entered);
    resume.send(())?;
    entered??;
    let failed = json_response(response?, StatusCode::SERVICE_UNAVAILABLE).await?;
    assert_eq!(failed["code"], "runtime_unavailable");

    // A successful queued read after dispatcher release is ordered after the
    // original closure. This is not a replay of the original POST.
    let list = json_response(request(app, "GET", "/v1/memories", Some(TOKEN)).await?, StatusCode::OK).await?;
    assert_eq!(list["items"], json!([]));
    let created = json_response(send(app, "POST", "/v1/memories", Some(TOKEN),
        Some(json!({"title":"committed","content":"independent positive marker"}))).await?,
        StatusCode::CREATED).await?;
    let id = created["id"].as_str().context("created entry has no ID")?;
    let loaded = json_response(request(app, "GET", &format!("/v1/memories/{id}"), Some(TOKEN)).await?, StatusCode::OK).await?;
    assert_eq!(loaded["content"], "independent positive marker");
    check_writer_deferral(app).await
}

async fn wait_for_deferral(post: &mut (impl Future<Output = Result<Response>> + Unpin)) -> Result<()> {
    let (notify, observed) = tokio::sync::oneshot::channel();
    crate::runtime_http::v1::writes::create_test_admission::watch_deferral(notify)?;
    tokio::select! {
        response = post => anyhow::bail!("create finished while writer held: {}", response?.status()),
        deferred = tokio::time::timeout(WAIT, observed) => { deferred??; Ok(()) }
    }
}

async fn check_writer_deferral(app: &axum::Router) -> Result<()> {
    let gate = crate::rpc_worker::shared_exclusive();
    let post = |marker| send(app, "POST", "/v1/memories", Some(TOKEN), Some(json!({"content": marker})));

    let held = gate.acquire(false);
    let mut pending = Box::pin(post("writer released"));
    wait_for_deferral(&mut pending).await?;
    let health = tokio::time::timeout(WAIT, request(app, "GET", "/v1/health", None)).await??;
    assert_eq!(health.status(), StatusCode::OK);
    drop(held);
    let published = json_response(pending.await?, StatusCode::CREATED).await?;

    let held = gate.acquire(false);
    let mut expired = Box::pin(post("writer deadline expired"));
    wait_for_deferral(&mut expired).await?;
    let rejected = json_response(tokio::time::timeout(WAIT, expired).await??,
        StatusCode::SERVICE_UNAVAILABLE).await?;
    assert_eq!(rejected["code"], "runtime_unavailable");
    drop(held);

    let held = gate.acquire(false);
    let mut cancelled = Box::pin(post("caller disconnected while deferred"));
    wait_for_deferral(&mut cancelled).await?;
    drop(cancelled);
    drop(held);

    let list = json_response(request(app, "GET", "/v1/memories", Some(TOKEN)).await?, StatusCode::OK).await?;
    let rows = list["items"].as_array().context("memory list is not an array")?;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|row| row["id"] == published["id"]));
    Ok(())
}
