fn required_asset_status(asset: &RequiredAsset<'_>) -> Result<AssetRequirement> {
    let mut requirement = AssetRequirement {
        name: asset.name.to_owned(),
        version: Some(asset.version.to_owned()),
        sha256: Some(asset.sha256.to_owned()),
        suggested_source: asset.suggested_source.to_owned(),
        state: AssetState::Missing,
        path: Some(asset.path.display().to_string()),
        expected_size: Some(asset.size),
        actual_size: None,
        actual_sha256: None,
        reason: None,
    };
    match std::fs::symlink_metadata(&asset.path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            requirement.state = AssetState::Corrupt;
            requirement.reason = Some("asset must be a regular file, not a symlink".to_owned());
        }
        Ok(metadata) if !metadata.is_file() => {
            requirement.state = AssetState::Corrupt;
            requirement.reason = Some("asset path is not a regular file".to_owned());
        }
        Ok(metadata) => {
            requirement.actual_size = Some(metadata.len());
            if metadata.len() < asset.size {
                requirement.state = AssetState::Incomplete;
                requirement.reason = Some(format!(
                    "expected {} bytes, found {}",
                    asset.size,
                    metadata.len()
                ));
            } else if metadata.len() > asset.size {
                requirement.state = AssetState::Corrupt;
                requirement.reason = Some(format!(
                    "expected {} bytes, found {}",
                    asset.size,
                    metadata.len()
                ));
            } else {
                requirement.actual_sha256 = Some(hash_asset(&asset.path)?.to_lowercase());
                let valid = requirement
                    .actual_sha256
                    .as_deref()
                    .is_some_and(|digest| digest.eq_ignore_ascii_case(&asset.sha256));
                requirement.state = if valid {
                    AssetState::Complete
                } else {
                    AssetState::Corrupt
                };
                if !valid {
                    requirement.reason = Some("size matches but SHA-256 differs".to_owned());
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            requirement.state = AssetState::Corrupt;
            requirement.reason = Some(format!("cannot inspect asset: {error}"));
        }
    }
    if let Some(reason) = &asset.incompatible_reason {
        requirement.state = AssetState::Incompatible;
        requirement.reason = Some(reason.clone());
    }
    Ok(requirement)
}

fn hash_asset(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn variant_file_state(path: &Path, expected_size: u64) -> Result<&'static str> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() && metadata.len() == expected_size => Ok("present"),
        Ok(_) => Ok("invalid_size"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok("missing"),
        Err(error) => Err(error).context("read installed model state"),
    }
}

/// Runtime readiness failures never authorize asset installation.
#[derive(Debug)]
pub(crate) struct AssetsUnavailable;

impl std::fmt::Display for AssetsUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("assets_missing: required model assets are unavailable; install or repair them with the CLI and retry")
    }
}

impl std::error::Error for AssetsUnavailable {}

/// Verify the selected profile without staging, downloading or changing selection.
pub fn verify_selection_assets(
    selection: &respire_app::core_sdk::profiles::Selection,
) -> Result<()> {
    selection.validate()?;
    selection.verify().context(AssetsUnavailable)
}

/// Reuse the pinned file inventory reported by assets.status, including CPU ORT.
pub fn verify_m3_assets() -> Result<()> {
    // Initialization verifies files only; requiring a loaded backend here creates a cold-start cycle.
    let status = file_assets_status()?;
    if !status.complete {
        return Err(anyhow::Error::new(AssetsUnavailable).context(serde_json::to_string(&status)?));
    }
    Ok(())
}

pub fn cpu_model_preparation_needed() -> Result<bool> {
    let directory = respire_app::memory::bge::discovered_model_dir()?;
    if !respire_app::memory::bge::cpu_owned_mode(respire_app::memory::onnx::configured_engine()?)? {
        return Ok(false);
    }
    Ok(!model_files_present(&directory)
        || !respire_app::memory::bge::cpu_ort_present(&directory)?)
}

#[cfg(test)]
mod tests { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/model_assets/tests.rs")); }
