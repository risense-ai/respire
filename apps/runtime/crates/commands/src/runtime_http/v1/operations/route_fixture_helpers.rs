use super::*;

pub(super) async fn check_light_status_and_legacy_aliases(app: &Router) -> Result<()> {
    let status = "/v1/status/light?space_id=main";
    assert_eq!(
        request(app, "GET", status, None).await?.status(),
        StatusCode::UNAUTHORIZED
    );
    let status = json_response(request(app, "GET", status, Some(TOKEN)).await?, StatusCode::OK).await?;
    assert_eq!(status["space_id"], "main");
    assert_eq!(status["memory_count"], 1);
    assert_eq!(status["diary_count"], 1);
    assert_eq!(status["indexed_count"], 0);
    assert!(status["last_updated_at"].is_string());

    let invalid_rpc = |path| send(app, "POST", path, None, Some(json!({})));
    let legacy = json_response(invalid_rpc("/rpc").await?, StatusCode::BAD_REQUEST).await?;
    assert!(legacy["error"].is_string());
    assert_eq!(
        json_response(invalid_rpc("/api/rpc").await?, StatusCode::BAD_REQUEST).await?,
        legacy
    );
    Ok(())
}


pub(super) fn owner_fixture_root(path: std::path::PathBuf, owner: bool) -> Result<std::path::PathBuf> {
    #[cfg(windows)]
    if owner {
        respire_spawn::owner_credential::LocalSpaceNamespace::make_fixture_runtime_parent(&path)?;
        // Keep the temporary local drive in the same form as the normal
        // native owner runtime environment without changing admission.
        if let Some(local) = path.to_str().and_then(|path| path.strip_prefix(r"\\?\")) {
            return Ok(std::path::PathBuf::from(local));
        }
    }
    let _ = owner;
    Ok(path)
}

pub(super) async fn trigger(app: &Router, token: &str) -> Result<String> {
    let response = request(app, "POST", "/v1/sync/trigger", Some(token)).await?;
    let operation = json_response(response, StatusCode::ACCEPTED).await?;
    let id = operation["operation_id"].as_str().context("operation ID")?;
    uuid::Uuid::parse_str(id)?;
    Ok(format!("/v1/operations/{id}"))
}
