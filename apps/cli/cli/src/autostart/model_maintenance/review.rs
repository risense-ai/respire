/// Read only committed selection metadata. Never open Core or create a library.
pub(super) fn saved_selection(profile: &Path) -> Result<Option<serde_json::Value>> {
    use rusqlite::{Connection, OpenFlags, OptionalExtension};
    let current = profile.join("rsrs.db");
    let legacy = profile.join("onememory.db");
    anyhow::ensure!(
        !(current.try_exists()? && legacy.try_exists()?),
        "both rsrs.db and the legacy database exist; select the intended library explicitly"
    );
    let path = if current.try_exists()? {
        current
    } else {
        legacy
    };
    check_path(&path)?;
    if !path.try_exists()? {
        return Ok(None);
    }
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    let value: Option<String> = connection
        .query_row(
            "SELECT value FROM meta WHERE key=?1",
            ["embedding.active.selection.v1"],
            |row| row.get(0),
        )
        .optional()?;
    value
        .map(|value| serde_json::from_str(&value).map_err(Into::into))
        .transpose()
}

fn local_plan(
    kind: ModelMaintenanceKind,
    root: &Path,
    profile: &Path,
) -> Result<ModelMaintenancePlan> {
    let selected = saved_selection(profile)?;
    if kind == ModelMaintenanceKind::ResetCpu {
        anyhow::ensure!(selected.is_none(),
            "library uses a model profile; use model switch PROFILE --backend onnx --engine cpu for CPU recovery");
    }
    let settings = cpu_settings(root)?;
    let registry = read_maintenance_file(&root.join("embedding-profiles-v1.json"))?;
    let selection_sha256 = hash(&serde_json::to_vec(&(
        MODEL_MAINTENANCE_SCHEMA_V,
        kind,
        root,
        profile,
        selected,
        registry,
        respire_client_host::env::var_os("RSRS_ENGINE"),
        respire_client_host::env::var_os("RSRS_MODEL_VARIANT"),
        respire_client_host::env::var_os("RSRS_M3_DIR"),
    ))?);
    let revision_sha256 = cpu_revision(&settings, &selection_sha256)?;
    let mut plan = ModelMaintenancePlan {
        kind,
        review: ModelMaintenanceReview {
            selection_sha256,
            revision_sha256,
        },
        preparation: None,
        catalog: None,
        uninstall_directory: None,
        cpu_candidate_sha256: None,
    };
    match kind {
        ModelMaintenanceKind::ResetCpu => {
            plan.cpu_candidate_sha256 = Some(hash(&cpu_candidate(&settings)?))
        }
        ModelMaintenanceKind::UninstallM3 => {
            let directory = local_m3_directory()?;
            check_path(&directory)?;
            anyhow::ensure!(
                !directory
                    .to_string_lossy()
                    .replace('\\', "/")
                    .contains("/usr/lib/respire/models"),
                "refusing to delete the system model dir"
            );
            plan.uninstall_directory = Some(directory);
        }
        ModelMaintenanceKind::InstallEngines => {
            #[cfg(windows)]
            {
                plan.catalog = Some(catalog_pin(root)?);
            }
        }
    }
    Ok(plan)
}

#[cfg(windows)]
fn catalog_pin(root: &Path) -> Result<ModelCatalogPin> {
    // These DLL pins were measured from the SHA-256-verified official NuGet package.
    let (arch, dll_size, dll_sha256) = if cfg!(target_arch = "x86_64") {
        (
            "x64",
            971504,
            "b401a47552a745755d65100828faefcf3df3895c759f96ac625ddf990711c8dd",
        )
    } else if cfg!(target_arch = "aarch64") {
        (
            "arm64",
            1091328,
            "457295ca2aec34db4aec7064cb401520bc85f29fd18f7e7bb9e25ddb849bedb1",
        )
    } else {
        anyhow::bail!("unsupported Windows ML architecture")
    };
    Ok(ModelCatalogPin {
        version: "2.4.89".into(),
        package_source: PACKAGE_SOURCE.into(),
        package_sha256: PACKAGE_SHA256.into(),
        archive_member: format!(
            "runtimes/win-{arch}/native/Microsoft.Windows.AI.MachineLearning.dll"
        ),
        destination: root.join("engines/winml-2.4.89/Microsoft.Windows.AI.MachineLearning.dll"),
        dll_size,
        dll_sha256: dll_sha256.into(),
    })
}

fn verify_catalog(pin: &ModelCatalogPin) -> Result<()> {
    check_path(&pin.destination)?;
    let metadata = fs::symlink_metadata(&pin.destination)?;
    anyhow::ensure!(
        metadata.is_file() && metadata.len() == pin.dll_size,
        "installed catalog is not the pinned regular file"
    );
    let mut bytes = Vec::new();
    fs::File::open(&pin.destination)?
        .take(pin.dll_size + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 == pin.dll_size && hash(&bytes) == pin.dll_sha256,
        "installed catalog differs from the official host pin"
    );
    Ok(())
}

fn verify_maintenance(
    takeover: &LibraryLock,
    maintenance: &mut super::lifecycle::Maintenance,
    plan: &ModelMaintenancePlan,
    root: &Path,
    profile: &Path,
) -> Result<ModelMaintenanceReceipt> {
    let mut receipt = ModelMaintenanceReceipt {
        plan: plan.clone(),
        phase: ModelMaintenancePhase::Verify,
        verified: false,
        config_applied: false,
        npu_ready: false,
        warning: None,
    };
    {
        let _installation = LibraryLock::acquire(&root.join("runtime"), Duration::ZERO)?;
        let _library = LibraryLock::acquire(profile, Duration::ZERO)?;
        anyhow::ensure!(
            local_plan(plan.kind, root, profile)?.review == plan.review,
            "model maintenance selection/revision changed during file verification"
        );
        match plan.kind {
            ModelMaintenanceKind::UninstallM3 => {
                let directory = plan
                    .uninstall_directory
                    .as_deref()
                    .context("host uninstall target missing")?;
                anyhow::ensure!(
                    fs::symlink_metadata(directory)
                        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
                    "model uninstall not complete; target still exists or cannot be inspected"
                );
            }
            ModelMaintenanceKind::InstallEngines => {
                if let Some(pin) = &plan.catalog {
                    verify_catalog(pin)?;
                }
            }
            ModelMaintenanceKind::ResetCpu => {
                anyhow::bail!("CPU reset requires its configuration publication receipt")
            }
        }
    }
    #[cfg(windows)]
    if plan.kind == ModelMaintenanceKind::InstallEngines {
        let preparation = plan
            .preparation
            .as_ref()
            .context("NPU preparation selection missing")?;
        let verified = super::model_control::model_install_verify_locked(
            takeover,
            maintenance,
            &respire_protocol::ModelInstallIntent {
                for_engine: Some("npu".into()),
                ..Default::default()
            },
            &preparation.selection_sha256,
        )?;
        let backend = verified
            .assets
            .backend_load
            .as_ref()
            .context("Runtime backend verification missing")?;
        let evidence = backend.evidence.as_ref().with_context(|| {
            format!(
                "Runtime returned no inference evidence: state={:?}, reason={:?}",
                backend.state, backend.reason
            )
        })?;
        anyhow::ensure!(
            verified.assets.complete
                && verified.assets.missing.is_empty()
                && !verified.assets.assets.is_empty()
                && verified
                    .assets
                    .assets
                    .iter()
                    .all(|asset| asset.state == respire_protocol::AssetState::Complete)
                && backend.state == respire_protocol::BackendLoadState::Loaded
                && backend.reason.is_none()
                && evidence.contract == 1
                && evidence.status == respire_protocol::BackendLoadState::Loaded
                && evidence.ready
                && evidence.fresh_session
                && evidence.inference_completed,
            "Runtime did not verify fresh core backend inference"
        );
        receipt.plan.preparation = Some(verified);
        receipt.warning = Some("npu_provider_unverified: core inference passed; catalog integrity does not attest unpinned provider payloads".into());
    }
    #[cfg(not(windows))]
    {
        let _ = (takeover, maintenance);
    }
    anyhow::ensure!(
        local_plan(plan.kind, root, profile)?.review == plan.review,
        "model maintenance selection/revision changed during backend verification"
    );
    receipt.verified = true;
    Ok(receipt)
}

struct CpuSettings {
    target: PathBuf,
    target_bytes: Option<Vec<u8>>,
    source: PathBuf,
    source_bytes: Option<Vec<u8>>,
    value: serde_json::Value,
}

fn read_maintenance_file(path: &Path) -> Result<Option<Vec<u8>>> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                anyhow::ensure!(
                    !metadata.file_type().is_symlink(),
                    "model maintenance path contains a symlink"
                );
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    anyhow::ensure!(
                        metadata.file_attributes() & 0x400 == 0,
                        "model maintenance path contains a reparse point"
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(
        metadata.is_file() && metadata.len() <= 64 * 1024,
        "model maintenance input must be a bounded regular file"
    );
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() <= 64 * 1024,
        "model maintenance input is too large"
    );
    Ok(Some(bytes))
}

fn cpu_settings(root: &Path) -> Result<CpuSettings> {
    let target = root.join("inference.json");
    let target_bytes = read_maintenance_file(&target)?;
    let mut source = target.clone();
    let mut source_bytes = target_bytes.clone();
    if source_bytes.is_none() && respire_client_host::env::var_os("RSRS_DATA_DIR").is_none() {
        let home = dirs::home_dir()
            .or_else(|| respire_client_host::env::var_os("HOME").map(PathBuf::from))
            .or_else(|| respire_client_host::env::var_os("USERPROFILE").map(PathBuf::from));
        for legacy in [".respire/inference.json", ".onememory/inference.json"] {
            let candidate = home
                .as_ref()
                .map(|home| home.join(legacy))
                .unwrap_or_else(|| PathBuf::from(format!("~/{legacy}")));
            if let Some(bytes) = read_maintenance_file(&candidate)? {
                source = candidate;
                source_bytes = Some(bytes);
                break;
            }
        }
    }
    let value: serde_json::Value = source_bytes
        .as_deref()
        .map(serde_json::from_slice)
        .transpose()?
        .unwrap_or_else(|| json!({"engine":"cpu"}));
    anyhow::ensure!(value.is_object(), "inference.json must contain an object");
    Ok(CpuSettings {
        target,
        target_bytes,
        source,
        source_bytes,
        value,
    })
}

fn cpu_revision(settings: &CpuSettings, selection: &str) -> Result<String> {
    let client = read_maintenance_file(&respire_client_host::paths::client_config_path())?;
    Ok(hash(&serde_json::to_vec(&(
        selection,
        &settings.target,
        &settings.target_bytes,
        &settings.source,
        &settings.source_bytes,
        &client,
    ))?))
}

fn cpu_candidate(settings: &CpuSettings) -> Result<Vec<u8>> {
    let mut value = settings.value.clone();
    if let Some(model) = value.get("model") {
        let model = model
            .as_str()
            .context("inference.json model must be a string")?
            .trim()
            .to_ascii_lowercase();
        anyhow::ensure!(
            matches!(
                model.as_str(),
                "quantized" | "fp16" | "int8" | "uint8" | "q4" | "q4f16"
            ),
            "unsupported model variant in inference.json"
        );
        value["model"] = json!(model);
    }
    value["engine"] = json!("cpu");
    value["force_cpu"] = json!(true);
    Ok(serde_json::to_vec_pretty(&value)?)
}

fn apply_cpu_reset(
    takeover: &LibraryLock,
    maintenance: &mut super::lifecycle::Maintenance,
    expected: &ModelMaintenancePlan,
    unchanged: impl Fn() -> Result<()>,
    publication_attempted: &mut bool,
) -> Result<ModelMaintenanceReceipt> {
    let (root, profile) = roots()?;
    anyhow::ensure!(
        takeover.protects(&root.join("runtime/takeover"))?,
        "model maintenance takeover mismatch"
    );
    let _installation = LibraryLock::acquire(&root.join("runtime"), Duration::ZERO)?;
    let _library = LibraryLock::acquire(&profile, Duration::ZERO)?;
    let registry_path = root.join("embedding-profiles-v1.json");
    let registry = read_maintenance_file(&registry_path)?;
    let reviewed = local_plan(ModelMaintenanceKind::ResetCpu, &root, &profile)?;
    anyhow::ensure!(
        reviewed.review == expected.review
            && reviewed.cpu_candidate_sha256 == expected.cpu_candidate_sha256
            && read_maintenance_file(&registry_path)? == registry,
        "model reset selection/revision conflict; no mutation performed"
    );
    let lock_path = root.join("inference.json.lock");
    check_path(&lock_path)?;
    match fs::symlink_metadata(&lock_path) {
        Ok(metadata) => anyhow::ensure!(
            metadata.is_file(),
            "inference configuration lock is not a regular file"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;
    lock.lock()?;
    let before = cpu_settings(&root)?;
    let _legacy_lock = if before.source != before.target {
        let path = before.source.with_file_name("inference.json.lock");
        read_maintenance_file(&path)?;
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        lock.lock()?;
        anyhow::ensure!(
            read_maintenance_file(&before.source)? == before.source_bytes,
            "legacy inference source changed while obtaining its configuration lock"
        );
        Some(lock)
    } else {
        None
    };
    let candidate = cpu_candidate(&before)?;
    anyhow::ensure!(
        cpu_revision(&before, &expected.review.selection_sha256)?
            == expected.review.revision_sha256
            && Some(hash(&candidate)) == expected.cpu_candidate_sha256,
        "model reset revision or candidate changed before CAS"
    );
    unchanged()?;
    maintenance
        .auth_launch_binding(&profile)?
        .context("model reset lost Paused authority")?;
    let staged = root.join(format!(".inference-reset-{}.json", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)?;
        file.write_all(&candidate)?;
        file.sync_all()?;
        unchanged()?;
        maintenance
            .auth_launch_binding(&profile)?
            .context("model reset lost Paused authority")?;
        let inspected = local_plan(ModelMaintenanceKind::ResetCpu, &root, &profile)?;
        anyhow::ensure!(
            inspected.review == expected.review
                && inspected.cpu_candidate_sha256 == expected.cpu_candidate_sha256,
            "model reset selection or candidate changed before CAS"
        );
        let current = cpu_settings(&root)?;
        anyhow::ensure!(
            cpu_revision(&current, &expected.review.selection_sha256)?
                == expected.review.revision_sha256
                && read_maintenance_file(&registry_path)? == registry
                && current.target_bytes == before.target_bytes,
            "model reset revision changed before CAS"
        );
        *publication_attempted = true;
        if before.target_bytes.is_some() {
            respire_spawn::durable::replace_file(&staged, &before.target)?;
        } else {
            respire_spawn::durable::publish_new_file(&staged, &before.target)?;
        }
        Ok(())
    })();
    if result.is_err() && staged.try_exists()? {
        fs::remove_file(&staged)?;
    }
    result?;
    anyhow::ensure!(
        read_maintenance_file(&before.target)?.as_deref() == Some(candidate.as_slice()),
        "CPU reset outcome unknown; inspect configuration, do not replay"
    );
    unchanged()?;
    maintenance
        .auth_launch_binding(&profile)?
        .context("model reset lost Paused authority")?;
    Ok(ModelMaintenanceReceipt {
        plan: reviewed,
        phase: ModelMaintenancePhase::Apply,
        verified: true,
        config_applied: true,
        npu_ready: false,
        warning: None,
    })
}
