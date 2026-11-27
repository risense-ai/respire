//! Main-space create admission through the scoped production HTTP router.
use super::*;

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
    Ok(())
}
