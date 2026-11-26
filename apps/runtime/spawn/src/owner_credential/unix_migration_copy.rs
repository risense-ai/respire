use super::*;
use std::path::PathBuf;

pub struct MigrationCopy {
    parent: File,
    path: PathBuf,
    pending: CString,
    file: File,
    identity: Identity,
    published: bool,
}

impl MigrationCopy {
    pub fn create(parent: &Path, pending: &str) -> io::Result<Self> {
        super::super::validate_migration_leaf(pending)?;
        let directory = open_base(parent)?;
        let leaf = CString::new(pending).map_err(|_| denied())?;
        let file = new_file(directory.as_raw_fd(), &leaf)?;
        let identity = Identity::from_metadata(&file.metadata()?);
        let result = Self {
            parent: directory,
            path: parent.join(pending),
            pending: leaf,
            file,
            identity,
            published: false,
        };
        check(&result.file, 0o600, false)?;
        Ok(result)
    }

    pub fn source_identity(&self, leaf: &str) -> io::Result<[u64; 2]> {
        source_at(&self.parent, leaf)
    }

    /// Open exactly the retained owner-private stage inode relative to the
    /// pinned directory, not a second client- or path-selected name.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn open_verified_peer(
        &self,
        leaf: &str,
        owner_root: [u64; 2],
        identity: [u64; 2],
        bytes: u64,
    ) -> io::Result<File> {
        super::super::validate_migration_leaf(leaf)?;
        let parent = self.parent.metadata()?;
        if [parent.dev(), parent.ino()] != owner_root {
            return Err(denied());
        }
        let leaf = CString::new(leaf).map_err(|_| denied())?;
        let file = named_file(self.parent.as_raw_fd(), &leaf, libc::O_RDONLY, 0)?;
        let metadata = check(&file, 0o600, false)?;
        if metadata.len() != bytes || [metadata.dev(), metadata.ino()] != identity
            || !Identity::from_metadata(&metadata).still_names(self.parent.as_raw_fd(), &leaf)
        {
            return Err(denied());
        }
        Ok(file)
    }

    #[cfg(target_os = "linux")]
    pub(super) fn archive_binding(&self) -> io::Result<([u64; 2], [u64; 2])> {
        let parent = self.parent.metadata()?;
        let file = check(&self.file, 0o600, false)?;
        if !self.identity.still_names(self.parent.as_raw_fd(), &self.pending) {
            return Err(denied());
        }
        Ok(([parent.dev(), parent.ino()], [file.dev(), file.ino()]))
    }

    #[cfg(target_os = "linux")]
    pub(super) fn fill_archive(&mut self, fill: impl FnOnce(&mut File) -> io::Result<()>) -> io::Result<()> {
        self.archive_binding()?;
        if self.file.metadata()?.len() != 0 {
            return Err(denied());
        }
        fill(&mut self.file)?;
        self.file.sync_all()?;
        self.archive_binding()?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn publish(mut self, final_name: &str) -> io::Result<()> {
        super::super::validate_migration_leaf(final_name)?;
        let final_name = CString::new(final_name).map_err(|_| denied())?;
        let parent = self.parent.as_raw_fd();
        if final_name == self.pending
            || check(&self.file, 0o600, false)?.len() == 0
            || !self.identity.still_names(parent, &self.pending)
        {
            return Err(denied());
        }
        self.file.sync_all()?;
        super::model_stage::no_replace(parent, &self.pending, &final_name)?;
        self.published = true;
        self.parent.sync_all()
    }
}

impl Drop for MigrationCopy {
    fn drop(&mut self) {
        let parent = self.parent.as_raw_fd();
        if !self.published && self.identity.still_names(parent, &self.pending) {
            // SAFETY: retained directory and exact exclusive inode identity checked above.
            unsafe { libc::unlinkat(parent, self.pending.as_ptr(), 0) };
        }
    }
}

/// Read-only directory/source identity witness. Unlike opening a database
/// file here, metadata-relative checks do not disturb SQLite's POSIX locks.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub struct MigrationSource(File);

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl MigrationSource {
    pub fn open(path: &Path, _pin_for_copy: bool) -> io::Result<Self> {
        open_base(path).map(Self)
    }

    pub fn identity(&self) -> io::Result<[u64; 2]> {
        let metadata = self.0.metadata()?;
        Ok([metadata.dev(), metadata.ino()])
    }

    pub fn file_identity(&self, leaf: &str) -> io::Result<[u64; 2]> {
        source_at(&self.0, leaf)
    }
}

fn source_at(parent: &File, leaf: &str) -> io::Result<[u64; 2]> {
    super::super::validate_migration_leaf(leaf)?;
    let leaf = CString::new(leaf).map_err(|_| denied())?;
    let mut value = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: the live directory descriptor and C leaf are retained. Do
    // not open/close a SQLite inode: classic POSIX locks are process-wide.
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            leaf.as_ptr(),
            value.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let value = unsafe { value.assume_init() };
    if value.st_mode & libc::S_IFMT != libc::S_IFREG
        || value.st_nlink != 1
        || value.st_uid != unsafe { libc::geteuid() }
    {
        return Err(denied());
    }
    Ok([value.st_dev as u64, value.st_ino as u64])
}
