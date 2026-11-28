//! Candidate-only file-backed inference. No Core model or index state is used.
use crate::{onnx::Engine, profiles::Selection};
use anyhow::{anyhow, ensure, Context, Result};
use ort::{
    session::Session,
    value::{Tensor, TensorElementType},
};
use serde_json::{json, Value};
use std::time::Instant;
use tokenizers::{Tokenizer, TruncationParams};

mod providers;
#[cfg(test)]
mod tests;

const TEXT: &str = "Respire candidate embedding verification";

pub fn run(selection: &Selection, dimensions: usize) -> Value {
    if crate::env::var("RSRS_CORE_TEST_MODE").as_deref() == Ok("1") {
        return run_isolated(selection, dimensions, true);
    }
    let started = Instant::now();
    let result = (|| {
        let gate = crate::model_selection::probe_gate()?;
        drop(gate);
        crate::worker::CandidateWorker::probe_report(selection, dimensions)
    })();
    result.unwrap_or_else(|error| {
        let mut report = empty_report(selection);
        let error = format!("{error:#}");
        report["error_code"] = json!("probe_worker_failed");
        report["diagnostic_log"] = json!(format!("Isolated candidate worker failed: {error}"));
        report["error"] = json!(error);
        report["total_ms"] = json!(ms(started));
        report
    })
}

fn empty_report(selection: &Selection) -> Value {
    json!({
        "ok":false,"model":selection.model,"variant":selection.variant,"engine":selection.engine,
        "execution_provider":null,"device":null,"dimensions":null,"vector_norm":null,
        "load_ms":null,"inference_ms":null,"total_ms":null,
        "weights_path":selection.profile.weights.as_ref().map(|file| &file.path),
        "os":std::env::consts::OS,"arch":std::env::consts::ARCH,"runtime_version":env!("CARGO_PKG_VERSION"),
        "error_code":null,"error":null,"diagnostic_log":"",
    })
}

pub(crate) fn run_isolated(selection: &Selection, dimensions: usize, test_mode: bool) -> Value {
    let started = Instant::now();
    let mut report = empty_report(selection);
    let mut stage = "probe_busy";
    let mut log = vec!["Starting isolated candidate probe".to_owned()];
    let result = (|| -> Result<()> {
        let gate = crate::model_selection::probe_gate()?;
        stage = "invalid_candidate";
        selection.validate()?;
        ensure!(
            selection.backend_profile.backend == "onnx",
            "model probe requires an ONNX profile"
        );
        let assets = &selection.profile;
        let weights = assets.weights.as_ref().context("weights are required")?;
        let tokenizer = assets.tokenizer.as_ref().context("tokenizer is required")?;
        stage = "asset_verification_failed";
        let stage_dir = if test_mode {
            None
        } else {
            Some(tempfile::tempdir()?)
        };
        for (name, asset) in [("model.onnx", weights), ("tokenizer.json", tokenizer)]
            .into_iter()
            .chain(
                assets
                    .companions
                    .iter()
                    .map(|asset| ("model.onnx_data", asset)),
            )
        {
            log.push(format!("Checking {}", asset.path.display()));
            if let Some(dir) = &stage_dir {
                let copy = std::fs::File::create_new(dir.path().join(name))?;
                crate::model_selection::copy_verified(
                    &asset.path,
                    asset.size,
                    &asset.sha256,
                    copy,
                )?;
            } else {
                let metadata = std::fs::metadata(&asset.path)
                    .with_context(|| format!("read {}", asset.path.display()))?;
                ensure!(
                    metadata.is_file() && metadata.len() > 0,
                    "candidate asset is not a nonempty file"
                );
            }
        }
        drop(gate);
        let load = Instant::now();
        let session = if test_mode {
            report["execution_provider"] = json!("deterministic_test");
            report["device"] = json!(format!("{:?} fixture", selection.engine));
            None
        } else {
            stage = "execution_provider_unavailable";
            let window = assets.embedding["max_tokens"]
                .as_i64()
                .context("token window missing")?;
            let mut options = providers::configure(selection.engine, window, &mut log)?;
            log.push(format!(
                "Selected {} on {}",
                options.provider, options.device
            ));
            report["execution_provider"] = json!(options.provider);
            report["device"] = json!(options.device);
            stage = "model_load_failed";
            let dir = stage_dir
                .as_ref()
                .context("verified candidate directory missing")?;
            let tokenizer = Tokenizer::from_file(dir.path().join("tokenizer.json"))
                .map_err(|error| anyhow!("load tokenizer: {error}"))?;
            let session = options
                .builder
                .commit_from_file(dir.path().join("model.onnx"))?;
            Some((session, tokenizer))
        };
        report["load_ms"] = json!(ms(load));
        log.push("Candidate session loaded".to_owned());
        stage = "inference_failed";
        let inference = Instant::now();
        let configured_dims = usize::try_from(
            assets.embedding["dimensions"]
                .as_u64()
                .context("model dimensions missing")?,
        )?;
        let vector = match session {
            Some((mut session, tokenizer)) => embed(&mut session, selection, tokenizer)?,
            None => vec![1.0 / (configured_dims as f32).sqrt(); configured_dims],
        };
        report["inference_ms"] = json!(ms(inference));
        report["dimensions"] = json!(vector.len());
        stage = "invalid_embedding";
        ensure!(
            vector.len() == dimensions,
            "expected {dimensions} dimensions, got {}",
            vector.len()
        );
        ensure!(
            vector.iter().all(|value| value.is_finite()),
            "embedding contains non-finite values"
        );
        let norm = vector
            .iter()
            .map(|value| f64::from(*value).powi(2))
            .sum::<f64>()
            .sqrt();
        ensure!(norm.is_finite() && norm > 1e-8, "invalid embedding norm");
        report["vector_norm"] = json!(if assets.embedding["normalize"] == true {
            vector
                .iter()
                .map(|value| {
                    let normalized = (f64::from(*value) / norm) as f32;
                    f64::from(normalized).powi(2)
                })
                .sum::<f64>()
                .sqrt()
        } else {
            norm
        });
        log.push(format!("Verified {} finite embedding values", vector.len()));
        Ok(())
    })();
    match result {
        Ok(()) => report["ok"] = json!(true),
        Err(error) => {
            let error = format!("{error:#}");
            log.push(format!("{stage}: {error}"));
            report["error_code"] = json!(stage);
            report["error"] = json!(error);
        }
    }
    report["total_ms"] = json!(ms(started));
    report["diagnostic_log"] = json!(log.join("\n"));
    report
}

fn ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

fn embed(
    session: &mut Session,
    selection: &Selection,
    mut tokenizer: Tokenizer,
) -> Result<Vec<f32>> {
    let profile = &selection.profile;
    tokenizer.with_padding(None);
    let max_length = usize::try_from(
        profile.embedding["max_tokens"]
            .as_u64()
            .context("token window is missing")?,
    )?;
    tokenizer
        .with_truncation(Some(TruncationParams {
            max_length,
            ..Default::default()
        }))
        .map_err(|error| anyhow!("configure tokenizer: {error}"))?;
    let prefix = profile.embedding["query_prefix"]
        .as_str()
        .context("query prefix is missing")?;
    let encoding = tokenizer
        .encode(format!("{prefix}{TEXT}"), true)
        .map_err(|error| anyhow!("encode probe: {error}"))?;
    let mut ids = encoding.get_ids().to_vec();
    let mut mask = encoding.get_attention_mask().to_vec();
    let mut types = encoding.get_type_ids().to_vec();
    ensure!(!ids.is_empty(), "tokenizer produced no tokens");
    let fixed_length = session
        .inputs()
        .iter()
        .find(|input| profile.onnx["input_ids"] == input.name())
        .and_then(|input| input.dtype().tensor_shape())
        .and_then(|shape| shape.get(1))
        .copied()
        .filter(|length| *length > 0)
        .map(usize::try_from)
        .transpose()?;
    let padding_length = fixed_length
        .or_else(|| (profile.embedding["pad_to_max_tokens"] == true).then_some(max_length));
    if let Some(length) = padding_length {
        ensure!(
            length <= max_length && length >= ids.len(),
            "fixed ONNX input shape differs from the model token window"
        );
        let pad = u32::try_from(profile.embedding["pad_token_id"].as_u64().unwrap_or(0))?;
        ids.resize(length, pad);
        mask.resize(length, 0);
        types.resize(length, 0);
    }
    let mut inputs = Vec::<(String, ort::session::SessionInputValue<'_>)>::new();
    for input in session.inputs() {
        let data = if profile.onnx["input_ids"] == input.name() {
            &ids
        } else if profile.onnx["attention_mask"] == input.name() {
            &mask
        } else if profile.onnx["token_type_ids"] == input.name() {
            &types
        } else {
            anyhow::bail!("unexpected ONNX input {}", input.name())
        };
        let shape = [1, data.len()];
        let value = match input.dtype().tensor_type() {
            Some(TensorElementType::Int64) => Tensor::from_array((
                shape,
                data.iter().map(|v| i64::from(*v)).collect::<Vec<_>>(),
            ))?
            .into_dyn(),
            Some(TensorElementType::Int32) => Tensor::from_array((
                shape,
                data.iter()
                    .copied()
                    .map(i32::try_from)
                    .collect::<std::result::Result<Vec<_>, _>>()?,
            ))?
            .into_dyn(),
            other => anyhow::bail!("unsupported ONNX input type {other:?}"),
        };
        inputs.push((input.name().to_owned(), value.into()));
    }
    let output = profile.onnx["output"]
        .as_str()
        .context("output tensor name is missing")?;
    let outputs = session.run(inputs)?;
    let (shape, values) = outputs
        .get(output)
        .with_context(|| format!("output tensor {output} is missing"))?
        .try_extract_tensor::<f32>()?;
    match (profile.embedding["pooling"].as_str(), &**shape) {
        (Some("sentence_embedding"), [1, width]) if *width > 0 => Ok(values.to_vec()),
        (Some("cls"), [1, tokens, width])
            if usize::try_from(*tokens)? == ids.len() && *width > 0 =>
        {
            let width = usize::try_from(*width)?;
            Ok(values
                .get(..width)
                .context("truncated embedding output")?
                .to_vec())
        }
        (Some("mean"), [1, tokens, width])
            if usize::try_from(*tokens)? == ids.len() && *width > 0 =>
        {
            let width = usize::try_from(*width)?;
            let mut vector = vec![0.0; width];
            let mut count = 0.0;
            for (token, present) in values.chunks_exact(width).zip(mask) {
                if present == 0 {
                    continue;
                }
                count += 1.0;
                for (sum, value) in vector.iter_mut().zip(token) {
                    *sum += value;
                }
            }
            ensure!(count > 0.0, "mean pooling requires an unmasked token");
            for value in &mut vector {
                *value /= count;
            }
            Ok(vector)
        }
        _ => anyhow::bail!("unexpected embedding output shape {shape:?}"),
    }
}
