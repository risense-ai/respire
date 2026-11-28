    use super::*;
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all="lowercase")]
    pub enum Engine { Npu, Gpu, #[default] Cpu }
    impl Engine {
        pub fn parse(value: &str) -> Result<Self> {
            match value { "cpu" => Ok(Self::Cpu), "gpu" => Ok(Self::Gpu), "npu" => Ok(Self::Npu), _ => bail!("engine must be npu, gpu, or cpu; automatic mode is no longer supported") }
        }
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ModelVariant { pub name: &'static str, pub sha256: &'static str, pub size: u64 }
    /// Pinned Xenova/bge-m3@4de13258 ONNX variants.
    pub const MODEL_VARIANTS: &[ModelVariant] = &[
        ModelVariant { name: "quantized", sha256: "0826f8c1ab9edf1801db86c61919d4d108e8bfc0b809ec823ad366882ff0b77d", size: 569694530 },
        ModelVariant { name: "fp16", sha256: "4f1a646a3d4f39985589e9991a717044ede8278617fe55e3d246838bc05055e9", size: 1133992936 },
        ModelVariant { name: "int8", sha256: "a206e10e995aa2a833924bcd725ba5dd6c3425cd34bac3cf2b5677cd2a1c51d6", size: 568456694 },
        ModelVariant { name: "uint8", sha256: "3222211565d0d5da2fe89e0c40856ed879e5c5dd3369fbbe685213469498e811", size: 568456765 },
        ModelVariant { name: "q4", sha256: "4f6d9786712f8dbf00efbb08ffd819395ef05d860249e852b4efc16022f753a6", size: 1248237611 },
        ModelVariant { name: "q4f16", sha256: "8305fdfa48b5702ecb4a6d84c1a52a4d1cb27d69880ff7f69d1a93d795864f25", size: 699932840 },
    ];
    pub const M3_PROFILE_VARIANTS: &[ModelVariant] = &[
        ModelVariant { name: "bnb4", size: 1229364419, sha256: "3781d63fa70c05bd65e1d4ed66ca48933a36b3ff7c75b89eea051d8d625cc093" },
        ModelVariant { name: "fp32", size: 607298, sha256: "5d89a0010dd39aa2cfa8b22bb49f06904c5bbf5877135f877da419480f40cde3" },
    ];
    pub const M3_EXTERNAL_DATA: ModelVariant = ModelVariant { name: "model.onnx_data", size: 2266820608, sha256: "1eebfb28493f67bba03ce0ef64bfdc7fc5a3bd9d7493f818bb1d78cd798416b4" };
    pub const GRANITE_311M_VARIANTS: &[ModelVariant] = &[
        ModelVariant { name: "quint8_avx2", size: 313421909, sha256: "f1fdd44e7e1ac51f12ab7957c7bd092e064d596c288513bf9d326842f669edee" },
        ModelVariant { name: "standard", size: 1247170481, sha256: "75f9f258bf5013f5fe8a4dad61dd0fd16ac0cbaa7a106e3d3f41c2d04a42d541" },
    ];
    pub const GRANITE_97M_VARIANTS: &[ModelVariant] = &[
        ModelVariant { name: "quint8_avx2", size: 98247878, sha256: "a6022dd8220ea6f6595562a1328ee216f4a94faa55362f2f4747c80f1e78772e" },
        ModelVariant { name: "standard", size: 390004608, sha256: "68e592b160673d30250824c1116bc6ab33f70efb22b97c9e1d7ce1e69c1c9d70" },
    ];
    pub const BGE_BASE_ZH_VARIANTS: &[ModelVariant] = &[
        ModelVariant { name: "quantized", size: 102868746, sha256: "b665f3bba56c3119bc76ba131ebcc544d720a7408cb11581bdf354aaa0198d43" },
        ModelVariant { name: "fp16", size: 203679052, sha256: "9ed3f10ed06aafe0320567019d18fdec255339350525a33ae18f85f4def288f0" },
        ModelVariant { name: "int8", size: 102407793, sha256: "c11bdcfad37eaf53e43362885cbe5fad1b44d5b704da753a04f8c0b2f7768064" },
        ModelVariant { name: "uint8", size: 102407824, sha256: "5e2f9e9f07472751e0a5a0b21bcfa49cb0ac1eaab094784a3f30425a8bcc1d80" },
        ModelVariant { name: "q4", size: 120342974, sha256: "dab1bb68aaf39bbfa4ef7d218091174f01cd25781a07f36c654a2e3037d4376a" },
        ModelVariant { name: "q4f16", size: 81549931, sha256: "8278a219a9ae056860dcde7ce8241bee057a6ab0243454c85facf4d6e8325777" },
        ModelVariant { name: "bnb4", size: 115035086, sha256: "85bb3a0990c1ebf3e575d7a43938da383ae7233eba27af7e19341553e4583594" },
        ModelVariant { name: "fp32", size: 406953171, sha256: "5e5619f7cca7380b824d329c157dba10bee7cc00d0c139e82fdb7906051b8e4f" },
    ];
    pub fn parse_variant(value: &str) -> Result<ModelVariant> {
        let wanted = value.trim().to_ascii_lowercase();
        MODEL_VARIANTS.iter().copied().find(|variant| variant.name == wanted).ok_or_else(|| {
            anyhow::anyhow!("model variant must be one of: {}",
                MODEL_VARIANTS.iter().map(|variant| variant.name).collect::<Vec<_>>().join(", "))
        })
    }
    pub fn variant_file_name(variant: &str) -> String {
        match variant {
            "fp32" | "standard" => "onnx/model.onnx".to_owned(),
            _ => format!("onnx/model_{variant}.onnx"),
        }
    }

    pub fn has_legacy_settings() -> Result<bool> {
        let (path, _) = read_inference_json()?;
        Ok(path.is_file() || crate::env::var_os("RSRS_M3_DIR").is_some()
            || crate::env::var_os("RSRS_ENGINE").is_some()
            || crate::env::var_os("RSRS_MODEL_VARIANT").is_some()
            || crate::env::var_os("RSRS_CPU_OWNED_ORT_FILE").is_some())
    }
    /// CPU keeps the quantized graph; accelerators default to fp16.
    pub fn default_variant(engine: Engine) -> &'static str {
        match engine { Engine::Cpu => "quantized", _ => "fp16" }
    }
    pub(crate) fn settings_dir() -> std::path::PathBuf {
        crate::env::var_os("RSRS_DATA_DIR").map(std::path::PathBuf::from)
            .unwrap_or_else(|| bge::expand_home("~/.rsrs"))
    }
    pub(crate) fn host_cache_dir() -> std::path::PathBuf { settings_dir().join("engines/openvino-cache") }
    fn read_inference_json() -> Result<(std::path::PathBuf, Value)> {
        let mut path = settings_dir().join("inference.json");
        if !path.exists() && crate::env::var_os("RSRS_DATA_DIR").is_none() {
            if let Some(home) = dirs::home_dir() {
                for legacy in [".respire", ".onememory"] {
                    let candidate = home.join(legacy).join("inference.json");
                    if candidate.exists() { path = candidate; break; }
                }
            }
        }
        let value = if path.exists() { serde_json::from_slice::<Value>(&std::fs::read(&path)?)? }
            else { json!({"engine":"cpu"}) };
        anyhow::ensure!(value.is_object(), "inference.json must contain an object");
        Ok((path, value))
    }
    fn stored_variant(value: &Value) -> Result<Option<ModelVariant>> {
        value.get("model").map(|model| {
            parse_variant(model.as_str().context("inference.json model must be a string")?)
        }).transpose()
    }
    pub fn configured_engine() -> Result<Engine> {
        if let Some((engine, _)) = crate::model_selection::selection() { return Ok(engine); }
        let (_, value) = read_inference_json()?;
        if value["force_cpu"].as_bool() == Some(true) { return Ok(Engine::Cpu); }
        if let Ok(value) = crate::env::var("RSRS_ENGINE") { return Engine::parse(&value); }
        match value["engine"].as_str().context("inference.json missing engine")? {
            "auto" => Ok(Engine::Cpu), value => Engine::parse(value),
        }
    }
    /// Selected onnx variant; falls back to the engine default when unset.
    pub fn configured_variant(engine: Engine) -> Result<ModelVariant> {
        if let Some((_, variant)) = crate::model_selection::selection() { return Ok(variant); }
        if let Ok(value) = crate::env::var("RSRS_MODEL_VARIANT") { return parse_variant(&value); }
        let (_, value) = read_inference_json()?;
        stored_variant(&value)?.map_or_else(|| parse_variant(default_variant(engine)), Ok)
    }
    /// Whether the operator picked a variant (setting or env), not the engine default.
    pub fn explicit_model_selection() -> Result<bool> {
        if crate::model_selection::selection().is_some() { return Ok(true); }
        if crate::env::var("RSRS_MODEL_VARIANT").is_ok() { return Ok(true); }
        let (_, value) = read_inference_json()?;
        Ok(stored_variant(&value)?.is_some())
    }
    pub fn inference_status() -> Result<Value> {
        if crate::worker::enabled() { return crate::worker::status(); }
        let mut status: Value = execute("engine_control", json!({"action":"inference_status"}))?;
        let diagnostics = crate::host::provider_diagnostics()?;
        if !diagnostics.is_empty() { status["provider_registration_errors"] = json!(diagnostics); }
        Ok(status)
    }
    pub(crate) fn engine_settings(engine: Engine, force_cpu: bool, model: Option<&str>) -> Result<(std::path::PathBuf, Value)> {
        let directory = settings_dir();
        let (_, existing) = read_inference_json()?;
        let selected = match model {
            Some(model) => Some(parse_variant(model)?),
            None => stored_variant(&existing)?,
        };
        let mut value = existing;
        value["engine"] = json!(engine);
        value["force_cpu"] = json!(force_cpu);
        if let Some(model) = selected { value["model"] = json!(model.name); }
        Ok((directory.join("inference.json"), value))
    }
    fn commit_engine_settings(path: &std::path::Path, value: &Value, reset: impl FnOnce() -> Result<()>) -> Result<()> {
        use std::io::Write;
        reset()?;
        let parent = path.parent().context("inference settings have no parent directory")?;
        std::fs::create_dir_all(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(&serde_json::to_vec_pretty(value)?)?;
        temp.as_file().sync_all()?;
        temp.persist(path).map_err(|error| error.error)?;
        #[cfg(unix)]
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    }

    pub(crate) fn inference_config_lock() -> Result<std::fs::File> {
        let directory = settings_dir();
        std::fs::create_dir_all(&directory)?;
        let path = directory.join("inference.json.lock");
        let lock = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
        lock.lock()?;
        Ok(lock)
    }
    /// Thread-local draft for host preparation; does not change settings or sessions.
    pub fn with_selection<T>(engine: Engine, variant: &str, action: impl FnOnce() -> Result<T>) -> Result<T> {
        crate::model_selection::scoped(engine, parse_variant(variant)?, action)
    }
    /// Verify installed pinned files and the selected host path without loading or downloading.
    pub fn validate_selection(engine: Engine, variant: &str) -> Result<()> {
        let variant = parse_variant(variant)?;
        crate::model_selection::check_overrides(engine, variant)?;
        crate::model_selection::scoped(engine, variant, || crate::model_selection::validate(engine, variant))
    }
    /// Validate, load, infer and persist without ever downloading assets.
    pub fn load_selection(engine: Engine, variant: &str) -> Result<Value> {
        anyhow::ensure!(crate::profiles::current()?.is_none(),"library has an active model profile; use model switch to change it");
        let _gate = crate::model_selection::gate(true)?;
        let variant = parse_variant(variant)?;
        crate::model_selection::check_overrides(engine, variant)?;
        execute("host_load_selection", json!({"engine":engine,"variant":variant.name}))
    }
    pub fn save_engine(engine: Engine) -> Result<()> { save_engine_with_variant(engine, None) }
    pub fn save_engine_with_variant(engine: Engine, model: Option<&str>) -> Result<()> {
        save_engine_with_variant_revision(engine, model, None)?;
        Ok(())
    }
    pub fn save_engine_with_variant_revision(engine: Engine, model: Option<&str>, expected_revision: Option<&str>) -> Result<String> {
        anyhow::ensure!(crate::profiles::current()?.is_none(),"library has an active model profile; use model switch to change it");
        let _gate = crate::model_selection::gate(true)?;
        let _config_lock = inference_config_lock()?;
        let revision_path = settings_dir().join("inference.json");
        if let Some(expected) = expected_revision { ensure_engine_revision(&revision_path, expected)?; }
        let (path, value) = engine_settings(engine, false, model)?;
        commit_engine_settings(&path, &value, reset_sessions_locked)?;
        Ok(bytes_revision(&serde_json::to_vec_pretty(&value)?))
    }
    /// Persist an explicit model variant selection without touching the engine.
    pub fn save_model_variant(model: &str) -> Result<()> {
        anyhow::ensure!(crate::profiles::current()?.is_none(),"library has an active model profile; use model switch to change it");
        let _gate = crate::model_selection::gate(true)?;
        let _config_lock = inference_config_lock()?;
        let (path, mut value) = read_inference_json()?;
        if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
        value["model"] = json!(parse_variant(model)?.name);
        if value.get("engine").is_none() { value["engine"] = json!("cpu"); }
        commit_engine_settings(&path, &value, || Ok(()))
    }
    pub fn reset_sessions() -> Result<()> {
        let _gate = crate::model_selection::gate(true)?;
        reset_sessions_locked()
    }
    // The caller holds the model gate through reset.
    pub(crate) fn reset_sessions_locked() -> Result<()> {
        execute::<()>("engine_control", json!({"action":"reset"}))?;
        crate::host::reset_model_cache()
    }
    pub fn reset_cpu_config() -> Result<()> {
        let _gate = crate::model_selection::gate(true)?;
        let _config_lock = inference_config_lock()?;
        let (path, value) = engine_settings(Engine::Cpu, true, None)?;
        commit_engine_settings(&path, &value, || Ok(()))
    }
    fn bytes_revision(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
    }
    fn ensure_engine_revision(path: &std::path::Path, expected: &str) -> Result<()> {
        anyhow::ensure!(expected.len() == 64 && expected.bytes().all(|byte| byte.is_ascii_digit() || byte.is_ascii_lowercase()),
            "engine revision must be a lowercase SHA-256 digest");
        let actual = if path.exists() { std::fs::read(path)? } else { Vec::new() };
        anyhow::ensure!(bytes_revision(&actual) == expected,
            "engine revision conflict: inference settings changed since this operation started");
        Ok(())
    }
    #[cfg(windows)]
    fn provider_root() -> std::path::PathBuf {
        std::env::var_os("RESPIRE_CANDIDATE_PROVIDER_ROOT")
            .filter(|_| std::env::var_os("RESPIRE_PRIVATE_CORE_CANDIDATE").as_deref() == Some(std::ffi::OsStr::new("1")))
            .map(std::path::PathBuf::from).unwrap_or_else(settings_dir)
    }
    #[cfg(windows)]
    pub fn accelerator_catalog_metadata() -> (std::path::PathBuf, u64, String) {
        use sha2::{Digest, Sha256};
        // Embedded bytes come from the pinned, SHA-256-verified official NuGet payload.
        let expected = crate::host_winml::DLL;
        (
            provider_root().join("engines/winml-2.4.89/Microsoft.Windows.AI.MachineLearning.dll"),
            expected.len() as u64,
            format!("{:x}", Sha256::digest(expected)),
        )
    }
    pub fn accelerator_catalog_path() -> Result<Option<std::path::PathBuf>> {
        #[cfg(windows)] {
            let (dll, size, sha256) = accelerator_catalog_metadata();
            crate::model_selection::verify(&dll, size, &sha256)
                .context("assets_missing: Windows ML catalog is missing or invalid; install or repair it with the host CLI")?;
            Ok(Some(dll))
        }
        #[cfg(not(windows))] { Ok(None) }
    }
    #[cfg(windows)]
    pub(crate) fn host_providers() -> Result<Vec<crate::host_winml::Provider>> {
        let dll = accelerator_catalog_path()?.context("Windows ML catalog path missing")?;
        let mut providers = crate::host_winml::Catalog::open(&dll)?.providers()?;
        let manifest = provider_root().join("engines/providers.json");
        if manifest.exists() {
            let saved: std::collections::BTreeMap<String, std::path::PathBuf> = serde_json::from_slice(&std::fs::read(manifest)?)?;
            for (name, path) in saved { providers.push(crate::host_winml::Provider { name, ready:path.is_file(), path:Some(path) }); }
        }
        Ok(providers)
    }
    #[cfg(test)]
    mod variant_settings_tests {
        use super::*;
        #[test]
        fn failed_session_reset_does_not_persist_a_partial_selection() -> Result<()> {
            let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos();
            let directory = std::env::temp_dir().join(format!("respire-engine-commit-{}-{nonce}",std::process::id()));
            std::fs::create_dir(&directory)?;
            let path = directory.join("inference.json");
            let previous = br#"{"engine":"cpu","model":"quantized"}"#;
            std::fs::write(&path, previous)?;
            let selected = json!({"engine":"gpu","model":"fp16","force_cpu":false});
            assert!(commit_engine_settings(&path, &selected, || anyhow::bail!("native reset failed")).is_err());
            assert_eq!(std::fs::read(&path)?, previous);
            commit_engine_settings(&path, &selected, || Ok(()))?;
            assert_eq!(serde_json::from_slice::<Value>(&std::fs::read(&path)?)?, selected);
            std::fs::remove_dir_all(directory)?;
            Ok(())
        }
        #[test]
        fn missing_legacy_selection_is_distinct_from_malformed_selection() -> Result<()> {
            assert_eq!(stored_variant(&json!({"engine":"gpu"}))?, None);
            assert_eq!(stored_variant(&json!({"model":" FP16 "}))?.map(|v| v.name), Some("fp16"));
            for value in [json!(null), json!(1), json!(false), json!([]), json!({})] {
                assert!(stored_variant(&json!({"model":value})).is_err());
            }
            assert!(stored_variant(&json!({"model":"unknown"})).is_err());
            Ok(())
        }
        #[test]
        fn engine_revision_fence_rejects_changed_settings() -> Result<()> {
            let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos();
            let path = std::env::temp_dir().join(format!("respire-engine-revision-{}-{nonce}",std::process::id()));
            let previous = br#"{"engine":"cpu","model":"quantized"}"#;
            std::fs::write(&path, previous)?;
            ensure_engine_revision(&path, &bytes_revision(previous))?;
            assert!(ensure_engine_revision(&path, &bytes_revision(b"changed")).is_err());
            std::fs::remove_file(&path)?;
            Ok(())
        }
    }
    pub fn install_accelerators() -> Result<Vec<String>> { bail!("accelerator installation is host-managed; run rsrs model install-engines from the host CLI") }
