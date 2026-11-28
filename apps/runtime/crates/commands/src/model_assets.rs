//! Read-only model inventory, integrity verification and backend readiness.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};

use respire_app::memory::bge::model_files_present;
use respire_protocol::{
    AssetRequirement, AssetState, AssetsStatus, BackendLoadState, BackendLoadStatus,
};

/// HuggingFace repo (where the ONNX actually lives, not the BAAI source repo).
pub const MODEL_REPO: &str = "Xenova/bge-m3";
/// Verified revision (this commit's tokenizer/onnx hashes match the local read-only model).
pub const MODEL_REVISION: &str = "4de13258303883538bd53b696b452bf8099f0858";
const TOKENIZER_SHA256: &str = "6710678b12670bc442b99edc952c4d996ae309a7020c1fa0096dd245c2faf790";

pub fn model_directory() -> Result<PathBuf> {
    respire_app::memory::bge::m3_model_dir()
}


/// Cheap runtime inventory for settings UIs. Presence/size is not a checksum or
/// provider compatibility claim; inference and the installer still verify hashes.
pub fn variant_inventory() -> Result<serde_json::Value> {
    let directory = respire_app::memory::bge::discovered_model_dir()?;
    let baseline = model_files_present(&directory);
    respire_app::memory::onnx::MODEL_VARIANTS
        .iter()
        .map(|variant| {
            let state = if baseline {
                variant_file_state(
                    &directory.join(respire_app::memory::onnx::variant_file_name(variant.name)),
                    variant.size,
                )?
            } else {
                "baseline_missing"
            };
            Ok(serde_json::json!({"name":variant.name,"size":variant.size,"state":state}))
        })
        .collect::<Result<Vec<_>>>()
        .map(serde_json::Value::from)
}

#[derive(Clone)]
struct RequiredAsset<'a> {
    name: &'a str,
    version: &'a str,
    path: PathBuf,
    size: u64,
    sha256: String,
    suggested_source: &'a str,
    incompatible_reason: Option<String>,
}

const PROBE_INPUT_LIMIT: usize = 256 * 1024;
const PROBE_OUTPUT_LIMIT: usize = 64 * 1024;
const PROBE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(150);
static PROBE_HELPER_HOST: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[derive(Clone, Copy)]
pub enum BackendProbeHost {
    Serving = 1,
    PrivateControl = 2,
}

/// Only the standalone runtime installs the early private helper entry.
pub fn enable_backend_probe_host(host: BackendProbeHost) {
    PROBE_HELPER_HOST.store(host as usize, std::sync::atomic::Ordering::Release);
}

fn probe_needs_serving_admission(host: usize) -> Result<bool> {
    match host {
        1 => Ok(true),
        2 => Ok(false),
        _ => anyhow::bail!("this host image has not installed the private backend probe entry"),
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeSelection {
    profile: Option<respire_app::core_sdk::profiles::Selection>,
    engine: String,
    variant: String,
    directory: PathBuf,
    cpu_owned: bool,
    cpu_file: Option<PathBuf>,
    settings: std::collections::BTreeMap<String, String>,
}

impl ProbeSelection {
    fn capture() -> Result<Self> {
        // Read the authoritative persisted choice, not a stale worker-thread TLS copy.
        Self::capture_choice(respire_app::embedding::saved_selection()?)
    }

    fn capture_choice(profile: Option<respire_app::core_sdk::profiles::Selection>) -> Result<Self> {
        use respire_app::memory::{bge, onnx};
        let mut settings = std::collections::BTreeMap::new();
        for name in ["RSRS_DATA_DIR", "RSRS_ORT_PROFILE", "RSRS_INTRA_THREADS"] {
            if let Some(value) = respire_app::env::var_os(name) {
                settings.insert(name.to_owned(), value.into_string().map_err(|_| anyhow!("non-Unicode probe setting {name}"))?);
            }
        }
        if let Some(selection) = &profile {
            selection.validate()?;
            return Ok(Self { engine: serde_json::to_value(selection.engine)?.as_str().context("invalid profile engine")?.to_owned(),
                variant: selection.variant.clone().unwrap_or_default(), profile, directory: PathBuf::new(),
                cpu_owned: false, cpu_file: None, settings });
        }
        let engine = onnx::configured_engine()?;
        let variant = onnx::configured_variant(engine)?;
        let directory = std::path::absolute(bge::discovered_model_dir()?)?;
        let cpu_owned = bge::cpu_owned_mode(engine)?;
        let cpu_file = if cpu_owned { Some(std::path::absolute(match respire_app::env::var_os("RSRS_CPU_OWNED_ORT_FILE") {
            Some(path) => PathBuf::from(path), None => bge::cpu_ort_path(&directory)?,
        })?) } else { None };
        Ok(Self { profile, engine: serde_json::to_value(engine)?.as_str().context("invalid legacy engine")?.to_owned(),
            variant: variant.name.to_owned(), directory, cpu_owned, cpu_file, settings })
    }

    fn identity(&self) -> Result<String> {
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(self)?)))
    }

    fn probe(&self) -> Result<serde_json::Value> {
        if let Some(selection) = &self.profile {
            return respire_app::core_sdk::profiles::scoped(selection.clone(), respire_app::core_sdk::backend_load_probe);
        }
        respire_app::memory::onnx::with_selection(
            respire_app::memory::onnx::Engine::parse(&self.engine)?, &self.variant,
            respire_app::core_sdk::backend_load_probe,
        )
    }
}

fn read_probe_frame(mut input: impl Read) -> Result<Vec<u8>> {
    let mut length = [0u8; 4];
    input.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    anyhow::ensure!(length <= PROBE_INPUT_LIMIT, "probe request exceeds input limit");
    let mut bytes = vec![0; length];
    input.read_exact(&mut bytes)?;
    Ok(bytes)
}

// Only the runtime's early private entry calls this. No account, store, hooks or listener is opened.
pub fn run_backend_probe_helper() -> Result<()> {
    let bytes = read_probe_frame(std::io::stdin().lock())?;
    // The request pipe remains open as the parent's lifetime lease on Unix.
    respire_spawn::watch_probe_parent()?;
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Request { nonce: String, selection: ProbeSelection }
    let request: Request = serde_json::from_slice(&bytes)?;
    anyhow::ensure!(request.nonce.len() == 36, "invalid private probe nonce");
    let identity = request.selection.identity()?;
    let result = request.selection.probe().map_err(|error| format!("{error:#}"));
    let output = serde_json::to_vec(&serde_json::json!({"nonce":request.nonce,"selection_identity":identity,
        "pid":std::process::id(),"result":result}))?;
    anyhow::ensure!(output.len() <= PROBE_OUTPUT_LIMIT, "probe response exceeds output limit");
    std::io::stdout().lock().write_all(&output)?;
    Ok(())
}

struct OwnedProbe(respire_spawn::ProbeChild);

impl OwnedProbe {
    fn retire(&mut self) -> Result<()> {
        if self.0.try_wait()?.is_some() { return Ok(()); }
        // The child retains its OS identity; no PID lookup or process-name kill is used.
        if let Err(error) = self.0.kill() {
            if self.0.try_wait()?.is_none() { return Err(error.into()); }
        }
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if self.0.try_wait()?.is_some() { return Ok(()); }
            anyhow::ensure!(std::time::Instant::now() < until, "probe termination was not confirmed");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}

impl Drop for OwnedProbe {
    fn drop(&mut self) {
        if let Err(error) = self.retire() {
            // Never publish a drained receipt while an owned native child is unaccounted for.
            // Exit closes the job/lease pipe; Linux also has a kernel parent-death signal.
            eprintln!("fatal probe cleanup failure: {error:#}");
            std::process::exit(2);
        }
    }
}

fn probe_cancelled() -> bool {
    crate::rpc_worker::STOPPING.load(std::sync::atomic::Ordering::Acquire)
        || crate::rpc_serve::control_state() != "running"
}

fn managed_backend_probe(selection: &ProbeSelection) -> Result<serde_json::Value> {
    use std::{process::{Command, Stdio}, sync::mpsc, time::Instant};
    let serving = probe_needs_serving_admission(
        PROBE_HELPER_HOST.load(std::sync::atomic::Ordering::Acquire))?;
    anyhow::ensure!(!probe_cancelled(), "runtime is draining; probe was not started");
    let nonce = uuid::Uuid::new_v4().to_string();
    // Authenticated private verification runs while the serving host is Paused.
    // Serving probes retain drain accounting until owned-child cleanup finishes.
    let _admission = if serving {
        Some(crate::rpc_serve::begin_admission(nonce.clone(), "backend_load_probe")
            .context("runtime drain rejected the backend probe")?)
    } else {
        None
    };
    let request = serde_json::to_vec(&serde_json::json!({"nonce":nonce,"selection":selection}))?;
    anyhow::ensure!(request.len() <= PROBE_INPUT_LIMIT, "probe request exceeds input limit");
    let mut command = Command::new(std::fs::canonicalize(std::env::current_exe()?)?);
    command.env_clear();
    for name in ["SystemRoot", "SystemDrive", "USERPROFILE", "HOME", "APPDATA", "LOCALAPPDATA", "TEMP", "TMP", "TMPDIR", "XDG_CACHE_HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME"] {
        if let Some(value) = std::env::var_os(name) { command.env(name, value); }
    }
    for (name, value) in &selection.settings { command.env(name, value); }
    if selection.profile.is_none() {
        command.env("RSRS_M3_DIR", &selection.directory)
            .env("RSRS_CPU_OWNED_ORT_EXPERIMENT", if selection.cpu_owned { "1" } else { "0" });
        if let Some(path) = &selection.cpu_file { command.env("RSRS_CPU_OWNED_ORT_FILE", path); }
    }
    command.arg("--backend-load-probe-internal")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    let deadline = Instant::now() + PROBE_DEADLINE;
    // Declared before child: its writer stays alive until owned-child cleanup finishes.
    let (_lease, lease_end) = mpsc::channel::<()>();
    let mut child = OwnedProbe(respire_spawn::spawn_probe_helper(&mut command)?);
    let pid = child.0.id();
    let mut input = child.0.take_stdin().context("probe stdin missing")?;
    let output = child.0.take_stdout().context("probe stdout missing")?;
    let (send, receive) = mpsc::channel();
    let input_send = send.clone();
    std::thread::Builder::new().name("backend-probe-input".into()).spawn(move || {
        let result = input.write_all(&(request.len() as u32).to_be_bytes()).and_then(|_| input.write_all(&request));
        if let Err(error) = result { let _ = input_send.send(Err(anyhow!(error))); }
        // Keep the sole parent writer alive without holding any application lock.
        let _ = lease_end.recv();
        drop(input);
    })?;
    std::thread::Builder::new().name("backend-probe-output".into()).spawn(move || {
        let result = (|| -> Result<Vec<u8>> {
            let mut bytes = Vec::new();
            output.take(PROBE_OUTPUT_LIMIT as u64 + 1).read_to_end(&mut bytes)?;
            anyhow::ensure!(bytes.len() <= PROBE_OUTPUT_LIMIT, "probe response exceeds output limit");
            Ok(bytes)
        })();
        let _ = send.send(result);
    })?;
    let bytes = loop {
        anyhow::ensure!(!probe_cancelled(), "runtime drain cancelled native probe");
        anyhow::ensure!(Instant::now() < deadline, "native probe exceeded its hard deadline");
        match receive.recv_timeout(std::time::Duration::from_millis(20)) {
            Ok(result) => break result?,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(error) => return Err(error.into()),
        }
    };
    // EOF is not proof of exit: a provider may close stdout and then hang in teardown.
    loop {
        if let Some(status) = child.0.try_wait()? { anyhow::ensure!(status.success(), "native probe helper failed"); break; }
        anyhow::ensure!(!probe_cancelled() && Instant::now() < deadline, "native probe cancelled before confirmed exit");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let response: serde_json::Value = serde_json::from_slice(&bytes)?;
    anyhow::ensure!(response["nonce"] == nonce && response["pid"].as_u64() == Some(u64::from(pid))
        && response["selection_identity"] == selection.identity()?, "probe helper receipt identity mismatch");
    let result: std::result::Result<serde_json::Value, String> = serde_json::from_value(response["result"].clone())?;
    result.map_err(|error| anyhow!("native probe: {error}"))
}

/// File discovery changes after installation; bind configured paths, not that side effect.
fn preparation_selection(mut selection: ProbeSelection) -> Result<ProbeSelection> {
    if selection.profile.is_none() {
        selection.directory = std::path::absolute(model_directory()?)?;
        if selection.cpu_owned && respire_app::env::var_os("RSRS_CPU_OWNED_ORT_FILE").is_none() {
            selection.cpu_file = Some(std::path::absolute(respire_app::memory::bge::cpu_ort_cache_path()?)?);
        }
    }
    Ok(selection)
}

fn preparation_snapshot(
    intent: &respire_protocol::ModelInstallIntent,
) -> Result<(String, ProbeSelection)> {
    use respire_app::memory::onnx;
    if let Some(model) = &intent.model { onnx::parse_variant(model)?; }
    let authoritative = preparation_selection(ProbeSelection::capture()?)?;
    let target = if let Some(engine) = &intent.for_engine {
        anyhow::ensure!(matches!(engine.as_str(), "cpu" | "gpu" | "npu"), "invalid preparation engine");
        let engine = onnx::Engine::parse(engine)?;
        onnx::with_selection(engine, intent.model.as_deref().unwrap_or(onnx::default_variant(engine)), || {
            preparation_selection(ProbeSelection::capture_choice(None)?)
        })?
    } else {
        authoritative.clone()
    };
    // Bind the core selection, independently of host download plans and file readiness.
    let selection_sha256 = format!("{:x}", Sha256::digest(serde_json::to_vec(
        &(respire_protocol::HOST_MODEL_SCHEMA_V, intent, &authoritative, &target))?));
    Ok((selection_sha256, target))
}

/// Report core requirements for the authenticated selection without planning installation.
pub fn model_preparation_plan(intent: &respire_protocol::ModelInstallIntent) -> Result<respire_protocol::ModelPreparation> {
    let epoch = crate::rpc_worker::GENERATION.load(std::sync::atomic::Ordering::Acquire);
    let (selection_sha256, target) = preparation_snapshot(intent)?;
    let assets = file_assets_status_for(&target)?;
    anyhow::ensure!(crate::rpc_worker::GENERATION.load(std::sync::atomic::Ordering::Acquire) == epoch
        && preparation_snapshot(intent)?.0 == selection_sha256, "model preparation scope changed while reporting requirements");
    Ok(respire_protocol::ModelPreparation { selection_sha256, assets })
}

/// Authenticate installed core assets and execute a fresh backend probe.
pub fn model_preparation_verify(
    intent: &respire_protocol::ModelInstallIntent,
    selection_sha256: &str,
) -> Result<respire_protocol::ModelPreparation> {
    anyhow::ensure!(selection_sha256.len() == 64
        && selection_sha256.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "invalid model preparation selection digest");
    let epoch = crate::rpc_worker::GENERATION.load(std::sync::atomic::Ordering::Acquire);
    let (current_sha256, target) = preparation_snapshot(intent)?;
    anyhow::ensure!(current_sha256 == selection_sha256, "model preparation selection changed");
    let unchanged = || -> Result<()> {
        anyhow::ensure!(!probe_cancelled()
            && crate::rpc_worker::GENERATION.load(std::sync::atomic::Ordering::Acquire) == epoch
            && preparation_snapshot(intent)?.0 == selection_sha256, "model preparation selection changed");
        Ok(())
    };
    let files = file_assets_status_for(&target)?;
    unchanged()?;
    respire_app::memory::bge::invalidate_cpu_ort_validation()?;
    let assets = with_backend_load(files, || {
        unchanged()?;
        let evidence = managed_backend_probe(&target)?;
        unchanged()?;
        Ok(evidence)
    });
    unchanged()?;
    Ok(respire_protocol::ModelPreparation { selection_sha256: selection_sha256.to_owned(), assets })
}

pub fn assets_status() -> Result<AssetsStatus> {
    let epoch = crate::rpc_worker::GENERATION.load(std::sync::atomic::Ordering::Acquire);
    let selection = ProbeSelection::capture()?;
    let identity = selection.identity()?;
    let status = file_assets_status_for(&selection)?;
    Ok(with_backend_load(status, || {
        anyhow::ensure!(!probe_cancelled()
            && crate::rpc_worker::GENERATION.load(std::sync::atomic::Ordering::Acquire) == epoch
            && ProbeSelection::capture()?.identity()? == identity,
            "selected assets changed before native probing");
        let evidence = managed_backend_probe(&selection)?;
        anyhow::ensure!(!probe_cancelled()
            && crate::rpc_worker::GENERATION.load(std::sync::atomic::Ordering::Acquire) == epoch
            && ProbeSelection::capture()?.identity()? == identity,
            "selected assets changed during native probing; readiness discarded");
        Ok(evidence)
    }))
}

fn with_backend_load(
    mut status: AssetsStatus,
    probe: impl FnOnce() -> Result<serde_json::Value>,
) -> AssetsStatus {
    if !status.complete {
        let provider_unverified = status.assets.iter().any(|asset| asset.name == "windows-npu-provider" && asset.state != AssetState::Complete);
        status.backend_load = Some(BackendLoadStatus {
            state: if provider_unverified { BackendLoadState::Failed } else { BackendLoadState::NotRun },
            reason: Some(if provider_unverified { "npu_provider_unverified" } else { "assets_incomplete" }.to_owned()),
            evidence: None,
        });
        return status;
    }
    match probe().and_then(|value| {
        anyhow::ensure!(
            value["contract"] == 1
                && value["status"] == "loaded"
                && value["ready"] == true
                && value["fresh_session"] == true
                && value["inference_completed"] == true,
            "native backend did not prove fresh loading and inference"
        );
        serde_json::from_value(value).context("invalid native backend load evidence")
    }) {
        Ok(evidence) => {
            status.backend_load = Some(BackendLoadStatus {
                state: BackendLoadState::Loaded,
                reason: None,
                evidence: Some(evidence),
            });
        }
        Err(error) => {
            eprintln!("backend load verification failed: {error:#}");
            status.complete = false;
            status.backend_load = Some(BackendLoadStatus {
                state: BackendLoadState::Failed,
                reason: Some("backend_load_failed".to_owned()),
                evidence: None,
            });
        }
    }
    status
}

fn file_assets_status() -> Result<AssetsStatus> {
    // Pre-initialization checks stay file-only and must not open the account database.
    file_assets_status_for(&ProbeSelection::capture_choice(respire_app::core_sdk::profiles::current()?)?)
}

fn file_assets_status_for(selection: &ProbeSelection) -> Result<AssetsStatus> {
    if let Some(profile) = &selection.profile {
        profile.validate()?;
        anyhow::ensure!(matches!(profile.backend_profile.backend.as_str(), "onnx" | "mlx"),
            "asset load diagnostics require a native backend");
        let mut assets = Vec::new();
        for (name, resource) in [("profile-weights", &profile.profile.weights), ("profile-tokenizer", &profile.profile.tokenizer)] {
            let resource = resource.as_ref().context("selected profile resource missing")?;
            assets.push(RequiredAsset { name, version: "profile-v1", path: resource.path.clone(), size: resource.size,
                sha256: resource.sha256.clone(), suggested_source: "selected model profile", incompatible_reason: None });
        }
        for resource in &profile.profile.companions {
            assets.push(RequiredAsset { name: "profile-external-data", version: "profile-v1", path: resource.path.clone(), size: resource.size,
                sha256: resource.sha256.clone(), suggested_source: "selected model profile", incompatible_reason: None });
        }
        return required_assets_status(&assets);
    }
    let engine = respire_app::memory::onnx::Engine::parse(&selection.engine)?;
    let variant = respire_app::memory::onnx::parse_variant(&selection.variant)?;
    let directory = &selection.directory;
    let accelerator_supported = engine == respire_app::memory::onnx::Engine::Cpu
        || cfg!(windows)
        || cfg!(target_os = "macos");
    let incompatible_reason = (!accelerator_supported)
        .then(|| "the selected inference engine is unsupported on this OS".to_owned());
    let mut assets = vec![
        RequiredAsset {
            name: "bge-m3-tokenizer",
            version: MODEL_REVISION,
            path: directory.join("tokenizer.json"),
            size: 17_082_821,
            sha256: TOKENIZER_SHA256.to_owned(),
            suggested_source: "https://huggingface.co/Xenova/bge-m3",
            incompatible_reason: incompatible_reason.clone(),
        },
        RequiredAsset {
            name: "bge-m3-model",
            version: variant.name,
            path: directory.join(respire_app::memory::onnx::variant_file_name(variant.name)),
            size: variant.size,
            sha256: variant.sha256.to_owned(),
            suggested_source: "https://huggingface.co/Xenova/bge-m3",
            incompatible_reason,
        },
    ];
    if selection.cpu_owned {
        // The owned loader consumes the ORT container, not a second legacy ONNX model.
        assets[1] = RequiredAsset {
            name: "bge-m3-cpu-ort",
            version: respire_app::memory::bge::CPU_ORT_REVISION,
            path: selection.cpu_file.clone().context("CPU-owned snapshot lacks its container")?,
            size: respire_app::memory::bge::CPU_ORT_SIZE,
            sha256: respire_app::memory::bge::CPU_ORT_SHA256.to_owned(),
            suggested_source: respire_app::memory::bge::CPU_ORT_URL,
            incompatible_reason: None,
        };
    }
    #[cfg(windows)]
    if engine == respire_app::memory::onnx::Engine::Npu {
        let (path, size, sha256) = respire_app::memory::onnx::accelerator_catalog_metadata();
        assets.push(RequiredAsset {
            name: "windows-ml-catalog",
            version: "2.4.89",
            path,
            size,
            sha256,
            suggested_source: "https://www.nuget.org/packages/Microsoft.Windows.AI.MachineLearning/2.4.89",
            incompatible_reason: None,
        });
        let mut status = required_assets_status(&assets)?;
        // The catalog pin does not attest hardware-specific provider payloads.
        let provider = AssetRequirement {
            name: "windows-npu-provider".to_owned(),
            version: None,
            sha256: None,
            suggested_source: "host CLI accelerator installer (official hardware provider payload)".to_owned(),
            state: AssetState::Incompatible,
            path: None,
            expected_size: None,
            actual_size: None,
            actual_sha256: None,
            reason: Some("NPU provider payload integrity and compatibility are unverified: no pinned provider metadata is available; catalog verification alone does not establish readiness".to_owned()),
        };
        status.complete = false;
        // This is a provider attestation failure, not a downloadable file with a path.
        status.assets.push(provider);
        return Ok(status);
    }
    required_assets_status(&assets)
}

fn required_assets_status(assets: &[RequiredAsset<'_>]) -> Result<AssetsStatus> {
    let mut reports = Vec::with_capacity(assets.len());
    for asset in assets {
        reports.push(required_asset_status(asset)?);
    }
    let missing: Vec<_> = reports
        .iter()
        .filter(|asset| asset.state != AssetState::Complete)
        .cloned()
        .collect();
    Ok(AssetsStatus {
        complete: missing.is_empty(),
        missing,
        assets: reports,
        backend_load: None,
    })
}


include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/model_assets/parts/required_asset_status.rs"));
