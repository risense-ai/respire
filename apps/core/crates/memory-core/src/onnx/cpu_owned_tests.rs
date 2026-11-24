use super::*;
use crate::i18n::fixtures::onnx as text;
use crate::{bge::BgeEmbedder, search::Embedder};

#[test]
fn owned_cpu_two_lanes_outputs_failure_retirement_and_recovery() -> Result<()> {
    let model_path = std::env::var_os("RSRS_CORE_TEST_ORT_FILE").context("explicit ORT fixture required")?;
    let root = std::env::var_os("RSRS_CORE_TEST_MODEL_DIR").context("explicit original model fixture required")?;
    let root = std::path::PathBuf::from(root);
    let model = std::fs::read(model_path)?;
    let original = std::fs::read(root.join("onnx/model_quantized.onnx"))?;
    let tokenizer = std::fs::read(root.join("tokenizer.json"))?;
    let mut config = serde_json::json!({"engine":"cpu", "execution_timeout_secs":120,"asset_id":"owned-cpu-test"});
    let texts = [text::SHARED_WEIGHT_TEXT, "English semantic recall", "fn main() { println!(\"memory\"); }"];
    let baseline = BgeEmbedder::load_bytes("m3", &original, &tokenizer, &config)?;
    let expected: Vec<Vec<f32>> = texts.iter().map(|text| baseline.try_embed_query(text)).collect::<Result<_>>()?;
    reset_sessions()?;
    config["cpu_owned_ort_experiment"] = serde_json::json!(true);
    let foreground = BgeEmbedder::load_bytes("m3", &model, &tokenizer, &config)?;
    let background = foreground.for_background()?;
    for (text, expected) in texts.iter().zip(&expected) {
        let actual = foreground.try_embed_query(text)?;
        assert_eq!(&actual, expected, "complete normalized vector differs");
        assert_eq!(background.try_embed(text)?, actual);
    }
    let state = cpu_owned::status();
    assert_eq!(state["owners"], 1);
    assert_eq!(state["native_lanes"], 2);
    assert_eq!(state["model_bytes"], model.len());
    std::thread::scope(|scope| -> Result<()> {
        let fg = scope.spawn(|| foreground.try_embed_query(texts[0]));
        let bg = scope.spawn(|| background.try_embed(&"Background indexing a long document. ".repeat(350)));
        assert_eq!(fg.join().map_err(|_| anyhow!("foreground test panicked"))??, expected[0]);
        assert_eq!(bg.join().map_err(|_| anyhow!("background test panicked"))??.len(), 1024);
        Ok(())
    })?;
    let lease = foreground.lease()?;
    assert!(reset_sessions().is_err(), "reset must not retire an active whole-operation lease");
    assert_eq!(cpu_owned::status()["owners"], 1);
    drop(lease);
    reset_sessions()?;
    assert!(foreground.try_embed_query(texts[0]).is_err());
    assert!(background.try_embed(texts[0]).is_err());
    assert_eq!(cpu_owned::status()["owners"], 0, "idle compatibility handles must not pin the buffer");
    assert_eq!(cpu_owned::status()["native_lanes"], 0);
    for failed_lane in [1, 2] {
        cpu_owned::FAIL_LOAD_LANE.store(failed_lane, Ordering::Release);
        let failure = BgeEmbedder::load_bytes("m3", &model, &tokenizer, &config).err().context("lane fault should reject load")?;
        assert!(format!("{failure:#}").contains("construction failure"));
        assert_eq!(cpu_owned::status()["owners"], 0);
        assert_eq!(cpu_owned::status()["native_lanes"], 0);
        let recovered = BgeEmbedder::load_bytes("m3", &model, &tokenizer, &config)?;
        assert_eq!(recovered.try_embed_query(texts[0])?, expected[0]);
        reset_sessions()?;
    }
    for failed_lane in [1, 2] {
        cpu_owned::FAIL_SPAWN_LANE.store(failed_lane, Ordering::Release);
        let started = std::time::Instant::now();
        let failure = BgeEmbedder::load_bytes("m3", &model, &tokenizer, &config).err().context("spawn fault must reject load")?;
        assert!(format!("{failure:#}").contains("spawn failed"));
        assert!(started.elapsed() < Duration::from_secs(10), "spawn failure cleanup must return promptly");
        assert_eq!(cpu_owned::status()["owners"], 0, "spawn failure must join started lanes and release model");
        assert_eq!(cpu_owned::status()["native_lanes"], 0);
        let recovered = BgeEmbedder::load_bytes("m3", &model, &tokenizer, &config)?;
        assert_eq!(recovered.try_embed_query(texts[0])?, expected[0]);
        reset_sessions()?;
    }
    let callback = |_: *mut std::ffi::c_void, _: *mut std::ffi::c_void, _: *const std::ffi::c_void| -> Result<()> {
        anyhow::bail!("must not retain or silently ignore synchronous Host options")
    };
    let rejected = BgeEmbedder::load_bytes_with_native_options("m3", &model, &tokenizer, &config, &callback).err().context("callback mode must reject")?;
    assert!(format!("{rejected:#}").contains("does not support host profiling/provider callbacks"));
    assert_eq!(cpu_owned::status()["owners"], 0);
    cpu_owned::verify_native_failure_cancel_and_recovery(&model, InferenceOptions::from_config(&config)?)?;
    cpu_owned::verify_real_lane_panic_teardown(&model, InferenceOptions::from_config(&config)?)?;
    Ok(())
}

#[test]
fn streamed_owned_input_parity_failures_and_shared_lifetime() -> Result<()> {
    let model_path = std::env::var_os("RSRS_CORE_TEST_ORT_FILE").context("explicit ORT fixture required")?;
    let root = std::path::PathBuf::from(std::env::var_os("RSRS_CORE_TEST_MODEL_DIR").context("explicit tokenizer fixture required")?);
    let tokenizer = std::fs::read(root.join("tokenizer.json"))?;
    let config = serde_json::json!({"engine":"cpu", "execution_timeout_secs":120,"asset_id":"streamed-owned-test", "cpu_owned_ort_experiment":true});
    let model = std::fs::read(&model_path)?;
    let baseline = BgeEmbedder::load_bytes("m3", &model, &tokenizer, &config)?;
    drop(model);
    let texts = [text::ORDERED_BUFFER_TEXT, "English memory ownership", "fn main() { let x = 42; }"];
    let expected = texts.iter().map(|text| baseline.try_embed_query(text)).collect::<Result<Vec<_>>>()?;
    reset_sessions()?;
    let foreground = BgeEmbedder::load_owned_bytes("m3", std::fs::read(&model_path)?, &tokenizer, &config)?;
    let background = foreground.for_background()?;
    for (text, vector) in texts.iter().zip(&expected) {
        assert_eq!(foreground.try_embed_query(text)?, *vector);
        assert_eq!(background.try_embed(text)?, *vector);
    }
    // A matching cache hit consumes its otherwise unused owned upload.
    let cached = BgeEmbedder::load_owned_bytes("m3", std::fs::read(&model_path)?, &tokenizer, &config)?;
    assert_eq!(cpu_owned::status()["owners"], 1);
    drop(foreground); drop(background); drop(cached);
    let shared = BgeEmbedder::load_cached("m3", &config)?;
    assert_eq!(shared.try_embed_query(texts[0])?, expected[0]);
    let lease = shared.lease()?;
    let mut replacement = config.clone(); replacement["asset_id"] = serde_json::json!("replacement");
    assert!(BgeEmbedder::load_owned_bytes("m3", std::fs::read(&model_path)?, &tokenizer, &replacement).is_err());
    assert_eq!(shared.try_embed_query(texts[0])?, expected[0]);
    drop(lease); reset_sessions()?;
    for failed_lane in [1, 2] {
        cpu_owned::FAIL_LOAD_LANE.store(failed_lane, Ordering::Release);
        assert!(BgeEmbedder::load_owned_bytes("m3", std::fs::read(&model_path)?, &tokenizer, &config).is_err());
        assert_eq!(cpu_owned::status()["owners"], 0);
        assert_eq!(cpu_owned::status()["native_lanes"], 0);
        assert!(BgeEmbedder::load_cached("m3", &config).is_err());
        cpu_owned::FAIL_SPAWN_LANE.store(failed_lane, Ordering::Release);
        assert!(BgeEmbedder::load_owned_bytes("m3", std::fs::read(&model_path)?, &tokenizer, &config).is_err());
        assert_eq!(cpu_owned::status()["owners"], 0);
        assert_eq!(cpu_owned::status()["native_lanes"], 0);
    }
    cpu_owned::FAIL_OWNER_SPAWN.store(true, Ordering::Release);
    assert!(BgeEmbedder::load_owned_bytes("m3", std::fs::read(&model_path)?, &tokenizer, &config).is_err());
    assert_eq!(cpu_owned::status()["owners"], 0);
    assert!(BgeEmbedder::load_owned_bytes("m3", std::fs::read(&model_path)?, b"invalid tokenizer", &config).is_err());
    assert_eq!(cpu_owned::status()["owners"], 0);
    let recovered = BgeEmbedder::load_owned_bytes("m3", std::fs::read(&model_path)?, &tokenizer, &config)?;
    assert_eq!(recovered.try_embed_query(texts[0])?, expected[0]);
    reset_sessions()?;
    let model = std::fs::read(&model_path)?;
    cpu_owned::verify_real_lane_panic_teardown(&model, InferenceOptions::from_config(&config)?)?;
    cpu_owned::verify_native_failure_cancel_and_recovery(&model, InferenceOptions::from_config(&config)?)?;
    Ok(())
}
