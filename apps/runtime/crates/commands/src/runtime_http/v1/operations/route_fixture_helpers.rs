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

#[cfg(windows)]
#[path = "../backup_export.rs"]
mod backup_export;

pub(super) fn is_owner_route(test: &str) -> bool {
    #[cfg(windows)]
    if test == backup_export::windows_tests::LOOPBACK {
        return true;
    }
    test == SPACE_TEST
}

pub(super) async fn check_owner_route(
    test: &str, app: &Router, root: &std::path::Path, urk: [u8; 32],
) -> Result<()> {
    #[cfg(windows)]
    if test == backup_export::windows_tests::LOOPBACK {
        return backup_export::windows_tests::check_loopback(root, urk).await;
    }
    let _ = (test, urk);
    space_members_tests::device_space_tests::check(app, root, true).await
}
