use super::*;
use respire_app::memory::crypto;
use respire_spawn::owner_credential::LocalSpaceNamespace;
use std::path::Path;

#[path = "device_space_owner_views_tests.rs"]
mod owner_views;
#[path = "device_space_retire_errors_tests.rs"]
mod retire_errors;
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

pub(in super::super) async fn check(app: &Router, root: &Path, local: bool) -> Result<()> {
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
        local,
    ).await?;
    assert_eq!(service::data_dir().canonicalize()?, before);
    check_sealed_state(beta["id"].as_str().context("beta public ID")?).await?;
    let restored = request(app, "GET", &format!("{BASE}/{}", beta["id"].as_str().context("beta public ID")?), Some(TOKEN)).await?;
    assert_eq!(json_response(restored, StatusCode::OK).await?, beta);

    // Directory presence and a valid-shape foreign ciphertext do not confer ownership.
    let storage = LocalSpaceNamespace::open_existing(root)?.context("native area missing")?;
    let local_state_slot = uuid::Uuid::new_v4().simple().to_string();
    let second_state_slot = uuid::Uuid::new_v4().simple().to_string();
    assert!(storage.state_record(&local_state_slot)?.is_none());
    storage.replace_state_record(&local_state_slot, b"initial private state", || Ok(()))?;
    assert_eq!(storage.state_record(&local_state_slot)?.as_deref(), Some(b"initial private state".as_slice()));
    let refused = storage.replace_state_record(&local_state_slot, b"unadmitted private state", || {
        Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
    });
    assert!(refused.is_err());
    assert_eq!(storage.state_record(&local_state_slot)?.as_deref(), Some(b"initial private state".as_slice()));
    storage.replace_state_record(&local_state_slot, b"replacement private state", || Ok(()))?;
    assert_eq!(storage.state_record(&local_state_slot)?.as_deref(), Some(b"replacement private state".as_slice()));
    assert!(storage.state_record(&second_state_slot)?.is_none());
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
    crate::runtime_http::v1::operations::organize_split::tests::check_device_scope(
        app, beta["id"].as_str().context("beta ID")?, alpha["id"].as_str().context("alpha ID")?,
    ).await?;
    if local {
        check_delete(app, root, &alpha, &beta, &foreign).await?;
    }
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


async fn check_memory_isolation(app: &Router, alpha: &str, beta: &str, local: bool) -> Result<()> {
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
    if local {
        check_active_recall(app, alpha, beta, &alpha_entry, &beta_entry, &default).await?;
    } else {
        let refusal = send(app, "PATCH", &format!("{BASE}/{beta}"), Some(TOKEN),
            Some(json!({"name":"must not rename on unproved restore", "active":true}))).await?;
        assert_eq!(refusal.headers()[header::CONTENT_TYPE], "application/problem+json");
        let refusal = json_response(refusal, StatusCode::CONFLICT).await?;
        assert_eq!(refusal["code"], "original_space_not_verified");
        assert_eq!(refusal["status"], 409);
        assert!(refusal["type"].is_string());
        let beta = request(app, "GET", &format!("{BASE}/{beta}"), Some(TOKEN)).await?;
        let beta = json_response(beta, StatusCode::OK).await?;
        assert_eq!(beta["name"], "offline beta");
        assert_eq!(beta["active"], false);
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


async fn check_sealed_state(beta: &str) -> Result<()> {
    use crate::runtime_http::v1::{rest_dispatch, transport};
    use respire_app::auth::DeviceSpacePatch;
    let parts = Request::get("/v1/spaces")
        .header(header::HOST, BOUND.to_string())
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(())?.into_parts().0;
    let scope = transport::authorize(&parts, BOUND, BOUND).await
        .map_err(|failure| anyhow::anyhow!("fixture authorization: {failure:?}"))?;
    let beta = beta.to_owned();
    rest_dispatch::submit(scope, rest_dispatch::Access::Write, move |ctx| {
        let patch = |name: &str, active| DeviceSpacePatch {
            name: Some(name.to_owned()), active: Some(active),
        };
        let actual = || ctx.library.active_device_space(ctx.ownership).map_err(|_| transport::unavailable());
        let read = || {
            let item = ctx.library.get_device_space(ctx.ownership, &beta)
                .map_err(|_| transport::unavailable())?;
            serde_json::to_value(item).map_err(|_| transport::unavailable())
        };
        assert_eq!(actual()?, None);
        let rejected = ctx.library.patch_device_space(ctx.ownership, &beta,
            patch("unadmitted beta", true), || false);
        assert!(rejected.is_err());
        assert_eq!(actual()?, None);
        assert_eq!(read()?["name"], "offline beta");
        let renamed = ctx.library.patch_device_space(ctx.ownership, &beta,
            patch("renamed and active beta", true), || ctx.may_commit()).map_err(|_| transport::unavailable())?;
        let renamed = serde_json::to_value(renamed).map_err(|_| transport::unavailable())?;
        assert_eq!(renamed["name"], "renamed and active beta");
        assert_eq!(renamed["active"], true);
        assert_eq!(actual()?.as_deref(), Some(beta.as_str()));
        assert_eq!(read()?, renamed);
        ctx.library.patch_device_space(ctx.ownership, &beta,
            DeviceSpacePatch { name: Some("offline beta".to_owned()), active: None },
            || ctx.may_commit()).map_err(|_| transport::unavailable())?;
        ctx.library.restore_original_space(ctx.ownership, || ctx.may_commit())
            .map_err(|_| transport::unavailable())?;
        assert_eq!(actual()?, None);
        assert_eq!(read()?["active"], false);
        Ok(())
    }).await.map_err(|failure| anyhow::anyhow!("sealed device space state: {failure:?}"))
}


async fn check_active_recall(
    app: &Router, alpha: &str, beta: &str, alpha_entry: &Value, beta_entry: &Value,
    original_entries: &Value,
) -> Result<()> {
    let beta_path = format!("{BASE}/{beta}");
    let denied = send(app, "PATCH", &beta_path, None, Some(json!({"active":true}))).await?;
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    let wrong = send(app, "PATCH", &format!("{BASE}/main"), Some(TOKEN), Some(json!({
        "name":"not a mutable local catalog member", "active":true,
    }))).await?;
    assert_eq!(wrong.headers()[header::CONTENT_TYPE], "application/problem+json");
    assert_eq!(json_response(wrong, StatusCode::CONFLICT).await?["code"], "original_space_not_verified");
    let selected = send(app, "PATCH", &beta_path, Some(TOKEN), Some(json!({
        "name":"Router verified beta", "active":true,
    }))).await?;
    let selected = json_response(selected, StatusCode::OK).await?;
    assert_eq!(selected, json!({"id":beta,"name":"Router verified beta","role":"owner","active":true}));
    let get = request(app, "GET", &beta_path, Some(TOKEN)).await?;
    assert_eq!(json_response(get, StatusCode::OK).await?, selected);
    let active_delete = send(app, "DELETE", &beta_path, Some(TOKEN), None).await?;
    assert_eq!(json_response(active_delete, StatusCode::CONFLICT).await?["code"], "space_is_active");

    let list = request(app, "GET", "/v1/memories", Some(TOKEN)).await?;
    assert_eq!(json_response(list, StatusCode::OK).await?["items"], json!([beta_entry]));
    let status = request(app, "GET", "/v1/status/light", Some(TOKEN)).await?;
    assert_eq!(json_response(status, StatusCode::OK).await?["space_id"], beta);
    let recall = |selected, query| async move {
        let mut body = json!({"query":query});
        if let Some(selected) = selected {
            body["space_id"] = json!(selected);
        }
        let reply = send(app, "POST", "/v1/memories/recall", Some(TOKEN), Some(body)).await?;
        json_response(reply, StatusCode::OK).await
    };
    let explicit = recall(Some(alpha), "Isolation fixture alpha").await?;
    assert_eq!(explicit["hits"][0]["memory"]["id"], alpha_entry["id"]);
    let implicit = recall(None, "Isolation fixture alpha").await?;
    assert!(implicit["hits"].as_array().context("default hits")?.iter()
        .all(|hit| hit["memory"]["id"] != alpha_entry["id"]));
    let beta_hit = recall(None, "Isolation fixture beta").await?;
    assert_eq!(beta_hit["hits"][0]["memory"]["id"], beta_entry["id"]);
    owner_views::check(app, alpha, beta, alpha_entry, beta_entry).await?;
    let counts_before = scoped_outbox_counts(alpha, beta).await?;
    let reply = send(app, "POST", "/v1/memories", Some(TOKEN), Some(json!({
        "content": "default write under active beta",
    }))).await?;
    let saved = json_response(reply, StatusCode::CREATED).await?;
    let saved_id = saved["id"].as_str().context("active default memory ID")?;
    let saved_path = format!("/v1/memories/{saved_id}");
    let alpha_id = alpha_entry["id"].as_str().context("alpha memory ID")?;
    let alpha_path = format!("/v1/memories/{alpha_id}");
    assert_eq!(request(app, "GET", &alpha_path, Some(TOKEN)).await?.status(), StatusCode::NOT_FOUND);
    assert_eq!(send(app, "PATCH", &alpha_path, Some(TOKEN), Some(json!({"content":"cannot change alpha"}))).await?.status(), StatusCode::NOT_FOUND);
    assert_eq!(send(app, "DELETE", &alpha_path, Some(TOKEN), None).await?.status(), StatusCode::NOT_FOUND);
    let detail = request(app, "GET", &saved_path, Some(TOKEN)).await?;
    assert_eq!(json_response(detail, StatusCode::OK).await?, saved);
    let edited = send(app, "PATCH", &saved_path, Some(TOKEN), Some(json!({"content":"updated beta memory"}))).await?;
    assert_eq!(json_response(edited, StatusCode::OK).await?["content"], "updated beta memory");
    let counts_after = scoped_outbox_counts(alpha, beta).await?;
    assert_eq!((counts_after.0, counts_after.1), (counts_before.0, counts_before.1));
    assert!(counts_after.2 > counts_before.2);
    assert_eq!(send(app, "DELETE", &saved_path, Some(TOKEN), None).await?.status(), StatusCode::NO_CONTENT);
    let reply = request(app, "GET", "/v1/memories", Some(TOKEN)).await?;
    assert_eq!(json_response(reply, StatusCode::OK).await?["items"], json!([beta_entry]));

    let child_sync = request(app, "GET", "/v1/sync/status", Some(TOKEN)).await?;
    assert_eq!(json_response(child_sync, StatusCode::OK).await?["state"], "offline");
    let blocked_sync = send(app, "POST", "/v1/sync/trigger", Some(TOKEN), Some(json!({}))).await?;
    assert_eq!(json_response(blocked_sync, StatusCode::CONFLICT).await?["code"], "space_operation_unavailable");
    let diary = send(app, "POST", "/v1/diary", Some(TOKEN), Some(json!({
        "date": "2026-01-02", "content": "device-local beta diary",
    }))).await?;
    let diary = json_response(diary, StatusCode::OK).await?;
    let diary_path = "/v1/diary?date=2026-01-02";
    let implicit_diary = request(app, "GET", diary_path, Some(TOKEN)).await?;
    assert_eq!(json_response(implicit_diary, StatusCode::OK).await?["entries"], json!([diary]));
    let alpha_diary = request(app, "GET", &format!("{diary_path}&space_id={alpha}"), Some(TOKEN)).await?;
    assert_eq!(json_response(alpha_diary, StatusCode::OK).await?["entries"], json!([]));
    let counts_after_diary = scoped_outbox_counts(alpha, beta).await?;
    assert_eq!((counts_after_diary.0, counts_after_diary.1), (counts_before.0, counts_before.1));

    let restored = send(app, "PATCH", &format!("{BASE}/main"), Some(TOKEN), Some(json!({
        "active":true,
    }))).await?;
    assert_eq!(json_response(restored, StatusCode::OK).await?,
        json!({"id":"main","name":"main","role":"owner","active":true}));
    let reset_label = send(app, "PATCH", &beta_path, Some(TOKEN), Some(json!({"name":"offline beta"}))).await?;
    assert_eq!(json_response(reset_label, StatusCode::OK).await?["active"], false);
    let original = request(app, "GET", "/v1/memories", Some(TOKEN)).await?;
    assert_eq!(&json_response(original, StatusCode::OK).await?, original_entries);
    Ok(())
}


async fn check_delete(app: &Router, root: &Path, alpha: &Value, beta: &Value, foreign: &str) -> Result<()> {
    let target = beta["id"].as_str().context("delete beta public ID")?;
    let path = format!("{BASE}/{target}");
    let alpha_id = alpha["id"].as_str().context("surviving alpha public ID")?;
    let alpha_path = format!("{BASE}/{alpha_id}");
    let unknown = format!("{BASE}/local-{foreign}");
    let unauthorized = send(app, "DELETE", &path, None, None).await?;
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    let before = json_response(request(app, "GET", &alpha_path, Some(TOKEN)).await?, StatusCode::OK).await?;
    let reply = send(app, "DELETE", &unknown, Some(TOKEN), None).await?;
    assert_eq!(json_response(reply, StatusCode::NOT_FOUND).await?["code"], "space_not_found");
    let original = send(app, "DELETE", &format!("{BASE}/main"), Some(TOKEN), None).await?;
    assert_eq!(json_response(original, StatusCode::NOT_FOUND).await?["code"], "space_not_found");

    expired_delete_cannot_retire(target).await?;
    assert_eq!(json_response(request(app, "GET", &path, Some(TOKEN)).await?, StatusCode::OK).await?, *beta);
    retire_errors::post_detach_error_is_uncertain(app).await?;
    let untouched = retire_errors::fill_quarantine(app, root, &path).await?;
    let deleted = send(app, "DELETE", &path, Some(TOKEN), None).await?;
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert!(axum::body::to_bytes(deleted.into_body(), 0).await?.is_empty());
    expect_missing(app, target).await?;
    for file in untouched {
        assert_eq!(fs::read(file)?, b"unrecognized fixture data");
    }
    let absent = request(app, "GET", &format!("/v1/memories?space_id={target}"), Some(TOKEN)).await?;
    assert_eq!(json_response(absent, StatusCode::NOT_FOUND).await?["code"], "space_not_found");
    let already = send(app, "DELETE", &path, Some(TOKEN), None).await?;
    assert_eq!(json_response(already, StatusCode::NOT_FOUND).await?["code"], "space_not_found");
    let storage = LocalSpaceNamespace::open_existing(root)?.context("native area missing")?;
    assert!(storage.receipt(foreign)?.is_some(), "failed authorization must not retire a foreign receipt");
    assert!(!storage.candidates()?.iter().any(|id| target.strip_prefix("local-") == Some(id.as_str())));
    assert_eq!(json_response(request(app, "GET", &alpha_path, Some(TOKEN)).await?, StatusCode::OK).await?, before);
    let reply = request(app, "GET", &format!("/v1/memories?space_id={alpha_id}"), Some(TOKEN)).await?;
    let page = json_response(reply, StatusCode::OK).await?;
    assert_eq!(page["items"][0]["content"], "Isolation fixture alpha");
    Ok(())
}

async fn expired_delete_cannot_retire(raw: &str) -> Result<()> {
    use crate::runtime_http::v1::{rest_dispatch, transport};
    use axum::response::IntoResponse;
    use respire_app::auth::DeviceSpaceRetirement;
    let parts = Request::get(BASE)
        .header(header::HOST, BOUND.to_string())
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(())?.into_parts().0;
    let scope = transport::authorize(&parts, BOUND, BOUND).await
        .map_err(|failure| anyhow::anyhow!("fixture authorization: {failure:?}"))?;
    let raw = raw.to_owned();
    let (ready, staged) = tokio::sync::oneshot::channel();
    let (resume, release) = std::sync::mpsc::channel();
    let (done, finished) = tokio::sync::oneshot::channel();
    let request = tokio::spawn(rest_dispatch::submit_device_space_delete(scope, move |ctx, admission| {
        let result = ctx.library.delete_device_space(ctx.ownership, &raw, || {
            let _ = ready.send(());
            release.recv_timeout(WAIT).is_ok() && ctx.may_commit() && admission.admit()
        });
        let _ = done.send(result.is_err());
        match result {
            Ok(DeviceSpaceRetirement::Retired) => Ok(()),
            _ => Err(transport::unavailable()),
        }
    }));
    tokio::time::timeout(WAIT, staged).await??;
    let response = tokio::time::timeout(WAIT, request).await??;
    assert_eq!(response.err().context("unadmitted deletion must time out")?.into_response().status(),
        StatusCode::SERVICE_UNAVAILABLE);
    resume.send(())?;
    assert!(tokio::time::timeout(WAIT, finished).await??);
    Ok(())
}
