//! Root every removal mutation, including rollback, in the verified Windows
//! owner namespace. Nothing here takes a caller-supplied filesystem path.
use super::super::RemovalMetadata;
use super::model_stage::identity;
use super::*;

const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
const READ_WRITE_SHARE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE;
const JOURNAL_DIRECTORY: &str = ".runtime-model-removal-v1";

pub(super) fn require_no_recovery(base: HANDLE) -> io::Result<()> {
    match relative_open(
        base,
        OsStr::new(JOURNAL_DIRECTORY),
        FILE_READ_ATTRIBUTES,
        SHARE_ALL,
        None,
        FILE_OPEN,
        0,
    ) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        _ => Err(denied("managed model removal recovery pending")),
    }
}

pub struct ModelRemovalRoot {
    base: OwnedHandle,
    model: OwnedHandle,
    model_name: String,
}

pub struct ModelQuarantine<'a> {
    root: &'a ModelRemovalRoot,
    directory: OwnedHandle,
    metadata: Vec<(RemovalMetadata, File)>,
}

impl ModelRemovalRoot {
    pub fn open(base: &Path, model: &str) -> io::Result<Self> {
        let base = pinned_base_with_sharing(base, READ_WRITE_SHARE, true)?;
        let model_handle = relative_open(
            base.as_raw_handle(),
            OsStr::new(model),
            META | FILE_ADD_FILE | FILE_TRAVERSE,
            READ_WRITE_SHARE,
            None,
            FILE_OPEN,
            FILE_DIRECTORY_FILE,
        )?;
        let root = Self {
            base,
            model: model_handle,
            model_name: model.to_owned(),
        };
        root.check_directories()?;
        Ok(root)
    }

    fn check_directories(&self) -> io::Result<()> {
        let user = CurrentUser::query()?;
        checked_directory(self.base.as_raw_handle())?;
        checked_directory(self.model.as_raw_handle())?;
        user.verify_handle(self.base.as_raw_handle(), false)?;
        user.verify_handle(self.model.as_raw_handle(), false)
    }

    pub fn reauthorize(&self, base: &Path) -> io::Result<()> {
        self.check_directories()?;
        let named_base = pinned_base_with_sharing(base, SHARE_ALL, true)?;
        if identity(named_base.as_raw_handle())? != identity(self.base.as_raw_handle())? {
            return Err(denied("managed model namespace identity changed"));
        }
        let named_model = relative_open(
            self.base.as_raw_handle(),
            OsStr::new(&self.model_name),
            META | FILE_TRAVERSE,
            SHARE_ALL,
            None,
            FILE_OPEN,
            FILE_DIRECTORY_FILE,
        )?;
        if identity(named_model.as_raw_handle())? != identity(self.model.as_raw_handle())? {
            return Err(denied("managed model directory identity changed"));
        }
        Ok(())
    }

    pub fn open_model_asset(&self, leaf: &str) -> io::Result<File> {
        let handle = relative_open(
            self.model.as_raw_handle(),
            asset_leaf(leaf)?,
            GENERIC_READ | META,
            FILE_SHARE_READ | FILE_SHARE_DELETE,
            None,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE,
        )?;
        check_asset(handle.as_raw_handle())?;
        Ok(File::from(handle))
    }

    pub fn create_quarantine(&self) -> io::Result<ModelQuarantine<'_>> {
        let user = CurrentUser::query()?;
        let private = user.descriptor()?;
        let directory = relative_open(
            self.base.as_raw_handle(),
            OsStr::new(JOURNAL_DIRECTORY),
            META | DELETE | FILE_ADD_FILE | FILE_TRAVERSE,
            READ_WRITE_SHARE,
            Some(&private),
            FILE_CREATE,
            FILE_DIRECTORY_FILE,
        )?;
        checked_directory(directory.as_raw_handle())?;
        user.verify_handle(directory.as_raw_handle(), true)?;
        Ok(ModelQuarantine {
            root: self,
            directory,
            metadata: Vec::new(),
        })
    }
}

impl ModelQuarantine<'_> {
    pub fn create_metadata(&mut self, kind: RemovalMetadata) -> io::Result<File> {
        self.check_quarantine()?;
        if kind == RemovalMetadata::Committed || self.metadata.iter().any(|(slot, _)| *slot == kind)
        {
            return Err(denied("managed model journal slot unavailable"));
        }
        let user = CurrentUser::query()?;
        let descriptor = user.descriptor()?;
        let file = relative_open(
            self.directory.as_raw_handle(),
            metadata_leaf(kind),
            GENERIC_WRITE | META | DELETE,
            FILE_SHARE_READ | FILE_SHARE_DELETE,
            Some(&descriptor),
            FILE_CREATE,
            FILE_NON_DIRECTORY_FILE,
        )?;
        checked_file(file.as_raw_handle(), Some(0))?;
        user.verify_handle(file.as_raw_handle(), true)?;
        let file = File::from(file);
        let writer = file.try_clone()?;
        self.metadata.push((kind, file));
        Ok(writer)
    }

    fn check_quarantine(&self) -> io::Result<()> {
        checked_directory(self.directory.as_raw_handle())?;
        CurrentUser::query()?.verify_handle(self.directory.as_raw_handle(), true)?;
        self.root.check_directories()
    }

    pub fn move_from_model(&self, leaf: &str, expected: &File, index: usize) -> io::Result<()> {
        self.check_quarantine()?;
        let source = named_asset(self.root.model.as_raw_handle(), asset_leaf(leaf)?, expected)?;
        let destination = index_leaf(index)?;
        rename_relative(
            source.as_raw_handle(),
            self.directory.as_raw_handle(),
            destination,
            0,
        )
    }

    pub fn restore_to_model(&self, leaf: &str, expected: &File, index: usize) -> io::Result<()> {
        // Have the OS perform the no-replace check on the exact destination
        // directory. A failed recovery leaves the protected journal in place.
        self.check_quarantine()?;
        let source = match named_asset(self.directory.as_raw_handle(), index_leaf(index)?, expected)
        {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                open_expected_asset(
                    self.root.model.as_raw_handle(),
                    asset_leaf(leaf)?,
                    expected,
                    META,
                )?;
                return Ok(());
            }
            result => result?,
        };
        rename_relative(
            source.as_raw_handle(),
            self.root.model.as_raw_handle(),
            asset_leaf(leaf)?,
            0,
        )
    }

    pub fn delete_detached(&self, expected: &File, index: usize) -> io::Result<()> {
        self.check_quarantine()?;
        let source = named_asset(self.directory.as_raw_handle(), index_leaf(index)?, expected)?;
        mark_delete(source.as_raw_handle())
    }

    pub fn publish_marker(&mut self) -> io::Result<()> {
        self.check_quarantine()?;
        let (kind, marker) = self
            .metadata
            .iter_mut()
            .find(|(slot, _)| *slot == RemovalMetadata::CommitTemp)
            .ok_or_else(|| denied("missing managed model commit marker"))?;
        verify_metadata(self.directory.as_raw_handle(), *kind, marker)?;
        rename_relative(
            marker.as_raw_handle(),
            self.directory.as_raw_handle(),
            metadata_leaf(RemovalMetadata::Committed),
            0,
        )?;
        *kind = RemovalMetadata::Committed;
        Ok(())
    }

    pub fn delete_metadata(&mut self, kind: RemovalMetadata) -> io::Result<()> {
        self.check_quarantine()?;
        if let Some(index) = self.metadata.iter().position(|(slot, _)| *slot == kind) {
            let (_, file) = &self.metadata[index];
            verify_metadata(self.directory.as_raw_handle(), kind, file)?;
            mark_delete(file.as_raw_handle())?;
            self.metadata.swap_remove(index);
        }
        Ok(())
    }

    pub fn sync(&self) -> io::Result<()> {
        // Journal and marker files are flushed through their original handles.
        self.check_quarantine()
    }

    pub fn finish(&self) -> io::Result<()> {
        self.check_quarantine()?;
        mark_delete(self.directory.as_raw_handle())
    }
}

fn check_asset(handle: HANDLE) -> io::Result<()> {
    checked_file(handle, None)?;
    // An inherited read-only ACE is acceptable for assets, but no other
    // unprivileged OS user may modify their object or its namespace.
    CurrentUser::query()?.verify_handle(handle, false)
}

fn named_asset(parent: HANDLE, leaf: &OsStr, expected: &File) -> io::Result<OwnedHandle> {
    open_expected_asset(parent, leaf, expected, META | DELETE)
}

fn open_expected_asset(
    parent: HANDLE,
    leaf: &OsStr,
    expected: &File,
    access: u32,
) -> io::Result<OwnedHandle> {
    let file = relative_open(
        parent,
        leaf,
        access,
        SHARE_ALL,
        None,
        FILE_OPEN,
        FILE_NON_DIRECTORY_FILE,
    )?;
    check_asset(file.as_raw_handle())?;
    if identity(file.as_raw_handle())? != identity(expected.as_raw_handle())? {
        return Err(denied("managed model file identity changed"));
    }
    Ok(file)
}

fn asset_leaf(name: &str) -> io::Result<&OsStr> {
    if matches!(
        name,
        "model_quantized.onnx" | "model.safetensors" | "tokenizer.json"
    ) {
        Ok(OsStr::new(name))
    } else {
        Err(denied("invalid managed model artifact name"))
    }
}

fn index_leaf(index: usize) -> io::Result<&'static OsStr> {
    match index {
        0 => Ok(OsStr::new("0")),
        1 => Ok(OsStr::new("1")),
        _ => Err(denied("invalid model artifact index")),
    }
}

fn metadata_leaf(kind: RemovalMetadata) -> &'static OsStr {
    OsStr::new(match kind {
        RemovalMetadata::Journal => "journal.json",
        RemovalMetadata::CommitTemp => "commit.tmp",
        RemovalMetadata::Committed => "committed",
    })
}

fn verify_metadata(parent: HANDLE, kind: RemovalMetadata, expected: &File) -> io::Result<()> {
    let file = relative_open(
        parent,
        metadata_leaf(kind),
        META,
        SHARE_ALL,
        None,
        FILE_OPEN,
        FILE_NON_DIRECTORY_FILE,
    )?;
    checked_file(file.as_raw_handle(), None)?;
    CurrentUser::query()?.verify_handle(file.as_raw_handle(), true)?;
    if identity(file.as_raw_handle())? != identity(expected.as_raw_handle())? {
        return Err(denied("managed model journal entry changed"));
    }
    Ok(())
}
