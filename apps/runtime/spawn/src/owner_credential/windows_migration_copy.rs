use super::*;

const SHARE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

pub struct MigrationCopy {
    parent: OwnedHandle,
    path: PathBuf,
    pending: String,
    export_pin: Option<OwnedHandle>,
    file: File,
    published: bool,
}

fn identity(file: HANDLE) -> io::Result<[u64; 2]> {
    let info = metadata(file)?;
    Ok([u64::from(info.dwVolumeSerialNumber),
        (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow)])
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
            export_pin: None,
            file: File::from(handle),
            published: false,
        };
        checked_file(result.file.as_raw_handle(), Some(0))?;
        user.verify_handle(result.file.as_raw_handle(), true)?;
        Ok(result)
    }

    pub fn source_identity(&self, leaf: &str) -> io::Result<[u64; 2]> {
        super::super::validate_migration_leaf(leaf)?;
        if self.export_pin.is_some() {
            if leaf != self.pending {
                return Err(denied("unexpected backup stage name"));
            }
            return identity(self.checked_name(leaf, 0)?.as_raw_handle());
        }
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
        identity(handle.as_raw_handle())
    }

    /// Keep the admitted stage name stable while SQLite opens it by path.
    pub fn create_for_export(parent: &Path, leaf: &str) -> io::Result<Self> {
        let mut stage = Self::create(parent, leaf)?;
        stage.export_pin = Some(stage.open_checked(leaf, GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE)?);
        Ok(stage)
    }

    pub fn open_verified_peer(
        &self, leaf: &str, owner_root: [u64; 2], expected: [u64; 2], bytes: u64,
    ) -> io::Result<File> {
        super::super::validate_migration_leaf(leaf)?;
        if self.export_pin.is_none() || leaf != self.pending
            || identity(self.parent.as_raw_handle())? != owner_root
        {
            return Err(denied("backup stage root or name changed"));
        }
        let handle = self.checked_name(leaf, GENERIC_READ)?;
        checked_file(handle.as_raw_handle(), Some(bytes))?;
        if identity(handle.as_raw_handle())? != expected {
            return Err(denied("backup stage identity changed"));
        }
        Ok(File::from(handle))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn checked_name(&self, leaf: &str, access: u32) -> io::Result<OwnedHandle> {
        self.open_checked(leaf, access, SHARE)
    }

    fn open_checked(&self, leaf: &str, access: u32, share: u32) -> io::Result<OwnedHandle> {
        let entry = relative_open(
            self.parent.as_raw_handle(),
            OsStr::new(leaf),
            access | META,
            share,
            None,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE,
        )?;
        checked_file(entry.as_raw_handle(), None)?;
        CurrentUser::query_managed()?.verify_handle(entry.as_raw_handle(), true)?;
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
        drop(self.export_pin.take());
        if !self.published {
            if let Ok(handle) = self.checked_name(&self.pending, DELETE) {
                let _ = mark_delete(handle.as_raw_handle());
            }
        }
    }
}

/// Check source and companion names relative to the admitted directory.
pub struct MigrationSource(OwnedHandle);

impl MigrationSource {
    pub fn open(path: &Path, pin_for_copy: bool) -> io::Result<Self> {
        let share = if pin_for_copy { FILE_SHARE_READ | FILE_SHARE_WRITE } else { SHARE };
        let directory = pinned_base_with_sharing(path, share, true)?;
        CurrentUser::query_managed()?.verify_managed_base(directory.as_raw_handle())?;
        Ok(Self(directory))
    }

    pub fn identity(&self) -> io::Result<[u64; 2]> {
        identity(self.0.as_raw_handle())
    }

    pub fn file_identity(&self, leaf: &str) -> io::Result<[u64; 2]> {
        let handle = self.open_source(leaf, META, SHARE)?;
        identity(handle.as_raw_handle())
    }

    /// Hold the database name stable until SQLite has opened its reader.
    pub fn pin_database(&self, leaf: &str) -> io::Result<File> {
        self.open_source(leaf, GENERIC_READ | META, FILE_SHARE_READ | FILE_SHARE_WRITE)
            .map(File::from)
    }

    fn open_source(&self, leaf: &str, access: u32, share: u32) -> io::Result<OwnedHandle> {
        super::super::validate_migration_leaf(leaf)?;
        let handle = relative_open(
            self.0.as_raw_handle(), OsStr::new(leaf), access, share,
            None, FILE_OPEN, FILE_NON_DIRECTORY_FILE,
        )?;
        checked_file(handle.as_raw_handle(), None)?;
        CurrentUser::query_managed()?.verify_managed_base(handle.as_raw_handle())?;
        Ok(handle)
    }
}
