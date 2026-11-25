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

pub(super) async fn check_organize_suggestions(app: &Router) -> Result<()> {
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
