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

mod manifest;
pub use manifest::{managed_runtime_path, read_manifest, LegacyRuntimeMigrationRequired, ManagedManifest, ManagedRuntimeRepairRequired};
pub(crate) use manifest::verified_migrated_image;
use manifest::{safe_metadata_path, valid_sha256, validate_installed};

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
