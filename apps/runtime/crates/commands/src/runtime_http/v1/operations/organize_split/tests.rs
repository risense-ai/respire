use super::{
    super::tests::{production_router_fixture, send, BOUND, TOKEN},
    *,
};
use anyhow::{Context, Result};
use axum::{body::to_bytes, http::header, Router};
use respire_app::{MemoryEntry, MemoryTransport};
use serde_json::{json, Value};
use std::{fs, path::Path, time::Duration};

pub(in crate::runtime_http::v1::operations) const TEST: &str =
    "runtime_http::v1::operations::organize_split::tests::scoped_split_on_production_router";
const PATH: &str = "/v1/organize/split";
const SPACE: &str = "outline-fixture";
const NODE: &str = "split-source";
const CHILD: &str = "split-existing-child";
const WAIT: Duration = Duration::from_secs(5);

#[test]
fn scoped_split_on_production_router() -> Result<()> {
    production_router_fixture(TEST)
}

fn text() -> String {
    format!(
        "# Opening\r\n{}\r\n\r\n# Closing\r\n{}\r\n",
        "opening words ".repeat(85),
        "closing words ".repeat(85),
    )
}

fn make_entry(id: &str, content: String) -> MemoryEntry {
    let mut entry = writes::new_entry("sse-fixture".into(), "Preserved title".into(), content);
    entry.id = id.into();
    entry
}

fn response_error(error: Problem) -> anyhow::Error {
    anyhow::anyhow!("{error:?}")
}

#[test]
fn structural_plan_preserves_source_and_fences() -> Result<()> {
    let inputs = [
        (text(), "# Closing"),
        (
            format!(
                "Start\n{}\n\nEnd\n---\n{}\n",
                "a".repeat(1300),
                "b".repeat(1300)
            ),
            "End\n---",
        ),
        (
            format!("{}\r\n\r\n{}", "a".repeat(1250), "b".repeat(1250)),
            "bbbb",
        ),
        (
            format!(
                "{}\n\n```text\n# Inside fence\n{}\n```\n",
                "a".repeat(1250),
                "b".repeat(1200)
            ),
            "```text\n# Inside fence",
        ),
        ("\u{1f642}".repeat(2_001), "\u{1f642}"),
    ];
    for (text, second) in inputs {
        let entry = make_entry("fixture", text.clone());
        let spec = plan::build(&entry).map_err(response_error)?;
        assert_eq!(spec.items.len(), 2);
        assert_eq!(spec.items[0].content.clone() + &spec.items[1].content, text);
        assert!(spec.items[1].content.starts_with(second));
        let summary = spec.summary.context("outline body")?;
        for item in &spec.items {
            assert!(item.content.chars().count() <= 2_000);
            assert!(item.title.chars().count() <= 20);
            assert!(summary.lines().any(|line| line == item.title));
            assert!(item.parent.is_empty());
        }
        let again = plan::build(&entry).map_err(response_error)?;
        assert_eq!(Some(summary), again.summary);
        assert_eq!(spec.items[1].content, again.items[1].content);
    }
    let short = make_entry("short", "x".repeat(2_000));
    assert_eq!(plan::build(&short).unwrap_err().body()["status"], 400);
    let atomic = make_entry("fence", format!("```\n{}\n```", "x".repeat(2_001)));
    assert_eq!(plan::build(&atomic).unwrap_err().body()["status"], 422);
    Ok(())
}

async fn fixture_scope() -> Result<RestScope> {
    let parts = axum::http::Request::post(PATH)
        .header(header::HOST, BOUND.to_string())
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(())?
        .into_parts()
        .0;
    transport::authorize(&parts, BOUND, BOUND)
        .await
        .map_err(response_error)
}

pub(in crate::runtime_http::v1::operations) async fn check(
    app: &Router,
    token_path: &Path,
) -> Result<()> {
    let scope = fixture_scope().await?;
    rest_dispatch::submit(scope.clone(), Access::Write, |ctx| {
        let mut source = make_entry(NODE, text());
        source.parent_id = "split-parent".into();
        source.tags = vec!["retained".into()];
        let mut child = make_entry(CHILD, "Pre-existing child".into());
        child.parent_id = NODE.into();
        let mut foreign = make_entry("split-foreign", text());
        foreign.user = "other-owner".into();
        let mut diary = make_entry("split-diary", text());
        diary.importance = "trivial".into();
        for entry in [
            source,
            child,
            foreign,
            diary,
            make_entry("split-parent", "Original parent".into()),
            make_entry("split-short", "Too short".into()),
            make_entry("split-fence", format!("```\n{}\n```", "x".repeat(2_001))),
        ] {
            ctx.library
                .create_memory(ctx.store, &entry)
                .map_err(|_| transport::unavailable())?;
        }
        Ok(())
    })
    .await
    .map_err(response_error)?;

    let post = |body| send(app, "POST", PATH, Some(TOKEN), Some(body));
    assert_eq!(
        send(app, "POST", PATH, None, Some(json!({"memory_id": NODE})))
            .await?
            .status(),
        401
    );
    assert_eq!(
        send(app, "GET", PATH, Some(TOKEN), None).await?.status(),
        405
    );
    assert_eq!(
        send(
            app,
            "POST",
            &format!("{PATH}?space_id=main"),
            Some(TOKEN),
            Some(json!({"memory_id": NODE}))
        )
        .await?
        .status(),
        400
    );
    for body in [
        json!({}),
        json!({"memory_id":" "}),
        json!({"memory_id": NODE, "space_id":""}),
    ] {
        problem(post(body).await?, 400, "invalid_request").await?;
    }
    for id in ["missing", "split-fore", "split-foreign"] {
        problem(
            post(json!({"memory_id":id})).await?,
            404,
            "memory_not_found",
        )
        .await?;
    }
    problem(
        post(json!({"memory_id":NODE,"space_id":"main"})).await?,
        404,
        "space_not_found",
    )
    .await?;
    problem(
        post(json!({"memory_id":"split-short"})).await?,
        400,
        "split_too_short",
    )
    .await?;
    for id in ["split-fence", "split-diary"] {
        problem(
            post(json!({"memory_id":id})).await?,
            422,
            "split_ineligible",
        )
        .await?;
    }
    let settings = respire_app::service::data_dir().join("agent.json");
    fs::write(&settings, br#"{"readonly":true}"#)?;
    let denied = post(json!({"memory_id":NODE})).await;
    fs::write(settings, b"{}")?;
    problem(denied?, 403, "write_disabled").await?;

    check_commit_errors(app, scope).await?;
    let receipt = value(post(json!({"memory_id":NODE,"space_id":SPACE})).await?, 202).await?;
    assert_eq!(receipt["status"], "pending");
    let operation = operation_path(&receipt)?;
    let events = terminal(app, &operation, "completed").await?;
    assert_eq!(events["kind"], "organize_split");
    assert_eq!(events["progress_percent"], 100);

    let page = value(
        send(app, "GET", "/v1/memories?limit=50", Some(TOKEN), None).await?,
        200,
    )
    .await?;
    let entries = page["items"].as_array().context("memory page entries")?;
    let outline = entries
        .iter()
        .find(|entry| entry["id"] == NODE)
        .context("split outline")?;
    assert_eq!(outline["title"], "Preserved title");
    assert_eq!(outline["parent_id"], "split-parent");
    assert_eq!(outline["tags"], json!(["retained"]));
    assert_eq!(outline["content"], "## Outline\n1. Opening\n2. Closing\n");
    let mut children: Vec<_> = entries
        .iter()
        .filter(|entry| entry["parent_id"] == NODE)
        .collect();
    children.sort_by(|a, b| a["title"].as_str().cmp(&b["title"].as_str()));
    assert_eq!(children.len(), 3);
    assert_eq!(children[2]["id"], CHILD);
    assert_eq!(children[2]["content"], "Pre-existing child");
    let restored = children[..2]
        .iter()
        .map(|entry| entry["content"].as_str().context("split child body"))
        .collect::<Result<Vec<_>>>()?
        .concat();
    assert_eq!(restored, text());

    fs::write(token_path, "rotated-split-fixture-bearer")?;
    for path in [operation.clone(), format!("{operation}/events")] {
        assert_eq!(
            send(app, "GET", &path, Some(TOKEN), None).await?.status(),
            401
        );
        problem(
            send(
                app,
                "GET",
                &path,
                Some("rotated-split-fixture-bearer"),
                None,
            )
            .await?,
            404,
            "operation_not_found",
        )
        .await?;
    }
    Ok(())
}

async fn check_commit_errors(app: &Router, scope: RestScope) -> Result<()> {
    let input = SplitRequest {
        memory_id: NODE.into(),
        space_id: None,
    };
    let prepared = rest_dispatch::submit(scope.clone(), Access::Write, move |ctx| {
        prepare(ctx, &input)
    })
    .await
    .map_err(response_error)?;
    let (stale, cancelled, rejected) = rest_dispatch::submit(scope, Access::Write, move |ctx| {
        let mut prepared = prepared;
        let count = ctx.store.count().map_err(|_| transport::unavailable())?;
        let apply = |candidate: &Prepared| {
            ctx.library.apply_split(
                ctx.store,
                &ctx.scope.account,
                &candidate.source,
                &candidate.spec,
                || Ok(()),
            )
        };
        prepared.spec.items[1].kind = "invalid-kind".into();
        assert!(apply(&prepared).is_err());
        assert_eq!(
            ctx.store.count().map_err(|_| transport::unavailable())?,
            count
        );
        let source = ctx
            .store
            .active_memory_exact(NODE)
            .map_err(|_| transport::unavailable())?
            .ok_or_else(writes::missing_memory)?;
        assert_eq!(source.ciphertext, prepared.source.ciphertext);
        prepared.spec.items[1].kind = "context".into();
        let unadmitted = ctx.library.apply_split(
            ctx.store,
            &ctx.scope.account,
            &prepared.source,
            &prepared.spec,
            || anyhow::bail!("fixture commit rejection"),
        );
        assert!(unadmitted.is_err());
        assert_eq!(
            ctx.store.count().map_err(|_| transport::unavailable())?,
            count
        );

        let cancelled = state::registry().begin(ctx.scope.clone(), "organize_split")?;
        assert!(cancelled.start()?);
        cancelled.entry().cancel()?;
        let input = SplitRequest {
            memory_id: NODE.into(),
            space_id: None,
        };
        execute(ctx, &cancelled, &input, &prepared)?;
        assert_eq!(
            ctx.store.count().map_err(|_| transport::unavailable())?,
            count
        );

        ctx.library
            .update_memory_exact(ctx.store, NODE, |entry| entry.title.push_str(" changed"))
            .map_err(|_| transport::unavailable())?
            .ok_or_else(writes::missing_memory)?;
        let job = state::registry().begin(ctx.scope.clone(), "organize_split")?;
        assert!(job.start()?);
        let input = SplitRequest {
            memory_id: NODE.into(),
            space_id: None,
        };
        job.fail_problem(&execute(ctx, &job, &input, &prepared).expect_err("stale plan accepted"));
        assert_eq!(
            ctx.store.count().map_err(|_| transport::unavailable())?,
            count
        );
        ctx.library
            .update_memory_exact(ctx.store, NODE, |entry| {
                entry.title = "Preserved title".into()
            })
            .map_err(|_| transport::unavailable())?
            .ok_or_else(writes::missing_memory)?;

        let rejected = Arc::new(state::registry().begin(ctx.scope.clone(), "organize_split")?);
        let mut expired = ctx.scope.clone();
        expired.generation += 1;
        let outcome =
            rest_dispatch::enqueue_write_result(expired, |_| panic!("rejected job executed"))?;
        Ok((job.id()?, cancelled.id()?, (rejected, outcome)))
    })
    .await
    .map_err(response_error)?;
    let rejected_path = format!(
        "/v1/operations/{}",
        rejected.0.id().map_err(response_error)?
    );
    supervise(rejected.0, rejected.1).await;
    let status = terminal(app, &format!("/v1/operations/{stale}"), "failed").await?;
    assert_eq!(status["error"]["code"], "memory_changed");
    terminal(app, &format!("/v1/operations/{cancelled}"), "cancelled").await?;
    let status = terminal(app, &rejected_path, "failed").await?;
    assert_eq!(status["error"]["code"], "unauthorized");
    Ok(())
}

pub(in crate::runtime_http::v1::operations) async fn check_device_scope(
    app: &Router,
    source_space: &str,
    other_space: &str,
) -> Result<()> {
    let memories = "/v1/memories";
    let page_path = |space| format!("{memories}?space_id={space}");
    let page = |path| send(app, "GET", path, Some(TOKEN), None);
    let source_page = page_path(source_space);
    let other_path = page_path(other_space);
    let before = value(page(&other_path).await?, 200).await?;
    let main = value(page(memories).await?, 200).await?;
    let created = value(
        send(
            app,
            "POST",
            memories,
            Some(TOKEN),
            Some(json!({
                "space_id": source_space, "content": text(),
            })),
        )
        .await?,
        201,
    )
    .await?;
    let id = created["id"].as_str().context("local split source ID")?;
    let post = |body| send(app, "POST", PATH, Some(TOKEN), Some(body));
    problem(
        post(json!({"memory_id":id,"space_id":other_space})).await?,
        404,
        "memory_not_found",
    )
    .await?;

    let select = |path, active| {
        send(
            app,
            "PATCH",
            path,
            Some(TOKEN),
            Some(json!({"active": active})),
        )
    };
    let source_path = format!("/v1/spaces/{source_space}");
    let selection_path = format!("/v1/spaces/{other_space}");
    value(select(&source_path, true).await?, 200).await?;
    let scope = fixture_scope().await?;
    let input = SplitRequest {
        memory_id: id.into(),
        space_id: None,
    };
    let prepared = rest_dispatch::submit(scope.clone(), Access::Write, move |ctx| {
        prepare(ctx, &input)
    })
    .await
    .map_err(response_error)?;
    assert_eq!(prepared.space, source_space);
    value(select(&selection_path, true).await?, 200).await?;
    let stale = rest_dispatch::submit(scope, Access::Write, move |ctx| {
        let job = state::registry().begin(ctx.scope.clone(), "organize_split")?;
        assert!(job.start()?);
        let input = SplitRequest {
            memory_id: prepared.source.id.clone(),
            space_id: None,
        };
        job.fail_problem(
            &execute(ctx, &job, &input, &prepared).expect_err("different selection accepted"),
        );
        job.id()
    })
    .await
    .map_err(response_error)?;
    let status = terminal(app, &format!("/v1/operations/{stale}"), "failed").await?;
    assert_eq!(status["error"]["code"], "split_space_changed");
    assert_eq!(value(page(&other_path).await?, 200).await?, before);

    let receipt = value(
        post(json!({"memory_id":id,"space_id":source_space})).await?,
        202,
    )
    .await?;
    terminal(app, &operation_path(&receipt)?, "completed").await?;
    let source = value(page(&source_page).await?, 200).await?;
    let entries = source["items"].as_array().context("local split items")?;
    let mut children: Vec<_> = entries
        .iter()
        .filter(|entry| entry["parent_id"] == id)
        .collect();
    children.sort_by(|a, b| a["title"].as_str().cmp(&b["title"].as_str()));
    assert_eq!(children.len(), 2);
    let restored = children
        .iter()
        .map(|entry| entry["content"].as_str().context("local split body"))
        .collect::<Result<Vec<_>>>()?
        .concat();
    assert_eq!(restored, text());
    assert_eq!(value(page(&other_path).await?, 200).await?, before);
    value(select(&selection_path, false).await?, 200).await?;
    assert_eq!(value(page(memories).await?, 200).await?, main);
    Ok(())
}

fn operation_path(receipt: &Value) -> Result<String> {
    let id = receipt["operation_id"].as_str().context("operation id")?;
    uuid::Uuid::parse_str(id)?;
    Ok(format!("/v1/operations/{id}"))
}

async fn terminal(app: &Router, operation: &str, phase: &str) -> Result<Value> {
    let response = send(
        app,
        "GET",
        &format!("{operation}/events"),
        Some(TOKEN),
        None,
    )
    .await?;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/event-stream"
    );
    let body = tokio::time::timeout(WAIT, to_bytes(response.into_body(), 8192)).await??;
    let wire = std::str::from_utf8(&body)?;
    assert!(
        wire.lines()
            .any(|line| line.strip_prefix("event: ") == Some(phase)),
        "{wire}"
    );
    let status = value(send(app, "GET", operation, Some(TOKEN), None).await?, 200).await?;
    assert_eq!(status["state"], phase);
    Ok(status)
}

async fn value(response: Response, status: u16) -> Result<Value> {
    assert_eq!(response.status(), status);
    Ok(serde_json::from_slice(
        &to_bytes(response.into_body(), 128 * 1024).await?,
    )?)
}

async fn problem(response: Response, status: u16, code: &str) -> Result<()> {
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/problem+json"
    );
    let body = value(response, status).await?;
    assert_eq!(body["status"], status);
    assert_eq!(body["code"], code);
    Ok(())
}
