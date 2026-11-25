use super::*;

const CATALOG: &str = ".respire-local-spaces-v1";
const INSTALLATION: &str = "installation.id";
const FINAL: &str = "owner.receipt";
const STAGING: &str = ".pending";
const MAX_RECEIPT: usize = 8192;
const SHARE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE;

pub struct LocalSpaceNamespace {
    _base: OwnedHandle,
    catalog: OwnedHandle,
    path: PathBuf,
    user: CurrentUser,
}

pub struct FreshLocalSpace {
    catalog: OwnedHandle,
    _staging: OwnedHandle,
    directory: OwnedHandle,
    path: PathBuf,
    name: String,
    user: CurrentUser,
    published: bool,
}

impl LocalSpaceNamespace {

    #[cfg(feature = "test-private-space-root")]
    #[doc(hidden)]
    pub fn make_fixture_installation(temporary_parent: &Path) -> io::Result<PathBuf> {
        let parent = pinned_base_with_sharing(temporary_parent, SHARE, true)?;
        let user = CurrentUser::query_managed()?;
        let name = "owner-only-installation";
        let _root = managed_child(parent.as_raw_handle(), name, &user, true)?;
        Ok(temporary_parent.join(name))
    }
    pub fn open(profile: &Path) -> io::Result<Self> {
        let base = pinned_base_with_sharing(profile, SHARE, true)?;
        let user = CurrentUser::query_managed()?;
        user.verify_managed_base(base.as_raw_handle())?;
        let catalog = managed_child(base.as_raw_handle(), CATALOG, &user, false)?;
        Ok(Self { _base: base, catalog, path: profile.join(CATALOG), user })
    }

    pub fn open_existing(profile: &Path) -> io::Result<Option<Self>> {
        let base = pinned_base_with_sharing(profile, SHARE, true)?;
        let user = CurrentUser::query_managed()?;
        user.verify_managed_base(base.as_raw_handle())?;
        match open_managed_child(base.as_raw_handle(), CATALOG, &user) {
            Ok(catalog) => Ok(Some(Self { _base: base, catalog, path: profile.join(CATALOG), user })),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn existing_installation(&self) -> io::Result<Option<[u8; 16]>> {
        self.user.verify_new_managed_directory(self.catalog.as_raw_handle())?;
        read_record(self.catalog.as_raw_handle(), INSTALLATION, &self.user, 16)?
            .map(|bytes| {
                let id: [u8; 16] = bytes.try_into()
                    .map_err(|_| denied("invalid installation key length"))?;
                if id == [0; 16] {
                    return Err(denied("empty installation key"));
                }
                Ok(id)
            })
            .transpose()
    }

    pub fn candidates(&self) -> io::Result<Vec<String>> {
        self.user.verify_new_managed_directory(self.catalog.as_raw_handle())?;
        super::super::candidate_local_ids(&self.path)
    }

    pub fn installation(&self, nonce: &str, candidate: [u8; 16]) -> io::Result<[u8; 16]> {
        let nonce = checked_id(nonce)?;
        if candidate == [0; 16] {
            return Err(denied("empty installation key"));
        }
        if let Some(id) = self.existing_installation()? {
            return Ok(id);
        }
        let staged = format!(".installation-pending-{nonce}");
        match publish(self.catalog.as_raw_handle(), &staged, INSTALLATION, &self.user, &candidate) {
            Ok(()) => Ok(candidate),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                self.existing_installation()?.ok_or_else(|| denied("installation publication did not complete"))
            }
            Err(error) => Err(error),
        }
    }

    pub fn fresh(&self, id: &str) -> io::Result<FreshLocalSpace> {
        let name = format!("local-{}", checked_id(id)?);
        let user = CurrentUser::query_managed()?;
        match open_managed_child(self.catalog.as_raw_handle(), &name, &user) {
            Ok(_) => return Err(io::Error::new(io::ErrorKind::AlreadyExists, "device space already exists")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let staging = managed_child(self.catalog.as_raw_handle(), STAGING, &user, false)?;
        let (slot, directory) = reserve_staged_child(staging.as_raw_handle(), &user)?;
        Ok(FreshLocalSpace {
            catalog: self.catalog.try_clone()?,
            _staging: staging,
            directory,
            path: self.path.join(STAGING).join(slot),
            name,
            user,
            published: false,
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
        let name = format!("local-{}", checked_id(id)?);
        let directory = match open_managed_child(self.catalog.as_raw_handle(), &name, &self.user) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let record = read_record(directory.as_raw_handle(), FINAL, &self.user, MAX_RECEIPT)?;
        if let (Some(receipt), Some(authorize)) = (record.as_deref(), authorize) {
            if !authorize(receipt) {
                return Ok(None);
            }
            verify_library_files(directory.as_raw_handle(), &self.user)?;
        }
        Ok(record.map(|bytes| use_path(&self.path.join(name), bytes)))
    }
}

fn verify_library_files(directory: HANDLE, user: &CurrentUser) -> io::Result<()> {
    for (leaf, required) in [
        ("onememory.db", true), ("lock.db", true), ("lock.db-journal", false),
        ("onememory.db-journal", false), ("onememory.db-wal", false), ("onememory.db-shm", false),
    ] {
        let file = match relative_open(
            directory, OsStr::new(leaf), META, SHARE | FILE_SHARE_DELETE,
            None, FILE_OPEN, FILE_NON_DIRECTORY_FILE,
        ) {
            Ok(file) => file,
            Err(error) if !required && error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let handle = file.as_raw_handle();
        checked_file(handle, None)?;
        if user.verify_handle(handle, true).is_err() {
            user.verify_managed_base(handle)?;
            user.verify_inherited_model_ace(handle)?;
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
            return Err(denied("private space receipt is outside limits"));
        }
        let directory = self.directory.as_raw_handle();
        self.user.verify_new_managed_directory(directory)?;
        publish(directory, ".owner-receipt.pending", FINAL, &self.user, bytes)?;
        self.user.verify_new_managed_directory(self.catalog.as_raw_handle())?;
        admit()?;
        rename_relative(directory, self.catalog.as_raw_handle(), OsStr::new(&self.name), 0)?;
        self.published = true;
        Ok(())
    }
}

impl Drop for FreshLocalSpace {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        let stage = self.directory.as_raw_handle();
        if self.user.verify_new_managed_directory(stage).is_err() {
            return;
        }
        for leaf in super::super::LOCAL_SPACE_STAGE_FILES {
            let Ok(file) = relative_open(
                stage, OsStr::new(leaf), DELETE | META, FILE_SHARE_READ | FILE_SHARE_DELETE,
                None, FILE_OPEN, FILE_NON_DIRECTORY_FILE,
            ) else {
                continue;
            };
            let handle = file.as_raw_handle();
            if checked_file(handle, None).is_ok() && (self.user.verify_handle(handle, true).is_ok()
                || (self.user.verify_managed_base(handle).is_ok()
                    && self.user.verify_inherited_model_ace(handle).is_ok()))
            {
                let _ = mark_delete(handle);
            }
        }
        // Handles only; an unknown file or non-empty stage stays quarantined.
        let _ = mark_delete(stage);
    }
}

fn checked_id(id: &str) -> io::Result<&str> {
    if !super::super::is_local_space_id(id) {
        return Err(denied("invalid local space identifier"));
    }
    Ok(id)
}

fn open_managed_child(parent: HANDLE, leaf: &str, user: &CurrentUser) -> io::Result<OwnedHandle> {
    let directory = relative_open(
        parent, OsStr::new(leaf), META | FILE_TRAVERSE | FILE_ADD_FILE, SHARE,
        None, FILE_OPEN, FILE_DIRECTORY_FILE,
    )?;
    checked_directory(directory.as_raw_handle())?;
    user.verify_new_managed_directory(directory.as_raw_handle())?;
    Ok(directory)
}

fn managed_child(parent: HANDLE, leaf: &str, user: &CurrentUser, exclusive: bool) -> io::Result<OwnedHandle> {
    let descriptor = user.managed_directory_descriptor()?;
    let access = META | FILE_TRAVERSE | FILE_ADD_FILE | if exclusive { DELETE } else { 0 };
    let directory = match relative_open(
        parent, OsStr::new(leaf), access, SHARE,
        Some(&descriptor), FILE_CREATE, FILE_DIRECTORY_FILE,
    ) {
        Ok(directory) => directory,
        Err(error) if !exclusive && error.kind() == io::ErrorKind::AlreadyExists => {
            return open_managed_child(parent, leaf, user);
        }
        Err(error) => return Err(error),
    };
    checked_directory(directory.as_raw_handle())?;
    user.verify_new_managed_directory(directory.as_raw_handle())?;
    Ok(directory)
}

fn reserve_staged_child(parent: HANDLE, user: &CurrentUser) -> io::Result<(String, OwnedHandle)> {
    // Fixed CREATE_NEW slots enforce the bound even across process races.
    for slot in 0..super::super::LOCAL_SPACE_STAGING_SLOTS {
        let name = format!("slot-{slot:02}");
        match managed_child(parent, &name, user, true) {
            Ok(directory) => return Ok((name, directory)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other("local space staging capacity reached"))
}

fn read_record(parent: HANDLE, leaf: &str, user: &CurrentUser, max: usize) -> io::Result<Option<Vec<u8>>> {
    let file = match relative_open(
        parent, OsStr::new(leaf), GENERIC_READ | META, FILE_SHARE_READ,
        None, FILE_OPEN, FILE_NON_DIRECTORY_FILE,
    ) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    checked_file(file.as_raw_handle(), None)?;
    user.verify_handle(file.as_raw_handle(), true)?;
    let mut bytes = Vec::new();
    File::from(file).take(max as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > max || bytes.is_empty() {
        return Err(denied("private space record length is invalid"));
    }
    Ok(Some(bytes))
}

fn publish(parent: HANDLE, pending: &str, final_name: &str, user: &CurrentUser, bytes: &[u8]) -> io::Result<()> {
    let descriptor = user.descriptor()?;
    let file = relative_open(
        parent, OsStr::new(pending), GENERIC_WRITE | META | DELETE, FILE_SHARE_READ | FILE_SHARE_DELETE,
        Some(&descriptor), FILE_CREATE, FILE_NON_DIRECTORY_FILE,
    )?;
    let mut file = File::from(file);
    let result = (|| {
        checked_file(file.as_raw_handle(), Some(0))?;
        user.verify_handle(file.as_raw_handle(), true)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        checked_file(file.as_raw_handle(), Some(bytes.len() as u64))?;
        // Windows no-replace rename relative to the retained directory handle.
        rename_relative(file.as_raw_handle(), parent, OsStr::new(final_name), 0)
    })();
    if result.is_err() {
        let _ = mark_delete(file.as_raw_handle());
    }
    result
}
