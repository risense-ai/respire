//! Assertions run inside the existing isolated production-router fixture.
use super::*;

pub(super) const TEST: &str =
    "runtime_http::v1::operations::tests::host_config_tests::host_config_on_production_router";

#[test]
fn host_config_on_production_router() -> Result<()> {
    production_router_fixture(TEST)
}

async fn check_patch(app: &Router, original: Option<&[u8]>) -> Result<()> {
    let config_path = service::client_config_path();
    let agent_path = service::agent_config_path();
    let mut client: Value = original
        .map(serde_json::from_slice)
        .transpose()?
        .unwrap_or_else(|| json!({}));
    client["unknown"] = json!({"nested":[1,2]});
    fs::write(&config_path, serde_json::to_vec(&client)?)?;
    fs::write(&agent_path, br#"{"unknown_agent":true}"#)?;
    let input = json!({"language":"zh","model_mirror":"https://hf-mirror.net","proxy_url":"socks5://127.0.0.1:1080","diary_mode":"verbose"});
    assert_eq!(
        send(app, "PATCH", "/v1/host/config", None, Some(input.clone()))
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        json_response(
            send(
                app,
                "PATCH",
                "/v1/host/config",
                Some(TOKEN),
                Some(input.clone())
            )
            .await?,
            StatusCode::OK
        )
        .await?,
        input
    );
    let saved: Value = serde_json::from_slice(&fs::read(&config_path)?)?;
    assert_eq!(saved["unknown"], client["unknown"]);
    assert_eq!(saved["data_dir"], client["data_dir"]);
    let saved_agent: Value = serde_json::from_slice(&fs::read(&agent_path)?)?;
    assert_eq!(saved_agent["unknown_agent"], true);
    assert_eq!(saved_agent["diary_mode"], "verbose");
    let partial = json_response(
        send(
            app,
            "PATCH",
            "/v1/host/config",
            Some(TOKEN),
            Some(json!({"proxy_url":null})),
        )
        .await?,
        StatusCode::OK,
    )
    .await?;
    assert_eq!(partial["proxy_url"], Value::Null);
    assert_eq!(partial["diary_mode"], "verbose");
    assert_eq!(partial["language"], "zh");
    let before = fs::read(&config_path)?;
    let before_agent = fs::read(&agent_path)?;
    for invalid in [
        json!({"language":null}),
        json!({"language":"xx"}),
        json!({"diary_mode":"other"}),
        json!({"model_mirror":"https://untrusted.invalid"}),
        json!({"proxy_url":"http://user:fixture@localhost:1234"}),
        json!({"data_dir":"other"}),
        json!([]),
    ] {
        assert_eq!(
            send(app, "PATCH", "/v1/host/config", Some(TOKEN), Some(invalid))
                .await?
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(fs::read(&config_path)?, before);
        assert_eq!(fs::read(&agent_path)?, before_agent);
    }
    service::set_workspace_mode("readonly")?;
    assert_eq!(
        send(
            app,
            "PATCH",
            "/v1/host/config",
            Some(TOKEN),
            Some(json!({"language":"en"}))
        )
        .await?
        .status(),
        StatusCode::FORBIDDEN
    );
    service::set_workspace_mode("normal")?;
    assert_eq!(
        json_response(
            send(
                app,
                "PATCH",
                "/v1/host/config",
                Some(TOKEN),
                Some(json!({}))
            )
            .await?,
            StatusCode::OK
        )
        .await?["language"],
        "zh"
    );
    match original {
        Some(bytes) => fs::write(&config_path, bytes)?,
        None => fs::remove_file(&config_path)?,
    }
    fs::remove_file(&agent_path)?;
    Ok(())
}

pub(super) async fn check(app: &Router) -> Result<()> {
    let config_path = service::client_config_path();
    let original = fs::read(&config_path).ok();
    let config_get = || request(app, "GET", "/v1/host/config", Some(TOKEN));
    assert_eq!(
        request(app, "GET", "/v1/host/config", None).await?.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(app, "GET", "/v1/host/config?unexpected=1", Some(TOKEN))
            .await?
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        json_response(config_get().await?, StatusCode::OK).await?,
        json!({"language":"en", "model_mirror":"auto", "diary_mode":"concise", "proxy_url":null})
    );
    check_patch(app, original.as_deref()).await?;
    let mut configured: Value = original
        .as_deref()
        .map(serde_json::from_slice)
        .transpose()?
        .unwrap_or_else(|| json!({}));
    configured["lang"] = json!("zh");
    configured["model_mirror"] = json!("https://hf-mirror.net");
    configured["proxy_url"] = json!("socks5://127.0.0.1:1080");
    fs::write(&config_path, serde_json::to_vec(&configured)?)?;
    let agent_path = service::data_dir().join("agent.json");
    fs::write(&agent_path, br#"{"diary_mode":"verbose","preserve":true}"#)?;
    assert_eq!(
        json_response(config_get().await?, StatusCode::OK).await?,
        json!({"language":"zh", "model_mirror":"https://hf-mirror.net", "diary_mode":"verbose", "proxy_url":"socks5://127.0.0.1:1080"})
    );
    configured["proxy_url"] = json!("http://synthetic:fixture@localhost:3128");
    fs::write(&config_path, serde_json::to_vec(&configured)?)?;
    let failure = config_get().await?;
    assert_eq!(failure.status(), StatusCode::SERVICE_UNAVAILABLE);
    let bytes = to_bytes(failure.into_body(), 4096).await?;
    assert!(!String::from_utf8_lossy(&bytes).contains("synthetic:fixture"));
    fs::write(&config_path, b"{")?;
    assert_eq!(
        config_get().await?.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    match original {
        Some(bytes) => fs::write(&config_path, bytes)?,
        None => fs::remove_file(&config_path)?,
    }
    fs::write(&agent_path, b"{")?;
    assert_eq!(
        config_get().await?.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    fs::remove_file(&agent_path)?;
    assert_eq!(
        request(app, "PATCH", "/v1/host/config", Some(TOKEN))
            .await?
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(app, "HEAD", "/v1/host/config", Some(TOKEN))
            .await?
            .status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    Ok(())
}
