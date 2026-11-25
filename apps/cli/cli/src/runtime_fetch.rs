//! Managed `rsrs-runtime` discovery, download and installation for the host CLI.
//!
//! The runtime ships as a separate `rsrs-runtime` binary from the
//! `risense-ai/respire-runtime` repository. This module owns everything the CLI
//! needs to find, verify, download and retire runtime installations:
//!
//! * Discovery order (see [`resolve_runtime_bin`]): explicit `RSRS_RUNTIME_BIN`
//!   override, the managed directory (verified against `runtime.json`), the
//!   legacy combined-binary stable location, and finally a release-channel
//!   download. Portable images require the explicit override.
//! * Downloads come from the official release assets with a gh-proxy mirror
//!   fallback (same rule as client2 `mirrorURL` and the model installer's
//!   explicit Runtime mirror override). Archives are sha256-verified against the released
//!   `runtime-build-<triple>.json` (`archive_sha256`, with `sha256` as its
//!   legacy alias; `binary_sha256` independently checks the extracted image) and extracted with a strict whitelist (regular
//!   files and directories only, no absolute/`..`/`\`/`:`
//!   paths, bounded sizes, 0700/0600 modes). Release archives name the binary
//!   with the full triple (`rsrs-runtime-<triple>(.exe)`); install placement
//!   renames it to the bare [`RUNTIME_BINARY`] without touching its bytes.
//! * Concurrent first launches (CLI and client2) serialize on the
//!   `<installation_root>/bin/runtime/.install.lock` native file lock and the switch into
//!   `<installation_root>/bin/runtime/<version>/` is atomic; the previous version
//!   directory is kept as a rollback point.
//!
//! Runtime releases publish `runtime-build-<triple>.json` with
//! `{"runtime_version","tag","triple","binary_sha256","binary_file"}`
//! (`sha256` is an alias for `archive_sha256` only).

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::io::Write;
use std::path::Component;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::model_progress;
use respire_commands::autostart::RuntimeChannel;
use respire_commands::rpc_client::PROTOCOL_V;

/// Release repository that publishes the runtime artifacts.
pub const RUNTIME_REPO: &str = "risense-ai/respire-runtime";
const RELEASE_BASE: &str = "https://github.com";
const MIRROR_PREFIX: &str = "https://gh-proxy.com";

/// Managed runtime binary name for the running platform.
pub const RUNTIME_BINARY: &str = if cfg!(windows) {
    "rsrs-runtime.exe"
} else {
    "rsrs-runtime"
};
/// Binary name of the legacy combined CLI+runtime layout.
pub const LEGACY_COMBINED_BINARY: &str = if cfg!(windows) { "rsrs.exe" } else { "rsrs" };
/// Shared managed-install manifest, next to `<installation_root>/bin/runtime/`.
pub const MANIFEST_FILE: &str = "runtime.json";
/// Per-release integrity manifest shipped inside the archive.
pub const CORE_MANIFEST_FILE: &str = "core-runtime.json";

/// Release archives and `runtime-build-<triple>.json` name the binary with
/// the full triple (`rsrs-runtime-<triple>(.exe)`). The managed layout stores
/// it under the bare [`RUNTIME_BINARY`]; this helper returns the archive-side
/// name for `triple`.
pub fn release_binary_name(triple: &str) -> String {
    let extension = if cfg!(windows) { ".exe" } else { "" };
    format!("rsrs-runtime-{triple}{extension}")
}

/// Release asset triples, aligned with client2's target list plus the musl
/// flavors published by the runtime repository:
/// `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`, `aarch64-apple-darwin`,
/// `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
/// `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`.
///
/// Note: client2 maps darwin-amd64 releases to the obfuscated asset triple
/// `x86_64-apple-darwin` (a legacy name scramble kept for asset-name
/// compatibility with older clients). Host selection and explicit overrides
/// use that same asset name for `x86_64` Apple platforms.
pub const TRIPLES: &[&str] = &[
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
    "aarch64-apple-darwin",
    DARWIN_AMD64_RELEASE_TRIPLE,
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
];

/// Obfuscated release triple used for darwin-amd64 assets (see [`TRIPLES`]).
pub const DARWIN_AMD64_RELEASE_TRIPLE: &str = "x86_64-apple-darwin";

/// Hard extraction bounds, matching client2 `extractRuntime` exactly.
const MAX_ENTRY_BYTES: u64 = 256 << 20;
const MAX_TOTAL_BYTES: u64 = 512 << 20;
/// Hard cap for the compressed archive transfer itself.
const MAX_ARCHIVE_BYTES: u64 = 512 << 20;
/// Release metadata documents are tiny; refuse anything larger (client2: 2 MiB).
const MAX_METADATA_BYTES: usize = 2 << 20;
// The original metadata JSON is embedded as a string in the atomic selection.
const MAX_MANAGED_MANIFEST_BYTES: u64 = (MAX_METADATA_BYTES * 6 + 64 * 1024) as u64;
/// Concurrent CLI installs serialize on this lock in the existing CLI layout.
const INSTALL_LOCK_WAIT: Duration = Duration::from_secs(120);

/// Version selected for local regression fixtures.
#[cfg(test)]
const PIN_JSON: &str = include_str!("../../test-support/runtime-test-fixture.json");

// ---------------------------------------------------------------------------
// Runtime release versions
// ---------------------------------------------------------------------------

fn semver_cmp(ours: &str, theirs: &str) -> std::cmp::Ordering {
    fn split(value: &str) -> (Vec<u64>, Option<String>) {
        let (core, pre) = match value.split_once('-') {
            Some((core, pre)) => (core, Some(pre.to_owned())),
            None => (value, None),
        };
        let numbers = core
            .split('.')
            .map(|part| part.parse().unwrap_or(0))
            .collect();
        (numbers, pre)
    }
    let (ours_core, ours_pre) = split(ours);
    let (theirs_core, theirs_pre) = split(theirs);
    let by_core = ours_core.cmp(&theirs_core);
    if by_core != std::cmp::Ordering::Equal {
        return by_core;
    }
    match (ours_pre.as_deref(), theirs_pre.as_deref()) {
        (None, None) => std::cmp::Ordering::Equal,
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(a), Some(b)) => {
            let left = a.split('.').collect::<Vec<_>>();
            let right = b.split('.').collect::<Vec<_>>();
            for (x, y) in left.iter().zip(right.iter()) {
                let ordering = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(x), Ok(y)) => x.cmp(&y),
                    (Ok(_), Err(_)) => std::cmp::Ordering::Less,
                    (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
                    (Err(_), Err(_)) => x.cmp(y),
                };
                if ordering != std::cmp::Ordering::Equal {
                    return ordering;
                }
            }
            left.len().cmp(&right.len())
        }
    }
}

/// The build-time pinned runtime version plus its accept range.
#[cfg(test)]
#[derive(Debug, Deserialize)]
pub struct RuntimeFixture {
    pub required: String,
}

#[cfg(test)]
static PIN: OnceLock<std::result::Result<RuntimeFixture, String>> = OnceLock::new();

#[cfg(test)]
pub fn pinned() -> Result<&'static RuntimeFixture> {
    PIN.get_or_init(|| serde_json::from_str(PIN_JSON).map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|error| anyhow::anyhow!("invalid Runtime test fixture: {error}"))
}

/// Semver-shaped version charset shared with client2 (`semver.IsValid` plus a
/// separator ban), so version strings are always safe path components.
pub fn valid_version(version: &str) -> bool {
    fn identifier(part: &str) -> bool {
        !part.is_empty()
            && part.split('.').all(|piece| {
                !piece.is_empty()
                    && piece
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            })
    }
    fn core_and_prerelease(version: &str) -> bool {
        let (core, prerelease) = version.split_once('-').unwrap_or((version, ""));
        let numbers = core.split('.');
        if numbers.clone().count() != 3
            || !numbers
                .clone()
                .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return false;
        }
        prerelease.is_empty() || identifier(prerelease)
    }
    match version.split_once('+') {
        Some((core_pre, build)) => {
            !build.is_empty() && identifier(build) && core_and_prerelease(core_pre)
        }
        None => core_and_prerelease(version),
    }
}

/// Release asset triple for the running platform. The darwin-amd64 asset keeps
/// its obfuscated triple name (see [`TRIPLES`]); Linux follows the compiled
/// ABI, with an explicit asset override available through `RSRS_RUNTIME_TRIPLE`.
pub fn release_triple() -> Result<String> {
    if let Ok(raw) = respire_client_host::env::var("RSRS_RUNTIME_TRIPLE") {
        let trimmed = raw.trim();
        anyhow::ensure!(
            TRIPLES.contains(&trimmed),
            "RSRS_RUNTIME_TRIPLE must be one of the published triples"
        );
        return Ok(trimmed.to_owned());
    }
    let triple = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => DARWIN_AMD64_RELEASE_TRIPLE,
        ("linux", "x86_64") if cfg!(target_env = "musl") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") if cfg!(target_env = "musl") => "aarch64-unknown-linux-musl",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        _ => bail!("no published rsrs-runtime asset for this platform"),
    };
    Ok(triple.to_owned())
}

// ---------------------------------------------------------------------------
// Managed layout
// ---------------------------------------------------------------------------

/// CLI installation root (`~/.rsrs` unless isolated).
pub fn installation_root() -> PathBuf {
    respire_client_host::paths::main_data_dir()
}

/// Managed runtime directory: `<installation_root>/bin/runtime/`.
pub fn runtime_dir() -> PathBuf {
    if let Some(selected) = crate::embedding::selection() {
        return selected.runtime_dir.clone();
    }
    installation_root().join("bin").join("runtime")
}

/// Shared manifest read and written by both the CLI and client2.
pub fn manifest_path() -> PathBuf {
    if let Some(selected) = crate::embedding::selection() {
        return selected.manifest_path.clone();
    }
    installation_root().join("bin").join(MANIFEST_FILE)
}

/// Installed version directory: `<installation_root>/bin/runtime/<version>/`.
pub fn version_dir(version: &str) -> PathBuf {
    runtime_dir().join(version)
}

/// Expected managed binary path for `version`.
pub fn managed_binary_path(version: &str) -> PathBuf {
    version_dir(version).join(RUNTIME_BINARY)
}

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
    fn from_release(metadata: &BuildMetadata, bytes: &[u8]) -> Result<Self> {
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

    fn bound_metadata(&self, triple: &str) -> Result<BuildMetadata> {
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

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn safe_metadata_path(raw: &str) -> bool {
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

fn validate_installed(manifest: &ManagedManifest) -> Result<PathBuf> {
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

/// Read-only, release-proof-bound capability check before offline keygen admission.
/// Embeddings without authenticated capability metadata deliberately fail closed.
pub(crate) fn require_offline_keygen_handoff() -> Result<(PathBuf, String)> {
    let manifest = read_manifest()?.context("offline_keygen_handoff_unsupported: installed Runtime required")?;
    let binary = validate_installed(&manifest)?.canonicalize()?;
    let metadata = manifest.bound_metadata(&release_triple()?)
        .context("offline_keygen_handoff_unsupported: authenticated capability metadata required")?;
    require_handoff_capability(&metadata.capabilities)?;
    Ok((binary, manifest.sha256))
}

fn require_handoff_capability(capabilities: &BTreeMap<String, String>) -> Result<()> {
    anyhow::ensure!(capabilities.get("offline_keygen_handoff").map(String::as_str) == Some("1"),
        "offline_keygen_handoff_unsupported: upgrade Runtime before generating local keys");
    Ok(())
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// Resolve the explicit or verified managed Runtime, or install a release when
/// no selection exists. The stable CLI path does not take part in discovery.
pub fn resolve_runtime_bin() -> Result<PathBuf> {
    if let Some(selected) = crate::embedding::selection() {
        return Ok(selected.verify()?.path().to_path_buf());
    }
    if let Ok(raw) = respire_client_host::env::var("RSRS_RUNTIME_BIN") {
        let explicit = PathBuf::from(raw.trim());
        anyhow::ensure!(
            !explicit.as_os_str().is_empty(),
            "RSRS_RUNTIME_BIN is empty"
        );
        anyhow::ensure!(explicit.is_file(), "RSRS_RUNTIME_BIN does not name a file");
        return Ok(explicit);
    }
    if let Some(manifest) = read_manifest().context(ManagedRuntimeRepairRequired)? {
        return validate_installed(&manifest);
    }
    download_and_install(None, None)
}

// ---------------------------------------------------------------------------
// Release metadata and download sources
// ---------------------------------------------------------------------------

include!("runtime_fetch/metadata.rs");

include!("runtime_fetch/release_source.rs");

include!("runtime_fetch/download.rs");

include!("runtime_fetch/archive.rs");

include!("runtime_fetch/publication.rs");

include!("runtime_fetch/install.rs");

#[cfg(test)]
#[path = "runtime_fetch_tests.rs"]
pub(crate) mod runtime_fetch_tests;

#[path = "runtime_shared.rs"]
pub(crate) mod shared;

#[cfg(test)]
mod revision4_install_validation;

#[cfg(test)]
mod offline_keygen_capability_tests;
