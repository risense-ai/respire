use super::*;
use crate::runtime_http::v1::{rest_dispatch, transport};
use axum::response::IntoResponse;
use respire_app::auth::DeviceSpaceRetirement;
use std::path::PathBuf;

pub(super) async fn fill_quarantine(app: &Router, root: &Path, target: &str) -> Result<Vec<PathBuf>> {
    let storage = LocalSpaceNamespace::open_existing(root)?.context("native area missing")?;
    let mut untouched = Vec::new();
    for number in 0..16 {
        let slot = storage.make_fixture_retirement_slot(number)?;
        let file = slot.join("outside-known-cleanup-set.bin");
        fs::write(&file, b"unrecognized fixture data")?;
        untouched.push(file);
    }
    let reply = send(app, "DELETE", target, Some(TOKEN), None).await?;
    let problem = json_response(reply, StatusCode::CONFLICT).await?;
    assert_eq!(problem["code"], "space_retirement_quarantine_full");
    assert_eq!(request(app, "GET", target, Some(TOKEN)).await?.status(), StatusCode::OK);
    let empty = untouched.remove(0);
    // Empty owner-only slot simulates interrupted cleanup, which must be reclaimed.
    fs::remove_file(empty)?;
    Ok(untouched)
}

pub(super) async fn post_detach_error_is_uncertain(app: &Router) -> Result<()> {
    let created = create(app, Some(TOKEN), json!({"name":"post-detach outcome fixture"})).await?;
    let info = json_response(created, StatusCode::CREATED).await?;
    let raw = info["id"].as_str().context("sacrificial fixture ID")?.to_owned();
    let target = raw.clone();
    let parts = Request::get(BASE)
        .header(header::HOST, BOUND.to_string())
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(())?.into_parts().0;
    let scope = transport::authorize(&parts, BOUND, BOUND).await
        .map_err(|failure| anyhow::anyhow!("fixture authorization: {failure:?}"))?;
    let result = rest_dispatch::submit_device_space_delete::<(), _>(scope, move |ctx, admission| {
        let outcome = ctx.library.delete_device_space(ctx.ownership, &raw, || {
            ctx.may_commit() && admission.admit()
        }).map_err(|_| transport::unavailable())?;
        assert!(matches!(outcome, DeviceSpaceRetirement::Retired));
        // Inject a publication/storage error after the actual native detachment.
        // This is the same error boundary used when a post-rename Unix fsync fails.
        Err(transport::unavailable())
    }).await;
    let response = result.err().context("missing uncertain outcome")?.into_response();
    let problem = json_response(response, StatusCode::CONFLICT).await?;
    assert_eq!(problem["code"], "space_delete_outcome_unknown");
    assert_eq!(problem["retryable"], false);
    expect_missing(app, &target).await?;
    Ok(())
}
