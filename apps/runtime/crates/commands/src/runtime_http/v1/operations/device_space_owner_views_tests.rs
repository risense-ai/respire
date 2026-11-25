use super::*;
use crate::runtime_http::v1::{reads, rest_dispatch, transport};
use respire_spawn::owner_credential::{proof, read_owner_credential};

pub(super) async fn check(app: &Router, alpha: &str, beta: &str, a: &Value, b: &Value) -> Result<()> {
    let scope = transport::authorize(
        &Request::get("/v1/spaces").header(header::HOST, BOUND.to_string())
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}")).body(())?.into_parts().0,
        BOUND, BOUND,
    ).await.map_err(|failure| anyhow::anyhow!("fixture authorization: {failure:?}"))?;
    let alpha = alpha.to_owned();
    let beta = beta.to_owned();
    let a = a["id"].as_str().context("alpha grant root")?.to_owned();
    let b = b["id"].as_str().context("beta grant root")?.to_owned();
    let saved_alpha = alpha.clone();
    let (alpha_grant, beta_grant) = rest_dispatch::submit(scope.clone(), rest_dispatch::Access::Write, move |ctx| {
        let create = |space, root: String, label| {
            reads::with_memory_store(ctx, Some(space), |store, _| {
                store.create_grant(&root, label).map(|(grant, _)| grant.id)
                    .map_err(|_| transport::unavailable())
            })
        };
        Ok((create(alpha, a, "alpha-only grant")?, create(beta, b, "beta-only grant")?))
    }).await.map_err(|failure| anyhow::anyhow!("fixture grants: {failure:?}"))?;
    let native = obtain_owner_lease(app).await?;
    let get = |path| request(app, "GET", path, Some(&native));
    let stats = json_response(get("/v1/stats").await?, StatusCode::OK).await?;
    assert_eq!(stats["local_total"], 1);
    assert_eq!(stats["remote_configured"], false);
    let grants = json_response(get("/v1/grants").await?, StatusCode::OK).await?;
    let grants = grants["items"].as_array().context("selected grant list")?;
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0]["id"], beta_grant);
    let wrong = send(app, "DELETE", &format!("/v1/grants/{alpha_grant}"), Some(&native), None).await?;
    assert_eq!(wrong.status(), StatusCode::NO_CONTENT);
    let revoke = send(app, "DELETE", &format!("/v1/grants/{beta_grant}"), Some(&native), None).await?;
    assert_eq!(revoke.status(), StatusCode::NO_CONTENT);
    let grants = json_response(get("/v1/grants").await?, StatusCode::OK).await?;
    assert_eq!(grants["items"][0]["revoked"], true);
    rest_dispatch::submit(scope, rest_dispatch::Access::Read, move |ctx| {
        reads::with_memory_store(ctx, Some(saved_alpha), |store, _| {
            let grants = store.list_grants().map_err(|_| transport::unavailable())?;
            assert_eq!(grants.len(), 1);
            assert_eq!(grants[0].id, alpha_grant);
            assert!(!grants[0].revoked);
            Ok(())
        })
    }).await.map_err(|failure| anyhow::anyhow!("alpha grant invariant: {failure:?}"))?;
    for route in ["/v1/query-log", "/v1/sync/conflicts"] {
        assert_eq!(get(route).await?.status(), StatusCode::OK);
    }
    Ok(())
}

async fn obtain_owner_lease(app: &Router) -> Result<String> {
    let health = json_response(request(app, "GET", "/v1/health", None).await?, StatusCode::OK).await?;
    let instance = health["instance_id"].as_str().context("serving instance ID")?;
    let base = crate::rpc_worker::runtime_dir();
    let record = tokio::time::timeout(WAIT, async {
        loop {
            match read_owner_credential(&base, instance) {
                Ok(record) => return record,
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
    }).await.context("owner fixture did not publish its real private credential")?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis();
    let challenge = proof::issue(record.as_bytes(), 63, u64::try_from(stamp)?)
        .context("native owner proof")?;
    let scopes = ["grants:read", "grants:revoke", "stats:read", "query-log:read", "reviews:read", "sync-conflicts:read"];
    let request = Request::post("/v1/auth/handshake")
        .header(header::HOST, BOUND.to_string())
        .header(header::CONTENT_TYPE, "application/json")
        .header("X-Respire-Handshake-Proof", challenge.as_str())
        .extension(ConnectInfo(crate::network::Peer(BOUND)))
        .body(Body::from(serde_json::to_vec(&json!({"scopes": scopes}))?))?;
    let response = app.clone().oneshot(request).await?;
    let lease = json_response(response, StatusCode::OK).await?;
    Ok(lease["access_token"].as_str().context("native owner token")?.to_owned())
}
