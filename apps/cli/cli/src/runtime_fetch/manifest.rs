use super::*;

/// CLI version selection in `<installation_root>/bin/runtime.json`. Existing
/// version/hash fields remain readable; reuse also requires the release proof.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ManagedManifest {
    pub version: String,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<ReleaseProof>,
}

/// Exact metadata accepted from the official release or a trusted offline file.
/// Its digest and full file inventory travel with the atomic version selection.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReleaseProof {
    pub metadata_sha256: String,
    pub metadata_json: String,
}

/// Callers must propagate this error instead of falling back to a serve shim.
#[derive(Debug)]
pub struct ManagedRuntimeRepairRequired;

impl std::fmt::Display for ManagedRuntimeRepairRequired {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("repair required: managed Runtime integrity verification failed")
    }
}

impl std::error::Error for ManagedRuntimeRepairRequired {}

/// An automatic install must not select a new protocol for an unbound old owner.
#[derive(Debug)]
pub struct LegacyRuntimeMigrationRequired;

impl std::fmt::Display for LegacyRuntimeMigrationRequired {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("legacy_runtime_migration_required: Runtime ownership or installation state could not be verified; stop any previous Runtime and inspect the reported state before upgrading")
    }
}

impl std::error::Error for LegacyRuntimeMigrationRequired {}

impl ManagedManifest {
    pub(super) fn from_release(metadata: &BuildMetadata, bytes: &[u8]) -> Result<Self> {
        let manifest = Self {
            version: metadata.runtime_version.clone(),
            sha256: metadata.binary_sha256()?.to_owned(),
            release: Some(ReleaseProof {
                metadata_sha256: hex::encode(Sha256::digest(bytes)),
                metadata_json: std::str::from_utf8(bytes)?.to_owned(),
            }),
        };
        manifest.bound_metadata(metadata.target_triple()?)?;
        Ok(manifest)
    }

    pub(super) fn bound_metadata(&self, triple: &str) -> Result<BuildMetadata> {
        let proof = self.release.as_ref().context(
            "repair required: managed Runtime has no complete release proof; \
             reinstall with trusted schema-2 metadata and the complete archive",
        )?;
        anyhow::ensure!(
            proof.metadata_json.len() <= MAX_METADATA_BYTES
                && valid_sha256(&proof.metadata_sha256)
                && hex::encode(Sha256::digest(proof.metadata_json.as_bytes()))
                    .eq_ignore_ascii_case(&proof.metadata_sha256),
            "managed Runtime release metadata digest mismatch"
        );
        let metadata: BuildMetadata = serde_json::from_str(&proof.metadata_json)?;
        anyhow::ensure!(
            metadata.schema_version == 2,
            "repair required: legacy release metadata does not prove the complete resource set"
        );
        metadata.validate(&self.version, triple)?;
        anyhow::ensure!(
            metadata
                .files
                .iter()
                .any(|file| file.path == CORE_MANIFEST_FILE),
            "repair required: release proof does not authenticate core-runtime.json"
        );
        anyhow::ensure!(
            metadata.binary_sha256()?.eq_ignore_ascii_case(&self.sha256),
            "managed Runtime selection disagrees with its release proof"
        );
        Ok(metadata)
    }
}

/// Read the managed manifest without validating the binary on disk.
pub fn read_manifest() -> Result<Option<ManagedManifest>> {
    if let Some(selected) = crate::embedding::selection() {
        let runtime = selected.verify()?;
        // The embedding verifies Client2's selection and inventory. Do not invent
        // an official release proof for that independently validated layout.
        return Ok(Some(ManagedManifest {
            version: runtime.version().to_owned(),
            sha256: runtime.sha256().to_owned(),
            release: None,
        }));
    }
    let path = manifest_path();
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.len() <= MAX_MANAGED_MANIFEST_BYTES,
        "repair required: invalid runtime manifest"
    );
    let manifest: ManagedManifest = serde_json::from_slice(&fs::read(&path)?)
        .context("repair required: invalid runtime manifest")?;
    anyhow::ensure!(
        valid_version(&manifest.version) && valid_sha256(&manifest.sha256),
        "repair required: invalid runtime manifest"
    );
    Ok(Some(manifest))
}

pub(super) fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(super) fn safe_metadata_path(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= 1024
        && !raw.contains(['\\', ':'])
        && raw.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && {
                    let stem = part.split('.').next().unwrap_or(part).to_ascii_uppercase();
                    !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                        && !(stem.len() == 4
                            && (stem.starts_with("COM") || stem.starts_with("LPT"))
                            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
                }
                && !part.ends_with(['.', ' '])
                && !part.chars().any(char::is_control)
        })
        && Path::new(raw)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

pub(super) fn validate_installed(manifest: &ManagedManifest) -> Result<PathBuf> {
    if let Some(selected) = crate::embedding::selection() {
        let runtime = selected.verify()?;
        anyhow::ensure!(manifest.version == runtime.version()
            && manifest.sha256.eq_ignore_ascii_case(runtime.sha256()),
            "embedded Runtime differs from its selected manifest");
        return Ok(runtime.path().to_path_buf());
    }
    (|| {
        let triple = release_triple()?;
        let metadata = manifest.bound_metadata(&triple)?;
        let directory = version_dir(&manifest.version);
        // Authenticate core-runtime.json before trusting its resource declarations.
        metadata.verify_files(&directory)?;
        verify_stage(&directory, RUNTIME_BINARY, &triple, &manifest.sha256)
    })()
    .context(ManagedRuntimeRepairRequired)
}

/// A migrated owner may still be draining from its authenticated legacy image.
pub(crate) fn verified_migrated_image(image: &str) -> Result<Option<PathBuf>> {
    let Some(version) = Path::new(image).parent().and_then(Path::file_name).and_then(|name| name.to_str()) else {
        return Ok(None);
    };
    if !valid_version(version) {
        return Ok(None);
    }
    let directory = installation_root().join("runtime").join(version);
    if !crate::mcp::exe_matches_dest(image, &directory.join(RUNTIME_BINARY)) {
        return Ok(None);
    }
    let receipt = runtime_dir().join(".receipts").join(format!("{version}.json"));
    let info = match fs::symlink_metadata(&receipt) {
        Ok(info) => info,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(info.is_file() && !info.file_type().is_symlink()
        && info.len() <= MAX_MANAGED_MANIFEST_BYTES, "invalid migrated Runtime receipt");
    let manifest: ManagedManifest = serde_json::from_slice(&fs::read(receipt)?)?;
    anyhow::ensure!(manifest.version == version, "migrated Runtime receipt version mismatch");
    let triple = release_triple()?;
    let metadata = manifest.bound_metadata(&triple)?;
    metadata.verify_files(&directory)?;
    Ok(Some(verify_stage(&directory, RUNTIME_BINARY, &triple, &manifest.sha256)?))
}

/// The managed binary the running CLI should currently use, if any. Used by
/// lifecycle checks to compare a live runtime's reported executable path.
pub fn managed_runtime_path() -> Result<Option<PathBuf>> {
    match read_manifest().context(ManagedRuntimeRepairRequired)? {
        Some(manifest) => Ok(Some(validate_installed(&manifest)?)),
        None => Ok(None),
    }
}
