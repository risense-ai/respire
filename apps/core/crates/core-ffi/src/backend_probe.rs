//! Fresh native load diagnostics. All resource I/O and module verification belong to the host.
use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use respire_core::{bge::BgeEmbedder, onnx::InferenceOptions};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Probe {
    config: Value,
    model_base64: String,
    tokenizer_base64: String,
    text: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HostIdentity {
    verified: bool,
    resources_verified: bool,
    provider: String,
    modules: Vec<ModuleIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    runtime_api_version: Option<u32>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ModuleIdentity {
    id: String,
    sha256: String,
    #[serde(default)]
    symbols: std::collections::BTreeMap<String, String>,
}

fn acknowledged(transport: &mut dyn crate::Transport, request: Value) -> Result<()> {
    let response = transport.request(&request)?;
    ensure!(response["ok"].as_bool() == Some(true), "host backend load probe {} unavailable or unsupported: {}", request["phase"], response);
    Ok(())
}

pub(super) fn verify_modules(evidence: Value, observation: &Value) -> Result<HostIdentity> {
    let evidence: HostIdentity = serde_json::from_value(evidence).context("invalid host backend identity")?;
    ensure!(evidence.verified && evidence.resources_verified, "host did not verify loaded backend modules and resources");
    ensure!(!evidence.provider.trim().is_empty(), "host backend identity missing provider");
    ensure!(!evidence.modules.is_empty(), "host backend identity missing modules");
    let selected = observation["selected"].as_str().context("backend observation missing selected provider")?;
    let expected_provider = match selected {
        "CPU" => Some("CPUExecutionProvider"),
        "Gpu: DmlExecutionProvider" => Some("DmlExecutionProvider"),
        "MLX CPU" | "MLX GPU" => Some(selected),
        "CoreML CPUAndGPU" | "CoreML CPUAndNeuralEngine" => Some("CoreMLExecutionProvider"),
        "Host" => None,
        value if value.starts_with("Npu: ") && !value[5..].trim().is_empty() => Some(&value[5..]),
        _ => anyhow::bail!("backend observation has an unsupported selected provider"),
    };
    if let Some(expected) = expected_provider {
        ensure!(evidence.provider == expected, "host provider identity differs from the selected backend");
    }
    let valid_address = |address: &str| address.strip_prefix("0x")
        .filter(|hex| !hex.is_empty() && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .and_then(|hex| usize::from_str_radix(hex, 16).ok()).is_some_and(|address| address != 0);
    for module in &evidence.modules {
        ensure!(!module.id.trim().is_empty(), "host backend module missing id");
        ensure!(module.sha256.len() == 64 && module.sha256.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)), "host backend module missing lowercase SHA-256");
        ensure!(module.symbols.values().all(|address| valid_address(address)), "host backend module has an invalid symbol address");
    }
    match observation["runtime"]["kind"].as_str() {
        Some("onnxruntime") => {
            ensure!(!selected.starts_with("MLX "), "ONNX runtime observation has an MLX provider");
            ensure!(observation["runtime"]["api_address"].as_str().is_some_and(valid_address), "backend observation has an invalid runtime API address");
            let version = observation["runtime"]["api_version"].as_u64().filter(|version| *version > 0)
                .context("backend observation missing runtime API version")?;
            ensure!(evidence.runtime_api_version.map(u64::from) == Some(version), "host runtime API verification differs from the loaded runtime");
            let symbols = observation["runtime"]["symbols"].as_object().context("backend observation missing runtime symbols")?;
            ensure!(["CreateSessionFromArray", "Run"].iter().all(|name| symbols.contains_key(*name)), "backend observation missing required runtime symbols");
            for (name, address) in symbols {
                let address = address.as_str().filter(|address| valid_address(address))
                    .context("backend observation has an invalid runtime symbol address")?;
                ensure!(evidence.modules.iter().any(|module| module.symbols.get(name).map(String::as_str) == Some(address)), "host did not identify the module containing runtime symbol {name}");
            }
        }
        Some("mlx") => {
            ensure!(matches!(selected, "MLX CPU" | "MLX GPU"), "MLX runtime observation has a non-MLX provider");
            ensure!(evidence.runtime_api_version.is_none(), "MLX backend identity must omit runtime API version");
        }
        _ => anyhow::bail!("backend observation missing supported runtime kind"),
    }
    Ok(evidence)
}

pub(super) fn dispatch(input: Value, transport: &mut dyn crate::Transport) -> Result<Value> {
    let input: Probe = serde_json::from_value(input).context("invalid backend load probe")?;
    let model = input.config["model"].as_str().filter(|value| !value.is_empty()).context("backend load probe missing model")?;
    respire_core::embedding::EmbeddingProfile::from_config(model, &input.config)?;
    let options = InferenceOptions::from_config(&input.config)?;
    let backend = input.config["backend"].as_str().unwrap_or("onnx");
    ensure!(matches!(backend, "onnx" | "mlx"), "backend load probe unsupported for {backend}; native backend required");
    ensure!(backend != "mlx" || cfg!(all(feature = "mlx", target_os = "macos", target_arch = "aarch64")), "MLX backend unavailable in this SDK build");
    ensure!(input.config["asset_id"].as_str().is_some_and(|value| !value.is_empty()), "host model configuration missing asset_id");
    ensure!(input.config.get("reuse_current").is_none_or(|value| value.as_bool() == Some(false)), "backend load probe reuse_current must be false when supplied");
    ensure!(!input.text.trim().is_empty(), "backend load probe requires nonempty text");
    let model_bytes = STANDARD.decode(&input.model_base64).context("invalid probe model base64")?;
    let tokenizer_bytes = STANDARD.decode(&input.tokenizer_base64).context("invalid probe tokenizer base64")?;
    ensure!(!model_bytes.is_empty() && !tokenizer_bytes.is_empty(), "backend load probe requires nonempty model and tokenizer buffers");
    // This phase precedes every ORT call, so the host can load its pinned native libraries.
    acknowledged(transport, json!({"operation":"backend_load_probe", "phase":"load", "backend":backend, "engine":options.engine}))?;
    if backend == "onnx" {
        respire_core::onnx::with_native_environment(|environment, api| {
            acknowledged(transport, json!({"operation":"backend_load_probe", "phase":"register",
                "environment_address":format!("{environment:p}"), "api_address":format!("{api:p}")}))
        })?;
    }
    let transport = std::cell::RefCell::new(transport);
    let configure = |options: *mut std::ffi::c_void, environment: *mut std::ffi::c_void, device: *const std::ffi::c_void| {
        respire_core::onnx::with_native_environment(|_, api| {
            acknowledged(*transport.borrow_mut(), json!({"operation":"backend_load_probe", "phase":"configure",
                "options_address":format!("{options:p}"), "environment_address":format!("{environment:p}"),
                "api_address":format!("{api:p}"), "device_address":format!("{device:p}"),
                "host_execution_provider":input.config["host_execution_provider"].as_bool() == Some(true),
                "execution_identity":input.config["execution_identity"]}))
        })
    };
    // BGE applies the same effective CPU-owned policy as inference and skips its options callback.
    let native_options = if backend == "onnx" {
        Some(&configure as &dyn Fn(_, _, _) -> Result<()>)
    } else { None };
    BgeEmbedder::backend_load_probe(model, &model_bytes, &tokenizer_bytes, &input.config, &input.text, native_options, |mut observation| {
        let evidence = transport.borrow_mut().request(&json!({"operation":"backend_load_probe", "phase":"verify", "observation":observation}))?;
        let evidence = verify_modules(evidence, &observation)?;
        observation["host_identity"] = serde_json::to_value(evidence)?;
        observation["ready"] = json!(true);
        observation["status"] = json!("loaded");
        observation["module_identity_source"] = json!("host_verified");
        Ok(observation)
    })
}
