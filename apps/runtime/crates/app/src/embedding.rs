//! Host model registry, resource preparation and remote embedding transport.
use crate::core_sdk::onnx::Engine;
use crate::core_sdk::profiles::{BackendProfile, ModelProfile, Resource, Selection};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::{Path, PathBuf},
};

mod catalog;
pub use catalog::{default_selection, variant_names, variants, DEFAULT_MODEL, DEFAULT_VARIANT};

#[derive(Default, Serialize, Deserialize)]
pub struct Registry {
    pub models: BTreeMap<String, ModelProfile>,
    pub backends: BTreeMap<String, BackendProfile>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    configured_variants: BTreeMap<String, BTreeMap<String, ModelProfile>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Configure {
    pub model_profile: Option<ModelProfile>,
    pub backend_profile: Option<BackendProfile>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Choice {
    pub model: String,
    pub backend: String,
    #[serde(default)]
    pub engine: Engine,
    #[serde(default)]
    pub variant: Option<String>,
}
fn registry_path() -> PathBuf {
    crate::service::main_data_dir().join("embedding-profiles-v1.json")
}

pub fn saved_selection() -> Result<Option<Selection>> {
    use rusqlite::OptionalExtension;
    let path = crate::service::database_path(&crate::service::data_dir())?;
    if !path.is_file() {
        return Ok(None);
    }
    let connection = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    let saved: Option<String> = connection
        .query_row(
            "SELECT value FROM meta WHERE key=?1",
            [crate::core_sdk::profiles::ACTIVE_SELECTION_KEY],
            |row| row.get(0),
        )
        .optional()?;
    saved
        .map(|value| serde_json::from_str(&value).map_err(Into::into))
        .transpose()
}
fn resource(
    root: &std::path::Path,
    file: &str,
    size: u64,
    hash: &str,
    repo: &str,
    revision: &str,
) -> Resource {
    Resource {
        path: root.join(file),
        size,
        sha256: hash.to_owned(),
        download: Some(format!(
            "https://huggingface.co/{repo}/resolve/{revision}/{file}"
        )),
    }
}
/// Pins come from the Runtime release, before any host-supplied overrides.
pub fn builtin_registry() -> Result<Registry> {
    let mut registry = Registry { models: catalog::models()?, ..Registry::default() };
    registry.backends.insert(
        "onnx".to_owned(),
        BackendProfile {
            id: "onnx".to_owned(),
            backend: "onnx".to_owned(),
            native: None,
            provider: None,
            protocol: None,
            endpoint: None,
            keyring_service: None,
            keyring_account: None,
        },
    );
    registry.backends.insert(
        "mlx".to_owned(),
        BackendProfile {
            id: "mlx".to_owned(),
            backend: "mlx".to_owned(),
            native: None,
            provider: None,
            protocol: None,
            endpoint: None,
            keyring_service: None,
            keyring_account: None,
        },
    );
    Ok(registry)
}

pub fn registry() -> Result<Registry> {
    let mut registry = builtin_registry()?;
    match std::fs::read(registry_path()) {
        Ok(bytes) => {
            let saved: Registry = serde_json::from_slice(&bytes)?;
            registry.models.extend(saved.models);
            registry.models.remove(catalog::LEGACY_DISTILL);
            registry.backends.extend(saved.backends);
            registry.configured_variants = saved.configured_variants;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    Ok(registry)
}
static REGISTRY_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());
pub fn configure(configure: Configure) -> Result<Value> {
    let _write = REGISTRY_WRITE
        .lock()
        .map_err(|_| anyhow::anyhow!("profile registry lock poisoned"))?;
    anyhow::ensure!(
        configure.model_profile.is_some() || configure.backend_profile.is_some(),
        "configuration must include a model_profile or backend_profile"
    );
    let mut registry = registry()?;
    if let Some(model) = configure.model_profile {
        anyhow::ensure!(!model.id.trim().is_empty(), "model profile ID is empty");
        anyhow::ensure!(model.id != catalog::LEGACY_DISTILL, "legacy model is read-only");
        if let Some(variant) = pinned_variant(&model) {
            registry.configured_variants.entry(model.id.clone()).or_default()
                .insert(variant.to_owned(), model.clone());
        }
        registry.models.insert(model.id.clone(), model);
    }
    if let Some(backend) = configure.backend_profile {
        anyhow::ensure!(!backend.id.trim().is_empty(), "backend profile ID is empty");
        if backend.backend == "host_transport" {
            validate_url(
                backend
                    .endpoint
                    .as_deref()
                    .context("embedding endpoint required")?,
            )?;
            anyhow::ensure!(
                backend.protocol.as_deref() == Some("openai_embeddings"),
                "unsupported embedding protocol"
            );
        } else {
            anyhow::ensure!(
                matches!(backend.backend.as_str(), "onnx" | "mlx"),
                "unsupported embedding backend"
            );
        }
        registry.backends.insert(backend.id.clone(), backend);
    }
    let path = registry_path();
    let parent = path.parent().context("registry parent missing")?;
    std::fs::create_dir_all(parent)?;
    let staged = parent.join(format!("embedding-profiles-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&staged)?;
        file.write_all(&serde_json::to_vec_pretty(&registry)?)?;
        file.sync_all()?;
        std::fs::rename(&staged, &path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result?;
    Ok(json!({"saved":true,"activated":false,"registry":registry}))
}
/// Derive the same per-profile directory used by the Runtime publisher.
pub fn runtime_owned_namespace(installation: &Path, profile: &Path) -> Result<PathBuf> {
    use sha2::{Digest, Sha256};
    let key = profile.to_str().context("managed profile is not UTF-8")?;
    Ok(installation.join("runtime").join("model-assets-v1")
        .join(format!("{:x}", Sha256::digest(key.as_bytes()))))
}

fn managed_entry(path: &Path, directory: bool) -> Option<std::fs::Metadata> {
    let attrs = std::fs::symlink_metadata(path).ok()?;
    if attrs.file_type().is_symlink() || (directory && !attrs.is_dir()) || (!directory && !attrs.is_file()) {
        return None;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if attrs.file_attributes() & 0x400 != 0 { return None; }
    }
    (path.canonicalize().ok()?.as_path() == path).then_some(attrs)
}

fn project_managed_pair(profile: &mut ModelProfile, installation: &Path, account: &Path) -> Option<()> {
    let namespace = runtime_owned_namespace(installation, account).ok()?;
    let installation_runtime = installation.join("runtime");
    let assets = namespace.parent()?;
    let guard = namespace.join(".ownership");
    for directory in [installation_runtime.as_path(), assets, &namespace, &guard] {
        managed_entry(directory, true)?;
    }
    managed_entry(&guard.join("lock.db"), false)?;
    if !profile.id.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        || !profile.id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return None;
    }
    let model = namespace.join(&profile.id);
    managed_entry(&model, true)?;
    let weights = profile.weights.as_mut()?;
    let filename = match weights.path.extension()?.to_str()? {
        "onnx" => "model_quantized.onnx",
        "safetensors" => "model.safetensors",
        _ => return None,
    };
    let tokenizer = profile.tokenizer.as_mut()?;
    let managed_weights = model.join(filename);
    let managed_tokenizer = model.join("tokenizer.json");
    if managed_entry(&managed_weights, false)?.len() != weights.size
        || managed_entry(&managed_tokenizer, false)?.len() != tokenizer.size
    {
        return None;
    }
    weights.path = managed_weights;
    tokenizer.path = managed_tokenizer;
    Some(())
}

#[cfg(test)]
mod managed_tests {
    use super::*;
    use std::fs;

    #[test]
    fn managed_projection_requires_complete_pair_and_selected_profile() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let installation = fixture.path().canonicalize()?;
        let alice = installation.join("alice");
        let bob = installation.join("bob");
        fs::create_dir(&alice)?;
        fs::create_dir(&bob)?;
        let mut catalog = builtin_registry()?;
        let mut choice = catalog.models.remove("bge-base-zh-v1.5").context("fixture model")?;
        choice.weights.as_mut().context("fixture weights")?.size = 4;
        choice.tokenizer.as_mut().context("fixture tokenizer")?.size = 3;
        let legacy = choice.weights.as_ref().context("fixture weights")?.path.clone();
        let own = runtime_owned_namespace(&installation, &alice)?;
        let guard = own.join(".ownership");
        fs::create_dir_all(&guard)?;
        fs::write(guard.join("lock.db"), [])?;
        let candidate = own.join(&choice.id);
        fs::create_dir(&candidate)?;
        let weights = candidate.join("model_quantized.onnx");
        fs::write(&weights, b"data")?;
        assert!(project_managed_pair(&mut choice, &installation, &alice).is_none());
        assert_eq!(choice.weights.as_ref().context("fixture weights")?.path, legacy);
        let tokenizer = candidate.join("tokenizer.json");
        fs::write(&tokenizer, b"tok")?;
        assert!(project_managed_pair(&mut choice, &installation, &bob).is_none());
        assert!(project_managed_pair(&mut choice, &installation, &alice).is_some());
        assert_eq!(choice.weights.as_ref().context("managed weights")?.path, weights);
        assert_eq!(choice.tokenizer.as_ref().context("managed tokenizer")?.path, tokenizer);
        Ok(())
    }
}

pub fn select(choice: Choice) -> Result<Selection> {
    select_from_registry(choice, &registry()?)
}

fn pinned_variant(profile: &ModelProfile) -> Option<&'static str> {
    let weights = profile.weights.as_ref()?;
    variants(&profile.id).iter().find(|pin| weights.size == pin.size && weights.sha256.eq_ignore_ascii_case(pin.sha256)).map(|pin| pin.name)
}

pub fn select_from_registry(choice: Choice, registry: &Registry) -> Result<Selection> {
    anyhow::ensure!(choice.model != catalog::LEGACY_DISTILL, "legacy model is read-only");
    let mut profile = registry
        .models
        .get(&choice.model)
        .context("unknown model profile")?
        .clone();
    let mut selected_variant = choice.variant;
    let mut project = false;
    if !variants(&choice.model).is_empty() {
        let requested = selected_variant.as_deref().or_else(|| pinned_variant(&profile));
        let builtin = catalog::model(&choice.model, requested)?;
        let name = pinned_variant(&builtin).context("curated variant pin missing")?;
        if let Some(configured) = registry.configured_variants.get(&choice.model).and_then(|models| models.get(name)) {
            profile = configured.clone();
        } else if pinned_variant(&profile) != Some(name) && (selected_variant.is_some() || pinned_variant(&profile).is_some()) {
            if choice.model == "m3" {
                let previous = profile.weights.as_ref().context("M3 weights missing")?;
                let mut weights = builtin.weights.clone().context("curated M3 weights missing")?;
                let root = previous.path.parent().and_then(Path::parent).context("M3 model root missing")?;
                weights.path = root.join(crate::core_sdk::onnx::variant_file_name(name));
                profile.weights = Some(weights);
                profile.companions.clear();
            } else {
                profile = builtin.clone();
            }
        }
        if pinned_variant(&profile) == Some(name) {
            profile.isolate_engines = true;
            if profile.companions.is_empty() && !builtin.companions.is_empty() {
                let weights = profile.weights.as_ref().context("curated variant weights missing")?;
                profile.companions = builtin.companions.iter().cloned().map(|mut resource| {
                    resource.path = weights.path.with_file_name("model.onnx_data");
                    resource
                }).collect();
            }
            selected_variant = Some(name.to_owned());
            project = matches!(name, "quantized") && serde_json::to_value(&profile)? == serde_json::to_value(&builtin)?;
        }
    } else if let Some(variant) = selected_variant.as_deref() {
        anyhow::ensure!(variant == if choice.model == "m3-mlx" { "fp16" } else { "quantized" },
            "custom models select resource paths in their profile");
    }
    if project {
        if let Ok(roots) = crate::service::resolved_roots() {
            if let (Ok(installation), Ok(account)) = (
                roots.installation_root.canonicalize(), roots.account_data_root.canonicalize(),
            ) {
                // Runtime still validates exact resource hashes on activation.
                let _ = project_managed_pair(&mut profile, &installation, &account);
            }
        }
    }
    if profile.isolate_engines {
        for resource in profile.weights.iter_mut().chain(&mut profile.tokenizer).chain(&mut profile.companions) {
            resource.path = std::path::absolute(&resource.path)?;
        }
    }
    let selection = Selection {
        model: choice.model,
        backend: choice.backend.clone(),
        engine: choice.engine,
        variant: selected_variant,
        profile,
        backend_profile: registry
            .backends
            .get(&choice.backend)
            .context("unknown backend profile")?
            .clone(),
        generation: String::new(),
    };
    selection.validate()?;
    Ok(selection)
}
fn validate_url(url: &str) -> Result<()> {
    let (scheme, remainder) = url
        .split_once("://")
        .context("absolute embedding URL required")?;
    let authority = remainder.split('/').next().unwrap_or("");
    anyhow::ensure!(
        !authority.is_empty()
            && !url.chars().any(char::is_whitespace)
            && !url.contains(['@', '?', '#', '\\']),
        "embedding URL cannot contain credentials, query or fragment"
    );
    let loopback = ["localhost", "127.0.0.1", "[::1]"].iter().any(|host| {
        authority == *host
            || authority
                .strip_prefix(host)
                .is_some_and(|v| v.starts_with(':') && v[1..].bytes().all(|b| b.is_ascii_digit()))
    });
    anyhow::ensure!(
        scheme == "https" || (scheme == "http" && loopback),
        "embedding URL requires HTTPS (HTTP is allowed only for loopback fixtures)"
    );
    Ok(())
}
pub fn install_transport() -> Result<()> {
    crate::core_sdk::profiles::set_transport(embedding_request)
}
fn embedding_request(request: &Value, selection: &Selection) -> Result<Value> {
    let backend = &selection.backend_profile;
    anyhow::ensure!(
        request["operation"] == "embedding"
            && request["protocol"] == "openai_embeddings"
            && request["provider"].as_str() == backend.provider.as_deref()
            && backend.protocol.as_deref() == Some("openai_embeddings"),
        "embedding callback/provider mismatch"
    );
    let endpoint = backend
        .endpoint
        .as_deref()
        .context("embedding endpoint is missing")?;
    validate_url(endpoint)?;
    let model = request["model"]
        .as_str()
        .context("embedding callback model is missing")?;
    let input = request["input"]
        .as_array()
        .context("embedding callback input is missing")?;
    anyhow::ensure!(
        Some(model) == selection.profile.embedding["model_id"].as_str()
            && !input.is_empty()
            && input.iter().all(Value::is_string),
        "invalid embedding callback input"
    );
    let body = json!({"model":model,"input":input});
    let timeout = request["timeout"]
        .as_u64()
        .filter(|seconds| *seconds > 0)
        .context("embedding callback timeout missing")?;
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(timeout))
        .redirects(0)
        .build();
    let mut call = agent.post(endpoint);
    match (&backend.keyring_service, &backend.keyring_account) {
        (Some(service), Some(account)) => {
            let secret = crate::credential_io::Entry::new(service, account, "native")
                .get_password()
                .map_err(|_| anyhow::anyhow!("embedding credential must be supplied by the host"))?;
            call = call.set("Authorization", &format!("Bearer {secret}"));
        }
        (None, None) => (),
        _ => anyhow::bail!("embedding keyring service and account must be configured together"),
    }
    let response = call.send_json(body.clone()).map_err(|error| match error {
        ureq::Error::Status(status, _) => {
            anyhow::anyhow!("embedding provider HTTP status {status}")
        }
        _ => anyhow::anyhow!("embedding provider transport failed"),
    })?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() <= 32 * 1024 * 1024,
        "embedding provider response is too large"
    );
    serde_json::from_slice(&bytes).context("invalid embedding provider JSON")
}
