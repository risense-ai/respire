//! Owner-only import staging custody and crash-orphan startup cleanup.
use crate::runtime_http::v1::{problem::V1Result, transport};
use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use uuid::Uuid;

pub(super) struct StagedImport {
    #[cfg(windows)]
    stage: respire_spawn::owner_credential::PrivateRestoreStage,
    #[cfg(not(windows))]
    path: tempfile::TempPath,
}

impl StagedImport {
    pub(super) fn create_and_write(
        profile: &Path,
        operation_hint: uuid::Uuid,
        write_fn: impl FnOnce(&mut std::fs::File) -> V1Result<()>,
    ) -> V1Result<Self> {
        #[cfg(windows)]
        {
            let leaf = format!(".backup-archive-{}", operation_hint.simple());
            let stage = respire_spawn::owner_credential::PrivateRestoreStage::create(profile, &leaf)
                .map_err(|_| transport::unavailable())?;
            let mut file = stage.writer().map_err(|_| transport::unavailable())?;
            write_fn(&mut file)?;
            use std::io::Write;
            file.flush().map_err(|_| transport::unavailable())?;
            file.sync_all().map_err(|_| transport::unavailable())?;
            drop(file);
            let staged = Self { stage };
            staged.verify_private()?;
            Ok(staged)
        }
        #[cfg(not(windows))]
        {
            let _ = operation_hint;
            use std::io::Write;
            use std::os::unix::fs::PermissionsExt;
            let mut temp = tempfile::Builder::new()
                .prefix(&format!(".backup-archive-{}-", Uuid::new_v4().simple()))
                .suffix(".pending")
                .tempfile_in(profile)
                .map_err(|_| transport::unavailable())?;
            temp.as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|_| transport::unavailable())?;
            write_fn(temp.as_file_mut())?;
            temp.as_file_mut().flush().map_err(|_| transport::unavailable())?;
            temp.as_file().sync_all().map_err(|_| transport::unavailable())?;
            let staged = Self {
                path: temp.into_temp_path(),
            };
            staged.verify_private()?;
            Ok(staged)
        }
    }

    pub(super) fn path(&self) -> &Path {
        #[cfg(windows)]
        {
            self.stage.path()
        }
        #[cfg(not(windows))]
        {
            self.path.as_ref()
        }
    }

    pub(super) fn verify_private(&self) -> V1Result<()> {
        #[cfg(windows)]
        {
            self.stage
                .open_verified_read()
                .map(|_| ())
                .map_err(|_| transport::unavailable())
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let meta = std::fs::symlink_metadata(self.path.as_ref())
                .map_err(|_| transport::unavailable())?;
            if !meta.is_file()
                || meta.file_type().is_symlink()
                || (meta.permissions().mode() & 0o777) != 0o600
                || meta.uid() != current_euid()
            {
                return Err(transport::unavailable());
            }
            Ok(())
        }
    }
}

#[cfg(unix)]
fn current_euid() -> u32 {
    respire_spawn::effective_uid()
}

fn is_hex32(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_stage_file_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".pending") else {
        return false;
    };
    let Some(rest) = stem
        .strip_prefix(".backup-archive-")
        .or_else(|| stem.strip_prefix(".backup-export-"))
    else {
        return false;
    };
    !rest.is_empty()
        && rest.len() <= 64
        && rest
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn is_stage_dir_name(name: &str) -> bool {
    let Some(rest) = name
        .strip_prefix(".backup-archive-")
        .or_else(|| name.strip_prefix(".backup-export-"))
    else {
        return false;
    };
    is_hex32(rest)
}

fn is_safe_regular_owner_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !meta.is_file() || meta.file_type().is_symlink() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != current_euid() {
            return false;
        }
    }
    true
}

fn try_remove_stage_dir(dir: &Path) {
    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        return;
    };
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != current_euid() {
            return;
        }
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<PathBuf> = Vec::new();
    for (idx, item) in entries.enumerate() {
        if idx >= 16 {
            return;
        }
        let Ok(entry) = item else {
            return;
        };
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            return;
        };
        if !matches!(
            name_str,
            "prepared.db" | "prepared.db-journal" | "prepared.db-wal" | "prepared.db-shm"
        ) {
            return;
        }
        let child = entry.path();
        if !is_safe_regular_owner_file(&child) {
            return;
        }
        children.push(child);
    }
    for child in children {
        if std::fs::remove_file(&child).is_err() {
            return;
        }
    }
    let _ = std::fs::remove_dir(dir);
}

/// Remove crashed orphaned `.backup-archive-*` / `.backup-export-*` stage files and directories
/// inside a verified canonical profile protected by `LibraryLock`.
pub(in crate::runtime_http::v1) fn sweep_orphaned_stages(
    profile: &Path,
    ownership: &respire_app::lock::LibraryLock,
) {
    if !ownership.protects(profile).unwrap_or(false)
        || profile.canonicalize().ok().as_deref() != Some(profile)
    {
        return;
    }
    let Ok(entries) = std::fs::read_dir(profile) else {
        return;
    };
    for (idx, item) in entries.enumerate() {
        if idx >= 4096 {
            break;
        }
        let Ok(entry) = item else {
            continue;
        };
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        let path = entry.path();
        if is_stage_file_name(name_str) && is_safe_regular_owner_file(&path) {
            let _ = std::fs::remove_file(&path);
        } else if is_stage_dir_name(name_str) {
            try_remove_stage_dir(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use std::io::Write;
    use std::time::Duration;

    #[test]
    fn sweep_orphaned_stages_removes_crashed_stages_only() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().canonicalize()?;
        let lock = respire_app::lock::LibraryLock::acquire(&root, Duration::from_secs(5))?;
        let orphan_imp_file = root.join(".backup-archive-0123456789abcdef0123456789abcdef.pending");
        let orphan_exp_file = root.join(".backup-export-0123456789abcdef0123456789abcdef.pending");
        let orphan_imp_dir = root.join(".backup-archive-0123456789abcdef0123456789abcdef");
        let orphan_exp_dir = root.join(".backup-export-fedcba9876543210fedcba9876543210");
        let unexpected_dir = root.join(".backup-archive-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let keep = root.join("keep.txt");
        std::fs::write(&orphan_imp_file, b"stale")?;
        std::fs::write(&orphan_exp_file, b"stale")?;
        std::fs::create_dir(&orphan_imp_dir)?;
        std::fs::write(orphan_imp_dir.join("prepared.db"), b"db")?;
        std::fs::create_dir(&orphan_exp_dir)?;
        std::fs::write(orphan_exp_dir.join("prepared.db"), b"db")?;
        std::fs::create_dir(&unexpected_dir)?;
        std::fs::write(unexpected_dir.join("unexpected.txt"), b"keep")?;
        std::fs::write(&keep, b"unrelated")?;
        #[cfg(unix)]
        let linked = {
            let p = root.join(".backup-archive-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.pending");
            std::os::unix::fs::symlink(&keep, &p)?;
            p
        };

        sweep_orphaned_stages(&root, &lock);

        assert!(!orphan_imp_file.exists());
        assert!(!orphan_exp_file.exists());
        assert!(!orphan_imp_dir.exists());
        assert!(!orphan_exp_dir.exists());
        assert!(keep.exists());
        assert!(unexpected_dir.join("unexpected.txt").exists());
        #[cfg(unix)]
        assert!(std::fs::symlink_metadata(&linked).is_ok());
        Ok(())
    }

    #[test]
    fn staged_import_enforces_owner_only_permissions() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let staged = StagedImport::create_and_write(dir.path(), uuid::Uuid::new_v4(), |file| {
            file.write_all(b"owner-only-bytes")
                .map_err(|_| transport::unavailable())
        })
        .map_err(|e| anyhow::anyhow!("create stage failed: {e:?}"))?;
        staged
            .verify_private()
            .map_err(|e| anyhow::anyhow!("private verification failed: {e:?}"))?;
        let path = staged.path().to_owned();
        assert!(path.exists());
        drop(staged);
        assert!(!path.exists());
        Ok(())
    }
}
