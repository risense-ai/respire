//! Staging for verified model pairs under the same Windows owner boundary as
//! the Runtime credential. All mutations use retained handles and leaf names.
use super::*;

const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

pub struct ModelStage {
    base: OwnedHandle,
    directory: OwnedHandle,
    files: Vec<StagedFile>,
    committed: bool,
}

struct StagedFile {
    name: &'static str,
    identity: (u32, u32, u32),
    writer: Option<File>,
}

pub(super) fn identity(handle: HANDLE) -> io::Result<(u32, u32, u32)> {
    let info = metadata(handle)?;
    Ok((
        info.dwVolumeSerialNumber,
        info.nFileIndexHigh,
        info.nFileIndexLow,
    ))
}

pub fn same_model_file(left: &File, right: &File) -> io::Result<bool> {
    Ok(identity(left.as_raw_handle())? == identity(right.as_raw_handle())?)
}

impl ModelStage {
    pub fn create(base: &Path, name: &str) -> io::Result<Self> {
        let base = pinned_base_with_sharing(base, FILE_SHARE_READ | FILE_SHARE_WRITE, true)?;
        let user = CurrentUser::query_managed()?;
        user.verify_managed_base(base.as_raw_handle())?;
        super::model_removal::require_no_recovery(base.as_raw_handle())?;
        let descriptor = user.descriptor()?;
        let directory = relative_open(
            base.as_raw_handle(),
            OsStr::new(name),
            META | DELETE | FILE_ADD_FILE | FILE_TRAVERSE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            Some(&descriptor),
            FILE_CREATE,
            FILE_DIRECTORY_FILE,
        )?;
        let stage = Self {
            base,
            directory,
            files: Vec::new(),
            committed: false,
        };
        checked_directory(stage.directory.as_raw_handle())?;
        user.verify_handle(stage.directory.as_raw_handle(), true)?;
        Ok(stage)
    }

    pub fn create_file(&mut self, name: &'static str) -> io::Result<File> {
        let user = CurrentUser::query()?;
        let descriptor = user.descriptor()?;
        let file = relative_open(
            self.directory.as_raw_handle(),
            OsStr::new(name),
            GENERIC_WRITE | META | DELETE,
            FILE_SHARE_READ | FILE_SHARE_DELETE,
            Some(&descriptor),
            FILE_CREATE,
            FILE_NON_DIRECTORY_FILE,
        )?;
        let result = (|| {
            checked_file(file.as_raw_handle(), Some(0))?;
            user.verify_handle(file.as_raw_handle(), true)?;
            identity(file.as_raw_handle())
        })();
        let identity = match result {
            Ok(value) => value,
            Err(error) => {
                let _ = mark_delete(file.as_raw_handle());
                return Err(error);
            }
        };
        let writer = File::from(file);
        let result = writer.try_clone();
        self.files.push(StagedFile {
            name,
            identity,
            writer: Some(writer),
        });
        result
    }

    pub fn seal(&self) -> io::Result<()> {
        for file in &self.files {
            file.writer
                .as_ref()
                .ok_or_else(|| denied("staged writer closed"))?
                .sync_all()?;
        }
        Ok(())
    }

    pub fn publish(
        &mut self,
        base: &Path,
        destination: &str,
        verify: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        let user = CurrentUser::query_managed()?;
        let current_base = pinned_base_with_sharing(base, SHARE_ALL, true)?;
        let base = self.base.as_raw_handle();
        if identity(base)? != identity(current_base.as_raw_handle())? {
            return Err(denied("managed model namespace identity changed"));
        }
        user.verify_managed_base(base)?;
        super::model_removal::require_no_recovery(base)?;
        let directory = self.directory.as_raw_handle();
        checked_directory(directory)?;
        user.verify_handle(directory, true)?;
        for file in &self.files {
            let handle = self.named_file(file.name, META)?;
            checked_file(handle.as_raw_handle(), None)?;
            if identity(handle.as_raw_handle())? != file.identity {
                return Err(denied("managed model staged file identity changed"));
            }
            user.verify_handle(handle.as_raw_handle(), true)?;
        }
        // NTFS refuses to rename directories with open child handles. The
        // exact retained stage directory is owner-only and cannot be moved
        // while its handle is open. Close children before the native rename.
        for file in &mut self.files {
            drop(file.writer.take());
        }
        let destination = OsStr::new(destination);
        let present = match relative_open(
            base,
            destination,
            DELETE | META | FILE_TRAVERSE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            FILE_OPEN,
            FILE_DIRECTORY_FILE,
        ) {
            Ok(previous) => {
                checked_directory(previous.as_raw_handle())?;
                user.verify_handle(previous.as_raw_handle(), false)?;
                Some(previous)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        verify()?;
        if let Some(previous) = present {
            // NTFS permits this only when the exact opened directory is empty.
            // Never replace an existing pair or recursively remove old files.
            mark_delete(previous.as_raw_handle())?;
        }
        rename_relative(directory, base, destination, 0)?;
        self.committed = true;
        Ok(())
    }

    fn named_file(&self, name: &str, access: u32) -> io::Result<OwnedHandle> {
        relative_open(
            self.directory.as_raw_handle(),
            OsStr::new(name),
            access,
            SHARE_ALL,
            None,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE,
        )
    }
}

impl Drop for ModelStage {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        for file in &self.files {
            if let Some(writer) = &file.writer {
                let _ = mark_delete(writer.as_raw_handle());
            } else if let Ok(found) = self.named_file(file.name, META | DELETE) {
                if identity(found.as_raw_handle()).ok() == Some(file.identity) {
                    let _ = mark_delete(found.as_raw_handle());
                }
            }
        }
        self.files.clear();
        let _ = mark_delete(self.directory.as_raw_handle());
    }
}
