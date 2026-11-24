use super::*;
use crate::i18n::fixtures::onnx as text;
use crate::bge::BgeEmbedder;

fn test_model_bytes() -> Option<(Vec<u8>, Vec<u8>)> {
    let dir = std::path::PathBuf::from(std::env::var_os("RSRS_CORE_TEST_MODEL_DIR")?);
    Some((std::fs::read(dir.join("onnx/model_quantized.onnx")).ok()?,
          std::fs::read(dir.join("tokenizer.json")).ok()?))
}

fn test_config() -> serde_json::Value {
    serde_json::json!({"engine":"cpu", "execution_timeout_secs":120, "asset_id":"explicit-native-test-fixture"})
}
use crate::search::Embedder;

#[test]
fn intra_op_is_pinned_to_one() {
    assert_eq!(INTRA_OP_THREADS, 1);
}

#[test]
fn common_host_options_run_before_model_load_and_propagate_errors() -> Result<()> {
    let calls = std::cell::Cell::new(0usize);
    let callback = |options: *mut std::ffi::c_void, environment: *mut std::ffi::c_void,
                    _device: *const std::ffi::c_void| -> Result<()> {
        anyhow::ensure!(!options.is_null() && !environment.is_null(), "missing native options");
        calls.set(calls.get() + 1);
        anyhow::bail!("host-option-regression-sentinel")
    };
    let error = create(&[], InferenceOptions::from_config(&test_config())?, Some(&callback))
        .err().ok_or_else(|| anyhow!("host option failure must abort loading"))?;
    assert!(format!("{error:#}").contains("host-option-regression-sentinel"));
    assert_eq!(calls.get(), 1);
    let limit = EXECUTION_LIMIT_SECS.load(Ordering::Acquire);
    let mut probe_config = test_config();
    probe_config["execution_timeout_secs"] = serde_json::json!(17);
    let error = BgeEmbedder::backend_load_probe("m3", b"invalid-model", b"{}", &probe_config, "probe", Some(&callback), |_| {
        anyhow::bail!("failed native load must not reach identity inspection")
    }).err().ok_or_else(|| anyhow!("probe must use the inference native options callback"))?;
    assert!(format!("{error:#}").contains("host-option-regression-sentinel"));
    assert_eq!(calls.get(), 2);
    assert_eq!(EXECUTION_LIMIT_SECS.load(Ordering::Acquire), limit, "failed probe changed the resident timeout diagnostic");
    Ok(())
}

#[test]
fn thread_policy_validates_each_request_and_keeps_foreground_single_threaded() -> Result<()> {
    let mut config = test_config();
    let default = InferenceOptions::from_config(&config)?;
    assert_eq!(default.intra_threads, 1);
    assert_eq!(default.for_background().intra_threads, 1);
    for threads in [1, 4, 16] {
        config["background_intra_threads"] = serde_json::json!(threads);
        let policy = InferenceOptions::from_config(&config)?;
        assert_eq!(policy.intra_threads, 1);
        assert_eq!(policy.for_background().intra_threads, threads);
        if threads != 1 { assert_ne!(default, policy, "cache identity must include thread policy"); }
    }
    for invalid in [serde_json::json!(0), serde_json::json!(17), serde_json::json!(-1), serde_json::json!(1.5), serde_json::json!("4"), serde_json::Value::Null] {
        config["background_intra_threads"] = invalid;
        assert!(InferenceOptions::from_config(&config).is_err());
    }
    Ok(())
}

#[test]
fn concurrent_thread_policies_do_not_share_process_state() -> Result<()> {
    std::thread::scope(|scope| -> Result<()> {
        let checks: Vec<_> = [1, 4, 16].into_iter().map(|threads| scope.spawn(move || -> Result<()> {
            for _ in 0..100 {
                let config = serde_json::json!({"engine":"cpu","execution_timeout_secs":120,"background_intra_threads":threads});
                let options = InferenceOptions::from_config(&config)?;
                assert_eq!(options.intra_threads, 1);
                assert_eq!(options.for_background().intra_threads, threads);
            }
            Ok(())
        })).collect();
        for check in checks { check.join().map_err(|_| anyhow!("policy test panicked"))??; }
        Ok(())
    })
}

#[test]
fn native_failure_does_not_disable_the_same_session() -> Result<()> {
    let Some((model, _)) = test_model_bytes() else { return Ok(()); };
    let mut session = commit_onnx_session_for(&model, InferenceOptions::from_config(&test_config())?)?;
    let error = session.run(&[i64::MAX], &[1], &[0], 1, 1)
        .err().ok_or_else(|| anyhow!("out-of-vocabulary input should fail in ORT"))?;
    let message = format!("{error:#}");
    assert!(message.contains("indices") || message.contains("Gather"), "native cause missing: {message}");
    let cancelled = ort::session::RunOptions::new()?;
    cancelled.terminate()?;
    let cancel_error = run_local(&mut session.session, &session.selected, &[0], &[1], &[0], 1, 1, &cancelled, &crate::embedding::OnnxNames::default())
        .err().ok_or_else(|| anyhow!("cancelled run should fail in ORT"))?;
    assert!(format!("{cancel_error:#}").contains("terminate"));
    let deadline_error = session.run_with_deadline(&[0; 512], &[1; 512], &[0; 512], 1, 512, std::time::Duration::ZERO)
        .err().ok_or_else(|| anyhow!("expired native execution should return no embedding"))?;
    assert!(format!("{deadline_error:#}").contains("execution timed out"));
    assert!(!inference_stalled(), "completed cancellation must clear the stall state");
    let output = session.run(&[0], &[1], &[0], 1, 1)?;
    assert!(matches!(&output, InferenceOutput::Native(_)));
    let (shape, vector) = output.tensor()?;
    assert_eq!(shape, &[1, 1, 1024]);
    assert_eq!(vector.len(), 1024);
    assert!(vector.iter().all(|value| value.is_finite()));
    Ok(())
}

// Explicit small pinned fixture; no download, production model directory, or
// inactive success. Run with --ignored and RSRS_CORE_TEST_SMALL_MODEL_DIR.
#[test]
#[ignore = "requires the pinned BGE Small ONNX and tokenizer fixtures"]
fn pinned_small_borrowed_onnx_integrity_lanes_and_cancellation() -> Result<()> {
    let root = std::path::PathBuf::from(std::env::var_os("RSRS_CORE_TEST_SMALL_MODEL_DIR")
        .context("explicit BGE Small fixture directory required")?);
    let model = std::fs::read(root.join("model_quantized.onnx"))?;
    let tokenizer = std::fs::read(root.join("tokenizer.json"))?;
    let name = "bge-small-zh-v1.5";
    let mut config = serde_json::json!({
        "engine": "cpu", "execution_timeout_secs": 120, "asset_id": "small-pinned-test",
        "model_sha256": "15b717c382bcb518ba457b93ea6850ede7f4f1cd8937454aa06972366cd19bcc",
        "tokenizer_sha256": "48cea5d44424912a6fd1ea647bf4fe50b55ab8b1e5879c3275f80e339e8fae26",
        "embedding": {
            "model_id": "Xenova/bge-small-zh-v1.5",
            "revision": "75c43b069aac4d136ba6bc1122f995fedcfd2781",
            "dimensions": 512, "max_tokens": 512, "pooling": "cls", "normalize": true,
            "query_prefix": text::RETRIEVAL_QUERY_PREFIX, "document_prefix": "",
            "pad_token_id": 0, "pad_to_max_tokens": false
        },
        "onnx": {
            "input_ids": "input_ids", "attention_mask": "attention_mask",
            "token_type_ids": "token_type_ids", "output": "last_hidden_state"
        }
    });
    let model_hash = config["model_sha256"].take();
    config["model_sha256"] = serde_json::json!("0".repeat(64));
    let error = BgeEmbedder::load_bytes(name, &model, &tokenizer, &config)
        .err().context("mismatched model must be rejected before native load")?;
    assert!(format!("{error:#}").contains("host-supplied model_sha256 differs"));
    config["model_sha256"] = model_hash;
    let provider = BgeEmbedder::load_bytes(name, &model, &tokenizer, &config)?;
    assert_eq!(provider.index_identity(), crate::embedding::index_identity(name, &config)?);
    let background = provider.for_background()?;
    let text = text::SEMANTIC_RETRIEVAL_TEXT;
    let before = provider.try_embed(text)?;
    assert_eq!(before, background.try_embed(text)?);
    assert_eq!(before.len(), 512);
    assert!(before.iter().all(|value| value.is_finite()));
    assert!(before.iter().any(|value| value.abs() > 1e-8));
    let mut changed_tokenizer = tokenizer;
    changed_tokenizer.push(b' ');
    let error = BgeEmbedder::load_bytes(name, &model, &changed_tokenizer, &config)
        .err().context("matching cache must still verify actual tokenizer bytes")?;
    assert!(format!("{error:#}").contains("host-supplied tokenizer_sha256 differs"));
    assert_eq!(provider.try_embed(text)?, before);

    let lease = provider.lease()?;
    assert!(reset_sessions().is_err());
    drop(lease);
    reset_sessions()?;
    assert!(provider.try_embed(text).is_err());
    assert!(background.try_embed(text).is_err());

    let profile = crate::embedding::EmbeddingProfile::from_config(name, &config)?;
    let names = serde_json::from_value(config["onnx"].clone())?;
    let mut session = commit_onnx_session_with_profile(
        &model, InferenceOptions::from_config(&config)?, None, &profile, &names)?;
    drop(model);
    let cancelled = session.run_with_deadline(&[0; 512], &[1; 512], &[0; 512], 1, 512, std::time::Duration::ZERO)
        .err().context("expired native run must not publish an embedding")?;
    assert!(format!("{cancelled:#}").contains("execution timed out"));
    assert!(!inference_stalled());
    let output = session.run(&[0], &[1], &[0], 1, 1)?;
    let (shape, values) = output.tensor()?;
    assert_eq!(shape, &[1, 1, 512]);
    assert_eq!(values.len(), 512);
    assert!(values.iter().all(|value| value.is_finite()));
    Ok(())
}

fn process_thread_count() -> Option<usize> {
    #[cfg(windows)]
    {
        let output = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!("(Get-Process -Id {}).Threads.Count", std::process::id()),
            ])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8(output.stdout).ok()?.trim().parse().ok()
    }
    #[cfg(unix)]
    {
        Some(std::fs::read_dir("/proc/self/task").ok()?.count())
    }
    #[cfg(not(any(windows, unix)))]
    {
        None
    }
}

#[test]
fn bge_embed_does_not_fan_out_intra_op_threads() -> Result<()> {
    let Some((model, tokenizer)) = test_model_bytes() else { return Ok(()); };
    let before = process_thread_count();
    let embedder = BgeEmbedder::load_bytes("m3", &model, &tokenizer, &test_config())?;
    for i in 0..8 {
        let vec = embedder.embed(&format!("ort intra-op cap regression {i}"));
        assert_eq!(vec.len(), 1024, "BGE-M3 hidden size");
    }
    if let (Some(before), Some(after)) = (before, process_thread_count()) {
        let spawned = after.saturating_sub(before);
        assert!(
            spawned < 8,
            "ORT intra-op must stay at 1 thread; process threads {before} -> {after} (+{spawned})"
        );
    }
    Ok(())
}

#[test]
fn bge_load_is_process_singleton() -> Result<()> {
    let Some((model, tokenizer)) = test_model_bytes() else { return Ok(()); };
    let a = BgeEmbedder::load_bytes("m3", &model, &tokenizer, &test_config())?;
    let after_first = crate::bge::FRESH_LOADS.load(std::sync::atomic::Ordering::SeqCst);
    let b = BgeEmbedder::load()?;
    a.probe("shared CPU probe")?;
    assert_eq!(
        crate::bge::FRESH_LOADS.load(std::sync::atomic::Ordering::SeqCst),
        after_first,
        "second load must reuse the same ORT session"
    );
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let vec = a.embed("singleton queue a");
            assert_eq!(vec.len(), 1024);
        });
        scope.spawn(|| {
            let vec = b.embed("singleton queue b");
            assert_eq!(vec.len(), 1024);
        });
    });
    assert_eq!(
        crate::bge::FRESH_LOADS.load(std::sync::atomic::Ordering::SeqCst),
        after_first,
        "concurrent embeds must not open a second session"
    );
    reset_sessions()?;
    assert!(a.probe("old session after explicit reset").is_err());
    BgeEmbedder::load_bytes("m3", &model, &tokenizer, &test_config())?.probe("new shared session after explicit reset")?;
    Ok(())
}
