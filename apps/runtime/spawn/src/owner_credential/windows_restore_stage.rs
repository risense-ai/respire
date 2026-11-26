use super::*;

const STAGED_FILE: &str = "prepared.db";
const NO_RENAME: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE;
const MOVE_SHARE: u32 = NO_RENAME | FILE_SHARE_DELETE;

/// A same-volume, owner-only restore workspace, even when the historical
/// profile inherited a broad DACL. Profile ACLs are never rewritten.
pub struct PrivateRestoreStage {
    base: OwnedHandle,
    directory: Removable,
    user: CurrentUser,
    path: PathBuf,
    file: Option<File>,
    file_id: (u32, u32, u32),
}

fn file_id(handle: HANDLE) -> io::Result<(u32, u32, u32)> {
    let data = metadata(handle)?;
    Ok((
        data.dwVolumeSerialNumber,
        data.nFileIndexHigh,
        data.nFileIndexLow,
    ))
}

fn stage_error(step: &str, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{step}: {error}"))
}
impl PrivateRestoreStage {
    pub fn create(root: &Path, name: &str) -> io::Result<Self> {
        super::super::validate_migration_leaf(name)?;
        let root = root.canonicalize()?;
        let base = pinned_base_with_sharing(&root, NO_RENAME, true)?;
        let user = CurrentUser::query_managed()?;
        user.verify_legacy_profile_owner(base.as_raw_handle())?;
        let private = user.managed_directory_descriptor()?;
        let handle = relative_open(
            base.as_raw_handle(),
            OsStr::new(name),
            META | FILE_TRAVERSE | FILE_ADD_FILE | DELETE,
            NO_RENAME,
            Some(&private),
            FILE_CREATE,
            FILE_DIRECTORY_FILE,
        )?;
        let directory = Removable(File::from(handle));
        checked_directory(directory.handle())?;
        user.verify_new_managed_directory(directory.handle())?;
        let private = user.descriptor()?;
        let handle = relative_open(
            directory.handle(),
            OsStr::new(STAGED_FILE),
            GENERIC_READ | GENERIC_WRITE | META,
            NO_RENAME,
            Some(&private),
            FILE_CREATE,
            FILE_NON_DIRECTORY_FILE,
        )?;
        let file = File::from(handle);
        let handle = file.as_raw_handle();
        checked_file(handle, Some(0))?;
        user.verify_handle(handle, true)?;
        let id = file_id(handle)?;
        let stage = Self {
            base,
            directory,
            user,
            path: root.join(name).join(STAGED_FILE),
            file: Some(file),
            file_id: id,
        };
        Ok(stage)
    }

    pub fn writer(&self) -> io::Result<File> {
        self.file
            .as_ref()
            .ok_or_else(|| denied("restore stage no longer holds its file"))?
            .try_clone()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn open_verified_read(&self) -> io::Result<File> {
        Ok(File::from(self.verify_stage(GENERIC_READ)?))
    }

    fn verify_stage(&self, access: u32) -> io::Result<OwnedHandle> {
        self.user
            .verify_new_managed_directory(self.directory.handle())?;
        let file = relative_open(
            self.directory.handle(),
            OsStr::new(STAGED_FILE),
            access | META,
            MOVE_SHARE,
            None,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE,
        )?;
        checked_file(file.as_raw_handle(), None)?;
        self.user.verify_handle(file.as_raw_handle(), true)?;
        if file_id(file.as_raw_handle())? != self.file_id {
            return Err(denied("restore stage file changed"));
        }
        Ok(file)
    }

    pub fn publish_replacing(&mut self, destination: &Path) -> io::Result<()> {
        let name = destination
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| denied("invalid restore target name"))?;
        super::super::validate_migration_leaf(name)?;
        let live = relative_open(
            self.base.as_raw_handle(),
            OsStr::new(name),
            META,
            MOVE_SHARE,
            None,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE,
        )?;
        checked_file(live.as_raw_handle(), None)
            .map_err(|e| stage_error("verify live restore target", e))?;
        drop(live);
        // SQLite connections have all closed. Release only the no-delete file
        // lease, retaining both protected directory handles across publication.
        if let Some(file) = self.file.take() {
            file.sync_all()
                .map_err(|e| stage_error("flush private restore stage", e))?;
        }
        let staged = self
            .verify_stage(0)
            .map_err(|e| stage_error("verify private restore stage", e))?;
        drop(staged);
        crate::durable::replace_private_restore_file(&self.path, destination)
            .map_err(|e| stage_error("write-through restore replace", e))?;
        let visible = relative_open(
            self.base.as_raw_handle(),
            OsStr::new(name),
            META,
            MOVE_SHARE,
            None,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE,
        )?;
        checked_file(visible.as_raw_handle(), None)?;
        self.user.verify_handle(visible.as_raw_handle(), true)?;
        if file_id(visible.as_raw_handle())? != self.file_id {
            return Err(denied("restore publication identity changed"));
        }
        Ok(())
    }

    pub fn cleanup(&mut self) -> io::Result<()> {
        drop(self.file.take());
        match self.verify_stage(DELETE) {
            Ok(file) => mark_delete(file.as_raw_handle())?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        mark_delete(self.directory.handle())
    }
}

impl Drop for PrivateRestoreStage {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn private_restore_stage_enforces_owner_only_acl_under_inherited_parent() -> io::Result<()> {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).map_err(|_| io::Error::other("fixture randomness failed"))?;
        let root = std::env::temp_dir().join(format!(
            "respire-restore-stage-test-{:016x}",
            u64::from_ne_bytes(random)
        ));
        std::fs::create_dir(&root)?;
        let _cleanup = TempDir(root.clone());
        let name = ".backup-archive-0123456789abcdef0123456789abcdef";
        let stage = PrivateRestoreStage::create(&root, name)?;
        let stage_path = stage.path().to_owned();
        let stage_dir = root.join(name);
        {
            let mut writer = stage.writer()?;
            writer.write_all(b"private-stage-bytes")?;
            writer.sync_all()?;
        }
        let mut reader = stage.open_verified_read()?;
        let mut read_back = String::new();
        std::io::Read::read_to_string(&mut reader, &mut read_back)?;
        assert_eq!(read_back, "private-stage-bytes");
        drop(reader);
        drop(stage);
        assert!(!stage_path.exists());
        assert!(!stage_dir.exists());
        Ok(())
    }
}
