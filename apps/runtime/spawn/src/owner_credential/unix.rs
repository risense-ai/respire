use super::{directory_name, OwnerRecord, OWNER_RECORD_LEN, SECRET_FILE};
use std::{
    ffi::{CStr, CString},
    fs::{File, Metadata},
    io::{self, Read, Write},
    mem::MaybeUninit,
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path},
};

struct Identity {
    dev: u64,
    ino: u64,
}

impl Identity {
    fn from_metadata(meta: &Metadata) -> Self {
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
        }
    }

    fn still_names(&self, dir: RawFd, name: &CStr) -> bool {
        let mut value = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: raw descriptor stays open in the guard; no symlink traversal.
        if unsafe {
            libc::fstatat(
                dir,
                name.as_ptr(),
                value.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return false;
        }
        let stat = unsafe { value.assume_init() };
        (stat.st_dev as u64, stat.st_ino as u64) == (self.dev, self.ino)
    }
}

/// Retains open handles to the exact directory and file it created. Drop
/// unlinks only those entries; it never sweeps a root or follows a link.
pub struct OwnerCredential {
    base: File,
    directory: File,
    name: CString,
    directory_id: Identity,
    secret: Option<(File, Identity)>,
}

impl Drop for OwnerCredential {
    fn drop(&mut self) {
        let secret = secret_name();
        // The target parent handles remain open, and identity mismatches are
        // skipped. Other processes of this same OS user are trusted.
        if self
            .secret
            .as_ref()
            .is_some_and(|(_, id)| id.still_names(self.directory.as_raw_fd(), secret))
        {
            unsafe { libc::unlinkat(self.directory.as_raw_fd(), secret.as_ptr(), 0) };
        }
        if self
            .directory_id
            .still_names(self.base.as_raw_fd(), &self.name)
        {
            unsafe {
                libc::unlinkat(
                    self.base.as_raw_fd(),
                    self.name.as_ptr(),
                    libc::AT_REMOVEDIR,
                )
            };
        }
    }
}

fn secret_name() -> &'static CStr {
    // The common and Windows implementation use exactly the same leaf name.
    debug_assert_eq!(SECRET_FILE.as_bytes(), c"secret".to_bytes());
    c"secret"
}

fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "owner credential storage not safely accessible",
    )
}

fn named_file(
    parent: RawFd,
    name: &CStr,
    flags: libc::c_int,
    mode: libc::mode_t,
) -> io::Result<File> {
    // SAFETY: file descriptor and C string are live; creation flags/mode are fixed.
    let fd = unsafe {
        libc::openat(
            parent,
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            mode as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn open_base(path: &Path) -> io::Result<File> {
    let mut components = path.components();
    if !matches!(components.next(), Some(Component::RootDir))
        || components.as_path().as_os_str().is_empty()
    {
        return Err(denied());
    }
    let mut directory = named_file(libc::AT_FDCWD, c"/", libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
    for component in components {
        let Component::Normal(part) = component else {
            return Err(denied());
        };
        let part = CString::new(part.as_bytes()).map_err(|_| denied())?;
        directory = named_file(
            directory.as_raw_fd(),
            &part,
            libc::O_RDONLY | libc::O_DIRECTORY,
            0,
        )?;
    }
    let meta = directory.metadata()?;
    if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o022 != 0 {
        return Err(denied());
    }
    require_local_ownership(&directory)?;
    reject_extended_acl(&directory)?;
    Ok(directory)
}

fn check(file: &File, mode: u32, directory: bool) -> io::Result<Metadata> {
    let meta = file.metadata()?;
    let kind_ok = if directory {
        meta.is_dir()
    } else {
        meta.is_file() && meta.nlink() == 1
    };
    if !kind_ok || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o7777 != mode {
        return Err(denied());
    }
    require_local_ownership(file)?;
    reject_extended_acl(file)?;
    Ok(meta)
}

// The credential file is an OS-owner authority boundary, not general Runtime
// storage. Do not assume a shared or user-space filesystem enforces ownership.
fn require_local_ownership(file: &File) -> io::Result<()> {
    let mut filesystem = MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: live file descriptor and exactly one writable statfs.
    if unsafe { libc::fstatfs(file.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
        return Err(denied());
    }
    let filesystem = unsafe { filesystem.assume_init() };
    #[cfg(target_os = "linux")]
    let accepted = owner_filesystem(filesystem.f_type as u32);
    #[cfg(target_os = "macos")]
    let accepted = {
        let raw = filesystem.f_fstypename.map(|byte| byte as u8);
        let name = raw.split(|byte| *byte == 0).next().unwrap_or_default();
        owner_filesystem(filesystem.f_flags, name)
    };
    if accepted {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "owner credential requires a verified local filesystem",
        ))
    }
}

#[cfg(target_os = "linux")]
fn owner_filesystem(magic: u32) -> bool {
    // Linux uapi/linux/magic.h: ext2/3/4 share one magic. Explicit allowlist:
    // ext, XFS, Btrfs and tmpfs enforce local kernel POSIX ownership/modes.
    // Unknown types, network, FUSE and overlay are intentionally denied.
    matches!(magic, 0xEF53 | 0x58465342 | 0x9123683E | 0x01021994)
}

#[cfg(target_os = "macos")]
fn owner_filesystem(flags: u32, name: &[u8]) -> bool {
    flags & libc::MNT_LOCAL as u32 != 0
        && flags & libc::MNT_IGNORE_OWNERSHIP as u32 == 0
        && matches!(name, b"apfs" | b"hfs")
}

#[cfg(target_os = "linux")]
fn reject_extended_acl(_: &File) -> io::Result<()> {
    // Linux POSIX ACL access to non-owners is bounded by the group mode mask.
    // Exact owner-only bits imply there is no effective non-owner ACL grant.
    Ok(())
}

#[cfg(target_os = "macos")]
fn reject_extended_acl(file: &File) -> io::Result<()> {
    use std::ffi::c_void;
    unsafe extern "C" {
        fn acl_get_fd(fd: libc::c_int) -> *mut c_void;
        fn acl_free(acl: *mut c_void) -> libc::c_int;
    }
    // Darwin extended ACLs are separate from POSIX permission bits. ENOENT
    // means FILESEC_ACL absent. Unknown retrieval errors fail closed too.
    let acl = unsafe { acl_get_fd(file.as_raw_fd()) };
    if acl.is_null() {
        if io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
            return Ok(());
        }
    } else {
        unsafe { acl_free(acl) };
    }
    Err(denied())
}

/// Store the caller's newly generated secret for this instance. Any existing
/// entry makes creation fail. This function never uses old file data.
pub fn create_owner_credential(
    base: &Path,
    instance_id: &str,
    record: &OwnerRecord,
) -> io::Result<OwnerCredential> {
    let name = directory_name(instance_id)?;
    let base = open_base(base)?;
    // SAFETY: retained verified parent, single validated leaf, no paths.
    if unsafe { libc::mkdirat(base.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let directory = named_file(
        base.as_raw_fd(),
        &name,
        libc::O_RDONLY | libc::O_DIRECTORY,
        0,
    )?;
    let directory_id = Identity::from_metadata(&check(&directory, 0o700, true)?);
    let mut guard = OwnerCredential {
        base,
        directory,
        name,
        directory_id,
        secret: None,
    };
    let file = new_file(guard.directory.as_raw_fd(), secret_name())?;
    let id = Identity::from_metadata(&file.metadata()?);
    // Retain the original inode even if writing or strict validation fails.
    let (file, id) = guard.secret.insert((file, id));
    check(file, 0o600, false)?;
    file.write_all(record.as_bytes())?;
    check(file, 0o600, false)?;
    if !id.still_names(guard.directory.as_raw_fd(), secret_name())
        || !guard
            .directory_id
            .still_names(guard.base.as_raw_fd(), &guard.name)
    {
        return Err(denied());
    }
    Ok(guard)
}

fn new_file(dir: RawFd, name: &CStr) -> io::Result<File> {
    named_file(
        dir,
        name,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        0o600,
    )
}

impl OwnerCredential {
    /// Publish a replacement with a fresh exclusive inode in this instance.
    /// On failure the current record is not adopted as authority by this call.
    pub fn publish(&mut self, record: &OwnerRecord) -> io::Result<()> {
        let base = self.base.as_raw_fd();
        let dir = self.directory.as_raw_fd();
        let leaf = secret_name();
        let staged_name = c".secret-next";
        if !self.directory_id.still_names(base, &self.name)
            || !self
                .secret
                .as_ref()
                .is_some_and(|(_, id)| id.still_names(dir, leaf))
        {
            return Err(denied());
        }
        check(&self.directory, 0o700, true)?;
        let mut file = new_file(dir, staged_name)?;
        let id = Identity::from_metadata(&file.metadata()?);
        let write_result = (|| {
            check(&file, 0o600, false)?;
            file.write_all(record.as_bytes())?;
            check(&file, 0o600, false)?;
            if !self.directory_id.still_names(base, &self.name)
                || !self
                    .secret
                    .as_ref()
                    .is_some_and(|(_, id)| id.still_names(dir, leaf))
                || !id.still_names(dir, staged_name)
            {
                return Err(denied());
            }
            // SAFETY: both names are fixed leaves of the retained instance
            // directory. Rename replaces the old record atomically.
            if unsafe { libc::renameat(dir, staged_name.as_ptr(), dir, leaf.as_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        })();
        if let Err(error) = write_result {
            if id.still_names(dir, staged_name) {
                unsafe { libc::unlinkat(dir, staged_name.as_ptr(), 0) };
            }
            return Err(error);
        }
        self.secret = Some((file, id));
        Ok(())
    }
}

/// Read exactly 48 raw bytes under the same owner and mode checks as creation.
pub fn read_owner_credential(base: &Path, instance_id: &str) -> io::Result<OwnerRecord> {
    let name = directory_name(instance_id)?;
    let base = open_base(base)?;
    let directory = named_file(
        base.as_raw_fd(),
        &name,
        libc::O_RDONLY | libc::O_DIRECTORY,
        0,
    )?;
    check(&directory, 0o700, true)?;
    let mut file = named_file(directory.as_raw_fd(), secret_name(), libc::O_RDONLY, 0)?;
    let mut value = [0; OWNER_RECORD_LEN];
    if check(&file, 0o600, false)?.len() != OWNER_RECORD_LEN as u64 {
        return Err(denied());
    }
    file.read_exact(&mut value)?;
    if file.read(&mut [0])? != 0 || check(&file, 0o600, false)?.len() != OWNER_RECORD_LEN as u64 {
        return Err(denied());
    }
    Ok(OwnerRecord::from_bytes(value))
}

#[cfg(test)]
#[path = "unix_tests.rs"]
mod tests;

#[path = "unix_model_stage.rs"]
mod model_stage;
pub use model_stage::{same_model_file, ModelStage};
#[path = "unix_model_removal.rs"]
mod model_removal;
pub use model_removal::ModelRemovalRoot;
