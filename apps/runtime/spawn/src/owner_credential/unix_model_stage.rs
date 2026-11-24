//! FD-relative, owner-private staging for a complete pinned model pair.
use super::*;
use std::cell::Cell;

struct Entry {
    name: CString,
    handle: File,
    identity: Identity,
}

/// The existing Unix ownership policy admits only local filesystems with
/// enforceable ownership; other processes of the same OS user are trusted.
pub struct ModelStage {
    base: File,
    base_id: Identity,
    stage: File,
    stage_id: Identity,
    name: CString,
    files: Vec<Entry>,
    sealed: Cell<bool>,
    published: bool,
}

fn leaf(name: &str) -> io::Result<CString> {
    if name.is_empty()
        || name.len() > 128
        || name == "."
        || name == ".."
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err(denied());
    }
    CString::new(name).map_err(|_| denied())
}

pub fn same_model_file(left: &File, right: &File) -> io::Result<bool> {
    Ok(identity_tuple(left)? == identity_tuple(right)?)
}

impl ModelStage {
    pub fn create(base: &Path, name: &str) -> io::Result<Self> {
        let name = leaf(name)?;
        let base = open_base(base)?;
        let base_id = Identity::from_metadata(&base.metadata()?);
        let parent = base.as_raw_fd();
        super::model_removal::require_no_recovery(parent)?;
        // SAFETY: parent is retained, name is a validated single leaf.
        if unsafe { libc::mkdirat(parent, name.as_ptr(), 0o700) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let stage = named_file(parent, &name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
        let stage_id = Identity::from_metadata(&stage.metadata()?);
        let guard = Self {
            base,
            base_id,
            stage,
            stage_id,
            name,
            files: Vec::new(),
            sealed: Cell::new(false),
            published: false,
        };
        guard.revalidate_stage()?;
        Ok(guard)
    }

    /// Caller writes through a clone of the precise exclusive inode retained
    /// here; the same handle must not be used to mutate the file after sealing.
    pub fn create_file(&mut self, name: &'static str) -> io::Result<File> {
        if self.published || self.sealed.get() || self.files.len() >= 2 {
            return Err(denied());
        }
        if !matches!(
            name,
            "model_quantized.onnx" | "model.safetensors" | "tokenizer.json"
        ) {
            return Err(denied());
        }
        if name != "tokenizer.json"
            && self
                .files
                .iter()
                .any(|entry| entry.name.as_c_str() != c"tokenizer.json")
        {
            return Err(denied());
        }
        self.revalidate_stage()?;
        let name = leaf(name)?;
        let handle = named_file(
            self.stage.as_raw_fd(),
            &name,
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )?;
        let identity = Identity::from_metadata(&handle.metadata()?);
        self.files.push(Entry {
            name,
            handle,
            identity,
        });
        let entry = self.files.last().ok_or_else(denied)?;
        check(&entry.handle, 0o600, false)?;
        entry.handle.try_clone()
    }

    pub fn seal(&self) -> io::Result<()> {
        self.revalidate_pair()?;
        for entry in &self.files {
            entry.handle.sync_all()?;
        }
        self.stage.sync_all()?;
        self.revalidate_pair()?;
        self.sealed.set(true);
        Ok(())
    }

    pub fn publish(
        &mut self,
        base: &Path,
        destination: &str,
        verify: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        if self.published || !self.sealed.get() {
            return Err(denied());
        }
        let dest = leaf(destination)?;
        if dest == self.name {
            return Err(denied());
        }
        let reopened = open_base(base)?;
        if identity_tuple(&reopened)? != (self.base_id.dev, self.base_id.ino) {
            return Err(denied());
        }
        self.revalidate_pair()?;
        let parent = self.base.as_raw_fd();
        super::model_removal::require_no_recovery(parent)?;
        let old = checked_old_destination(parent, &dest)?;
        verify()?;
        if let Some((_held, id)) = old {
            if !id.still_names(parent, &dest) {
                return Err(denied());
            }
            // SAFETY: exact held owner directory, relative to the held parent.
            // rmdir refuses nonempty entries and never unlinks a symlink or
            // recurses. The final rename still cannot replace any new entry.
            if unsafe { libc::unlinkat(parent, dest.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
                let error = io::Error::last_os_error();
                return Err(
                    if matches!(error.raw_os_error(), Some(libc::ENOTEMPTY | libc::EEXIST)) {
                        io::Error::from(io::ErrorKind::AlreadyExists)
                    } else {
                        error
                    },
                );
            }
        }
        no_replace(parent, &self.name, &dest)?;
        self.published = true;
        Ok(())
    }

    fn revalidate_stage(&self) -> io::Result<()> {
        check(&self.stage, 0o700, true)?;
        if !self.stage_id.still_names(self.base.as_raw_fd(), &self.name) {
            return Err(denied());
        }
        Ok(())
    }

    fn revalidate_pair(&self) -> io::Result<()> {
        self.revalidate_stage()?;
        if self.files.len() != 2 {
            return Err(denied());
        }
        for entry in &self.files {
            check(&entry.handle, 0o600, false)?;
            if !entry
                .identity
                .still_names(self.stage.as_raw_fd(), &entry.name)
            {
                return Err(denied());
            }
        }
        Ok(())
    }
}

fn checked_old_destination(parent: RawFd, name: &CStr) -> io::Result<Option<(File, Identity)>> {
    let dir = match named_file(parent, name, libc::O_RDONLY | libc::O_DIRECTORY, 0) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        result => result?,
    };
    let meta = dir.metadata()?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o022 != 0 {
        return Err(denied());
    }
    require_local_ownership(&dir)?;
    reject_extended_acl(&dir)?;
    let id = Identity::from_metadata(&meta);
    if !id.still_names(parent, name) {
        return Err(denied());
    }
    Ok(Some((dir, id)))
}

fn identity_tuple(file: &File) -> io::Result<(u64, u64)> {
    let id = Identity::from_metadata(&file.metadata()?);
    Ok((id.dev, id.ino))
}

fn no_replace(parent: RawFd, source: &CStr, dest: &CStr) -> io::Result<()> {
    // SAFETY: both leaves and the retained same-directory descriptor are
    // valid. Do not fall back to replacing rename when unsupported.
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            parent,
            source.as_ptr(),
            parent,
            dest.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    let result = unsafe {
        libc::renameatx_np(
            parent,
            source.as_ptr(),
            parent,
            dest.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

impl Drop for ModelStage {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        let stage = self.stage.as_raw_fd();
        for entry in &self.files {
            if entry.identity.still_names(stage, &entry.name) {
                // SAFETY: retained verified parent; unlink does not follow.
                unsafe { libc::unlinkat(stage, entry.name.as_ptr(), 0) };
            }
        }
        let base = self.base.as_raw_fd();
        if self.stage_id.still_names(base, &self.name) {
            // SAFETY: retained verified parent; fails for nonempty directories.
            unsafe { libc::unlinkat(base, self.name.as_ptr(), libc::AT_REMOVEDIR) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use super::super::tests::Scratch;

    fn root() -> io::Result<Scratch> {
        Scratch::model_fixture()
    }

    fn prepare(base: &Path) -> io::Result<ModelStage> {
        let mut staged = ModelStage::create(base, ".incoming-fixture")?;
        staged
            .create_file("model_quantized.onnx")?
            .write_all(b"synthetic weights")?;
        staged
            .create_file("tokenizer.json")?
            .write_all(b"synthetic tokenizer")?;
        staged.seal()?;
        Ok(staged)
    }

    #[test]
    fn cancellation_preserves_existing_pair_and_retires_only_staging() -> io::Result<()> {
        let root = root()?;
        let base = root.path().canonicalize()?;
        let mut staged = prepare(&base)?;
        let denied = staged.publish(&base, "m3", || Err(io::Error::other("cancelled")));
        assert_eq!(denied.unwrap_err().to_string(), "cancelled");
        assert!(!base.join("m3").exists());
        fs::create_dir(base.join("m3"))?;
        fs::write(base.join("m3").join("sentinel"), b"unchanged")?;
        assert_eq!(
            staged.publish(&base, "m3", || Ok(())).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        drop(staged);
        assert!(!base.join(".incoming-fixture").exists());
        assert_eq!(fs::read(base.join("m3").join("sentinel"))?, b"unchanged");
        Ok(())
    }

    #[test]
    fn owned_empty_destination_survives_cancel_and_supports_reinstall() -> io::Result<()> {
        let root = root()?;
        let base = root.path().canonicalize()?;
        let mut staged = prepare(&base)?;
        fs::create_dir(base.join("m3"))?;
        let before = identity_tuple(&File::open(base.join("m3"))?)?;
        let denied = staged.publish(&base, "m3", || Err(io::Error::other("cancelled")));
        assert_eq!(denied.unwrap_err().to_string(), "cancelled");
        assert_eq!(identity_tuple(&File::open(base.join("m3"))?)?, before);
        assert!(fs::read_dir(base.join("m3"))?.next().is_none());

        staged.publish(&base, "m3", || Ok(()))?;
        assert_eq!(
            fs::read(base.join("m3/model_quantized.onnx"))?,
            b"synthetic weights"
        );
        assert_eq!(
            fs::read(base.join("m3/tokenizer.json"))?,
            b"synthetic tokenizer"
        );
        Ok(())
    }

    #[test]
    fn symlink_destination_is_never_replaced_or_followed() -> io::Result<()> {
        let root = root()?;
        let base = root.path().canonicalize()?;
        let mut staged = prepare(&base)?;
        fs::create_dir(base.join("other"))?;
        std::os::unix::fs::symlink(base.join("other"), base.join("m3"))?;
        assert!(staged.publish(&base, "m3", || Ok(())).is_err());
        assert!(fs::symlink_metadata(base.join("m3"))?
            .file_type()
            .is_symlink());
        assert!(fs::read_dir(base.join("other"))?.next().is_none());
        Ok(())
    }

    #[test]
    fn sealed_pair_is_published_once_from_exact_base() -> io::Result<()> {
        let root = root()?;
        let base = root.path().canonicalize()?;
        let mut staged = prepare(&base)?;
        let other = self::root()?;
        assert_eq!(
            staged
                .publish(&other.path().canonicalize()?, "m3", || Ok(()))
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        staged.publish(&base, "m3", || Ok(()))?;
        assert!(staged.publish(&base, "m3-again", || Ok(())).is_err());
        drop(staged);
        assert!(!base.join(".incoming-fixture").exists());
        assert_eq!(
            fs::read(base.join("m3/model_quantized.onnx"))?,
            b"synthetic weights"
        );
        assert_eq!(
            fs::read(base.join("m3/tokenizer.json"))?,
            b"synthetic tokenizer"
        );
        Ok(())
    }
}
