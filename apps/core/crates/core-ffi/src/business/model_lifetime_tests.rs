use super::*;

#[test]
fn failed_replacement_unloads_retired_state_and_full_reload_recovers() -> Result<()> {
    let Some(root) = std::env::var_os("RSRS_CORE_TEST_MODEL_DIR") else { return Ok(()); };
    let root = std::path::PathBuf::from(root);
    let model = std::fs::read(root.join("onnx/model_quantized.onnx"))?;
    let tokenizer = std::fs::read(root.join("tokenizer.json"))?;
    let mut state = State::new(false);
    let mut config = json!({"model":"m3","engine":"cpu","execution_timeout_secs":120,
        "asset_id":"replacement-failure-test","session_options_id":"A"});
    for failure_at in [1, 2] {
        state.load(&config, &model, &tokenizer)?;
        // Busy replacement must preserve both config and usable model A.
        {
            let (provider, _lease) = state.model("m3")?;
            config["session_options_id"] = json!("B");
            let blocked = BgeEmbedder::load_bytes("m3", &model, &tokenizer, &config);
            assert!(blocked.is_err());
            assert_eq!(provider.try_embed_query("healthy busy model")?.len(), 1024);
        }
        let calls = std::cell::Cell::new(0);
        let failed = state.load_with_options(&config, &model, &tokenizer, &|_, _, _| {
            calls.set(calls.get() + 1);
            ensure!(calls.get() != failure_at, "intentional replacement session failure");
            Ok(())
        });
        assert!(failed.is_err());
        assert_eq!(calls.get(), failure_at);
        assert!(state.provider.is_none());
        assert!(state.config.is_none());
        assert!(state.model("m3").is_err());
        assert!(BgeEmbedder::load_cached("m3", &config).is_err(), "no partial group may be published");
        config["session_options_id"] = json!("A");
        state.load(&config, &model, &tokenizer)?;
        {
            let (provider, _lease) = state.model("m3")?;
            assert_eq!(provider.try_embed_query("A recovered without restart")?.len(), 1024);
        }
        dispatch(&mut state, "engine_control", json!({"action":"reset"}))?;
    }
    Ok(())
}

#[test]
fn quality_recall_holds_a_lease_between_both_local_queries() -> Result<()> {
    let Some(root) = std::env::var_os("RSRS_CORE_TEST_MODEL_DIR") else { return Ok(()); };
    let root = std::path::PathBuf::from(root);
    let model = std::fs::read(root.join("onnx/model_quantized.onnx"))?;
    let tokenizer = std::fs::read(root.join("tokenizer.json"))?;
    let mut state = State::new(false);
    state.load(&json!({"model":"m3","engine":"cpu","execution_timeout_secs":120,
        "asset_id":"quality-operation-lease-test"}), &model, &tokenizer)?;
    drop(model);
    drop(tokenizer);
    let vector = {
        let (provider, _lease) = state.model("m3")?;
        provider.try_embed_query("needle")?
    };
    let encoded = STANDARD.encode(vector.iter().flat_map(|value| value.to_le_bytes()).collect::<Vec<_>>());
    let snapshot = json!({"id":"quality-id","deleted":false,"updated_at":"2026-10-11T00:00:00Z",
        "kind":"context","tags":"","title":"needle","project":"","computer":"",
        "parent_id":"","created_at":"2026-10-11T00:00:00Z","content_head":"needle",
        "recall_count":0,"importance":"important",
        "entry":{"id":"quality-id","kind":"Context","tags":[],"title":"needle","content":"needle",
            "user":"fixture","computer":"fixture","project":"","created_at":"2026-10-11T00:00:00Z",
            "updated_at":"2026-10-11T00:00:00Z","emotion":0,"parent_id":"","importance":"important"},
        "features":{"artifact":"","legacy":encoded,"chunks":[]}});
    let key = "quality-operation-lease-test";
    crate::resident::publish(json!({"key":key,"model":"m3","snapshots":[snapshot],"removed":[]}))?;
    struct Selector;
    impl crate::Transport for Selector {
        fn request(&mut self, _: &Value) -> Result<Value> {
            Ok(json!({"choices":[{"message":{"content":"{\"ids\":[\"quality-id\"]}"}}]}))
        }
    }
    crate::selection::RESET_BETWEEN_QUERIES.store(true, std::sync::atomic::Ordering::SeqCst);
    let result = crate::selection::recall(&mut state, json!({"model":"m3","resident_key":key,
        "query":{"text":"needle","limit":1},"mode":"quality",
        "provider":{"name":"fixture","model":"fixture-model"}}), &mut Selector);
    crate::resident::release(&json!({"key":key}))?;
    let result = result?;
    assert!(crate::selection::RESET_BETWEEN_QUERIES_BLOCKED.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(result["warning"], Value::Null);
    assert_eq!(result["items"][0]["entry"]["id"], "quality-id");
    respire_core::onnx::reset_sessions()?;
    Ok(())
}

#[test]
fn reset_retires_idle_handles_but_preserves_an_active_operation() -> Result<()> {
    let Some(root) = std::env::var_os("RSRS_CORE_TEST_MODEL_DIR") else { return Ok(()); };
    let root = std::path::PathBuf::from(root);
    let model = std::fs::read(root.join("onnx/model_quantized.onnx"))?;
    let tokenizer = std::fs::read(root.join("tokenizer.json"))?;
    let mut config = json!({"model":"m3","engine":"cpu","execution_timeout_secs":120,
        "asset_id":"two-handle-lifetime-test","reuse_current":false});
    let mut first = State::new(false);
    let mut second = State::new(false);
    first.load(&config, &model, &tokenizer)?;
    config["reuse_current"] = Value::Bool(true);
    second.load(&config, &[], &[])?;
    {
        let (provider, _model_use) = second.model("m3")?;
        assert!(dispatch(&mut first, "engine_control", json!({"action":"reset"})).is_err());
        assert_eq!(provider.try_embed_query("active operation survives busy reset")?.len(), 1024);
        assert!(first.provider.is_some());
    }
    dispatch(&mut first, "engine_control", json!({"action":"reset"}))?;
    assert!(second.provider.is_some(), "idle handle must remain alive for this regression");
    assert!(second.model("m3").is_err(), "idle handle must not keep retired resources usable");
    config["reuse_current"] = Value::Bool(false);
    second.load(&config, &model, &tokenizer)?;
    let (provider, _model_use) = second.model("m3")?;
    assert_eq!(provider.try_embed_query("new model generation works")?.len(), 1024);
    Ok(())
}
