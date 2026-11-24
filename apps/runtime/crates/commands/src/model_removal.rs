//! Registered-ID removal domain. No transport is enabled until shared admission
//! and cross-process asset ownership can supply `Authority` (see integration notes).
mod owned_assets;
mod removal_plan;
#[cfg(test)]
use removal_plan::uninstall;

#[cfg(test)]
fn secure_temp() -> Result<(tempfile::TempDir, PathBuf)> {
    let directory = tempfile::tempdir()?;
    #[cfg(windows)]
    {
        let system = PathBuf::from(std::env::var_os("SystemRoot").context("Windows system path")?);
        let output = std::process::Command::new(system.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"))
            .args(["-NoProfile", "-NonInteractive", "-Command",
                "$ErrorActionPreference='Stop'; $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User; $acl=New-Object Security.AccessControl.DirectorySecurity; $acl.SetOwner($sid); $acl.SetAccessRuleProtection($true,$false); $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($sid,'FullControl','ContainerInherit,ObjectInherit','None','Allow')); [System.IO.Directory]::SetAccessControl($env:RESPIRE_STAGE_TEST_TEMP,$acl)"])
            .env("RESPIRE_STAGE_TEST_TEMP", directory.path())
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "synthetic stage DACL setup failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let root = directory.path().canonicalize()?;
    Ok((directory, root))
}
pub(crate) use removal_plan::{PendingRemoval, VerifiedRemoval};
pub(crate) mod serving;

use anyhow::{Context, Result};
use respire_app::{core_sdk::profiles, embedding::Registry, lock::LibraryLock};
use respire_spawn::owner_credential::{ModelRemovalRoot, RemovalMetadata};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Binding {
    pub owner: String,
    pub canonical_profile: PathBuf,
    pub instance: uuid::Uuid,
    pub generation: u64,
    pub vault_epoch: uuid::Uuid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub model_id: String,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Outcome {
    Removed,
    CleanupPending,
}

#[derive(Debug, Serialize)]
pub(crate) struct Receipt {
    pub model_id: String,
    pub outcome: Outcome,
    pub active_changed: bool,
    pub detached_files: usize,
    pub deleted_files: usize,
    pub cleanup_error: Option<String>,
}

/// This must cover every app scope sharing the asset root, including implicit
/// M3 selection and pending switches. Missing/uncertain inventory is an error.
#[derive(Clone)]
pub(crate) enum Active {
    ImplicitM3(profiles::ModelProfile),
    Explicit(profiles::Selection),
    Uncertain,
}

pub(crate) struct Scope<'a> {
    pub profile: &'a Path,
    pub ownership: &'a LibraryLock,
    pub active: Active,
    pub pending: Option<profiles::Selection>,
}

#[derive(Debug, PartialEq, Eq)]
struct ManagedPin {
    path: PathBuf,
    size: u64,
    sha256: String,
}

struct OwnedFile {
    pin: ManagedPin,
    file: File,
    modified: std::time::SystemTime,
}

pub(crate) struct State<'a> {
    pub binding: Binding,
    pub registry: Registry,
    pub installation_root: PathBuf,
    pub asset_ownership: &'a LibraryLock,
    pub scopes: Vec<Scope<'a>>,
}

/// The serving implementation must retain ModelSwitchReservation, then
/// with_owner_store_fence, and revalidate the private owner identity throughout
/// this call. Asset publication/removal must also share ownership of the
/// Runtime-managed asset namespace. Legacy/client-managed files are never
/// candidates for this online removal operation.
pub(crate) trait Authority {
    fn with_current<T>(
        &self,
        binding: &Binding,
        action: impl FnOnce(State<'_>) -> Result<T>,
    ) -> Result<T>;
    fn check(&self, binding: &Binding) -> Result<()>;
}

fn with_inventory<T>(
    authority: &impl Authority,
    binding: &Binding,
    request: &Request,
    apply: impl FnOnce(&Path, Vec<ManagedPin>) -> Result<T>,
) -> Result<T> {
    authority.with_current(binding, |state| {
        profiles::exclusive(|| {
            anyhow::ensure!(&state.binding == binding, "scope_mismatch");
            anyhow::ensure!(!state.binding.owner.is_empty(), "owner_unavailable");
            anyhow::ensure!(
                state.binding.canonical_profile.canonicalize()? == state.binding.canonical_profile,
                "profile_not_canonical"
            );
            anyhow::ensure!(!state.scopes.is_empty(), "scope_inventory_unavailable");
            anyhow::ensure!(
                state
                    .scopes
                    .iter()
                    .any(|scope| scope.profile == state.binding.canonical_profile),
                "current_profile_unprotected"
            );
            for scope in &state.scopes {
                anyhow::ensure!(
                    scope.ownership.protects(scope.profile)?,
                    "legacy_writer_unprotected"
                );
                match &scope.active {
                    Active::Uncertain => anyhow::bail!("selection_uncertain"),
                    Active::ImplicitM3(profile) => {
                        anyhow::ensure!(
                            profile.id == "m3"
                                && profile.weights.is_some()
                                && profile.tokenizer.is_some(),
                            "selection_uncertain"
                        );
                        anyhow::ensure!(request.model_id != "m3", "model_in_use");
                    }
                    Active::Explicit(selection) => {
                        anyhow::ensure!(selection.model != request.model_id, "model_in_use")
                    }
                }
                if let Some(pending) = &scope.pending {
                    anyhow::ensure!(pending.model != request.model_id, "model_in_use");
                }
            }
            let model = state
                .registry
                .models
                .get(&request.model_id)
                .context("unknown_model")?;
            anyhow::ensure!(model.id == request.model_id, "registry_identity_mismatch");
            let root = owned_assets::OwnedAssets::open(
                &state.installation_root,
                &state.binding.canonical_profile,
            )?;
            root.verify_ownership(state.asset_ownership)?;
            let model_dir = root.model_dir(&request.model_id)?;
            let resources = [
                model.weights.as_ref().context("no_local_assets")?,
                model.tokenizer.as_ref().context("no_local_assets")?,
            ];
            let mut files = Vec::new();
            for resource in resources {
                regular_owned_path(&model_dir, &resource.path)?;
                anyhow::ensure!(
                    !files
                        .iter()
                        .any(|file: &ManagedPin| file.path == resource.path),
                    "duplicate_asset"
                );
                for scope in &state.scopes {
                    let active = match &scope.active {
                        Active::Explicit(selection) => Some(&selection.profile),
                        Active::ImplicitM3(profile) => Some(profile),
                        Active::Uncertain => None,
                    };
                    for profile in active
                        .into_iter()
                        .chain(scope.pending.as_ref().map(|selection| &selection.profile))
                    {
                        for selected in [&profile.weights, &profile.tokenizer].into_iter().flatten()
                        {
                            anyhow::ensure!(selected.path != resource.path, "model_in_use");
                            if let Some(path) = canonical_if_present(&selected.path)? {
                                anyhow::ensure!(path != resource.path, "model_in_use");
                            }
                        }
                    }
                }
                for (id, other) in &state.registry.models {
                    if id == &request.model_id {
                        continue;
                    }
                    for shared in [&other.weights, &other.tokenizer].into_iter().flatten() {
                        anyhow::ensure!(shared.path != resource.path, "shared_asset");
                        if let Some(path) = canonical_if_present(&shared.path)? {
                            anyhow::ensure!(path != resource.path, "shared_asset");
                        }
                    }
                }
                files.push(ManagedPin {
                    path: resource.path.clone(),
                    size: resource.size,
                    sha256: resource.sha256.clone(),
                });
            }
            authority.check(binding)?;
            apply(root.path(), files)
        })
    })
}

fn canonical_if_present(path: &Path) -> Result<Option<PathBuf>> {
    match path.canonicalize() {
        Ok(path) => Ok(Some(path)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn regular_owned_path(root: &Path, path: &Path) -> Result<()> {
    anyhow::ensure!(
        path.is_absolute() && path.starts_with(root) && path != root,
        "asset_not_owned"
    );
    let relative = path.strip_prefix(root)?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        anyhow::ensure!(
            matches!(component, std::path::Component::Normal(_)),
            "asset_not_owned"
        );
        current.push(component);
        let metadata = fs::symlink_metadata(&current)?;
        anyhow::ensure!(!metadata.file_type().is_symlink(), "asset_link");
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            anyhow::ensure!(
                metadata.file_attributes() & 0x400 == 0,
                "asset_reparse_point"
            );
        }
    }
    anyhow::ensure!(
        path.canonicalize()? == path && fs::symlink_metadata(path)?.is_file(),
        "asset_not_regular"
    );
    Ok(())
}

fn verify_file(
    file: &mut File,
    size: u64,
    hash: &str,
    checkpoint: &impl Fn() -> Result<()>,
) -> Result<()> {
    use std::io::{Seek, SeekFrom};
    anyhow::ensure!(
        file.metadata()?.is_file() && file.metadata()?.len() == size,
        "asset_identity_changed"
    );
    file.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        checkpoint()?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    anyhow::ensure!(
        format!("{:x}", digest.finalize()).eq_ignore_ascii_case(hash),
        "asset_identity_changed"
    );
    Ok(())
}

#[derive(Serialize)]
struct Journal<'a> {
    model_id: &'a str,
    phase: &'a str,
    files: Vec<(&'a str, u64, &'a str)>,
}

fn transact(
    root: &ModelRemovalRoot,
    model: &str,
    files: Vec<OwnedFile>,
    check: impl Fn() -> Result<()>,
) -> Result<Receipt> {
    use std::io::Write;
    // The quarantine itself is the recovery barrier across process restart.
    // It and every write are rooted in the checked owner namespace.
    let mut stage = root
        .create_quarantine()
        .context("removal_recovery_required_or_stage_unavailable")?;
    let prepare = (|| -> Result<()> {
        let pins = files
            .iter()
            .map(|file| Ok((file.leaf()?, file.pin.size, file.pin.sha256.as_str())))
            .collect::<Result<Vec<_>>>()?;
        let mut journal = stage.create_metadata(RemovalMetadata::Journal)?;
        journal.write_all(&serde_json::to_vec(&Journal {
            model_id: model,
            phase: "prepared",
            files: pins,
        })?)?;
        journal.sync_all()?;
        drop(journal);
        stage.sync()?;
        Ok(())
    })();
    if let Err(error) = prepare {
        return Err(
            error.context("removal_recovery_required: preparation failed; assets unchanged")
        );
    }
    let mut attempted = Vec::new();
    let attempt = (|| -> Result<()> {
        for (index, owned) in files.iter().enumerate() {
            check()?;
            owned.unchanged()?;
            // Include this slot if an OS error follows its actual rename.
            // Rollback also accepts the exact file already at its source.
            attempted.push(index);
            stage.move_from_model(owned.leaf()?, &owned.file, index)?;
        }
        check()?;
        let mut marker = stage.create_metadata(RemovalMetadata::CommitTemp)?;
        marker.write_all(model.as_bytes())?;
        marker.sync_all()?;
        drop(marker);
        check()?;
        stage.publish_marker()?;
        Ok(())
    })();
    if let Err(error) = attempt {
        for index in attempted.iter().rev().copied() {
            let owned = &files[index];
            stage
                .restore_to_model(owned.leaf()?, &owned.file, index)
                .with_context(|| {
                    format!("removal_recovery_required: rollback failed; {error:#}")
                })?;
        }
        for owned in &files {
            owned
                .still_names(root)
                .context("removal_recovery_required: restored asset identity unverified")?;
        }
        stage
            .delete_metadata(RemovalMetadata::CommitTemp)
            .context("removal_recovery_required: rollback cleanup failed")?;
        stage
            .sync()
            .context("removal_recovery_required: rollback state not durable")?;
        stage
            .delete_metadata(RemovalMetadata::Journal)
            .context("removal_recovery_required: rollback journal retained")?;
        stage
            .finish()
            .context("removal_recovery_required: rollback stage retained")?;
        return Err(error.context("removal_rejected: assets restored"));
    }

    let detached_files = files.len();
    let mut deleted_files = 0;
    let cleanup = (|| -> Result<()> {
        // After the commit rename, a cleanup failure cannot undo removal.
        stage.sync()?;
        for (index, owned) in files.iter().enumerate() {
            stage.delete_detached(&owned.file, index)?;
            deleted_files += 1;
        }
        drop(files);
        stage.delete_metadata(RemovalMetadata::Committed)?;
        stage.delete_metadata(RemovalMetadata::Journal)?;
        stage.finish()?;
        Ok(())
    })();
    Ok(Receipt {
        model_id: model.to_owned(),
        outcome: if cleanup.is_ok() {
            Outcome::Removed
        } else {
            Outcome::CleanupPending
        },
        active_changed: false,
        detached_files,
        deleted_files,
        cleanup_error: cleanup.err().map(|error| format!("{error:#}")),
    })
}

#[cfg(test)]
fn digest_file(mut file: File) -> Result<String> {
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(test)]
mod tests;
