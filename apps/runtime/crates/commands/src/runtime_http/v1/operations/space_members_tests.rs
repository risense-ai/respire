//! Assertions extend the existing main and named-profile production-router fixtures.
use super::*;

pub(super) async fn check(app: &Router) -> Result<()> {
    let profile = service::data_dir().canonicalize()?;
    let root = service::main_data_dir().canonicalize()?;
    let id = if profile == root { "main" } else { "outline-fixture" };
    let path = format!("/v1/spaces/{id}/members");
    let members_path = profile.join("space_sessions.json");
    let session = fs::read(profile.join("session.json"))?;
    let client = fs::read(root.join("client.json")).ok();
    assert!(!members_path.try_exists()?);
    assert_eq!(request(app, "GET", &path, None).await?.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(request(app, "GET", &path, Some("wrong-token")).await?.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(json_response(request(app, "GET", &path, Some(TOKEN)).await?, StatusCode::OK).await?, json!({"members":[]}));
    let member = uuid::Uuid::new_v4().to_string();
    let viewer = uuid::Uuid::new_v4().to_string();
    let rows = json!([
        {"session_id":member,"device_name":"member-editor","readonly":false,"issued_at":"2026-11-25T00:00:00Z"},
        {"session_id":viewer,"device_name":"member-viewer","readonly":true,"issued_at":"2026-11-25T00:00:00Z"}
    ]);
    let original = serde_json::to_vec(&rows)?;
    fs::write(&members_path, &original)?;
    let expected = json!({"members":[
        {"member_id":member,"display_name":"member-editor","role":"editor","status":"active"},
        {"member_id":viewer,"display_name":"member-viewer","role":"viewer","status":"active"}
    ]});
    assert_eq!(json_response(request(app, "GET", &path, Some(TOKEN)).await?, StatusCode::OK).await?, expected);
    for other in ["main", "outline-fixture", "unknown", "%2E%2E", "unknown%2Fchild"] {
        if other != id {
            assert_eq!(request(app, "GET", &format!("/v1/spaces/{other}/members"), Some(TOKEN)).await?.status(), StatusCode::NOT_FOUND);
        }
    }
    assert_eq!(request(app, "GET", &format!("{path}?space_id={id}"), Some(TOKEN)).await?.status(), StatusCode::BAD_REQUEST);
    assert_eq!(send(app, "GET", &path, Some(TOKEN), Some(json!({}))).await?.status(), StatusCode::BAD_REQUEST);
    for method in ["HEAD", "POST", "PATCH", "DELETE"] {
        assert_eq!(request(app, method, &path, Some(TOKEN)).await?.status(), StatusCode::METHOD_NOT_ALLOWED);
    }
    assert_eq!(fs::read(&members_path)?, original);
    for malformed in [b"{".to_vec(), serde_json::to_vec(&json!([rows[0], rows[0]]))?, vec![b' '; 65537]] {
        fs::write(&members_path, &malformed)?;
        let response = request(app, "GET", &path, Some(TOKEN)).await?;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = to_bytes(response.into_body(), 4096).await?;
        assert!(!String::from_utf8_lossy(&bytes).contains(&member));
        assert_eq!(fs::read(&members_path)?, malformed);
    }
    fs::write(&members_path, &original)?;
    assert_eq!(json_response(request(app, "GET", &path, Some(TOKEN)).await?, StatusCode::OK).await?, expected);
    assert_eq!(fs::read(profile.join("session.json"))?, session);
    assert_eq!(fs::read(root.join("client.json")).ok(), client);
    fs::remove_file(&members_path)?;
    Ok(())
}
