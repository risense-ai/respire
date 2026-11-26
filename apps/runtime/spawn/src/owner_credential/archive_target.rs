use super::MigrationSource;
use std::{
    fs::{File, OpenOptions},
    io,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

/// O_PATH retains the SQLite inode without opening a database read descriptor.
pub struct ArchiveTarget {
    path: PathBuf,
    directory: MigrationSource,
    file: File,
}

impl ArchiveTarget {
    pub fn pin(path: PathBuf) -> io::Result<Self> {
        let parent = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
        let directory = MigrationSource::open(parent, true)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_NOFOLLOW)
            .open(&path)?;
        let result = Self {
            path,
            directory,
            file,
        };
        result.verify()?;
        Ok(result)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(feature = "rusqlite")]
    pub fn verify_connection(&self, connection: &rusqlite::Connection) -> io::Result<()> {
        let mut moved: std::ffi::c_int = 1;
        if connection.path().map(Path::new) != Some(self.path()) {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        // SAFETY: the caller borrows the live connection; SQLite writes one int.
        let rc = unsafe {
            rusqlite::ffi::sqlite3_file_control(
                connection.handle(),
                c"main".as_ptr(),
                rusqlite::ffi::SQLITE_FCNTL_HAS_MOVED,
                (&mut moved as *mut std::ffi::c_int).cast(),
            )
        };
        if rc != rusqlite::ffi::SQLITE_OK || moved != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        self.verify()
    }

    pub fn verify(&self) -> io::Result<()> {
        let parent = self.path.parent().ok_or(io::ErrorKind::InvalidInput)?;
        let leaf = self
            .path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or(io::ErrorKind::InvalidInput)?;
        let meta = self.file.metadata()?;
        if self.path.canonicalize()? != self.path
            || MigrationSource::open(parent, true)?.identity()? != self.directory.identity()?
            || self.directory.file_identity(leaf)? != [meta.dev(), meta.ino()]
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(())
    }
}
