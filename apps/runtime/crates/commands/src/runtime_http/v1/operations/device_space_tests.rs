use super::*;
use respire_app::memory::crypto;
use respire_spawn::owner_credential::LocalSpaceNamespace;
use std::path::Path;

const BASE: &str = "/v1/spaces";

async fn create(app: &Router, token: Option<&str>, body: Value) -> Result<Response> {
    send(app, "POST", BASE, token, Some(body)).await
}

async fn list(app: &Router) -> Result<Vec<Value>> {
    let value = json_response(request(app, "GET", BASE, Some(TOKEN)).await?, StatusCode::OK).await?;
    Ok(value["spaces"].as_array().context("local spaces shape")?.clone())
}

async fn expect_missing(app: &Router, suffix: &str) -> Result<()> {
    let reply = request(app, "GET", &format!("{BASE}/{suffix}"), Some(TOKEN)).await?;
    assert_eq!(reply.headers()[header::CONTENT_TYPE], "application/problem+json");
    let problem = json_response(reply, StatusCode::NOT_FOUND).await?;
    assert_eq!(problem["status"], 404);
    assert_eq!(problem["code"], "space_not_found");
    Ok(())
}

pub(super) async fn check(app: &Router, root: &Path) -> Result<()> {
    assert!(LocalSpaceNamespace::open_existing(root)?.is_none());
    assert_eq!(request(app, "GET", BASE, None).await?.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(create(app, Some("unrelated-fixture-bearer"), json!({"name":"denied"})).await?.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(request(app, "GET", &format!("{BASE}?bad=value"), Some(TOKEN)).await?.status(), StatusCode::BAD_REQUEST);
    assert_eq!(create(app, Some(TOKEN), json!({"name":" ", "role":"editor"})).await?.status(), StatusCode::BAD_REQUEST);
    assert!(list(app).await?.is_empty());
    assert!(LocalSpaceNamespace::open_existing(root)?.is_none());

    service::set_workspace_mode("readonly")?;
    let result = create(app, Some(TOKEN), json!({"name":"read-only forbidden"})).await;
    service::set_workspace_mode("normal")?;
    assert_eq!(result?.status(), StatusCode::FORBIDDEN);
    assert!(LocalSpaceNamespace::open_existing(root)?.is_none());

    let before = service::data_dir().canonicalize()?;
    let alpha = json_response(create(app, Some(TOKEN), json!({"name":"offline alpha"})).await?, StatusCode::CREATED).await?;
    let beta = json_response(create(app, Some(TOKEN), json!({"name":"offline beta"})).await?, StatusCode::CREATED).await?;
    for (item, name) in [(&alpha, "offline alpha"), (&beta, "offline beta")] {
        let id = item["id"].as_str().context("local space ID")?;
        let parsed = uuid::Uuid::parse_str(id.strip_prefix("local-").context("device ID prefix")?)?;
        assert_eq!(parsed.get_version_num(), 4);
        assert_eq!(item, &json!({"id":id,"name":name,"role":"owner","active":false}));
        let result = request(app, "GET", &format!("{BASE}/{id}"), Some(TOKEN)).await?;
        assert_eq!(json_response(result, StatusCode::OK).await?, *item);
    }
    assert_ne!(alpha["id"], beta["id"]);
    check_memory_isolation(
        app,
        alpha["id"].as_str().context("alpha ID")?,
        beta["id"].as_str().context("beta ID")?,
    ).await?;
    assert_eq!(service::data_dir().canonicalize()?, before);

    // Directory presence and a valid-shape foreign ciphertext do not confer ownership.
    let storage = LocalSpaceNamespace::open_existing(root)?.context("native area missing")?;
    let partial = uuid::Uuid::new_v4().simple().to_string();
    drop(storage.fresh(&partial)?);
    let foreign = uuid::Uuid::new_v4().simple().to_string();
    let foreign_key = crypto::derive_subkey(&crypto::generate_key(), b"rsrs:device-local-space-owner:1")?;
    let (nonce, sealed) = crypto::encrypt_bytes(&foreign_key, b"{\"version\":1}")?;
    storage.fresh(&foreign)?.publish(&serde_json::to_vec(&[nonce, sealed])?, || Ok(()))?;
    let mut occupied = Vec::new();
    for _ in 0..16 {
        occupied.push(storage.fresh(&uuid::Uuid::new_v4().simple().to_string())?);
    }
    assert!(storage.fresh(&uuid::Uuid::new_v4().simple().to_string()).is_err());
    assert_eq!(storage.candidates()?.len(), 3, "stages must not consume the committed quota");
    drop(occupied);
    drop(storage);
    expect_missing(app, &format!("local-{partial}")).await?;
    expect_missing(app, &format!("local-{foreign}")).await?;
    for id in [format!("local-{foreign}"), format!("local-{partial}")] {
        let path = format!("/v1/memories?space_id={id}");
        let response = request(app, "GET", &path, Some(TOKEN)).await?;
        assert_eq!(json_response(response, StatusCode::NOT_FOUND).await?["code"], "space_not_found");
        let response = send(app, "POST", "/v1/memories", Some(TOKEN),
            Some(json!({"space_id":id, "content":"must not be saved"}))).await?;
        assert_eq!(json_response(response, StatusCode::NOT_FOUND).await?["code"], "space_not_found");
        let response = send(app, "POST", "/v1/memories/recall", Some(TOKEN),
            Some(json!({"space_id":id, "query":"unknown space"}))).await?;
        assert_eq!(json_response(response, StatusCode::NOT_FOUND).await?["code"], "space_not_found");
    }
    expect_missing(app, "unverified-legacy-profile").await?;
    let visible = list(app).await?;
    assert_eq!(visible.len(), 2);
    assert!(visible.contains(&alpha) && visible.contains(&beta));
    timed_out_preparation_never_publishes(app, root, &visible).await?;
    Ok(())
}

async fn timed_out_preparation_never_publishes(
    app: &Router, root: &Path, visible: &[Value],
) -> Result<()> {
    use crate::runtime_http::v1::{rest_dispatch, transport};
    use axum::response::IntoResponse;
    let request = Request::get(BASE)
        .header(header::HOST, BOUND.to_string())
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(())?;
    let (parts, _) = request.into_parts();
    let scope = transport::authorize(&parts, BOUND, BOUND).await
        .map_err(|failure| anyhow::anyhow!("fixture authorization: {failure:?}"))?;
    let (staged_tx, staged_rx) = tokio::sync::oneshot::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    let request = tokio::spawn(rest_dispatch::submit_device_space_create(
        scope, move |ctx, admission| {
            let result = ctx.library.create_device_space(
                ctx.ownership, "expired isolated fixture".to_owned(), || true, || {
                    let _ = staged_tx.send(());
                    resume_rx.recv_timeout(WAIT).is_ok() && ctx.may_commit() && admission.admit()
                },
            );
            let _ = done_tx.send(result.is_err());
            result.map_err(|_| transport::unavailable())
        }
    ));
    tokio::time::timeout(WAIT, staged_rx).await??;
    // The worker has fully staged both SQLite and receipt but cannot yet admit.
    // Do not release it until the receiving request has definitively timed out.
    let deadline = tokio::time::timeout(WAIT, request).await??;
    assert_eq!(
        deadline.err().context("unadmitted prepare must time out")?.into_response().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    resume_tx.send(())?;
    assert!(tokio::time::timeout(WAIT, done_rx).await??);
    assert_eq!(list(app).await?, visible);
    let pending = root.join(".respire-local-spaces-v1").join(".pending");
    assert_eq!(fs::read_dir(pending)?.count(), 0, "owned failed stage must be cleaned");
    Ok(())
}


async fn check_memory_isolation(app: &Router, alpha: &str, beta: &str) -> Result<()> {
    let path = "/v1/memories";
    let default = json_response(request(app, "GET", path, Some(TOKEN)).await?, StatusCode::OK).await?;
    let create = |space, content| send(app, "POST", path, Some(TOKEN),
        Some(json!({"space_id":space, "content":content})));
    service::set_workspace_mode("readonly")?;
    let refused = create(alpha, "Read-only scope must not save").await;
    service::set_workspace_mode("normal")?;
    assert_eq!(refused?.status(), StatusCode::FORBIDDEN);
    let recall = send(app, "POST", "/v1/memories/recall", Some(TOKEN), Some(json!({
        "space_id": alpha, "query": "A new space has no candidates",
    }))).await?;
    assert_eq!(json_response(recall, StatusCode::OK).await?, json!({"hits":[]}));
    let empty = scoped_outbox_counts(alpha, beta).await?;
    assert_eq!([empty.1, empty.2], [0, 0]);
    let alpha_entry = json_response(create(alpha, "Isolation fixture alpha").await?, StatusCode::CREATED).await?;
    let beta_entry = json_response(create(beta, "Isolation fixture beta").await?, StatusCode::CREATED).await?;
    assert_ne!(alpha_entry["id"], beta_entry["id"]);
    let written = scoped_outbox_counts(alpha, beta).await?;
    assert_eq!(written.0, empty.0, "device-only writes must leave the parent outbox untouched");
    assert_eq!([written.1, written.2], [1, 1], "each dirty row belongs only to its child");
    for (space, own) in [(alpha, &alpha_entry), (beta, &beta_entry), (alpha, &alpha_entry)] {
        let reply = request(app, "GET", &format!("{path}?space_id={space}"), Some(TOKEN)).await?;
        let page = json_response(reply, StatusCode::OK).await?;
        assert_eq!(page["items"], json!([own]));
    }
    // Exercise the built-in pending-index lexical channel using two genuine
    // encrypted contents, not a fixture provider or a manufactured empty result.
    for (space, token, own) in [
        (Some(alpha), "alpha", Some(&alpha_entry)),
        (Some(beta), "beta", Some(&beta_entry)),
        (Some(alpha), "alpha", Some(&alpha_entry)),
        (None, "alpha", None),
    ] {
        let mut body = json!({"query": format!("Isolation fixture {token}"), "top_k": 10});
        if let Some(space) = space {
            body["space_id"] = json!(space);
        }
        let reply = send(app, "POST", "/v1/memories/recall", Some(TOKEN), Some(body)).await?;
        let recall = json_response(reply, StatusCode::OK).await?;
        let matches = recall["hits"].as_array().context("recall hits")?;
        if let Some(own) = own {
            assert!(!matches.is_empty(), "populated child recall must recover its own row");
            assert!(matches.iter().all(|hit| hit["memory"]["id"] == own["id"]));
        } else {
            assert!(matches.iter().all(|hit| hit["memory"]["id"] != alpha_entry["id"] &&
                hit["memory"]["id"] != beta_entry["id"]));
        }
    }
    let foreign_parent = send(app, "POST", path, Some(TOKEN), Some(json!({
        "space_id":beta, "content":"Must not adopt alpha parent", "parent_id":alpha_entry["id"],
    }))).await?;
    assert_eq!(foreign_parent.status(), StatusCode::NOT_FOUND);
    let reply = request(app, "GET", path, Some(TOKEN)).await?;
    assert_eq!(json_response(reply, StatusCode::OK).await?, default);
    let id = alpha_entry["id"].as_str().context("alpha memory ID")?;
    let reply = request(app, "GET", &format!("{path}/{id}"), Some(TOKEN)).await?;
    assert_eq!(reply.status(), StatusCode::NOT_FOUND);
    Ok(())
}


async fn scoped_outbox_counts(alpha: &str, beta: &str) -> Result<(i64, i64, i64)> {
    use crate::runtime_http::v1::{reads, rest_dispatch, transport};
    let parts = Request::get("/v1/memories")
        .header(header::HOST, BOUND.to_string())
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(())?.into_parts().0;
    let scope = transport::authorize(&parts, BOUND, BOUND).await
        .map_err(|failure| anyhow::anyhow!("fixture authorization: {failure:?}"))?;
    let alpha = alpha.to_owned();
    let beta = beta.to_owned();
    rest_dispatch::submit(scope, rest_dispatch::Access::Read, move |ctx| {
        let count = |store: &respire_app::transport::local::LocalStore| {
            store.sync_counts().map(|(pending, _, _)| pending).map_err(|_| transport::unavailable())
        };
        let a = reads::with_memory_store(ctx, Some(alpha), |store, _| count(store))?;
        let b = reads::with_memory_store(ctx, Some(beta), |store, _| count(store))?;
        Ok((count(ctx.store)?, a, b))
    }).await.map_err(|failure| anyhow::anyhow!("outbox scope: {failure:?}"))
}
