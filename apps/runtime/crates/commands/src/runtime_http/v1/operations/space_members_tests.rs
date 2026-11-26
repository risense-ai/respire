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
    for method in ["HEAD", "PATCH", "DELETE"] {
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
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    if id == "main" {
        device_space_tests::check(app, &root, false).await?;
    }
    Ok(())
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
#[path = "device_space_tests.rs"]
pub(super) mod device_space_tests;

pub(super) fn fixture_root(path: &std::path::Path, secure: bool) -> Result<std::path::PathBuf> {
    let root = path.canonicalize()?;
    #[cfg(windows)]
    if secure {
        return Ok(respire_spawn::owner_credential::LocalSpaceNamespace::make_fixture_installation(&root)?);
    }
    #[cfg(not(windows))]
    let _ = secure;
    Ok(root)
}

pub(super) async fn loopback() -> Result<()> {
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = socket.local_addr()?;
    let app = crate::runtime_http::router(addr);
    let task = tokio::spawn(async move {
        axum::serve(crate::network::Listener(socket),
            app.into_make_service_with_connect_info::<crate::network::Peer>()).await
    });
    let result = tokio::task::spawn_blocking(move || -> Result<()> {
        let url = format!("http://{addr}/v1/spaces/outline-fixture/members");
        let auth = format!("Bearer {TOKEN}");
        let body = json!({"identifier":"tablet","role":"viewer"});
        let post = |url: &str, token: &str, body: &Value| ureq::post(url)
            .timeout(WAIT).set("Authorization", token).send_json(body);
        assert!(matches!(post(&url, "Bearer foreign", &body), Err(ureq::Error::Status(401, _))));
        assert!(matches!(post(&url.replace("outline-fixture", "other"), &auth, &body), Err(ureq::Error::Status(404, _))));
        assert!(matches!(post(&url, &auth, &json!({"identifier":"","role":"owner"})), Err(ureq::Error::Status(400, _))));
        let local = format!("local-{}", uuid::Uuid::new_v4().simple());
        assert!(matches!(post(&url.replace("outline-fixture", &local), &auth, &body), Err(ureq::Error::Status(409, _))));
        let reply = post(&url, &auth, &body)?;
        assert_eq!(reply.status(), 201);
        let row: Value = reply.into_json()?;
        assert_eq!(row, json!({"member_id":row["member_id"],"display_name":"tablet","role":"viewer","status":"pending"}));
        uuid::Uuid::parse_str(row["member_id"].as_str().context("missing ID")?)?;
        assert_eq!(post(&url, &auth, &body)?.into_json::<Value>()?, row);
        assert_eq!(ureq::get(&url).timeout(WAIT).set("Authorization", &auth).call()?.into_json::<Value>()?,
            json!({"members":[row]}));
        Ok(())
    }).await?;
    task.abort();
    result
}

pub(super) const INVITE_TEST: &str = "runtime_http::v1::operations::tests::space_members_tests::pending_invite_on_loopback";
#[test]
fn pending_invite_on_loopback() -> Result<()> { production_router_fixture(INVITE_TEST) }

pub(super) fn fixture_session(test: &str, salt: String, nonce: String, wrapped: String) -> Value {
    // A malformed URL exercises the real sync failure before the HTTP client can connect.
    let mut session_data = json!({
        "user": "sse-fixture", "vault_version": 4, "kdf_salt": salt,
        "urk_nonce": nonce, "wrapped_urk": wrapped,
    });
    if test != super::SPACE_TEST {
        session_data["addr"] = json!("http://[");
        session_data["token"] = json!("fixture-sync-token");
    }
    if test == INVITE_TEST { session_data["addr"] = json!("http://127.0.0.1:1"); session_data["auth_salt"] = json!("fixture-salt"); }
    session_data
}
