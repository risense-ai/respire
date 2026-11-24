const MODEL_VARIANTS: &[(&str, u64, &str)] = &[
    ("quantized", 569694530, "0826f8c1ab9edf1801db86c61919d4d108e8bfc0b809ec823ad366882ff0b77d"),
    ("fp16", 1133992936, "4f1a646a3d4f39985589e9991a717044ede8278617fe55e3d246838bc05055e9"),
    ("int8", 568456694, "a206e10e995aa2a833924bcd725ba5dd6c3425cd34bac3cf2b5677cd2a1c51d6"),
    ("uint8", 568456765, "3222211565d0d5da2fe89e0c40856ed879e5c5dd3369fbbe685213469498e811"),
    ("q4", 1248237611, "4f6d9786712f8dbf00efbb08ffd819395ef05d860249e852b4efc16022f753a6"),
    ("q4f16", 699932840, "8305fdfa48b5702ecb4a6d84c1a52a4d1cb27d69880ff7f69d1a93d795864f25"),
];

fn host_asset(name: &str, revision: &str, path: PathBuf, size: u64, sha256: &str, source: String) -> Result<respire_protocol::AssetRequirement> {
    anyhow::ensure!(path.is_absolute() && size > 0 && sha256.len() == 64
        && sha256.bytes().all(|byte| byte.is_ascii_hexdigit()), "invalid host model asset pin");
    Ok(respire_protocol::AssetRequirement {
        name: name.to_owned(), version: Some(revision.to_owned()),
        path: Some(path.to_str().context("model path must be Unicode")?.to_owned()),
        expected_size: Some(size), sha256: Some(sha256.to_owned()), suggested_source: source,
        state: AssetState::Missing, actual_size: None, actual_sha256: None, reason: None,
    })
}

fn host_variant(directory: &Path, name: &str) -> Result<respire_protocol::AssetRequirement> {
    let normalized = name.trim().to_ascii_lowercase();
    let (variant, size, sha256) = MODEL_VARIANTS.iter().find(|(variant, _, _)| *variant == normalized)
        .context("model variant is absent from the host pinned catalog")?;
    let file = format!("onnx/model_{variant}.onnx");
    host_asset(if *variant == "quantized" { "bge-m3-baseline" } else { "bge-m3-model" },
        respire_commands::model_asset_files::MODEL_REVISION, directory.join(&file), *size, sha256,
        format!("https://huggingface.co/{}/resolve/{}/{file}", respire_commands::model_asset_files::MODEL_REPO, respire_commands::model_asset_files::MODEL_REVISION))
}

fn host_tokenizer(directory: &Path) -> Result<respire_protocol::AssetRequirement> {
    host_asset("bge-m3-tokenizer", respire_commands::model_asset_files::MODEL_REVISION,
        directory.join("tokenizer.json"), 17_082_821,
        "6710678b12670bc442b99edc952c4d996ae309a7020c1fa0096dd245c2faf790",
        format!("https://huggingface.co/{}/resolve/{}/tokenizer.json", respire_commands::model_asset_files::MODEL_REPO, respire_commands::model_asset_files::MODEL_REVISION))
}

fn host_cpu_container(path: &Path) -> Result<respire_protocol::AssetRequirement> {
    anyhow::ensure!(respire_client_host::env::var_os("RSRS_CPU_OWNED_ORT_FILE").is_none(),
        "unset RSRS_CPU_OWNED_ORT_FILE before preparing pinned weights");
    host_asset("bge-m3-cpu-ort", "bge-m3-cpu-ort-v1", path.to_owned(), 569654128,
        "7ff152e912bc234f5c90e4012f99897a7b2ce180f016db2321304057756b64ff",
        "https://github.com/risense-ai/respire-cli/releases/download/model-bge-m3-ort-v1/model_quantized.cpu-v1.ort".to_owned())
}

fn same_asset_pin(core: &respire_protocol::AssetRequirement, host: &respire_protocol::AssetRequirement) -> bool {
    core.path == host.path && core.expected_size == host.expected_size && core.sha256 == host.sha256
}

/// Runtime diagnostics describe core requirements, never executable download sources.
fn host_installation_plan(
    intent: &ModelInstallIntent,
    requirements: &AssetsStatus,
    selected: Option<&serde_json::Value>,
    directory: &Path,
    cpu_container: &Path,
) -> Result<AssetsStatus> {
    anyhow::ensure!(requirements.assets.len() == 2, "core selection must describe exactly its tokenizer and weights; prepare providers separately");
    let mut assets = Vec::new();
    let profile = selected.filter(|_| intent.for_engine.is_none());
    if let Some(selected) = profile {
        let backend = selected["backend_profile"]["backend"].as_str().context("profile backend missing")?;
        let engine = selected["engine"].as_str().context("profile engine missing")?;
        anyhow::ensure!((backend == "mlx" && cfg!(target_os = "macos"))
            || (backend == "onnx" && (engine == "cpu" || cfg!(any(windows, target_os = "macos")))),
            "selected profile backend is unsupported on this platform");
        let native = &selected["backend_profile"]["native"];
        anyhow::ensure!(engine != "npu" && (native.is_null()
            || (engine == "cpu" && native["provider"] == "CPUExecutionProvider")
            || (cfg!(windows) && engine == "gpu" && native["provider"] == "DmlExecutionProvider")),
            "native provider requires separate host component preparation and integrity attestation");
        let revision = selected["profile"]["embedding"]["revision"].as_str().context("profile revision missing")?;
        for (name, field) in [("profile-tokenizer", "tokenizer"), ("profile-weights", "weights")] {
            let resource = &selected["profile"][field];
            let path = PathBuf::from(resource["path"].as_str().context("profile resource path missing")?);
            let source = match &resource["download"] {
                serde_json::Value::Null => String::new(),
                serde_json::Value::String(source) => source.clone(),
                _ => anyhow::bail!("profile download source must be a string or null"),
            };
            assets.push(host_asset(name, revision, path,
                resource["size"].as_u64().context("profile resource size missing")?,
                resource["sha256"].as_str().context("profile resource SHA-256 missing")?,
                source)?);
        }
    } else {
        assets.push(host_tokenizer(directory)?);
        assets.push(host_variant(directory, "quantized")?);
        for requirement in &requirements.assets {
            match requirement.name.as_str() {
                "bge-m3-tokenizer" => {},
                "bge-m3-model" => {
                    let variant = requirement.version.as_deref().context("core model variant missing")?;
                    if variant != "quantized" { assets.push(host_variant(directory, variant)?); }
                },
                "bge-m3-cpu-ort" => {
                    // Runtime22 preparation pins the default cache independently of loader discovery.
                    assets.push(host_cpu_container(cpu_container)?);
                },
                _ => anyhow::bail!("core resource {} requires separate component preparation: {}", requirement.name,
                    requirement.reason.as_deref().unwrap_or("no host file installation contract")),
            }
        }
    }
    // Check identity and pins against the locally built catalog, ignoring diagnostic sources.
    let mut claimed = std::collections::BTreeSet::new();
    for requirement in &requirements.assets {
        anyhow::ensure!(claimed.insert(&requirement.name), "duplicate core asset requirement");
        anyhow::ensure!(requirement.state != AssetState::Incompatible,
            "core resource {} is incompatible: {}", requirement.name, requirement.reason.as_deref().unwrap_or("unsupported backend"));
        let host = assets.iter().find(|asset| asset.name == requirement.name
            || (requirement.name == "bge-m3-model" && requirement.version.as_deref() == Some("quantized")
                && asset.name == "bge-m3-baseline"))
            .context("core requirement is absent from the host installation catalog")?;
        anyhow::ensure!(same_asset_pin(requirement, host), "core/host model asset pin or destination mismatch for {}", requirement.name);
    }
    if let Some(extra) = &intent.model {
        for asset in [host_tokenizer(directory)?, host_variant(directory, "quantized")?, host_variant(directory, extra)?] {
            if let Some(existing) = assets.iter().find(|existing| existing.path == asset.path) {
                anyhow::ensure!(same_asset_pin(existing, &asset), "install intent has conflicting pins for one destination");
            } else { assets.push(asset); }
        }
    }
    Ok(AssetsStatus::installation_plan(assets))
}

/// Local policy only: the Runtime supplies artifact identity, never mirror authority.
fn local_model_sources(intent: &ModelInstallIntent, plan: &AssetsStatus) -> Result<Vec<Vec<String>>> {
    let config = respire_client_host::config::read_client_config(
        &respire_client_host::paths::client_config_path())?;
    let config_value = |name: &str| config.as_ref().and_then(|config| config[name].as_str()).map(str::to_owned);
    let mirror = intent.mirror.clone()
        .or_else(|| respire_client_host::env::var("RSRS_MIRROR").ok().filter(|value| !value.trim().is_empty()))
        .or_else(|| config_value("model_mirror")).unwrap_or_else(|| "auto".to_owned());
    let origins = if mirror.trim() == "auto" {
        vec!["https://hf-mirror.com".to_owned(), "https://hf-mirror.net".to_owned(), "https://huggingface.co".to_owned()]
    } else { vec![model_mirror_origin(&mirror)?] };
    let cpu_mirror = respire_client_host::env::var("RSRS_CPU_ORT_MIRROR").ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| config_value("model_cpu_ort_mirror").filter(|value| !value.trim().is_empty()))
        .unwrap_or_else(|| "auto".to_owned());
    plan.assets.iter().map(|asset| {
        let official = &asset.suggested_source;
        if asset.name.starts_with("profile-") && official.is_empty() {
            return Ok(Vec::new());
        }
        if asset.name == "bge-m3-cpu-ort" {
            anyhow::ensure!(official.starts_with("https://github.com/risense-ai/respire-cli/releases/download/"),
                "CPU artifact lacks its official pinned source");
            return match cpu_mirror.trim() {
                "auto" => Ok(vec![format!("https://gh-proxy.org/{official}"), official.clone()]),
                "github" | "https://github.com" | "https://github.com/" => Ok(vec![official.clone()]),
                value => {
                    anyhow::ensure!(!value.is_empty() && !value.chars().any(char::is_whitespace),
                        "CPU ORT mirror must be a nonempty origin without whitespace");
                    let supplied = if value.contains("://") { value.to_owned() } else { format!("https://{value}") };
                    let origin = url::Url::parse(&supplied).context("invalid CPU ORT mirror origin")?;
                    anyhow::ensure!(matches!(origin.scheme(), "http" | "https") && origin.host_str().is_some()
                        && origin.username().is_empty() && origin.password().is_none()
                        && origin.query().is_none() && origin.fragment().is_none() && origin.path() == "/",
                        "CPU ORT mirror must be an origin without credentials, path, query or fragment");
                    Ok(vec![format!("{}/{official}", origin.as_str().trim_end_matches('/'))])
                }
            };
        }
        let relative = official.strip_prefix("https://huggingface.co/")
            .context("model artifact lacks its official immutable source")?;
        // Profile resources previously used their own pinned download URL, not the M3 mirror.
        if asset.name.starts_with("profile-") { return Ok(vec![official.clone()]); }
        Ok(origins.iter().map(|origin| format!("{origin}/{relative}")).collect())
    }).collect()
}

fn model_mirror_origin(raw: &str) -> Result<String> {
    let raw = raw.trim();
    anyhow::ensure!(!raw.is_empty(), "--mirror must not be empty");
    let supplied = if raw.contains("://") { raw.to_owned() } else { format!("https://{raw}") };
    let (scheme, rest) = supplied.split_once("://").context("--mirror is invalid")?;
    anyhow::ensure!(scheme == "http" || scheme == "https", "--mirror only supports http/https");
    let host = rest.split('/').next().filter(|host| !host.is_empty()).context("--mirror is missing a host")?.trim();
    anyhow::ensure!(!host.contains(['@', '?', '#']) && !host.chars().any(char::is_whitespace),
        "mirror must be a host without credentials, query or fragment");
    Ok(format!("{scheme}://{host}"))
}
