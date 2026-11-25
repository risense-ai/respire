use super::*;
use std::path::PathBuf;

const CATALOG: &CStr = c".respire-local-spaces-v1";
const INSTALLATION: &CStr = c"installation.id";
const FINAL: &CStr = c"owner.receipt";
const STAGING: &CStr = c".pending";
const MAX_RECEIPT: usize = 8192;

pub struct LocalSpaceNamespace {
    base: File,
    catalog: File,
    identity: Identity,
    path: PathBuf,
}

pub struct FreshLocalSpace {
    catalog: File,
    parent: File,
    directory: File,
    identity: Identity,
    stage_name: CString,
    final_name: CString,
    path: PathBuf,
    published: bool,
}

impl LocalSpaceNamespace {
    pub fn open(profile: &Path) -> io::Result<Self> {
        let base = open_base(profile)?;
        let catalog = match mkdir(base.as_raw_fd(), CATALOG) {
            Ok(directory) => {
                base.sync_all()?;
                directory
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                open_directory(base.as_raw_fd(), CATALOG)?
            }
            Err(error) => return Err(error),
        };
        Self::from_parts(profile, base, catalog)
    }

    pub fn open_existing(profile: &Path) -> io::Result<Option<Self>> {
        let base = open_base(profile)?;
        match open_directory(base.as_raw_fd(), CATALOG) {
            Ok(catalog) => Self::from_parts(profile, base, catalog).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn from_parts(profile: &Path, base: File, catalog: File) -> io::Result<Self> {
        let identity = Identity::from_metadata(&check(&catalog, 0o700, true)?);
        let path = profile.join(std::ffi::OsStr::from_bytes(CATALOG.to_bytes()));
        Ok(Self { base, catalog, identity, path })
    }

    pub fn existing_installation(&self) -> io::Result<Option<[u8; 16]>> {
        self.verify()?;
        read_record(self.catalog.as_raw_fd(), INSTALLATION, 16)?
            .map(|bytes| {
                let id: [u8; 16] = bytes.try_into().map_err(|_| denied())?;
                if id == [0; 16] {
                    return Err(denied());
                }
                Ok(id)
            })
            .transpose()
    }

    pub fn installation(&self, nonce: &str, candidate: [u8; 16]) -> io::Result<[u8; 16]> {
        let nonce = checked_id(nonce)?;
        if candidate == [0; 16] {
            return Err(denied());
        }
        if let Some(id) = self.existing_installation()? {
            return Ok(id);
        }
        let stage = CString::new(format!(".installation-pending-{nonce}")).map_err(|_| denied())?;
        match publish(&self.catalog, &stage, INSTALLATION, &candidate) {
            Ok(()) => Ok(candidate),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                self.existing_installation()?.ok_or_else(denied)
            }
            Err(error) => Err(error),
        }
    }

    pub fn fresh(&self, id: &str) -> io::Result<FreshLocalSpace> {
        self.verify()?;
        let name = CString::new(format!("local-{}", checked_id(id)?)).map_err(|_| denied())?;
        match open_directory(self.catalog.as_raw_fd(), &name) {
            Ok(_) => return Err(io::Error::new(io::ErrorKind::AlreadyExists, "device space already exists")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let parent = match mkdir(self.catalog.as_raw_fd(), STAGING) {
            Ok(staging) => {
                self.catalog.sync_all()?;
                staging
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                open_directory(self.catalog.as_raw_fd(), STAGING)?
            }
            Err(error) => return Err(error),
        };
        let (stage_name, directory) = reserve_staged_child(parent.as_raw_fd())?;
        let path = self.path.join(".pending").join(std::ffi::OsStr::from_bytes(stage_name.to_bytes()));
        let identity = Identity::from_metadata(&check(&directory, 0o700, true)?);
        parent.sync_all()?;
        Ok(FreshLocalSpace {
            catalog: self.catalog.try_clone()?,
            parent, directory, identity, stage_name, final_name: name, path, published: false,
        })
    }

    pub fn receipt(&self, id: &str) -> io::Result<Option<Vec<u8>>> {
        self.with_receipt(id, None::<fn(&[u8]) -> bool>, |_, receipt| receipt)
    }

    /// Keep the verified native directory open for the synchronous operation.
    pub fn with_library_receipt<T>(
        &self, id: &str, authorize: impl FnOnce(&[u8]) -> bool, use_path: impl FnOnce(&Path) -> T,
    ) -> io::Result<Option<T>> {
        self.with_receipt(id, Some(authorize), |path, _| use_path(path))
    }

    fn with_receipt<T>(
        &self, id: &str, authorize: Option<impl FnOnce(&[u8]) -> bool>, use_path: impl FnOnce(&Path, Vec<u8>) -> T,
    ) -> io::Result<Option<T>> {
        self.verify()?;
        let name = CString::new(format!("local-{}", checked_id(id)?)).map_err(|_| denied())?;
        let directory = match open_directory(self.catalog.as_raw_fd(), &name) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let identity = Identity::from_metadata(&check(&directory, 0o700, true)?);
        let record = read_record(directory.as_raw_fd(), FINAL, MAX_RECEIPT)?;
        if !identity.still_names(self.catalog.as_raw_fd(), &name) {
            return Err(denied());
        }
        if let (Some(receipt), Some(authorize)) = (record.as_deref(), authorize) {
            if !authorize(receipt) {
                return Ok(None);
            }
            verify_library_files(directory.as_raw_fd())?;
        }
        let result = record.map(|bytes| {
            use_path(&self.path.join(std::ffi::OsStr::from_bytes(name.to_bytes())), bytes)
        });
        if !identity.still_names(self.catalog.as_raw_fd(), &name) {
            return Err(denied());
        }
        self.verify()?;
        Ok(result)
    }

    pub fn candidates(&self) -> io::Result<Vec<String>> {
        self.verify()?;
        let ids = super::super::candidate_local_ids(&self.path)?;
        self.verify()?;
        Ok(ids)
    }

    fn verify(&self) -> io::Result<()> {
        check(&self.catalog, 0o700, true)?;
        if !self.identity.still_names(self.base.as_raw_fd(), CATALOG) {
            return Err(denied());
        }
        Ok(())
    }
}

fn verify_library_files(directory: RawFd) -> io::Result<()> {
    for (leaf, required) in [
        (c"onememory.db", true), (c"lock.db", true), (c"lock.db-journal", false),
        (c"onememory.db-journal", false), (c"onememory.db-wal", false), (c"onememory.db-shm", false),
    ] {
        // O_NONBLOCK also keeps malformed non-regular entries from stalling.
        let file = match named_file(directory, leaf, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
            Ok(file) => file,
            Err(error) if !required && error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let stat = file.metadata()?;
        if !stat.is_file() || stat.nlink() != 1 || stat.uid() != unsafe { libc::geteuid() }
            || stat.mode() & 0o022 != 0
        {
            return Err(denied());
        }
        reject_extended_acl(&file)?;
        if !Identity::from_metadata(&stat).still_names(directory, leaf) {
            return Err(denied());
        }
    }
    Ok(())
}

impl FreshLocalSpace {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn publish(
        mut self, bytes: &[u8], admit: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        if bytes.is_empty() || bytes.len() > MAX_RECEIPT {
            return Err(denied());
        }
        check(&self.directory, 0o700, true)?;
        publish(&self.directory, c".owner-receipt.pending", FINAL, bytes)?;
        check(&self.catalog, 0o700, true)?;
        if !self.identity.still_names(self.parent.as_raw_fd(), &self.stage_name) {
            return Err(denied());
        }
        admit()?;
        super::model_stage::no_replace_between(
            self.parent.as_raw_fd(), &self.stage_name, self.catalog.as_raw_fd(), &self.final_name,
        )?;
        self.published = true;
        self.catalog.sync_all()?;
        self.parent.sync_all()
    }
}

impl Drop for FreshLocalSpace {
    fn drop(&mut self) {
        if self.published || check(&self.directory, 0o700, true).is_err()
            || !self.identity.still_names(self.parent.as_raw_fd(), &self.stage_name)
        {
            return;
        }
        let directory = self.directory.as_raw_fd();
        for leaf in super::super::LOCAL_SPACE_STAGE_FILES {
            let Ok(name) = CString::new(*leaf) else { continue };
            let Ok(file) = named_file(directory, &name, libc::O_RDONLY, 0) else { continue };
            let Ok(meta) = file.metadata() else { continue };
            if meta.is_file() && meta.nlink() == 1 && meta.mode() & 0o022 == 0
                && meta.uid() == unsafe { libc::geteuid() }
                && Identity::from_metadata(&meta).still_names(directory, &name)
            {
                // SAFETY: checked retained owner directory and exact known leaf/inode.
                unsafe { libc::unlinkat(directory, name.as_ptr(), 0) };
            }
        }
        if self.identity.still_names(self.parent.as_raw_fd(), &self.stage_name) {
            // Unknown children keep this directory quarantined. Never recurse.
            unsafe { libc::unlinkat(self.parent.as_raw_fd(), self.stage_name.as_ptr(), libc::AT_REMOVEDIR) };
            let _ = self.parent.sync_all();
        }
    }
}

fn checked_id(id: &str) -> io::Result<&str> {
    if !super::super::is_local_space_id(id) {
        return Err(denied());
    }
    Ok(id)
}

fn mkdir(parent: RawFd, name: &CStr) -> io::Result<File> {
    // SAFETY: retained parent descriptor, one validated basename and fixed mode.
    if unsafe { libc::mkdirat(parent, name.as_ptr(), 0o700) } != 0 {
        return Err(io::Error::last_os_error());
    }
    open_directory(parent, name)
}

fn reserve_staged_child(parent: RawFd) -> io::Result<(CString, File)> {
    // Fixed mkdirat slots cannot exceed the cap even with concurrent writers.
    for slot in 0..super::super::LOCAL_SPACE_STAGING_SLOTS {
        let name = CString::new(format!("slot-{slot:02}")).map_err(|_| denied())?;
        match mkdir(parent, &name) {
            Ok(directory) => return Ok((name, directory)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other("local space staging capacity reached"))
}

fn open_directory(parent: RawFd, name: &CStr) -> io::Result<File> {
    let directory = named_file(parent, name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
    check(&directory, 0o700, true)?;
    Ok(directory)
}

fn read_record(parent: RawFd, name: &CStr, max: usize) -> io::Result<Option<Vec<u8>>> {
    let file = match named_file(parent, name, libc::O_RDONLY, 0) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let identity = Identity::from_metadata(&check(&file, 0o600, false)?);
    let mut result = Vec::new();
    (&file).take(max as u64 + 1).read_to_end(&mut result)?;
    if result.is_empty() || result.len() > max
        || check(&file, 0o600, false)?.len() != result.len() as u64
        || !identity.still_names(parent, name)
    {
        return Err(denied());
    }
    Ok(Some(result))
}

fn publish(parent: &File, stage: &CStr, final_name: &CStr, bytes: &[u8]) -> io::Result<()> {
    let dir = parent.as_raw_fd();
    let mut file = new_file(dir, stage)?;
    let identity = Identity::from_metadata(&file.metadata()?);
    let outcome = (|| {
        check(&file, 0o600, false)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        if check(&file, 0o600, false)?.len() != bytes.len() as u64 || !identity.still_names(dir, stage) {
            return Err(denied());
        }
        // Same no-clobber primitive already used by the native model publisher.
        super::model_stage::no_replace(dir, stage, final_name)?;
        parent.sync_all()
    })();
    if outcome.is_err() && identity.still_names(dir, stage) {
        // SAFETY: helper removes only its own retained, exclusive pending inode.
        unsafe { libc::unlinkat(dir, stage.as_ptr(), 0) };
    }
    outcome
}
