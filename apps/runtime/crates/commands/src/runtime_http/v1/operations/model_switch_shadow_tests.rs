use super::super::super::super::{rest_dispatch, transport};
use super::*;
use crate::rpc_serve::{self, ModelSwitchReservation};
use respire_app::{core_sdk::profiles, diary, MemoryEngine, MemoryTransport};
use std::time::Duration;

pub(super) static LARGE_LIBRARY_KEYS: std::sync::OnceLock<respire_app::SessionKeys> =
    std::sync::OnceLock::new();

fn seed_large_library(store: &LocalStore) -> Result<()> {
    let keys = LARGE_LIBRARY_KEYS
        .get()
        .context("large library fixture keys")?;
    store.write_transaction(|| {
        for index in 0..513 {
            let entry = super::super::super::super::writes::new_entry(
                "sse-fixture".to_owned(),
                format!("Shadow fixture {index}"),
                "Checkpoint coverage".to_owned(),
            );
            assert!(store.put(&MemoryEngine::seal_for_indexing(
                keys,
                &entry,
                "sse-fixture"
            )?)?);
        }
        Ok(())
    })
}

pub(super) async fn check_live_shadow(app: &Router) -> Result<()> {
    let store = LocalStore::open_existing(&service::database_path(&service::data_dir())?)?;
    let incumbent = store.active_selection()?.context("incumbent selection")?;
    seed_large_library(&store)?;
    let target = embedding::select(embedding::Choice {
        model: CANCELLED_MODEL.to_owned(),
        backend: "onnx".to_owned(),
        engine: respire_app::core_sdk::onnx::Engine::Cpu,
        variant: None,
    })?;
    let generation = respire_app::core_sdk::profiles::generation_key_for_profile(&target)?;

    let read = rpc_serve::incumbent_read_fixture();
    assert!(ModelSwitchReservation::reserve("fixture-download".into())?.is_none());
    let reservation =
        ModelSwitchReservation::reserve_switch("fixture-switch".into(), CANCELLED_MODEL)?
            .context("incumbent read should allow switch reservation")?;
    assert!(
        ModelSwitchReservation::reserve_switch("fixture-competing".into(), CANCELLED_MODEL)?
            .is_none()
    );
    drop(reservation);
    let (checkpoint, release) = crate::rpc_serve::pause_after_checkpoint(CANCELLED_MODEL)?;
    let operation = accept(app, CANCELLED_MODEL).await?;
    tokio::task::spawn_blocking(move || checkpoint.recv_timeout(WAIT)).await??;
    drop(read);
    let (done, total) = store.pending_candidate_counts(&generation)?;
    assert!(done > 0 && done < total && total > 512);
    assert_eq!(
        store.active_selection()?.context("incumbent")?.generation,
        incumbent.generation
    );
    let task = operation.rsplit('/').next().context("task ID")?;
    let progress = crate::model_progress::control(task, false)?;
    assert_eq!(progress["done"], done);
    assert_eq!(progress["total"], total);
    let state = legacy_state()?;
    assert_eq!(state["id"], task);
    assert_eq!(state["state"], "pending");
    assert_eq!(state["semantic"], "incumbent_ready");

    let listed = json_response(
        request(app, "GET", "/v1/memories", Some(TOKEN)).await?,
        StatusCode::OK,
    )
    .await?;
    let id = listed["items"][0]["id"].as_str().context("listed memory")?;
    let path = format!("/v1/memories/{id}");
    let recall = send(
        app,
        "POST",
        "/v1/memories/recall",
        Some(TOKEN),
        Some(json!({"query":"Synthetic title"})),
    );
    let show = request(app, "GET", &path, Some(TOKEN));
    let remember = send(
        app,
        "POST",
        "/v1/memories",
        Some(TOKEN),
        Some(json!({"title":"Shadow write","content":"Written while the incumbent served"})),
    );
    let status = request(app, "GET", "/v1/health", None);
    let (recall, show, remember, status) = tokio::try_join!(recall, show, remember, status)?;
    assert!(!json_response(recall, StatusCode::OK).await?["hits"]
        .as_array()
        .context("recall hits")?
        .is_empty());
    assert_eq!(json_response(show, StatusCode::OK).await?["id"], id);
    let remembered = json_response(remember, StatusCode::CREATED).await?;
    assert_eq!(
        json_response(status, StatusCode::OK).await?["status"],
        "ready"
    );
    crate::rpc_serve::check_shadow_admission_fixture()?;

    let response = request(app, "DELETE", &operation, Some(TOKEN)).await?;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    finish(app, &operation, "cancelled").await?;
    drop(release);
    assert_eq!(
        store.active_selection()?.context("incumbent")?.generation,
        incumbent.generation
    );
    assert!(store.pending_selection()?.is_none());
    assert_eq!(store.pending_candidate_counts(&generation)?.0, done);

    let (checkpoint, release) = crate::rpc_serve::pause_after_checkpoint(CANCELLED_MODEL)?;
    let resumed = legacy_start(
        "model.switch",
        vec![json!({"model":CANCELLED_MODEL,"backend":"onnx"}).to_string()],
    )?;
    tokio::task::spawn_blocking(move || checkpoint.recv_timeout(WAIT)).await??;
    assert!(store.pending_candidate_counts(&generation)?.0 > done);
    let added = json_response(
        send(
            app,
            "POST",
            "/v1/diary",
            Some(TOKEN),
            Some(json!({
                "date":"2026-11-27","content":"Diary written during the resumed shadow build"
            })),
        )
        .await?,
        StatusCode::OK,
    )
    .await?;
    let added_id = added["id"].as_str().context("new diary ID")?;
    let remembered_id = remembered["id"].as_str().context("concurrent memory ID")?;
    let patched = send(
        app,
        "PATCH",
        &format!("/v1/memories/{remembered_id}"),
        Some(TOKEN),
        Some(json!({"content":"Edited while the candidate was rebuilding"})),
    )
    .await?;
    assert_eq!(patched.status(), StatusCode::OK);
    assert_dispatch_selection(&incumbent.generation).await?;
    let read = rpc_serve::incumbent_read_fixture();
    release.send(())?;
    tokio::time::timeout(Duration::from_secs(30), async {
        while !rpc_serve::shadow_reads_paused_fixture() {
            anyhow::ensure!(
                rpc_serve::model_switch_running(),
                "shadow switch stopped before draining reads: {}",
                legacy_state()?
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("candidate did not reach cutover while the incumbent read was held")??;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(rpc_serve::shadow_reads_paused_fixture());
    assert_eq!(crate::model_progress::control(&resumed, false)?["phase"], "switch-diary");
    assert_eq!(
        store.active_selection()?.context("incumbent")?.generation,
        incumbent.generation
    );
    drop(read);
    finish_legacy(&resumed).await?;
    assert_eq!(
        store
            .active_selection()?
            .context("activated selection")?
            .generation,
        generation
    );
    assert_dispatch_selection(&generation).await?;
    let (indexed, total) = store.pending_candidate_counts(&generation)?;
    assert_eq!(indexed, total);
    assert!(total > 512);
    assert!(!diary::candidate_needs_rebuild(&store, &generation)?);
    assert!(store.active_memory_exact(added_id)?.is_some());

    store.queue_selection(&incumbent, "fixture-interrupted")?;
    let restore = legacy_start("model.resume", Vec::new())?;
    finish_legacy(&restore).await?;
    assert_dispatch_selection(&incumbent.generation).await?;
    check_curated_variants(&store, &incumbent).await
}

async fn check_curated_variants(store: &LocalStore, incumbent: &profiles::Selection) -> Result<()> {
    use respire_app::core_sdk::onnx::Engine;
    let choices = [
        (embedding::DEFAULT_MODEL, "quint8_avx2"), (embedding::DEFAULT_MODEL, "standard"),
        ("granite-97m-r2", "quint8_avx2"), ("granite-97m-r2", "standard"),
        ("m3", "q4f16"), ("m3", "fp16"),
    ];
    for (model, variant) in choices {
        let mut target = embedding::select_from_registry(embedding::Choice {
            model:model.to_owned(), backend:"onnx".to_owned(), engine:Engine::Cpu, variant:Some(variant.to_owned()),
        }, &embedding::builtin_registry()?)?;
        let mut weights = incumbent.profile.weights.as_ref().context("fixture weights")?.clone();
        weights.path = weights.path.with_file_name(format!("shadow-{model}-{variant}.onnx"));
        fs::copy(&incumbent.profile.weights.as_ref().context("fixture weights")?.path, &weights.path)?;
        target.profile.weights = Some(weights);
        target.profile.tokenizer = incumbent.profile.tokenizer.clone();
        target.profile.onnx = incumbent.profile.onnx.clone();
        for key in ["dimensions", "pooling", "max_tokens", "pad_token_id"] {
            target.profile.embedding[key] = incumbent.profile.embedding[key].clone();
        }
        assert!(rpc_serve::shadow_target(store, &mut target)?);
        let before = store.active_generation_key()?;
        let (checkpoint, release) = rpc_serve::pause_after_checkpoint(model)?;
        store.queue_selection(&target, "variant-fixture")?;
        let task = legacy_start("model.resume", Vec::new())?;
        tokio::task::spawn_blocking(move || checkpoint.recv_timeout(WAIT)).await??;
        assert_eq!(store.active_generation_key()?, before);
        release.send(())?;
        finish_legacy(&task).await?;
        let selected = store.active_selection()?.context("activated variant")?;
        assert_eq!(selected.model, model);
        assert_eq!(selected.variant.as_deref(), Some(variant));
        assert_eq!(selected.generation, target.generation);
        assert_eq!(selected.profile.weights.context("activated weights")?.path,
            target.profile.weights.context("target weights")?.path);
    }
    store.queue_selection(incumbent, "variant-fixture-restore")?;
    let task = legacy_start("model.resume", Vec::new())?;
    finish_legacy(&task).await?;
    assert_dispatch_selection(&incumbent.generation).await
}

async fn assert_dispatch_selection(expected: &str) -> Result<()> {
    let bound = super::super::BOUND;
    let (parts, ()) = axum::http::Request::builder()
        .uri("/v1/models/active-assets")
        .header(axum::http::header::HOST, bound.to_string())
        .header(axum::http::header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(())?
        .into_parts();
    let scope = transport::authorize(&parts, bound, bound)
        .await
        .map_err(|problem| anyhow::anyhow!("{problem:?}"))?;
    let (active, stored) = rest_dispatch::submit(scope, rest_dispatch::Access::Read, |ctx| {
        let active = profiles::current().map_err(|_| transport::unavailable())?;
        let stored = ctx.store.active_selection().map_err(|_| transport::unavailable())?;
        Ok((active, stored))
    })
    .await
    .map_err(|problem| anyhow::anyhow!("{problem:?}"))?;
    assert_eq!(active.context("dispatch selection missing")?.generation, expected);
    assert_eq!(stored.context("stored selection missing")?.generation, expected);
    Ok(())
}

fn legacy_state() -> Result<Value> {
    let response = crate::rpc_serve::legacy_model_fixture("model.progress", Vec::new())?;
    anyhow::ensure!(response.ok, "legacy model.progress failed: {response:?}");
    Ok(response
        .envelope
        .context("model.progress envelope")?
        .summary["switch"]
        .clone())
}

fn legacy_start(method: &str, args: Vec<String>) -> Result<String> {
    let response = crate::rpc_serve::legacy_model_fixture(method, args)?;
    anyhow::ensure!(response.ok, "{method} failed: {response:?}");
    let pending = response.envelope.context("legacy switch envelope")?.summary;
    assert_eq!(pending["state"], "pending");
    assert_eq!(pending["semantic"], "incumbent_ready");
    response.operation_id.context("legacy model operation ID")
}

async fn finish_legacy(task: &str) -> Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while crate::rpc_serve::model_switch_running() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await?;
    let state = legacy_state()?;
    assert_eq!(state["id"], task);
    assert_eq!(state["state"], "ready", "{state}");
    Ok(())
}
