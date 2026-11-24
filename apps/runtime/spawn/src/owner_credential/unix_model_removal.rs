use super::super::RemovalMetadata;
use super::*;

/// Runtime-owned model directory under an exact retained namespace.
pub struct ModelRemovalRoot {
    base: File,
    base_path: std::path::PathBuf,
    base_id: Identity,
    model: File,
    model_id: Identity,
    model_name: CString,
}

const QUARANTINE: &CStr = c".runtime-model-removal-v1";

pub(super) fn require_no_recovery(base: RawFd) -> io::Result<()> {
    let mut info = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: retained parent descriptor and a fixed single leaf.
    if unsafe {
        libc::fstatat(
            base,
            QUARANTINE.as_ptr(),
            info.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } < 0
    {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::NotFound {
            return Ok(());
        }
    }
    Err(denied())
}

impl RemovalMetadata {
    fn name(self) -> &'static CStr {
        match self {
            Self::Journal => c"journal.json",
            Self::CommitTemp => c"commit.tmp",
            Self::Committed => c"committed",
        }
    }
}

/// No automatic cleanup: a retained quarantine is a recovery barrier.
pub struct ModelQuarantine<'a> {
    root: &'a ModelRemovalRoot,
    dir: File,
    id: Identity,
    metadata: Vec<(RemovalMetadata, File, Identity)>,
}

fn model_name(value: &str) -> io::Result<CString> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        || value.starts_with('.')
        || value.ends_with('.')
    {
        return Err(denied());
    }
    CString::new(value).map_err(|_| denied())
}

fn asset_name(value: &str) -> io::Result<&'static CStr> {
    match value {
        "model_quantized.onnx" => Ok(c"model_quantized.onnx"),
        "model.safetensors" => Ok(c"model.safetensors"),
        "tokenizer.json" => Ok(c"tokenizer.json"),
        _ => Err(denied()),
    }
}

fn detached(index: usize) -> io::Result<&'static CStr> {
    match index {
        0 => Ok(c"0"),
        1 => Ok(c"1"),
        _ => Err(denied()),
    }
}

impl ModelRemovalRoot {
    pub fn open(base: &Path, model: &str) -> io::Result<Self> {
        let name = model_name(model)?;
        let held_base = open_base(base)?;
        let base_id = Identity::from_metadata(&held_base.metadata()?);
        let holder = named_file(
            held_base.as_raw_fd(),
            &name,
            libc::O_RDONLY | libc::O_DIRECTORY,
            0,
        )?;
        let model_id = Identity::from_metadata(&check(&holder, 0o700, true)?);
        let root = Self {
            base: held_base,
            base_path: base.to_owned(),
            base_id,
            model: holder,
            model_id,
            model_name: name,
        };
        root.reauthorize(base)?;
        Ok(root)
    }

    pub fn reauthorize(&self, base: &Path) -> io::Result<()> {
        let reopened = open_base(base)?;
        let current = Identity::from_metadata(&reopened.metadata()?);
        if (current.dev, current.ino) != (self.base_id.dev, self.base_id.ino)
            || !self
                .model_id
                .still_names(self.base.as_raw_fd(), &self.model_name)
        {
            return Err(denied());
        }
        check(&self.model, 0o700, true)?;
        Ok(())
    }

    pub fn open_model_asset(&self, leaf: &str) -> io::Result<File> {
        self.reauthorize(&self.base_path)?;
        let name = asset_name(leaf)?;
        let parent = self.model.as_raw_fd();
        let file = named_file(parent, name, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
        held_asset(&file, parent, name)?;
        Ok(file)
    }

    pub fn create_quarantine(&self) -> io::Result<ModelQuarantine<'_>> {
        self.reauthorize(&self.base_path)?;
        // SAFETY: owned base descriptor and fixed entry; existing dirs fail.
        if unsafe { libc::mkdirat(self.base.as_raw_fd(), QUARANTINE.as_ptr(), 0o700) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let dir = named_file(
            self.base.as_raw_fd(),
            QUARANTINE,
            libc::O_RDONLY | libc::O_DIRECTORY,
            0,
        )?;
        let id = Identity::from_metadata(&check(&dir, 0o700, true)?);
        let quarantine = ModelQuarantine {
            root: self,
            dir,
            id,
            metadata: Vec::new(),
        };
        quarantine.reauthorize()?;
        Ok(quarantine)
    }
}

fn held_asset(file: &File, parent: RawFd, name: &CStr) -> io::Result<Identity> {
    let id = Identity::from_metadata(&check(file, 0o600, false)?);
    if !id.still_names(parent, name) {
        return Err(denied());
    }
    Ok(id)
}

impl ModelQuarantine<'_> {
    fn reauthorize(&self) -> io::Result<()> {
        self.root.reauthorize(&self.root.base_path)?;
        check(&self.dir, 0o700, true)?;
        if !self.id.still_names(self.root.base.as_raw_fd(), QUARANTINE) {
            return Err(denied());
        }
        Ok(())
    }

    /// Write/flush the returned held file before invoking a dependent phase.
    pub fn create_metadata(&mut self, kind: RemovalMetadata) -> io::Result<File> {
        self.reauthorize()?;
        if kind == RemovalMetadata::Committed
            || self.metadata.iter().any(|(slot, _, _)| *slot == kind)
        {
            return Err(denied());
        }
        let parent = self.dir.as_raw_fd();
        let file = new_file(parent, kind.name())?;
        let id = held_asset(&file, parent, kind.name())?;
        self.metadata.push((kind, file, id));
        self.metadata.last().ok_or_else(denied)?.1.try_clone()
    }

    pub fn publish_marker(&mut self) -> io::Result<()> {
        self.reauthorize()?;
        let parent = self.dir.as_raw_fd();
        let (kind, source, id) = self
            .metadata
            .iter_mut()
            .find(|(slot, _, _)| *slot == RemovalMetadata::CommitTemp)
            .ok_or_else(denied)?;
        check(source, 0o600, false)?;
        if !id.still_names(parent, kind.name()) {
            return Err(denied());
        }
        rename_no_replace(
            parent,
            kind.name(),
            parent,
            RemovalMetadata::Committed.name(),
        )?;
        *kind = RemovalMetadata::Committed;
        Ok(())
    }

    pub fn move_from_model(&self, leaf: &str, file: &File, index: usize) -> io::Result<()> {
        self.reauthorize()?;
        let model = self.root.model.as_raw_fd();
        let quarantine = self.dir.as_raw_fd();
        let from = asset_name(leaf)?;
        let to = detached(index)?;
        held_asset(file, model, from)?;
        rename_no_replace(model, from, quarantine, to)?;
        held_asset(file, quarantine, to)?;
        Ok(())
    }

    pub fn restore_to_model(&self, leaf: &str, file: &File, index: usize) -> io::Result<()> {
        self.reauthorize()?;
        let model = self.root.model.as_raw_fd();
        let quarantine = self.dir.as_raw_fd();
        let from = detached(index)?;
        let to = asset_name(leaf)?;
        match named_file(quarantine, from, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                held_asset(file, model, to)?;
                return Ok(());
            }
            opened => {
                opened?;
            }
        }
        held_asset(file, quarantine, from)?;
        rename_no_replace(quarantine, from, model, to)?;
        held_asset(file, model, to)?;
        Ok(())
    }

    pub fn delete_detached(&self, file: &File, index: usize) -> io::Result<()> {
        self.reauthorize()?;
        let dir = self.dir.as_raw_fd();
        let entry = detached(index)?;
        held_asset(file, dir, entry)?;
        unlink(dir, entry, 0)
    }

    pub fn delete_metadata(&mut self, kind: RemovalMetadata) -> io::Result<()> {
        self.reauthorize()?;
        let Some(index) = self.metadata.iter().position(|(slot, _, _)| *slot == kind) else {
            return Ok(());
        };
        let parent = self.dir.as_raw_fd();
        let (_, file, id) = &self.metadata[index];
        check(file, 0o600, false)?;
        if !id.still_names(parent, kind.name()) {
            return Err(denied());
        }
        unlink(parent, kind.name(), 0)?;
        self.metadata.swap_remove(index);
        Ok(())
    }

    /// Synchronize journal/marker directory separately from the file fsync.
    pub fn sync(&self) -> io::Result<()> {
        self.reauthorize()?;
        self.dir.sync_all()?;
        self.root.model.sync_all()?;
        self.root.base.sync_all()
    }

    pub fn finish(&self) -> io::Result<()> {
        self.reauthorize()?;
        // rmdir will fail without sweeping any unknown or remaining byte.
        unlink(self.root.base.as_raw_fd(), QUARANTINE, libc::AT_REMOVEDIR)
    }
}

fn unlink(parent: RawFd, name: &CStr, flags: libc::c_int) -> io::Result<()> {
    // SAFETY: caller's retained fd and inspected leaf; never follows a leaf.
    result(unsafe { libc::unlinkat(parent, name.as_ptr(), flags) } as i64)
}

fn rename_no_replace(from: RawFd, source: &CStr, to: RawFd, dest: &CStr) -> io::Result<()> {
    // SAFETY: live directory descriptors and only validated single leaves.
    // Unknown kernels/filesystems fail closed; never use replacing rename.
    #[cfg(target_os = "linux")]
    let code = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            from,
            source.as_ptr(),
            to,
            dest.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } as i64;
    #[cfg(target_os = "macos")]
    let code =
        unsafe { libc::renameatx_np(from, source.as_ptr(), to, dest.as_ptr(), libc::RENAME_EXCL) }
            as i64;
    result(code)
}

fn result(code: i64) -> io::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
