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
        user.verify_managed_base(parent_handle.as_raw_handle())?;
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
        source_at(&self.parent, leaf)
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

    /// Write the exact exclusive file without reopening its pathname or
    /// leaking a writable handle. Drop this stage on any error.
    pub fn write_exact_stage(
        &mut self,
        expected: u64,
        write: impl FnOnce(&mut dyn std::io::Write) -> io::Result<()>,
    ) -> io::Result<()> {
        let verify = |length| -> io::Result<()> {
            let named = self.checked_name(&self.pending, 0)?;
            checked_file(named.as_raw_handle(), Some(length))?;
            Ok(())
        };
        verify(0)?;
        write(&mut &self.file)?;
        verify(expected)?;
        self.file.sync_all()?;
        verify(expected)
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

/// Copy pins the source against rename. Later proof checks allow deletion
/// and are dropped before handing any bytes to the body consumer.
pub struct MigrationSource(OwnedHandle);

impl MigrationSource {
    pub fn open(path: &Path, pin_for_copy: bool) -> io::Result<Self> {
        let sharing = if pin_for_copy { FILE_SHARE_READ | FILE_SHARE_WRITE } else { SHARE };
        let directory = pinned_base_with_sharing(path, sharing, true)?;
        CurrentUser::query_managed()?.verify_managed_base(directory.as_raw_handle())?;
        Ok(Self(directory))
    }

    pub fn identity(&self) -> io::Result<[u64; 2]> {
        id_pair(self.0.as_raw_handle())
    }

    /// Reopen an owned private file by an expected native identity, not
    /// by an HTTP path or an unverified pending filename.
    pub fn open_verified_file(
        &self,
        leaf: &str,
        expected: [u64; 2],
        length: u64,
    ) -> io::Result<File> {
        super::super::validate_migration_leaf(leaf)?;
        let handle = relative_open(
            self.0.as_raw_handle(), OsStr::new(leaf), GENERIC_READ | META,
            FILE_SHARE_READ, None, FILE_OPEN, FILE_NON_DIRECTORY_FILE,
        )?;
        checked_file(handle.as_raw_handle(), Some(length))?;
        CurrentUser::query_managed()?.verify_handle(handle.as_raw_handle(), true)?;
        if id_pair(handle.as_raw_handle())? != expected {
            return Err(denied("private staged file identity changed"));
        }
        Ok(File::from(handle))
    }

    pub fn file_identity(&self, leaf: &str) -> io::Result<[u64; 2]> {
        source_at(&self.0, leaf)
    }
}

fn id_pair(handle: HANDLE) -> io::Result<[u64; 2]> {
    let (volume, high, low) = identity(handle)?;
    Ok([u64::from(volume), (u64::from(high) << 32) | u64::from(low)])
}

fn source_at(parent: &OwnedHandle, leaf: &str) -> io::Result<[u64; 2]> {
    super::super::validate_migration_leaf(leaf)?;
    let handle = relative_open(
        parent.as_raw_handle(),
        OsStr::new(leaf),
        META,
        SHARE,
        None,
        FILE_OPEN,
        FILE_NON_DIRECTORY_FILE,
    )?;
    checked_file(handle.as_raw_handle(), None)?;
    id_pair(handle.as_raw_handle())
}
