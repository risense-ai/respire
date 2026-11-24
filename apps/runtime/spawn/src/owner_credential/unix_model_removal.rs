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

#[cfg(test)]
mod tests {
    use super::super::model_stage::ModelStage;
    use super::*;
    use std::{
        cell::Cell,
        fs,
        io::Write,
        os::unix::fs::{MetadataExt, OpenOptionsExt},
    };

    const MODEL: &str = "bge-small-zh-v1.5";
    const WEIGHTS: &str = "model_quantized.onnx";
    const TOKENIZER: &str = "tokenizer.json";
    const PINNED_PAIR: [(&str, &[u8]); 2] = [
        (WEIGHTS, b"synthetic weights"),
        (TOKENIZER, b"synthetic tokenizer"),
    ];

    fn fixture_root() -> io::Result<(tempfile::TempDir, std::path::PathBuf)> {
        // Match the existing owner-credential tests: Linux container /tmp
        // can sit on overlayfs, which is not trusted by the OS owner policy.
        #[cfg(target_os = "linux")]
        let temp = tempfile::tempdir_in("/dev/shm").or_else(|_| tempfile::tempdir())?;
        #[cfg(target_os = "macos")]
        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        assert_eq!(fs::metadata(&base)?.mode() & 0o777, 0o700);
        Ok((temp, base))
    }

    fn stage_pair(base: &Path, incoming: &str) -> io::Result<ModelStage> {
        let mut stage = ModelStage::create(base, incoming)?;
        for (leaf, bytes) in PINNED_PAIR {
            stage.create_file(leaf)?.write_all(bytes)?;
        }
        stage.seal()?;
        Ok(stage)
    }

    fn install_fixture(base: &Path) -> io::Result<ModelRemovalRoot> {
        stage_pair(base, ".incoming-first-pair")?.publish(base, MODEL, || Ok(()))?;
        ModelRemovalRoot::open(base, MODEL)
    }

    fn prepared_quarantine(root: &ModelRemovalRoot) -> io::Result<ModelQuarantine<'_>> {
        let mut quarantine = root.create_quarantine()?;
        let mut journal = quarantine.create_metadata(RemovalMetadata::Journal)?;
        journal.write_all(br#"{"model_id":"bge-small-zh-v1.5","phase":"prepared"}"#)?;
        journal.sync_all()?;
        drop(journal);
        quarantine.sync()?;
        Ok(quarantine)
    }

    fn assert_missing(path: &Path) -> io::Result<()> {
        let error = fs::symlink_metadata(path).expect_err("expected absent filesystem entry");
        if error.kind() != io::ErrorKind::NotFound {
            return Err(error);
        }
        Ok(())
    }

    #[test]
    fn unix_removal_commits_exact_pair_and_cleans_private_quarantine() -> io::Result<()> {
        let (_temp, base) = fixture_root()?;
        let root = install_fixture(&base)?;
        let quarantine_path = base.join(".runtime-model-removal-v1");
        let model = base.join(MODEL);
        let files = [
            root.open_model_asset(WEIGHTS)?,
            root.open_model_asset(TOKENIZER)?,
        ];
        let mut quarantine = prepared_quarantine(&root)?;
        assert_eq!(fs::metadata(&quarantine_path)?.mode() & 0o777, 0o700);
        assert_eq!(
            fs::metadata(quarantine_path.join("journal.json"))?.mode() & 0o777,
            0o600
        );
        for (index, (leaf, bytes)) in PINNED_PAIR.into_iter().enumerate() {
            quarantine.move_from_model(leaf, &files[index], index)?;
            assert_missing(&model.join(leaf))?;
            let moved = quarantine_path.join(index.to_string());
            assert_eq!(fs::read(&moved)?, bytes);
            assert_eq!(fs::metadata(&moved)?.ino(), files[index].metadata()?.ino());
        }
        let mut marker = quarantine.create_metadata(RemovalMetadata::CommitTemp)?;
        marker.write_all(MODEL.as_bytes())?;
        marker.sync_all()?;
        drop(marker);
        assert_missing(&quarantine_path.join("committed"))?;
        quarantine.publish_marker()?;
        quarantine.sync()?;
        assert_missing(&quarantine_path.join("commit.tmp"))?;
        assert_eq!(
            fs::read(quarantine_path.join("committed"))?,
            MODEL.as_bytes()
        );
        assert_eq!(
            fs::metadata(quarantine_path.join("committed"))?.mode() & 0o777,
            0o600
        );

        for (index, file) in files.iter().enumerate() {
            quarantine.delete_detached(file, index)?;
            assert_missing(&quarantine_path.join(index.to_string()))?;
        }
        quarantine.delete_metadata(RemovalMetadata::Committed)?;
        quarantine.delete_metadata(RemovalMetadata::Journal)?;
        quarantine.finish()?;
        assert_missing(&quarantine_path)?;
        assert_eq!(model.read_dir()?.count(), 0);
        Ok(())
    }

    #[test]
    fn unix_removal_rollback_never_replaces_an_occupied_leaf() -> io::Result<()> {
        let (_temp, base) = fixture_root()?;
        let root = install_fixture(&base)?;
        let model = base.join(MODEL);
        let quarantine_path = base.join(".runtime-model-removal-v1");
        let weights = root.open_model_asset(WEIGHTS)?;
        let tokenizer = root.open_model_asset(TOKENIZER)?;
        let mut quarantine = prepared_quarantine(&root)?;
        quarantine.move_from_model(WEIGHTS, &weights, 0)?;

        // The caller cancels before moving the second asset. Its untouched
        // original is allowed when reconciling an uncertain attempted slot.
        quarantine.restore_to_model(TOKENIZER, &tokenizer, 1)?;
        assert_eq!(fs::read(model.join(TOKENIZER))?, PINNED_PAIR[1].1);
        let occupied = model.join(WEIGHTS);
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&occupied)?
            .write_all(b"unrelated fixture entry")?;
        let denied = quarantine
            .restore_to_model(WEIGHTS, &weights, 0)
            .unwrap_err();
        assert_eq!(denied.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&occupied)?, b"unrelated fixture entry");
        assert_eq!(fs::read(quarantine_path.join("0"))?, PINNED_PAIR[0].1);
        assert!(quarantine_path.join("journal.json").is_file());
        assert!(quarantine.finish().is_err());

        // Clear only the test-owned conflict and restore the exact original.
        fs::remove_file(&occupied)?;
        quarantine.restore_to_model(WEIGHTS, &weights, 0)?;
        assert_eq!(fs::read(&occupied)?, PINNED_PAIR[0].1);
        assert_eq!(fs::metadata(&occupied)?.ino(), weights.metadata()?.ino());
        quarantine.sync()?;
        quarantine.delete_metadata(RemovalMetadata::Journal)?;
        quarantine.finish()?;
        assert_missing(&quarantine_path)?;
        Ok(())
    }

    #[test]
    fn unix_recovery_barrier_blocks_stage_creation_and_sealed_publish() -> io::Result<()> {
        let (_temp, base) = fixture_root()?;
        let root = install_fixture(&base)?;
        let mut staged = stage_pair(&base, ".incoming-already-sealed")?;
        // Drop the quarantine holder: the private entry is intentionally
        // retained across operation/process lifetimes, never swept by Drop.
        drop(prepared_quarantine(&root)?);
        let protected = base.join(".runtime-model-removal-v1/journal.json");
        let journal_before = fs::read(&protected)?;

        assert!(ModelStage::create(&base, ".incoming-denied").is_err());
        assert_missing(&base.join(".incoming-denied"))?;
        let commit_checked = Cell::new(false);
        assert!(staged
            .publish(&base, "m3", || {
                commit_checked.set(true);
                Ok(())
            })
            .is_err());
        assert!(!commit_checked.get());
        assert_missing(&base.join("m3"))?;
        drop(staged);
        assert_missing(&base.join(".incoming-already-sealed"))?;
        assert_eq!(fs::read(&protected)?, journal_before);
        for (leaf, bytes) in PINNED_PAIR {
            assert_eq!(fs::read(base.join(MODEL).join(leaf))?, bytes);
        }
        Ok(())
    }
}
