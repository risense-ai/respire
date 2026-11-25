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
