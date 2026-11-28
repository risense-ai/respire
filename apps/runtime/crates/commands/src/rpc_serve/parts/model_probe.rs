use super::*;
use respire_app::{
    core_sdk::{model_probe, onnx::Engine},
    embedding,
};
use std::path::PathBuf;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    model: String,
    variant: String,
    engine: Engine,
    weights_path: Option<PathBuf>,
    tokenizer_path: Option<PathBuf>,
    companion_paths: Option<Vec<PathBuf>>,
    dimensions: Option<usize>,
    max_tokens: Option<u64>,
    query_prefix: Option<String>,
    pad_token_id: Option<u32>,
    granite: Option<bool>,
}

pub(super) fn respond(request: &RpcRequest) -> RpcResponse {
    let started = Instant::now();
    let raw = request
        .args
        .first()
        .context("model probe argument 0 must contain JSON")
        .and_then(|wire| serde_json::from_str::<Value>(wire).context("invalid model probe JSON"));
    let mut report = match raw
        .as_ref()
        .map_err(|error| anyhow::anyhow!("{error:#}"))
        .and_then(|value| probe(serde_json::from_value(value.clone())?))
    {
        Ok(value) => value,
        Err(error) => {
            let fields = raw.as_ref().ok();
            let get = |key| {
                fields
                    .and_then(|value| value.get(key))
                    .filter(|value| value.is_string())
            };
            json!({
                "ok":false,"model":get("model"),"variant":get("variant"),"engine":get("engine"),
                "execution_provider":null,"device":null,"dimensions":null,"vector_norm":null,
                "load_ms":null,"inference_ms":null,"weights_path":get("weights_path"),
                "os":std::env::consts::OS,"arch":std::env::consts::ARCH,
                "error_code":"invalid_candidate","error":format!("{error:#}"),
                "diagnostic_log":format!("Candidate selection rejected: {error:#}"),
            })
        }
    };
    report["total_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
    report["runtime_version"] = json!(bin_version());
    let mut response = status_response(request);
    response.envelope = Some(ResultEnvelope::new(
        "model.probe",
        OutputStatus::Ok,
        report,
        Vec::new(),
    ));
    response
}

fn probe(input: Input) -> Result<Value> {
    let choice = || embedding::Choice {
        model: input.model.clone(),
        backend: "onnx".to_owned(),
        engine: input.engine,
        variant: Some(input.variant.clone()),
    };
    let mut selection = embedding::select_from_registry(choice(), &embedding::builtin_registry()?)?;
    let configured = (input.weights_path.is_none() || input.tokenizer_path.is_none())
        .then(|| embedding::select(choice()))
        .transpose()?;
    let configured = configured.as_ref().map(|selection| &selection.profile);
    let assets = &mut selection.profile;
    let weights = assets
        .weights
        .as_mut()
        .context("candidate has no weights")?;
    let tokenizer = assets
        .tokenizer
        .as_mut()
        .context("candidate has no tokenizer")?;
    let path = |resource: &_, other: Option<&respire_app::core_sdk::profiles::Resource>| {
        let resource: &respire_app::core_sdk::profiles::Resource = resource;
        other
            .filter(|other| {
                other.size == resource.size && other.sha256.eq_ignore_ascii_case(&resource.sha256)
            })
            .map_or_else(|| resource.path.clone(), |other| other.path.clone())
    };
    weights.path = input.weights_path.unwrap_or_else(|| {
        path(weights, configured.and_then(|profile| profile.weights.as_ref()))
    });
    tokenizer.path = input.tokenizer_path.unwrap_or_else(|| {
        path(tokenizer, configured.and_then(|profile| profile.tokenizer.as_ref()))
    });
    weights.path = std::path::absolute(&weights.path)?;
    tokenizer.path = std::path::absolute(&tokenizer.path)?;
    if let Some(paths) = input.companion_paths {
        anyhow::ensure!(
            paths.len() == assets.companions.len(),
            "wrong number of companion paths"
        );
        for (resource, path) in assets.companions.iter_mut().zip(paths) {
            resource.path = std::path::absolute(path)?;
        }
    } else {
        for resource in &mut assets.companions {
            resource.path = weights.path.with_file_name("model.onnx_data");
        }
    }
    if let Some(tokens) = input.max_tokens {
        assets.embedding["max_tokens"] = json!(tokens);
    }
    if let Some(prefix) = input.query_prefix {
        assets.embedding["query_prefix"] = json!(prefix);
    }
    if let Some(pad) = input.pad_token_id {
        assets.embedding["pad_token_id"] = json!(pad);
    }
    if let Some(granite) = input.granite {
        if granite {
            assets
                .onnx
                .as_object_mut()
                .context("ONNX inputs missing")?
                .remove("token_type_ids");
        } else {
            assets.onnx["token_type_ids"] = json!("token_type_ids");
        }
    }
    let dimensions = input.dimensions.unwrap_or(usize::try_from(
        assets.embedding["dimensions"]
            .as_u64()
            .context("model dimensions missing")?,
    )?);
    anyhow::ensure!(
        (1..=65536).contains(&dimensions),
        "invalid expected dimensions"
    );
    Ok(model_probe::run(&selection, dimensions))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::Path, process::Command};

    fn with_probe_root(test: &str, check: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
        const CHILD_ROOT: &str = "RSRS_MODEL_PROBE_TEST_ROOT";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            return check(Path::new(&root));
        }
        let (_, test) = test.split_once("::").context("test path missing crate prefix")?;
        let root = tempfile::tempdir()?;
        let output = Command::new(std::env::current_exe()?)
            .args(["--exact", test, "--nocapture"])
            .env(CHILD_ROOT, root.path())
            .env("RSRS_DATA_DIR", root.path())
            .env("RSRS_CORE_TEST_MODE", "1")
            .env_remove("RSRS_DEFAULT_DATA_DIR")
            .output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        anyhow::ensure!(
            output.status.success() && stdout.contains("1 passed"),
            "model probe test child failed\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr),
        );
        Ok(())
    }

    fn request(input: &Value) -> Result<RpcRequest> {
        Ok(serde_json::from_value(json!({
            "v": PROTOCOL_V, "id": "probe-test", "method": "model.probe",
            "args": [input.to_string()],
        }))?)
    }

    #[test]
    fn explicit_paths_ignore_invalid_registry() -> Result<()> {
        with_probe_root(concat!(module_path!(), "::explicit_paths_ignore_invalid_registry"), |root| {
            fs::write(root.join("embedding-profiles-v1.json"), b"invalid JSON")?;
            let weights = root.join("candidate.onnx");
            let tokenizer = root.join("candidate-tokenizer.json");
            let companion = root.join("candidate.data");
            for path in [&weights, &tokenizer, &companion] {
                fs::write(path, b"candidate fixture")?;
            }
            let mut input = json!({
                "model": "m3", "variant": "fp32", "engine": "cpu",
                "weights_path": weights, "tokenizer_path": tokenizer,
                "companion_paths": [companion],
            });
            let run = |input: &Value| -> Result<Value> {
                Ok(respond(&request(input)?)
                    .envelope
                    .context("probe envelope missing")?
                    .summary)
            };
            let result = run(&input)?;
            assert_eq!(result["ok"], true, "{result}");
            assert_eq!(result["weights_path"], json!(std::path::absolute(weights)?));
            input["tokenizer_path"] = Value::Null;
            assert_eq!(run(&input)?["error_code"], "invalid_candidate");
            Ok(())
        })
    }

    #[test]
    fn dispatch_rejects_paused_model_probe() -> Result<()> {
        with_probe_root(concat!(module_path!(), "::dispatch_rejects_paused_model_probe"), |root| {
            let maintenance = root.join("runtime/autostart");
            fs::create_dir_all(&maintenance)?;
            fs::write(maintenance.join("maintenance.json"), br#"{"phase":"Paused"}"#)?;
            let (jobs, _) = std::sync::mpsc::channel();
            let response = dispatch_validated(request(&Value::Null)?, &jobs);
            assert!(!response.ok);
            assert_eq!(response.code.as_deref(), Some("runtime_maintenance"));
            Ok(())
        })
    }
}
