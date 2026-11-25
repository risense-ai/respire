use super::*;
use std::path::PathBuf;

/// Retains exact private directory identities for a single staged model job.
pub struct ModelJobRoot {
    base: File,
    slot: File,
    slot_leaf: CString,
    slot_identity: Identity,
    directory: File,
    index: File,
    spools: [Option<(File, Identity)>; 2],
    identity: Identity,
    index_identity: Identity,
    leaf: CString,
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
        let parent = open_base(base)?;
        use crate::owner_credential::model_job::{job_name, limit, SLOTS};
        let name = job_name()?;
        let leaf = CString::new(name.as_bytes()).map_err(|_| denied())?;
        let mut claim = None;
        for slot_name in SLOTS {
            let slot_leaf = CString::new(slot_name).map_err(|_| denied())?;
            match CreatedDirectory::create(&parent, &slot_leaf) {
                Ok(slot) => {
                    claim = Some((slot_name, slot_leaf, slot));
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        let (slot_name, slot_leaf, slot) = claim.ok_or_else(limit)?;
        after_claim()?;
        let directory = CreatedDirectory::create(slot.file()?, &leaf)?;
        let index = CreatedDirectory::create(directory.file()?, c"index")?;
        let (index, index_identity) = index.finish()?;
        let (directory, identity) = directory.finish()?;
        let (slot, slot_identity) = slot.finish()?;

        let job = Self {
            base: parent,
            slot,
            slot_leaf,
            slot_identity,
            directory,
            index,
            spools: [None, None],
            identity,
            index_identity,
            leaf,
            path: base.join(slot_name).join(name),
        };
        job.check()?;
        Ok(job)
    }

    pub fn root(&self) -> io::Result<&Path> {
        self.check()?;
        Ok(&self.path)
    }

    pub fn index_root(&self) -> io::Result<PathBuf> {
        Ok(self.root()?.join("index"))
    }

    fn check(&self) -> io::Result<()> {
        check(&self.directory, 0o700, true)?;
        check(&self.index, 0o700, true)?;
        check(&self.slot, 0o700, true)?;
        let base_path = self
            .path
            .parent()
            .and_then(Path::parent)
            .ok_or_else(denied)?;
        let current = open_base(base_path)?;
        let left = Identity::from_metadata(&current.metadata()?);
        let right = Identity::from_metadata(&self.base.metadata()?);
        if (left.dev, left.ino) != (right.dev, right.ino) {
            return Err(denied());
        }
        if !self.identity.still_names(self.slot.as_raw_fd(), &self.leaf)
            || !self
                .slot_identity
                .still_names(self.base.as_raw_fd(), &self.slot_leaf)
            || !self
                .index_identity
                .still_names(self.directory.as_raw_fd(), c"index")
        {
            return Err(denied());
        }
        Ok(())
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
        self.check()?;
        if self.spools[index].is_some() {
            return Err(denied());
        }
        let leaf = [c"source.rows", c"prepared.rows"][index];
        let file = named_file(
            self.directory.as_raw_fd(),
            leaf,
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )?;
        let identity = Identity::from_metadata(&check(&file, 0o600, false)?);
        let staged = file.try_clone();
        self.spools[index] = Some((file, identity));
        staged
    }

    /// Call only after successful native child reaping. Never sweeps unknown
    /// files, other jobs or links. Nonempty jobs stay within the retention cap.
    pub fn remove_empty(self) -> io::Result<()> {
        self.check()?;
        for (index, spool) in self.spools.iter().enumerate() {
            let Some((file, identity)) = spool else {
                continue;
            };
            let parent = self.directory.as_raw_fd();
            let leaf = [c"source.rows", c"prepared.rows"][index];
            check(file, 0o600, false)?;
            if !identity.still_names(parent, leaf) {
                return Err(denied());
            }
            // SAFETY: retained directory and exact owned regular file identity.
            if unsafe { libc::unlinkat(parent, leaf.as_ptr(), 0) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        remove_dir(&self.directory, c"index")?;
        remove_dir(&self.slot, &self.leaf)?;
        remove_dir(&self.base, &self.slot_leaf)
    }
}

struct CreatedDirectory<'a> {
    parent: &'a File,
    leaf: CString,
    file: Option<File>,
    identity: Option<Identity>,
}

impl<'a> CreatedDirectory<'a> {
    fn create(parent: &'a File, leaf: &CStr) -> io::Result<Self> {
        create_dir(parent, leaf)?;
        let directory = named_file(
            parent.as_raw_fd(),
            leaf,
            libc::O_RDONLY | libc::O_DIRECTORY,
            0,
        )?;
        let identity = Identity::from_metadata(&directory.metadata()?);
        let created = Self {
            parent,
            leaf: leaf.to_owned(),
            file: Some(directory),
            identity: Some(identity),
        };
        check(created.file()?, 0o700, true)?;
        Ok(created)
    }

    fn file(&self) -> io::Result<&File> {
        self.file.as_ref().ok_or_else(denied)
    }

    fn finish(mut self) -> io::Result<(File, Identity)> {
        let file = self.file.take().ok_or_else(denied)?;
        let identity = self.identity.take().ok_or_else(denied)?;
        Ok((file, identity))
    }
}

impl Drop for CreatedDirectory<'_> {
    fn drop(&mut self) {
        if self
            .identity
            .as_ref()
            .is_some_and(|identity| identity.still_names(self.parent.as_raw_fd(), &self.leaf))
        {
            let _ = remove_dir(self.parent, &self.leaf);
        }
    }
}
fn create_dir(parent: &File, name: &CStr) -> io::Result<()> {
    // SAFETY: parent is retained and name is a fixed/validated leaf.
    if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn remove_dir(parent: &File, name: &CStr) -> io::Result<()> {
    // SAFETY: held parent; removal applies only to an empty checked directory.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
