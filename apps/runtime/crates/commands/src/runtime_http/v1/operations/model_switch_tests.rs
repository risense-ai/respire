use super::{
    events, fs, json, json_response, production_router_fixture, request, send, service, thread,
    Context, Result, Router, StatusCode, Value, TOKEN, WAIT,
};
use respire_app::{embedding, transport::local::LocalStore};
use sha2::{Digest, Sha256};

pub(super) const TEST: &str =
    "runtime_http::v1::operations::tests::model_switch_tests::switch_uses_serving_store_and_rolls_back";
const MODEL: &str = "synthetic-cpu";
const BAD_MODEL: &str = "synthetic-invalid-onnx";
const CANCELLED_MODEL: &str = "synthetic-cancelled";
const PATH: &str = "/v1/models/switch";

#[test]
fn switch_uses_serving_store_and_rolls_back() -> Result<()> {
    production_router_fixture(TEST)
}

pub(super) fn seed(store: &LocalStore, urk: [u8; 32]) -> Result<()> {
    use respire_app::{
        memory::search::HashingEmbedder, MemoryEngine, MemoryTransport, SessionKeys,
    };
    // ONNX opset 13, Identity of one float initializer shaped [1, 4].
    let weights = hex::decode(concat!(
        "08093a8a010a1c0a0566697865641209656d62656464696e6722084964656e74697479120973796e7468657469632a1f0801",
        "0804100122100000803f000000000000000000000000420566697865645a210a09696e7075745f69647312140a120807120e",
        "0a0208010a081206746f6b656e73621b0a09656d62656464696e67120e0a0c080112080a0208010a0208044202100d",
    ))?;
    let tokenizer = br#"{"version":"1.0","model":{"type":"WordLevel","vocab":{"[UNK]":0},"unk_token":"[UNK]"}}"#;
    let resource = |name: &str, bytes: &[u8]| -> Result<Value> {
        let path = service::data_dir().join(name);
        fs::write(&path, bytes)?;
        Ok(json!({"path":path,"size":bytes.len(),"sha256":format!("{:x}", Sha256::digest(bytes))}))
    };
    let good = resource("synthetic.onnx", &weights)?;
    let bad = resource("invalid.onnx", b"invalid synthetic ONNX")?;
    let tokenizer = resource("fixture-tokenizer.json", tokenizer)?;
    for (id, weights) in [(MODEL, &good), (BAD_MODEL, &bad), (CANCELLED_MODEL, &good)] {
        embedding::configure(embedding::Configure {
            backend_profile: None,
            model_profile: Some(serde_json::from_value(json!({
                "id": id,
                "embedding": {"model_id": id, "revision": "fixture-r1", "dimensions": 4,
                    "max_tokens": 32, "pooling": "sentence_embedding", "normalize": true,
                    "query_prefix": "", "document_prefix": "", "pad_token_id": 0,
                    "pad_to_max_tokens": false},
                "onnx": {"input_ids": "input_ids", "output": "embedding"},
                "weights": weights, "tokenizer": tokenizer,
            }))?),
        })?;
    }
    let keys = SessionKeys::from_urk(urk)?;
    let entry = super::super::super::writes::new_entry(
        "sse-fixture".to_owned(),
        "Synthetic title".to_owned(),
        "Synthetic body".to_owned(),
    );
    assert!(store.put(&MemoryEngine::seal(
        &keys,
        &HashingEmbedder::default(),
        &entry,
        "sse-fixture",
    )?)?);
    let mut diary = super::super::super::writes::new_entry(
        "sse-fixture".to_owned(),
        "Synthetic diary".to_owned(),
        "Synthetic diary fragment".to_owned(),
    );
    diary.importance = "trivial".to_owned();
    assert!(store.put(&MemoryEngine::seal(
        &keys,
        &HashingEmbedder::default(),
        &diary,
        "sse-fixture",
    )?)?);
    Ok(())
}

pub(super) async fn check_routes(app: &Router) -> Result<()> {
    let post = |token, body| send(app, "POST", PATH, token, Some(body));
    assert_eq!(
        post(None, json!({"model_id":MODEL})).await?.status(),
        StatusCode::UNAUTHORIZED
    );
    for body in [
        json!({"model_id":"missing-profile"}),
        json!({"model_id":MODEL,"engine":"gpu"}),
        json!({"model_id":MODEL,"variant":"missing-variant"}),
    ] {
        assert_eq!(
            post(Some(TOKEN), body).await?.status(),
            StatusCode::BAD_REQUEST
        );
    }
    service::set_workspace_mode("readonly")?;
    assert_eq!(
        post(Some(TOKEN), json!({"model_id":MODEL})).await?.status(),
        StatusCode::FORBIDDEN
    );
    service::set_workspace_mode("normal")?;
    let operation = accept(app, MODEL).await?;
    finish(app, &operation, "completed").await?;
    let active = request(app, "GET", "/v1/models/active-assets", Some(TOKEN)).await?;
    assert_eq!(
        json_response(active, StatusCode::OK).await?["active_model_id"],
        MODEL
    );

    let (release, wait) = std::sync::mpsc::channel();
    let (locked, ready) = std::sync::mpsc::channel();
    let gate = thread::spawn(move || {
        respire_app::core_sdk::profiles::exclusive(|| {
            locked.send(())?;
            wait.recv_timeout(WAIT)?;
            Ok(())
        })
    });
    ready.recv_timeout(WAIT)?;
    let operation = accept(app, CANCELLED_MODEL).await?;
    tokio::time::timeout(WAIT, async {
        loop {
            let reply = request(app, "GET", &operation, Some(TOKEN)).await?;
            let state = json_response(reply, StatusCode::OK).await?;
            if state["progress_percent"]
                .as_u64()
                .is_some_and(|percent| percent >= 5)
            {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await??;
    assert_eq!(
        post(Some(TOKEN), json!({"model_id":MODEL})).await?.status(),
        StatusCode::CONFLICT
    );
    let cancel = request(app, "DELETE", &operation, Some(TOKEN)).await?;
    assert_eq!(
        json_response(cancel, StatusCode::ACCEPTED).await?["state"],
        "running"
    );
    release.send(())?;
    gate.join()
        .map_err(|_| anyhow::anyhow!("fixture model gate panicked"))??;
    finish(app, &operation, "cancelled").await?;

    let operation = accept(app, BAD_MODEL).await?;
    finish(app, &operation, "failed").await?;
    let active = request(app, "GET", "/v1/models/active-assets", Some(TOKEN)).await?;
    assert_eq!(
        json_response(active, StatusCode::OK).await?["active_model_id"],
        MODEL
    );
    Ok(())
}

async fn accept(app: &Router, model: &str) -> Result<String> {
    let response = send(
        app,
        "POST",
        PATH,
        Some(TOKEN),
        Some(json!({"model_id":model})),
    )
    .await?;
    let body = json_response(response, StatusCode::ACCEPTED).await?;
    assert_eq!(body["status"], "pending");
    let id = body["operation_id"].as_str().context("operation id")?;
    uuid::Uuid::parse_str(id)?;
    Ok(format!("/v1/operations/{id}"))
}

async fn finish(app: &Router, operation: &str, state: &str) -> Result<()> {
    let response = events(app, operation, TOKEN).await?;
    let bytes =
        tokio::time::timeout(WAIT, axum::body::to_bytes(response.into_body(), 8192)).await??;
    let wire = std::str::from_utf8(&bytes)?;
    assert!(
        wire.lines().any(|line| line == format!("event: {state}")),
        "{wire}"
    );
    let reply = request(app, "GET", operation, Some(TOKEN)).await?;
    let body = json_response(reply, StatusCode::OK).await?;
    assert_eq!(body["kind"], "model_switch");
    assert_eq!(body["state"], state);
    Ok(())
}

pub(super) fn check_store(store: &LocalStore) -> Result<()> {
    let selection = store.active_selection()?.context("active model")?;
    assert_eq!(selection.model, MODEL);
    assert!(!selection.generation.is_empty());
    assert_eq!(store.retrieval_model()?, MODEL);
    assert!(!store.index_pending(MODEL)?);
    assert!(store.pending_selection()?.is_none());
    assert!(store.meta_get("embedding_pending_task_v1")?.is_none());
    assert!(!respire_app::diary::needs_rebuild(store)?);
    Ok(())
}
