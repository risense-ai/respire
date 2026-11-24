use super::*;

#[test]
fn configured_chinese_query_input_preserves_the_fullwidth_separator() -> Result<()> {
    use crate::{embedding::EmbeddingProfile, i18n::fixtures::onnx as text};

    let model = "bge-small-zh-v1.5";
    let profile = EmbeddingProfile::from_config(model, &serde_json::json!({
        "embedding": {
            "model_id": "Xenova/bge-small-zh-v1.5",
            "revision": "75c43b069aac4d136ba6bc1122f995fedcfd2781",
            "dimensions": 512,
            "max_tokens": 512,
            "pooling": "cls",
            "normalize": true,
            "query_prefix": text::RETRIEVAL_QUERY_PREFIX,
            "document_prefix": "",
            "pad_token_id": 0,
            "pad_to_max_tokens": false
        }
    }))?;
    assert_eq!(
        query_input(&profile, text::SEMANTIC_RETRIEVAL_TEXT),
        text::EXPECTED_CHINESE_QUERY_INPUT,
    );
    assert_eq!(
        query_input(&EmbeddingProfile::m3(), text::SEMANTIC_RETRIEVAL_TEXT),
        text::SEMANTIC_RETRIEVAL_TEXT,
    );
    Ok(())
}

#[test]
fn reset_generation_and_canonical_slot_are_one_transition() -> Result<()> {
    use std::time::Duration;
    let Some(root) = std::env::var_os("RSRS_CORE_TEST_MODEL_DIR") else { return Ok(()); };
    let root = std::path::PathBuf::from(root);
    let model = std::fs::read(root.join("onnx/model_quantized.onnx"))?;
    let tokenizer = std::fs::read(root.join("tokenizer.json"))?;
    let config = serde_json::json!({"engine":"cpu", "execution_timeout_secs":120,
        "asset_id":"generation-transition-native-test"});
    let old = BgeEmbedder::load_bytes("m3", &model, &tokenizer, &config)?;
    let (entered_send, entered_receive) = std::sync::mpsc::channel();
    let (release_send, release_receive) = std::sync::mpsc::channel();
    crate::onnx::arm_reset_transition_hook(entered_send, release_receive)?;
    let reloaded = std::thread::scope(|scope| -> Result<BgeEmbedder> {
        let reset = scope.spawn(crate::onnx::reset_sessions);
        let entered = entered_receive.recv_timeout(Duration::from_secs(5));
        if let Err(error) = entered {
            let _ = release_send.send(());
            reset.join().map_err(|_| anyhow!("reset thread panicked"))??;
            return Err(anyhow!("reset did not reach transition hook: {error}"));
        }
        let shared_was_locked = SHARED.try_lock().is_err();
        let (started_send, started_receive) = std::sync::mpsc::channel();
        let (completed_send, completed_receive) = std::sync::mpsc::channel();
        let load = scope.spawn(move || {
            let _ = started_send.send(());
            let result = BgeEmbedder::load_bytes("m3", &model, &tokenizer, &config);
            let _ = completed_send.send(());
            result
        });
        let started = started_receive.recv_timeout(Duration::from_secs(1));
        let completed_before_release = completed_receive.recv_timeout(Duration::from_secs(2)).is_ok();
        let _ = release_send.send(());
        reset.join().map_err(|_| anyhow!("reset thread panicked"))??;
        let loaded = load.join().map_err(|_| anyhow!("load thread panicked"))??;
        started.map_err(|error| anyhow!("load did not start: {error}"))?;
        assert!(shared_was_locked, "canonical slot was unlocked before generation advanced");
        assert!(!completed_before_release, "load crossed the paused reset transition");
        Ok(loaded)
    })?;
    let generation = crate::onnx::current_generation_for_test();
    let foreground_generation = reloaded.session.lock().map_err(|_| anyhow!("session poisoned"))?
        .as_ref().context("missing foreground")?.generation_for_test();
    let background_generation = reloaded.background_session.as_ref().context("missing background")?
        .lock().map_err(|_| anyhow!("session poisoned"))?.as_ref().context("missing background")?.generation_for_test();
    assert_eq!(foreground_generation, generation);
    assert_eq!(background_generation, generation);
    assert_eq!(reloaded.try_embed_query("new generation works")?.len(), 1024);
    let active = reloaded.lease()?;
    assert!(crate::onnx::reset_sessions().is_err());
    assert_eq!(crate::onnx::current_generation_for_test(), generation);
    drop(active);
    assert!(old.try_embed_query("retired generation").is_err());
    Ok(())
}

#[test]
fn queued_embeddings_and_unwinding_protect_resource_lifetime() -> Result<()> {
    use std::time::{Duration, Instant};
    let Some(root) = std::env::var_os("RSRS_CORE_TEST_MODEL_DIR") else { return Ok(()); };
    let root = std::path::PathBuf::from(root);
    let model = std::fs::read(root.join("onnx/model_quantized.onnx"))?;
    let tokenizer = std::fs::read(root.join("tokenizer.json"))?;
    let config = serde_json::json!({"engine":"cpu", "execution_timeout_secs":120,
        "asset_id":"queued-lifetime-native-test"});
    let foreground = BgeEmbedder::load_bytes("m3", &model, &tokenizer, &config)?;
    let background = foreground.for_background()?;
    drop(model);
    drop(tokenizer);
    for (provider, queue) in [(&foreground, &crate::inference_queue::QUEUE),
                             (&background, &crate::inference_queue::BACKGROUND_QUEUE)] {
        std::thread::scope(|scope| -> Result<()> {
            // Keep admission occupied while the other thread obtains its own
            // internal embedding lease. No outer lease masks that requirement.
            let permit = queue.acquire(Duration::from_secs(1))?;
            let job = scope.spawn(|| provider.try_embed("queued operation survives busy retirement"));
            let deadline = Instant::now() + Duration::from_secs(5);
            while queue.status()?.0 == 0 {
                anyhow::ensure!(Instant::now() < deadline, "embedding did not enter admission queue");
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(BgeEmbedder::drop_shared().is_err());
            assert!(foreground.session.lock().map_err(|_| anyhow!("session poisoned"))?.is_some());
            assert!(foreground.background_session.as_ref().context("missing background")?
                .lock().map_err(|_| anyhow!("session poisoned"))?.is_some());
            assert!(foreground.assets.read().map_err(|_| anyhow!("assets poisoned"))?.is_some());
            drop(permit);
            assert_eq!(job.join().map_err(|_| anyhow!("queued inference thread panicked"))??.len(), 1024);
            Ok(())
        })?;
    }
    // Read-lease RAII must release on unwinding without poisoning the assets.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        foreground.lease().map(|_lease| panic!("intentional model lease unwind"))
    }));
    assert!(outcome.is_err());
    crate::onnx::reset_sessions()?;
    assert!(foreground.session.lock().map_err(|_| anyhow!("session poisoned"))?.is_none());
    assert!(foreground.assets.read().map_err(|_| anyhow!("assets poisoned"))?.is_none());
    assert!(background.try_embed("retired background handle").is_err());
    Ok(())
}

#[test]
fn reset_does_not_wait_on_a_model_initialization_lock() -> Result<()> {
    let held = SHARED.lock().map_err(|_| anyhow!("shared model poisoned"))?;
    let started = std::time::Instant::now();
    assert!(crate::onnx::reset_sessions().is_err());
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    drop(held);
    assert!(!crate::inference_queue::QUEUE.status()?.1);
    assert!(!crate::inference_queue::BACKGROUND_QUEUE.status()?.1);
    Ok(())
}

#[test]
fn resident_lanes_share_assets_but_not_sessions_or_admission() -> Result<()> {
    let Some(root) = std::env::var_os("RSRS_CORE_TEST_MODEL_DIR") else { return Ok(()); };
    let root = std::path::PathBuf::from(root);
    let model = std::fs::read(root.join("onnx/model_quantized.onnx"))?;
    let tokenizer = std::fs::read(root.join("tokenizer.json"))?;
    let config = serde_json::json!({"engine":"cpu", "execution_timeout_secs":120, "asset_id":"resident-native-test"});
    let foreground = BgeEmbedder::load_bytes("m3", &model, &tokenizer, &config)?;
    let background = foreground.for_background()?;
    let reused = BgeEmbedder::load_bytes("m3", &model, &tokenizer, &config)?;
    assert!(!Arc::ptr_eq(foreground.lane_session()?, background.lane_session()?));
    assert!(Arc::ptr_eq(&foreground.session, &reused.session));
    assert!(Arc::ptr_eq(&foreground.assets, &background.assets));
    assert!(foreground.assets.read().map_err(|_| anyhow!("assets poisoned"))?
        .as_ref().context("missing assets")?.model_bytes.is_empty());
    // Native sessions must remain usable after the Host releases both input buffers.
    drop(model);
    drop(tokenizer);
    let _permit = crate::inference_queue::QUEUE.acquire(std::time::Duration::from_secs(1))?;
    let held = foreground.session.lock().map_err(|_| anyhow!("foreground session poisoned"))?;
    assert!(foreground.retire_assets().is_err());
    assert!(foreground.assets.read().map_err(|_| anyhow!("assets poisoned"))?.is_some());
    let (send, receive) = std::sync::mpsc::channel();
    let job = std::thread::spawn(move || {
        let result = background.try_embed("background indexing must run while foreground is occupied");
        let _ = send.send(result);
    });
    let result = receive.recv_timeout(std::time::Duration::from_secs(30))
        .map_err(|error| anyhow!("background lane did not complete independently: {error}"));
    drop(held);
    drop(_permit);
    job.join().map_err(|_| anyhow!("background inference thread panicked"))?;
    let vector = result??;
    assert_eq!(vector.len(), 1024);
    let foreground_vector = foreground.try_embed_query("foreground recall still works")?;
    assert_eq!(foreground_vector.len(), 1024);
    assert_eq!(foreground.probe("CPU validation after input release")?["ready"], true);
    let borrowed_assets = foreground.lease()?;
    assert!(crate::onnx::reset_sessions().is_err());
    assert!(foreground.session.lock().map_err(|_| anyhow!("session poisoned"))?.is_some());
    assert!(foreground.background_session.as_ref().context("missing background")?
        .lock().map_err(|_| anyhow!("session poisoned"))?.is_some());
    assert_eq!(foreground_vector, foreground.try_embed_query("foreground recall still works")?);
    drop(borrowed_assets);
    // Keep idle compatibility handles alive, as separate worker TLS Core states do.
    crate::onnx::reset_sessions()?;
    assert!(foreground.session.lock().map_err(|_| anyhow!("session poisoned"))?.is_none());
    assert!(foreground.assets.read().map_err(|_| anyhow!("assets poisoned"))?.is_none());
    assert!(reused.try_embed_query("retired handle cannot infer").is_err());
    let model = std::fs::read(root.join("onnx/model_quantized.onnx"))?;
    let tokenizer = std::fs::read(root.join("tokenizer.json"))?;
    let generation = crate::onnx::current_generation_for_test();
    let execution_limit = crate::onnx::EXECUTION_LIMIT_SECS.load(std::sync::atomic::Ordering::Acquire);
    let mut probe_config = config.clone();
    probe_config["execution_timeout_secs"] = serde_json::json!(17);
    let observation = BgeEmbedder::backend_load_probe("m3", &model, &tokenizer, &probe_config,
        "cold backend load probe", None, Ok)?;
    assert_eq!(observation["fresh_session"], true);
    assert_eq!(observation["inference_completed"], true);
    assert_eq!(observation["selected"], "CPU");
    assert_eq!(observation["dimensions"], 1024);
    assert_eq!(observation["runtime"]["api_version"], ort::MINOR_VERSION);
    assert!(observation["runtime"]["symbols"]["Run"].as_str().is_some_and(|value| value.starts_with("0x")));
    assert!(SHARED.lock().map_err(|_| anyhow!("shared slot poisoned"))?.is_none());
    assert_eq!(crate::onnx::current_generation_for_test(), generation);
    assert_eq!(crate::onnx::EXECUTION_LIMIT_SECS.load(std::sync::atomic::Ordering::Acquire), execution_limit,
        "successful probe changed the resident timeout diagnostic");
    let reloaded = BgeEmbedder::load_bytes("m3", &model, &tokenizer, &config)?;
    drop(model);
    drop(tokenizer);
    assert!(reloaded.assets.read().map_err(|_| anyhow!("assets poisoned"))?
        .as_ref().context("missing assets")?.model_bytes.is_empty());
    assert_eq!(foreground_vector, reloaded.try_embed_query("foreground recall still works")?);
    assert_eq!(reloaded.try_embed_query(&"long input ".repeat(700))?.len(), 1024);
    Ok(())
}
#[test]
fn probe_distinguishes_engine_selection_from_node_placement() {
    assert_eq!(super::cpu_node_fallback_policy("Gpu: DmlExecutionProvider"), "allowed");
    assert_eq!(super::cpu_node_fallback_policy("CoreML CPUAndNeuralEngine"), "allowed");
    assert_eq!(super::cpu_node_fallback_policy("Npu: OpenVINOExecutionProvider"), "disabled");
    assert_eq!(super::cpu_node_fallback_policy("CPU"), "not_applicable");
    for invalid in [serde_json::Value::Null, serde_json::json!("false"), serde_json::json!(0), serde_json::json!(true)] {
        let config = serde_json::json!({"engine":"cpu", "execution_timeout_secs":120,
            "asset_id":"invalid-reuse-probe", "reuse_current":invalid});
        let result = BgeEmbedder::backend_load_probe("m3", b"model", b"{}", &config, "probe", None, Ok);
        assert!(result.is_err_and(|error| error.to_string().contains("reuse_current")), "invalid reuse flag must fail before native loading");
    }
}
