//! Local BGE-M3 quantized dense embeddings: CLS pooling and L2 normalization.

use super::onnx::{Engine, InferenceOptions, InferenceSession, InferenceOutput};
use anyhow::{anyhow, Context, Result};
use std::sync::{Arc, Mutex, RwLock};
use tokenizers::Tokenizer;

use super::search::Embedder;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Backend {
    Onnx,
    Mlx,
}

impl Backend {
    fn as_str(self) -> &'static str {
        match self { Self::Onnx => "onnx", Self::Mlx => "mlx" }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct ModelSpec {
    model: String,
    backend: Backend,
    embedding: crate::embedding::EmbeddingProfile,
    onnx: crate::embedding::OnnxNames,
    execution_identity: String,
}

impl ModelSpec {
    fn legacy_cpu(&self) -> bool {
        self.backend == Backend::Onnx
            && self.embedding == crate::embedding::EmbeddingProfile::m3()
            && self.onnx.input_ids == "input_ids"
            && self.onnx.attention_mask.as_deref() == Some("attention_mask")
            && self.onnx.token_type_ids.as_deref() == Some("token_type_ids")
            && matches!(self.onnx.output.as_deref(), None | Some("last_hidden_state"))
    }
}

/// One resident embedding session. ONNX and MLX share the same pooling path.
enum Session {
    Onnx(InferenceSession),
    #[cfg(all(feature = "mlx", target_os = "macos", target_arch = "aarch64"))]
    Mlx(super::mlx::MlxSession),
}

impl Session {
    fn run(&mut self, ids: &[i64], mask: &[i64], types: &[i64], batch: usize, seq: usize) -> Result<InferenceOutput> {
        match self {
            Self::Onnx(session) => session.run(ids, mask, types, batch, seq),
            #[cfg(all(feature = "mlx", target_os = "macos", target_arch = "aarch64"))]
            Self::Mlx(session) => session.run(ids, mask, types, batch, seq),
        }
    }

    fn run_reference(&mut self, ids: &[i64], mask: &[i64], types: &[i64], batch: usize, seq: usize) -> Result<InferenceOutput> {
        match self {
            Self::Onnx(session) => session.run(ids, mask, types, batch, seq),
            #[cfg(all(feature = "mlx", target_os = "macos", target_arch = "aarch64"))]
            Self::Mlx(session) => session.run_cpu(ids, mask, types, batch, seq),
        }
    }

    #[cfg(test)]
    fn generation_for_test(&self) -> u64 {
        match self {
            Self::Onnx(session) => session.generation_for_test(),
            #[cfg(all(feature = "mlx", target_os = "macos", target_arch = "aarch64"))]
            Self::Mlx(_) => unreachable!("MLX sessions are not covered by ONNX generation tests"),
        }
    }

    fn selected(&self) -> &str {
        match self {
            Self::Onnx(session) => &session.selected,
            #[cfg(all(feature = "mlx", target_os = "macos", target_arch = "aarch64"))]
            Self::Mlx(session) => session.selected(),
        }
    }

    fn stalled(&self) -> &Arc<std::sync::atomic::AtomicBool> {
        match self {
            Self::Onnx(session) => &session.stalled,
            #[cfg(all(feature = "mlx", target_os = "macos", target_arch = "aarch64"))]
            Self::Mlx(session) => session.stalled(),
        }
    }
}

fn mlx_supported() -> bool {
    cfg!(all(feature = "mlx", target_os = "macos", target_arch = "aarch64"))
}

#[cfg(all(feature = "mlx", target_os = "macos", target_arch = "aarch64"))]
fn create_session(backend: Backend, model_bytes: &[u8], options: InferenceOptions, native_options: Option<&dyn Fn(*mut std::ffi::c_void, *mut std::ffi::c_void, *const std::ffi::c_void) -> Result<()>>, spec: &ModelSpec) -> Result<Session> {
    match backend {
        Backend::Onnx => Ok(Session::Onnx(super::onnx::commit_onnx_session_with_profile(model_bytes, options, native_options, &spec.embedding, &spec.onnx).context("failed to load bge onnx")?)),
        Backend::Mlx => {
            anyhow::ensure!(native_options.is_none(), "MLX backend does not accept ONNX native options");
            Ok(Session::Mlx(super::mlx::MlxSession::load(model_bytes, options, &spec.embedding)?))
        }
    }
}

#[cfg(not(all(feature = "mlx", target_os = "macos", target_arch = "aarch64")))]
fn create_session(backend: Backend, model_bytes: &[u8], options: InferenceOptions, native_options: Option<&dyn Fn(*mut std::ffi::c_void, *mut std::ffi::c_void, *const std::ffi::c_void) -> Result<()>>, spec: &ModelSpec) -> Result<Session> {
    match backend {
        Backend::Onnx => Ok(Session::Onnx(super::onnx::commit_onnx_session_with_profile(model_bytes, options, native_options, &spec.embedding, &spec.onnx).context("failed to load bge onnx")?)),
        Backend::Mlx => anyhow::bail!("MLX backend requires a Core build with the mlx feature on Apple Silicon macOS"),
    }
}

/// Owns the initialized model prefix and its digest. Callers never supply a
/// digest separately from these bytes or obtain mutable backing storage.
pub struct ModelUpload {
    bytes: Vec<u8>,
    digest: sha2::Sha256,
}

impl ModelUpload {
    pub fn new(exact_capacity: usize) -> Result<Self, std::collections::TryReserveError> {
        use sha2::Digest;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(exact_capacity)?;
        Ok(Self { bytes, digest: sha2::Sha256::new() })
    }

    pub fn bytes(&self) -> &[u8] { &self.bytes }

    /// The caller validates the logical length. Never grow the allocation.
    pub fn append(&mut self, chunk: &[u8]) -> bool {
        use sha2::Digest;
        if chunk.len() > self.bytes.capacity() - self.bytes.len() { return false; }
        let offset = self.bytes.len();
        self.bytes.extend_from_slice(chunk);
        self.digest.update(&self.bytes[offset..]);
        true
    }

    #[cfg(feature = "cpu-owned-ort-experiment")]
    fn into_parts(self) -> (Vec<u8>, String) {
        use sha2::Digest;
        (self.bytes, format!("{:x}", self.digest.finalize()))
    }
}

struct ModelDigests {
    model: String,
    tokenizer: String,
}

impl ModelDigests {
    fn verified(model: &[u8], tokenizer: &[u8], config: &serde_json::Value) -> Result<Self> {
        use sha2::{Digest, Sha256};
        Self::verified_with_model_digest(format!("{:x}", Sha256::digest(model)), tokenizer, config)
    }

    fn verified_with_model_digest(model: String, tokenizer: &[u8], config: &serde_json::Value) -> Result<Self> {
        use sha2::{Digest, Sha256};
        let tokenizer = format!("{:x}", Sha256::digest(tokenizer));
        for (field, actual) in [("model_sha256", &model), ("tokenizer_sha256", &tokenizer)] {
            if let Some(expected) = config.get(field) {
                anyhow::ensure!(expected.as_str() == Some(actual.as_str()),
                    "host-supplied {field} differs from the actual buffer");
            }
        }
        Ok(Self { model, tokenizer })
    }

    fn index_identity(&self, spec: &ModelSpec) -> Result<String> {
        let mut config = serde_json::json!({"model_sha256":self.model, "tokenizer_sha256":self.tokenizer,
            "embedding":spec.embedding, "onnx":spec.onnx, "execution_identity":spec.execution_identity});
        if spec.backend == Backend::Mlx { config["backend"] = serde_json::json!("mlx"); }
        crate::embedding::index_identity(&spec.model, &config)
    }
}

struct ModelAssets {
    tokenizer: Tokenizer,
    model_bytes: Box<[u8]>,
    #[cfg(feature = "cpu-owned-ort-experiment")]
    cpu_owned: Option<super::onnx::CpuOwnedGroup>,
}

/// A borrowed operation lease; it exposes no model data and does not increment Arc counts.
pub struct ModelUse<'a> {
    _assets: std::sync::RwLockReadGuard<'a, Option<ModelAssets>>,
}

pub struct BgeEmbedder {
    /// Each resident session has its own admission queue and runs one call at a time.
    session: Arc<Mutex<Option<Session>>>,
    background_session: Option<Arc<Mutex<Option<Session>>>>,
    stalled: Arc<std::sync::atomic::AtomicBool>,
    background_stalled: Option<Arc<std::sync::atomic::AtomicBool>>,
    background: bool,
    selected: Arc<str>,
    /// Shared slots can be emptied on retirement even when idle handles remain alive.
    assets: Arc<RwLock<Option<ModelAssets>>>,
    options: InferenceOptions,
    asset_id: Arc<str>,
    session_options_id: Arc<str>,
    model_spec: Arc<ModelSpec>,
    index_identity: Arc<str>,
}

static SHARED: Mutex<Option<BgeEmbedder>> = Mutex::new(None);

#[cfg(test)]
#[path = "bge_tests.rs"]
mod tests;

#[cfg(test)]
pub(crate) static FRESH_LOADS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

impl Clone for BgeEmbedder {
    fn clone(&self) -> Self {
        Self {
            session: Arc::clone(&self.session),
            background_session: self.background_session.as_ref().map(Arc::clone),
            stalled: Arc::clone(&self.stalled),
            background_stalled: self.background_stalled.as_ref().map(Arc::clone),
            background: self.background,
            selected: Arc::clone(&self.selected),
            assets: Arc::clone(&self.assets),
            options: self.options,
            asset_id: Arc::clone(&self.asset_id),
            session_options_id: Arc::clone(&self.session_options_id),
            model_spec: Arc::clone(&self.model_spec),
            index_identity: Arc::clone(&self.index_identity),
        }
    }
}

impl BgeEmbedder {
    fn spec(&self) -> &crate::embedding::EmbeddingProfile { &self.model_spec.embedding }

    fn validate_prefix(&self, prefix: &str) -> Result<()> {
        if prefix.is_empty() { return Ok(()); }
        let assets = self.assets.read().map_err(|_| anyhow!("model assets poisoned"))?;
        let tokenizer = &assets.as_ref().context("model assets retired; host must reload model")?.tokenizer;
        let tokens = tokenizer.encode(prefix, true).map_err(|error| anyhow!("tokenize prefix: {error}"))?;
        anyhow::ensure!(tokens.get_ids().len() < self.spec().max_tokens, "embedding prefix leaves no token window for content");
        Ok(())
    }

    pub fn index_identity(&self) -> &str { &self.index_identity }

    fn configuration(model: &str, config: &serde_json::Value) -> Result<Arc<ModelSpec>> {
        let backend = match config.get("backend").and_then(serde_json::Value::as_str).unwrap_or("onnx") {
            "onnx" => Backend::Onnx,
            "mlx" => Backend::Mlx,
            other => anyhow::bail!("unsupported local model backend {other}"),
        };
        if backend == Backend::Mlx {
            anyhow::ensure!(mlx_supported(), "MLX backend requires a Core build with the mlx feature on Apple Silicon macOS");
        }
        anyhow::ensure!(config.get("model_sha256").is_some() == config.get("tokenizer_sha256").is_some(), "model_sha256 and tokenizer_sha256 must be supplied together");
        let embedding = crate::embedding::EmbeddingProfile::from_config(model, config)?;
        let onnx: crate::embedding::OnnxNames = config.get("onnx").filter(|value| !value.is_null()).map(|value| serde_json::from_value(value.clone())).transpose()?.unwrap_or_default();
        onnx.validate()?;
        let execution_identity = config.get("execution_identity").map(|value| value.as_str().context("execution_identity must be a string")).transpose()?.unwrap_or("").to_owned();
        anyhow::ensure!(config["host_execution_provider"].as_bool() != Some(true) || !execution_identity.is_empty(), "host execution provider requires execution_identity");
        anyhow::ensure!(config["host_execution_provider"].as_bool() != Some(true) || backend == Backend::Onnx, "host execution provider requires the onnx backend");
        Ok(Arc::new(ModelSpec { model:model.to_owned(), backend, embedding, onnx, execution_identity }))
    }

    /// Nonblocking diagnostics must not delay the host heartbeat during model loading.
    pub fn inference_thread_settings() -> Option<(usize, usize)> {
        let slot = SHARED.try_lock().ok()?;
        slot.as_ref().map(|provider| (provider.options.intra_threads, provider.options.background_intra_threads))
    }

    pub fn lease(&self) -> Result<ModelUse<'_>> {
        let assets = self.assets.read().map_err(|_| anyhow!("model assets poisoned"))?;
        anyhow::ensure!(assets.is_some(), "model assets retired; host must reload model");
        Ok(ModelUse { _assets: assets })
    }

    pub fn load() -> Result<Self> { Self::load_model("m3") }

    pub fn load_model(model: &str) -> Result<Self> {
        anyhow::ensure!(!super::onnx::inference_stalled(), "inference unresponsive; host runtime recovery required; no second session started");

        let slot = SHARED.lock().map_err(|_| anyhow!("embedder singleton poisoned"))?;
        if let Some(existing) = slot.as_ref().filter(|value| value.model_name() == model) { return Ok(existing.clone()); }
        Err(anyhow!("host must supply BGE-M3 model and tokenizer bytes before inference"))
    }

    /// Retire native resources without requiring idle compatibility handles to be dropped.
    pub fn drop_shared() -> Result<()> {
        let mut slot = SHARED.try_lock().map_err(|error| anyhow!("model initialization is still active; shared session reset was not applied: {error}"))?;
        if let Some(provider) = slot.as_ref() { provider.retire_assets()?; }
        *slot = None;
        #[cfg(test)]
        super::onnx::reset_transition_hook();
        super::onnx::advance_model_generation();
        Ok(())
    }

    fn retire_assets(&self) -> Result<()> {
        // Obtain every guard before changing a slot. A busy operation makes retirement
        // fail without partially releasing assets or interrupting its borrowed data.
        let mut foreground = self.session.try_lock()
            .map_err(|error| anyhow!("foreground model is in use; retirement not applied: {error}"))?;
        let shared_background = self.background_session.as_ref().is_some_and(|session| Arc::ptr_eq(session, &self.session));
        let mut background = if shared_background { None } else { self.background_session.as_ref().map(|session| session.try_lock()
            .map_err(|error| anyhow!("background model is in use; retirement not applied: {error}"))).transpose()? };
        let mut assets = self.assets.try_write()
            .map_err(|error| anyhow!("model assets are in use; retirement not applied: {error}"))?;
        drop(foreground.take());
        if let Some(session) = background.as_mut() { drop(session.take()); }
        drop(assets.take());
        Ok(())
    }

    pub fn model_name(&self) -> &str { &self.model_spec.model }

    /// Install an explicitly initialized session for compatibility callers.
    pub fn install_shared(&self) -> Result<()> {
        let mut slot = SHARED.lock().map_err(|_| anyhow!("embedder singleton poisoned"))?;
        *slot = Some(self.clone());
        Ok(())
    }

    pub fn for_background(&self) -> Result<Self> {
        self.background_session.as_ref().context("background inference session is not initialized")?;
        self.background_stalled.as_ref().context("background inference health is not initialized")?;
        let mut provider = self.clone();
        provider.background = true;
        Ok(provider)
    }

    fn lane_session(&self) -> Result<&Arc<Mutex<Option<Session>>>> {
        if self.background {
            self.background_session.as_ref().context("background inference session is not initialized")
        } else { Ok(&self.session) }
    }

    pub fn sessions_stalled() -> bool {
        match SHARED.try_lock() {
            Ok(slot) => slot.as_ref().is_some_and(|provider|
                provider.stalled.load(std::sync::atomic::Ordering::Acquire)
                || provider.background_stalled.as_ref().is_some_and(|state|
                    state.load(std::sync::atomic::Ordering::Acquire))),
            Err(_) => false,
        }
    }

    fn cpu_reference(&self) -> Result<Self> {
        let assets = self.assets.read().map_err(|_| anyhow!("model assets poisoned"))?;
        let assets = assets.as_ref().context("model assets retired; host must reload model")?;
        let tokenizer = assets.tokenizer.to_string(false)
            .map_err(|e| anyhow!("serialize in-memory tokenizer: {e}"))?;
        let identity = ModelDigests::verified(&assets.model_bytes, tokenizer.as_bytes(), &serde_json::Value::Null)?
            .index_identity(&self.model_spec)?;
        Self::load_memory(&assets.model_bytes, tokenizer.as_bytes(), InferenceOptions {
            engine: Engine::Cpu, host_execution_provider:false, ..self.options
        }, &self.asset_id, "", None, Arc::clone(&self.model_spec), identity)
    }

    fn input_tensors(&self, text: &str) -> Result<(Vec<i64>, Vec<i64>, Vec<i64>)> {
        // encode (including special tokens; truncate to model max_len)
        let mut enc = {
            let assets = self.assets.read().map_err(|_| anyhow!("model assets poisoned"))?;
            assets.as_ref().context("model assets retired; host must reload model")?
                .tokenizer.encode(text, true).map_err(|e| anyhow!("tokenize failed: {e}"))?
        };
        // Keep the execution within the profile window.
        let max_len = self.spec().max_tokens;
        if enc.get_ids().len() > max_len {
            enc.truncate(max_len, 0, tokenizers::TruncationDirection::Right);
        }
        let mut ids: Vec<i64> = enc.get_ids().iter().map(|&x| x as i64).collect();
        let mut mask: Vec<i64> = enc.get_attention_mask().iter().map(|&x| x as i64).collect();
        let mut types: Vec<i64> = enc.get_type_ids().iter().map(|&x| x as i64).collect();
        if self.spec().pad_to_max_tokens || self.selected.starts_with("Npu:") {
            ids.resize(max_len, self.spec().pad_token_id as i64);
            mask.resize(max_len, 0);
            types.resize(max_len, 0);
        }
        Ok((ids, mask, types))
    }

    /// Embed one text (tokenize → model → model-specific pooling → normalize).
    fn embed_inner(&self, text: &str) -> Result<Vec<f32>> {
        self.embed_inner_on(text, false)
    }

    fn embed_inner_reference(&self, text: &str) -> Result<Vec<f32>> {
        self.embed_inner_on(text, true)
    }

    fn embed_inner_on(&self, text: &str, reference: bool) -> Result<Vec<f32>> {
        // Protect queued admission as well as the native run from asset retirement.
        let _model_use = self.lease()?;
        let (ids, mask, types) = self.input_tensors(text)?;

        let stalled = if self.background {
            self.background_stalled.as_ref().context("background inference health is not initialized")?
        } else { &self.stalled };
        anyhow::ensure!(!stalled.load(std::sync::atomic::Ordering::Acquire), "inference session is unresponsive; host runtime recovery is required");
        let queue = if self.background { &crate::inference_queue::BACKGROUND_QUEUE } else { &crate::inference_queue::QUEUE };
        let _permit = queue.acquire(crate::inference_queue::WAIT_LIMIT)?;
        anyhow::ensure!(!stalled.load(std::sync::atomic::Ordering::Acquire), "inference session is unresponsive; host runtime recovery is required");
        #[cfg(feature = "cpu-owned-ort-experiment")]
        if self.options.cpu_owned_ort_experiment {
            anyhow::ensure!(!reference, "CPU owned ORT backend has no separate reference lane");
            let assets = self.assets.read().map_err(|_| anyhow!("model assets poisoned"))?;
            let group = assets.as_ref().and_then(|assets| assets.cpu_owned.as_ref()).context("CPU model retired; host must reload")?;
            return group.embed(self.background, ids, mask, types);
        }
        let mut session = self
            .lane_session()?
            .lock()
            .map_err(|_| anyhow!("session mutex poisoned"))?;
        let output = if reference {
            session.as_mut().context("model session retired; host must reload model")?
                .run_reference(&ids, &mask, &types, 1, ids.len())
        } else {
            session.as_mut().context("model session retired; host must reload model")?
                .run(&ids, &mask, &types, 1, ids.len())
        }.context("model run failed")?;
        let (shape, data) = output.tensor()?;

        self.spec().pool(shape, data, &mask)
    }

    pub fn probe(&self, text: &str) -> Result<serde_json::Value> {
        let _model_use = self.lease()?;
        let start = std::time::Instant::now();
        let vector = self.embed_inner(text)?;
        anyhow::ensure!(
            vector.len() == self.dims() && vector.iter().all(|v| v.is_finite()),
            "invalid BGE output"
        );
        let elapsed = start.elapsed().as_millis();
        let selected = self.selected.as_ref();
        // Reuse the shared CPU session. MLX validates GPU against its own CPU stream.
        let reference = if self.model_spec.backend == Backend::Mlx {
            self.embed_inner_reference(text)?
        } else if selected == "CPU" {
            self.embed_inner(text)?
        } else {
            self.cpu_reference()?.embed_inner(text)?
        };
        let cosine = crate::search::cosine_similarity(&vector, &reference);
        anyhow::ensure!(
            cosine >= 0.99,
            "inference validation failed (cosine {cosine:.4} below 0.99)"
        );
        let reference_label = if self.model_spec.backend == Backend::Mlx { "MLX CPU" } else { "CPU" };
        Ok(
            serde_json::json!({"selected":selected,"backend":self.model_spec.backend.as_str(),"fallbacks":[],
            "fallbacks_scope":"engine_selection",
            "cpu_node_fallback":cpu_node_fallback_policy(selected),
            "node_assignment":"not_measured",
            "ready":true,"dimensions":vector.len(),"elapsed_ms":elapsed,
            "cosine":cosine,"reference":reference_label,
            "model":self.model_name()}),
        )
    }
}

// The selected EP and cosine do not measure per-node execution placement.
// DML/CoreML may delegate unsupported operations to CPU without changing engines.
fn cpu_node_fallback_policy(selected: &str) -> &'static str {
    if selected.starts_with("Gpu:") || selected.starts_with("CoreML ") {
        "allowed"
    } else if selected == "Host" {
        "host_managed"
    } else if selected.starts_with("Npu:") {
        "disabled"
    } else {
        "not_applicable"
    }
}

fn query_input(profile: &crate::embedding::EmbeddingProfile, text: &str) -> String {
    format!("{}{}", profile.query_prefix, text)
}

impl Embedder for BgeEmbedder {
    fn try_embed(&self, text: &str) -> Result<Vec<f32>> {
        self.validate_prefix(&self.spec().document_prefix)?;
        self.embed_inner(&format!("{}{}", self.spec().document_prefix, text))
    }
    fn try_embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.validate_prefix(&self.spec().query_prefix)?;
        self.embed_inner(&query_input(self.spec(), text))
    }
    fn split_text(&self, text: &str) -> Result<Vec<String>> {
        let assets = self.assets.read().map_err(|_| anyhow!("model assets poisoned"))?;
        let assets = assets.as_ref().context("model assets retired; host must reload model")?;
        let encoding = assets.tokenizer
            .encode(text, false)
            .map_err(|e| anyhow!("tokenize chunk: {e}"))?;
        let ids = encoding.get_ids();
        let prefix_tokens = assets.tokenizer.encode(self.spec().document_prefix.as_str(), false).map_err(|error| anyhow!("tokenize document prefix: {error}"))?.get_ids().len();
        let special_tokens = assets.tokenizer.encode("", true).map_err(|error| anyhow!("tokenize special tokens: {error}"))?.get_ids().len();
        let reserved = (prefix_tokens + special_tokens).max(32.min(self.spec().max_tokens - 1));
        anyhow::ensure!(reserved < self.spec().max_tokens, "document prefix leaves no token window for content");
        let window = self.spec().max_tokens - reserved;
        let overlap = (window / 10).min(window - 1);
        if ids.len() <= window {
            return Ok(vec![text.to_owned()]);
        }
        let mut pieces = Vec::new();
        let mut start = 0;
        while start < ids.len() {
            let end = (start + window).min(ids.len());
            pieces.push(
                assets.tokenizer
                    .decode(&ids[start..end], true)
                    .map_err(|e| anyhow!("decode chunk: {e}"))?,
            );
            if end == ids.len() {
                break;
            }
            start = end - overlap;
        }
        Ok(pieces)
    }

    fn dims(&self) -> usize {
        self.spec().dimensions
    }

    /// Document side (no prefix) — used when storing memories.
    fn embed(&self, text: &str) -> Vec<f32> {
        self.try_embed(text).unwrap_or_else(|error| {
            eprintln!("BGE inference failed: {error:#}");
            Vec::new()
        })
    }

    /// Query side (BGE-M3 needs no instruction prefix) — used for retrieval.
    fn embed_query(&self, text: &str) -> Vec<f32> {
        self.try_embed_query(text)
            .unwrap_or_else(|error| {
                eprintln!("BGE query inference failed: {error:#}");
                Vec::new()
            })
    }
}

mod load;
