use super::*;

impl BgeEmbedder {
    /// The host reads and validates model assets. Core consumes only memory buffers.
    pub fn load_bytes(model: &str, model_bytes: &[u8], tokenizer_bytes: &[u8], config: &serde_json::Value) -> Result<Self> {
        Self::load_bytes_internal(model, model_bytes, tokenizer_bytes, config, None)
    }

    pub fn load_bytes_with_native_options(model: &str, model_bytes: &[u8], tokenizer_bytes: &[u8], config: &serde_json::Value,
        native_options: &dyn Fn(*mut std::ffi::c_void, *mut std::ffi::c_void, *const std::ffi::c_void) -> Result<()>) -> Result<Self> {
        Self::load_bytes_internal(model, model_bytes, tokenizer_bytes, config, Some(native_options))
    }

    /// Probe a fresh session without installing, reusing or retiring the shared model.
    pub fn backend_load_probe(model: &str, model_bytes: &[u8], tokenizer_bytes: &[u8], config: &serde_json::Value,
        text: &str, native_options: Option<&dyn Fn(*mut std::ffi::c_void, *mut std::ffi::c_void, *const std::ffi::c_void) -> Result<()>>,
        inspect: impl FnOnce(serde_json::Value) -> Result<serde_json::Value>) -> Result<serde_json::Value> {
        let spec = Self::configuration(model, config)?;
        let mut options = InferenceOptions::from_config(config)?;
        if options.host_execution_provider || !spec.legacy_cpu() { options.cpu_owned_ort_experiment = false; }
        anyhow::ensure!(config.get("reuse_current").is_none_or(|value| value.as_bool() == Some(false)), "backend load probe reuse_current must be false when supplied");
        anyhow::ensure!(!model_bytes.is_empty() && !tokenizer_bytes.is_empty(), "backend load probe requires nonempty model and tokenizer buffers");
        anyhow::ensure!(!text.trim().is_empty(), "backend load probe requires nonempty text");
        anyhow::ensure!(!crate::onnx::inference_stalled() && !Self::sessions_stalled(), "inference unresponsive; host runtime recovery required before probing");
        let asset_id = config["asset_id"].as_str().filter(|value| !value.is_empty()).context("host model configuration missing asset_id")?;
        let identity = ModelDigests::verified(model_bytes, tokenizer_bytes, config)?.index_identity(&spec)?;
        // The canonical slot lock also prevents reset from invalidating this temporary session.
        let _slot = SHARED.lock().map_err(|_| anyhow!("embedder singleton poisoned"))?;
        // Temporary loads must not replace the resident model's timeout diagnostic.
        let execution_limit = crate::onnx::EXECUTION_LIMIT_SECS.load(std::sync::atomic::Ordering::Acquire);
        let result = (|| {
            #[cfg(feature = "cpu-owned-ort-experiment")]
            let provider = if options.cpu_owned_ort_experiment {
                Self::load_cpu_owned(model_bytes, tokenizer_bytes, options, asset_id, "", Arc::clone(&spec), Some(identity))?
            } else { Self::load_memory(model_bytes, tokenizer_bytes, options, asset_id, "", native_options, Arc::clone(&spec), identity)? };
            #[cfg(not(feature = "cpu-owned-ort-experiment"))]
            let provider = Self::load_memory(model_bytes, tokenizer_bytes, options, asset_id, "", native_options, Arc::clone(&spec), identity)?;
            let vector = provider.try_embed_query(text)?;
            anyhow::ensure!(vector.len() == provider.dims() && vector.iter().all(|value| value.is_finite()) && vector.iter().any(|value| value.abs() > 1e-8), "backend load probe produced an invalid embedding");
            let runtime = if spec.backend == Backend::Onnx { crate::onnx::native_runtime_identity() }
                else { serde_json::json!({"kind":"mlx", "module_identity_source":"host"}) };
            // Inspect while the native session is alive; no comparison session or CPU fallback.
            let result = inspect(serde_json::json!({"backend":spec.backend.as_str(), "engine":options.engine,
                "selected":provider.selected.as_ref(), "runtime":runtime,
                "fresh_session":true, "inference_completed":true, "dimensions":vector.len(),
                "model":model, "index_identity":provider.index_identity(), "node_assignment":"not_measured",
                "cpu_node_fallback":cpu_node_fallback_policy(&provider.selected)}));
            drop(provider);
            result
        })();
        crate::onnx::EXECUTION_LIMIT_SECS.store(execution_limit, std::sync::atomic::Ordering::Release);
        result
    }

    pub(super) fn load_bytes_internal(model: &str, model_bytes: &[u8], tokenizer_bytes: &[u8], config: &serde_json::Value,
        native_options: Option<&dyn Fn(*mut std::ffi::c_void, *mut std::ffi::c_void, *const std::ffi::c_void) -> Result<()>>) -> Result<Self> {
        let spec = Self::configuration(model, config)?;
        let mut options = InferenceOptions::from_config(config)?;
        if options.host_execution_provider || !spec.legacy_cpu() { options.cpu_owned_ort_experiment = false; }
        anyhow::ensure!(!options.cpu_owned_ort_experiment || native_options.is_none(),
            "CPU owned ORT experiment does not support host profiling/provider callbacks; use explicit legacy mode");
        anyhow::ensure!(!crate::onnx::inference_stalled(), "inference unresponsive; host runtime recovery required; no second session started");
        let asset_id = config["asset_id"].as_str()
            .filter(|value| !value.is_empty()).ok_or_else(|| anyhow!("host model configuration missing asset_id"))?;
        let session_options_id = config["session_options_id"].as_str().unwrap_or("");
        // An asserted digest must be checked before any native load. A fresh
        // borrowed CPU load with no assertions only needs identity on success;
        // do not charge a failed lane start for hashing its entire input.
        let mut identity = if options.cpu_owned_ort_experiment
            && config.get("model_sha256").is_none() && config.get("tokenizer_sha256").is_none()
        {
            None
        } else {
            Some(ModelDigests::verified(model_bytes, tokenizer_bytes, config)?.index_identity(&spec)?)
        };
        let resolve_identity = |identity: Option<String>| -> Result<String> {
            match identity {
                Some(identity) => Ok(identity),
                None => ModelDigests::verified(model_bytes, tokenizer_bytes, config)?.index_identity(&spec),
            }
        };
        let mut slot = SHARED.lock().map_err(|_| anyhow!("embedder singleton poisoned"))?;
        if let Some(existing) = slot.as_ref() {
            if existing.matches(asset_id, session_options_id, options, &spec) {
                let effective = resolve_identity(identity.take())?;
                if existing.index_identity() == effective {
                    return Ok(existing.clone());
                }
                identity = Some(effective);
            }
            existing.retire_assets()?;
            *slot = None;
            crate::onnx::advance_model_generation();
        }
        *slot = None;
        #[cfg(feature = "cpu-owned-ort-experiment")]
        let mut created = if options.cpu_owned_ort_experiment {
            Self::load_cpu_owned(model_bytes, tokenizer_bytes, options, asset_id, session_options_id, Arc::clone(&spec), identity)?
        } else { Self::load_memory(model_bytes, tokenizer_bytes, options, asset_id, session_options_id, native_options, Arc::clone(&spec), resolve_identity(identity)?)? };
        #[cfg(not(feature = "cpu-owned-ort-experiment"))]
        let mut created = Self::load_memory(model_bytes, tokenizer_bytes, options, asset_id, session_options_id, native_options, Arc::clone(&spec), resolve_identity(identity)?)?;
        if spec.backend == Backend::Mlx {
            // MLX shares one resident session across lanes; its weights are already resident.
            created.background_session = Some(Arc::clone(&created.session));
            created.background_stalled = Some(Arc::clone(&created.stalled));
        } else if !options.cpu_owned_ort_experiment {
            let background = crate::onnx::commit_onnx_session_with_profile(model_bytes, options.for_background(), native_options, &spec.embedding, &spec.onnx)
                .context("failed to load background bge session")?;
            created.background_stalled = Some(Arc::clone(&background.stalled));
            created.background_session = Some(Arc::new(Mutex::new(Some(Session::Onnx(background)))));
        }
        *slot = Some(created.clone());
        Ok(created)
    }

    /// Additive streaming extension: moves a sealed Core allocation without copying it.
    #[cfg(feature = "cpu-owned-ort-experiment")]
    pub fn load_owned_bytes(model: &str, model_bytes: Vec<u8>, tokenizer_bytes: &[u8], config: &serde_json::Value) -> Result<Self> {
        Self::load_owned_with_digest(model, model_bytes, tokenizer_bytes, config, None)
    }

    #[cfg(feature = "cpu-owned-ort-experiment")]
    pub fn load_streamed_bytes(model: &str, upload: ModelUpload, tokenizer_bytes: &[u8], config: &serde_json::Value) -> Result<Self> {
        let (model_bytes, digest) = upload.into_parts();
        Self::load_owned_with_digest(model, model_bytes, tokenizer_bytes, config, Some(digest))
    }

    #[cfg(feature = "cpu-owned-ort-experiment")]
    pub(super) fn load_owned_with_digest(model: &str, model_bytes: Vec<u8>, tokenizer_bytes: &[u8], config: &serde_json::Value, digest: Option<String>) -> Result<Self> {
        let spec = Self::configuration(model, config)?;
        let mut options = InferenceOptions::from_config(config)?;
        if options.host_execution_provider || !spec.legacy_cpu() { options.cpu_owned_ort_experiment = false; }
        anyhow::ensure!(options.engine == Engine::Cpu && options.cpu_owned_ort_experiment, "streamed input requires CPU owned ORT mode");
        anyhow::ensure!(config.get("reuse_current").and_then(serde_json::Value::as_bool) != Some(true), "streamed input cannot request cached reuse");
        anyhow::ensure!(!crate::onnx::inference_stalled(), "inference unresponsive; host runtime recovery required");
        let asset_id = config["asset_id"].as_str().filter(|value| !value.is_empty()).ok_or_else(|| anyhow!("host model configuration missing asset_id"))?;
        let session_options_id = config["session_options_id"].as_str().unwrap_or("");
        let digests = match digest {
            Some(digest) => ModelDigests::verified_with_model_digest(digest, tokenizer_bytes, config)?,
            None => ModelDigests::verified(&model_bytes, tokenizer_bytes, config)?,
        };
        let identity = digests.index_identity(&spec)?;
        let mut slot = SHARED.lock().map_err(|_| anyhow!("embedder singleton poisoned"))?;
        if let Some(existing) = slot.as_ref() {
            if existing.matches(asset_id, session_options_id, options, &spec) && existing.index_identity() == identity { return Ok(existing.clone()); }
            existing.retire_assets()?;
            *slot = None;
            crate::onnx::advance_model_generation();
        }
        let created = Self::load_cpu_owned_input(Some(model_bytes), &[], tokenizer_bytes, options, asset_id, session_options_id, spec, Some(identity))?;
        *slot = Some(created.clone());
        Ok(created)
    }

    pub fn load_cached(model: &str, config: &serde_json::Value) -> Result<Self> {
        let spec = Self::configuration(model, config)?;
        let mut options = InferenceOptions::from_config(config)?;
        if options.host_execution_provider || !spec.legacy_cpu() { options.cpu_owned_ort_experiment = false; }
        let asset_id = config["asset_id"].as_str().ok_or_else(|| anyhow!("host model configuration missing asset_id"))?;
        let session_options_id = config["session_options_id"].as_str().unwrap_or("");
        anyhow::ensure!(!crate::onnx::inference_stalled(), "inference unresponsive; host runtime recovery required");
        let slot = SHARED.lock().map_err(|_| anyhow!("embedder singleton poisoned"))?;
        let existing = slot.as_ref().ok_or_else(|| anyhow!("host must supply model and tokenizer bytes"))?;
        anyhow::ensure!(existing.matches(asset_id, session_options_id, options, &spec) && (config.get("model_sha256").is_none() || crate::embedding::index_identity(model, config)? == existing.index_identity()), "cached model configuration differs; host must supply model and tokenizer bytes");
        Ok(existing.clone())
    }

    pub(super) fn matches(&self, asset_id: &str, session_options_id: &str, options: InferenceOptions, spec: &ModelSpec) -> bool {
        self.asset_id.as_ref() == asset_id && self.session_options_id.as_ref() == session_options_id && self.options == options && self.model_spec.as_ref() == spec
    }

    pub(super) fn load_memory(model_bytes: &[u8], tokenizer_bytes: &[u8], options: InferenceOptions, asset_id: &str, session_options_id: &str, native_options: Option<&dyn Fn(*mut std::ffi::c_void, *mut std::ffi::c_void, *const std::ffi::c_void) -> Result<()>>, spec: Arc<ModelSpec>, index_identity: String) -> Result<Self> {
        #[cfg(test)]
        FRESH_LOADS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        anyhow::ensure!(!model_bytes.is_empty(), "host supplied an empty model");
        let session = create_session(spec.backend, model_bytes, options, native_options, &spec)?;
        let mut tokenizer = Tokenizer::from_bytes(tokenizer_bytes)
            .map_err(|e| anyhow!("failed to parse host-supplied tokenizer: {e}"))?;
        tokenizer.with_truncation(None).map_err(|error| anyhow!("configure tokenizer: {error}"))?;
        tokenizer.with_padding(None);
        Ok(Self {
            selected: Arc::from(session.selected()),
            stalled: Arc::clone(session.stalled()),
            session: Arc::new(Mutex::new(Some(session))),
            background_session: None,
            background_stalled: None,
            background: false,
            assets: Arc::new(RwLock::new(Some(ModelAssets {
                tokenizer,
                #[cfg(feature = "cpu-owned-ort-experiment")]
                cpu_owned: None,
                // CPU native sessions own their initialized weights. Accelerator probes
                // retain their input until retirement, without retaining it in idle handles.
                model_bytes: if spec.backend == Backend::Mlx || (options.engine == Engine::Cpu && !options.host_execution_provider) {
                    Box::from(&[][..])
                } else {
                    Box::from(model_bytes)
                },
            }))),
            options,
            asset_id: Arc::from(asset_id),
            session_options_id: Arc::from(session_options_id),
            model_spec: spec, index_identity: Arc::from(index_identity),
        })
    }

    #[cfg(feature = "cpu-owned-ort-experiment")]
    pub(super) fn load_cpu_owned(model_bytes: &[u8], tokenizer_bytes: &[u8], options: InferenceOptions, asset_id: &str, session_options_id: &str, spec: Arc<ModelSpec>, index_identity: Option<String>) -> Result<Self> {
        Self::load_cpu_owned_input(None, model_bytes, tokenizer_bytes, options, asset_id, session_options_id, spec, index_identity)
    }

    #[cfg(feature = "cpu-owned-ort-experiment")]
    pub(super) fn load_cpu_owned_input(owned: Option<Vec<u8>>, model_bytes: &[u8], tokenizer_bytes: &[u8], options: InferenceOptions, asset_id: &str, session_options_id: &str, spec: Arc<ModelSpec>, index_identity: Option<String>) -> Result<Self> {
        #[cfg(test)]
        FRESH_LOADS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut tokenizer = Tokenizer::from_bytes(tokenizer_bytes).map_err(|error| anyhow!("parse host-supplied tokenizer: {error}"))?;
        tokenizer.with_truncation(None).map_err(|error| anyhow!("configure tokenizer: {error}"))?;
        tokenizer.with_padding(None);
        let stalled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let background_stalled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        // Count-only lane/watchdog health owners. Neither contains model bytes.
        anyhow::ensure!(owned.is_none() || index_identity.is_some(),
            "owned model identity must be verified before transfer");
        let cpu_owned = match owned {
            Some(bytes) => crate::onnx::CpuOwnedGroup::load_owned(bytes, options, Arc::clone(&stalled), Arc::clone(&background_stalled)),
            None => crate::onnx::CpuOwnedGroup::load(model_bytes, options, Arc::clone(&stalled), Arc::clone(&background_stalled)),
        }?;
        let index_identity = match index_identity {
            Some(identity) => identity,
            None => ModelDigests::verified(model_bytes, tokenizer_bytes, &serde_json::Value::Null)?
                .index_identity(&spec)?,
        };
        Ok(Self {
            session: Arc::new(Mutex::new(None)),
            background_session: Some(Arc::new(Mutex::new(None))),
            stalled, background_stalled: Some(background_stalled),
            background: false, selected: Arc::from("CPU"), options,
            assets: Arc::new(RwLock::new(Some(ModelAssets { tokenizer, model_bytes: Box::from(&[][..]), cpu_owned: Some(cpu_owned) }))),
            asset_id: Arc::from(asset_id), session_options_id: Arc::from(session_options_id),
            model_spec: spec, index_identity: Arc::from(index_identity),
        })
    }
}
