use super::*;

const SHARE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

pub struct MigrationCopy {
    parent: OwnedHandle,
    path: PathBuf,
    pending: String,
    user: CurrentUser,
    file: File,
    published: bool,
}

fn identity(file: HANDLE) -> io::Result<(u32, u32, u32)> {
    let info = metadata(file)?;
    Ok((
        info.dwVolumeSerialNumber,
        info.nFileIndexHigh,
        info.nFileIndexLow,
    ))
}

impl MigrationCopy {
    pub fn create(parent: &Path, pending: &str) -> io::Result<Self> {
        super::super::validate_migration_leaf(pending)?;
        let parent_handle =
            pinned_base_with_sharing(parent, FILE_SHARE_READ | FILE_SHARE_WRITE, true)?;
        let user = CurrentUser::query_managed()?;
        user.verify_legacy_profile_owner(parent_handle.as_raw_handle())?;
        let descriptor = user.descriptor()?;
        let handle = relative_open(
            parent_handle.as_raw_handle(),
            OsStr::new(pending),
            GENERIC_READ | GENERIC_WRITE | META,
            SHARE,
            Some(&descriptor),
            FILE_CREATE,
            FILE_NON_DIRECTORY_FILE,
        )?;
        let result = Self {
            parent: parent_handle,
            path: parent.join(pending),
            pending: pending.to_owned(),
            user,
            file: File::from(handle),
            published: false,
        };
        checked_file(result.file.as_raw_handle(), Some(0))?;
        result
            .user
            .verify_handle(result.file.as_raw_handle(), true)?;
        Ok(result)
    }

    pub fn source_identity(&self, leaf: &str) -> io::Result<[u64; 2]> {
        super::super::validate_migration_leaf(leaf)?;
        let handle = relative_open(
            self.parent.as_raw_handle(),
            OsStr::new(leaf),
            META,
            SHARE,
            None,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE,
        )?;
        checked_file(handle.as_raw_handle(), None)?;
        let (volume, high, low) = identity(handle.as_raw_handle())?;
        Ok([u64::from(volume), (u64::from(high) << 32) | u64::from(low)])
    }
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn checked_name(&self, leaf: &str, access: u32) -> io::Result<OwnedHandle> {
        let entry = relative_open(
            self.parent.as_raw_handle(),
            OsStr::new(leaf),
            access | META,
            SHARE,
            None,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE,
        )?;
        checked_file(entry.as_raw_handle(), None)?;
        self.user.verify_handle(entry.as_raw_handle(), true)?;
        if identity(entry.as_raw_handle())? != identity(self.file.as_raw_handle())? {
            return Err(denied("migration backup inode changed"));
        }
        Ok(entry)
    }

    pub fn publish(mut self, final_name: &str) -> io::Result<()> {
        super::super::validate_migration_leaf(final_name)?;
        if final_name == self.pending {
            return Err(denied("migration backup names alias"));
        }
        drop(self.checked_name(&self.pending, 0)?);
        if self.file.metadata()?.len() == 0 {
            return Err(denied("empty migration backup"));
        }
        self.file.sync_all()?;
        let destination = self.path.with_file_name(final_name);
        // Existing durable helper uses MOVEFILE_WRITE_THROUGH, without overwrite.
        // The exact owner-validated parent remains pinned without delete sharing.
        crate::durable::publish_new_file(&self.path, &destination)?;
        self.published = true;
        self.checked_name(final_name, 0)?;
        Ok(())
    }
}

impl Drop for MigrationCopy {
    fn drop(&mut self) {
        if !self.published {
            if let Ok(handle) = self.checked_name(&self.pending, DELETE) {
                let _ = mark_delete(handle.as_raw_handle());
            }
        }
    }
}
