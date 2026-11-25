use super::*;
use windows_sys::Win32::Storage::FileSystem::FILE_ADD_SUBDIRECTORY;

const SHARE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE;

pub struct ModelJobRoot {
    _base: OwnedHandle,
    slot: OwnedHandle,
    directory: OwnedHandle,
    index: OwnedHandle,
    spools: [Option<File>; 2],
    path: PathBuf,
}

impl ModelJobRoot {
    /// Caller serializes job admission under the original serving owner lock.
    pub fn create(base: &Path) -> io::Result<Self> {
        Self::create_with(base, || Ok(()))
    }

    pub(in crate::owner_credential) fn create_with(
        base: &Path,
        after_claim: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<Self> {
        let parent = pinned_base_with_sharing(base, SHARE, true)?;
        let user = CurrentUser::query_managed()?;
        user.verify_managed_base(parent.as_raw_handle())?;
        use crate::owner_credential::model_job::{job_name, limit, SLOTS};
        let name = job_name()?;
        let mut claim = None;
        for name in SLOTS {
            match create_dir(parent.as_raw_handle(), name, &user) {
                Ok(slot) => {
                    claim = Some((name, slot));
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        let (slot_name, mut slot) = claim.ok_or_else(limit)?;
        after_claim()?;
        let mut directory = create_dir(slot.raw()?, &name, &user)?;
        let mut index = create_dir(directory.raw()?, "index", &user)?;
        let index = index.finish()?;
        let directory = directory.finish()?;
        let slot = slot.finish()?;
        Ok(Self {
            _base: parent,
            slot,
            directory,
            index,
            spools: [None, None],
            path: base.join(slot_name).join(name),
        })
    }

    pub fn root(&self) -> io::Result<&Path> {
        let base = self
            .path
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| denied("model job has no base"))?;
        let current = pinned_base_with_sharing(base, SHARE, true)?;
        let user = CurrentUser::query_managed()?;
        user.verify_managed_base(current.as_raw_handle())?;
        if model_stage::identity(current.as_raw_handle())?
            != model_stage::identity(self._base.as_raw_handle())?
        {
            return Err(denied("model job base identity changed"));
        }
        for directory in [&self.slot, &self.directory, &self.index] {
            checked_directory(directory.as_raw_handle())?;
            user.verify_new_managed_directory(directory.as_raw_handle())?;
        }
        Ok(&self.path)
    }

    pub fn index_root(&self) -> io::Result<PathBuf> {
        Ok(self.root()?.join("index"))
    }

    /// Allocate the single Runtime-owned encrypted spool. The Core child
    /// never receives this path; the caller retains the exact opened file.
    pub fn create_source_spool(&mut self) -> io::Result<File> {
        self.create_spool(0)
    }

    /// Only verified prepared opaque artifacts belong in this file.
    pub fn create_prepared_spool(&mut self) -> io::Result<File> {
        self.create_spool(1)
    }

    fn create_spool(&mut self, index: usize) -> io::Result<File> {
        self.root()?;
        if self.spools[index].is_some() {
            return Err(denied("model job source spool already exists"));
        }
        let user = CurrentUser::query()?;
        let descriptor = user.descriptor()?;
        let source = relative_open(
            self.directory.as_raw_handle(),
            OsStr::new(["source.rows", "prepared.rows"][index]),
            GENERIC_READ | GENERIC_WRITE | META | DELETE,
            SHARE,
            Some(&descriptor),
            FILE_CREATE,
            FILE_NON_DIRECTORY_FILE,
        )?;
        checked_file(source.as_raw_handle(), Some(0))?;
        user.verify_handle(source.as_raw_handle(), true)?;
        let source = File::from(source);
        let access = source.try_clone();
        self.spools[index] = Some(source);
        access
    }

    /// Call only after successful native child reaping. Never sweeps unknown
    /// files, other jobs or links. Nonempty jobs stay within the retention cap.
    pub fn remove_empty(self) -> io::Result<()> {
        self.root()?;
        for file in self.spools.into_iter().flatten() {
            let user = CurrentUser::query()?;
            checked_file(file.as_raw_handle(), None)?;
            user.verify_handle(file.as_raw_handle(), true)?;
            mark_delete(file.as_raw_handle())?;
        }
        mark_delete(self.index.as_raw_handle())?;
        drop(self.index);
        mark_delete(self.directory.as_raw_handle())?;
        drop(self.directory);
        mark_delete(self.slot.as_raw_handle())
    }
}

struct CreatedDirectory(Option<OwnedHandle>);
impl CreatedDirectory {
    fn raw(&self) -> io::Result<HANDLE> {
        self.0
            .as_ref()
            .map(AsRawHandle::as_raw_handle)
            .ok_or_else(|| denied("model job lost its staged directory"))
    }
    fn finish(&mut self) -> io::Result<OwnedHandle> {
        self.0
            .take()
            .ok_or_else(|| denied("model job lost its staged directory"))
    }
}
impl Drop for CreatedDirectory {
    fn drop(&mut self) {
        if let Some(handle) = &self.0 {
            // The handle belongs to this setup's exclusive FILE_CREATE.
            let _ = mark_delete(handle.as_raw_handle());
        }
    }
}

fn create_dir(parent: HANDLE, leaf: &str, user: &CurrentUser) -> io::Result<CreatedDirectory> {
    let descriptor = user.managed_directory_descriptor()?;
    let handle = relative_open(
        parent,
        OsStr::new(leaf),
        META | DELETE | FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY | FILE_TRAVERSE,
        SHARE,
        Some(&descriptor),
        FILE_CREATE,
        FILE_DIRECTORY_FILE,
    )?;
    let handle = CreatedDirectory(Some(handle));
    checked_directory(handle.raw()?)?;
    user.verify_new_managed_directory(handle.raw()?)?;
    Ok(handle)
}
