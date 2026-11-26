//! Linux owner-private wire custody shared by the staging and reopening actors.
//! Neither a ready file nor an operation ID grants supervisor/restore authority.
use super::{check, denied, migration_copy::MigrationCopy, named_file, open_base, Identity};
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::File,
    io::{self, Read, Write},
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::{Path, PathBuf},
};

const MAGIC: &[u8; 8] = b"RSRSAC01";
const INTENT_BYTES: usize = 96;
const MAX_WIRE_BYTES: u64 = 4 * 1024 * 1024 * 1024 + 64 * 1024 + 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArchiveOperation([u8; 16]);

impl ArchiveOperation {
    pub fn from_v4_bytes(bytes: [u8; 16]) -> io::Result<Self> {
        if bytes[6] >> 4 != 4 || bytes[8] & 0xc0 != 0x80 {
            return Err(denied());
        }
        Ok(Self(bytes))
    }

    fn leaf(self, suffix: &str) -> String {
        use std::fmt::Write;
        let mut output = String::with_capacity(60);
        output.push_str(".backup-archive-");
        for value in self.0 {
            let _ = write!(&mut output, "{value:02x}");
        }
        output.push('.');
        output.push_str(suffix);
        output
    }
}

/// The independent supervisor handoff must pin this exact intent digest.
/// Reconstructing this value from an untrusted request is not authorization.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackupArchiveHandoff {
    operation: ArchiveOperation,
    intent_sha256: [u8; 32],
}

impl BackupArchiveHandoff {
    pub fn from_approved_intent(operation: ArchiveOperation, intent_sha256: [u8; 32]) -> Self {
        Self { operation, intent_sha256 }
    }

    pub fn intent_sha256(self) -> [u8; 32] {
        self.intent_sha256
    }
}

struct Intent {
    operation: ArchiveOperation,
    root: [u64; 2],
    file: [u64; 2],
    bytes: u64,
    sha256: [u8; 32],
}

impl Intent {
    fn encode(&self) -> [u8; INTENT_BYTES] {
        let mut result = [0; INTENT_BYTES];
        result[..8].copy_from_slice(MAGIC);
        result[8..24].copy_from_slice(&self.operation.0);
        for (part, value) in result[24..64].chunks_exact_mut(8)
            .zip(self.root.into_iter().chain(self.file).chain([self.bytes]))
        {
            part.copy_from_slice(&value.to_le_bytes());
        }
        result[64..].copy_from_slice(&self.sha256);
        result
    }

    fn decode(raw: &[u8; INTENT_BYTES], operation: ArchiveOperation) -> io::Result<Self> {
        if &raw[..8] != MAGIC || raw[8..24] != operation.0 {
            return Err(denied());
        }
        let word = |offset: usize| -> io::Result<u64> {
            let bytes = raw[offset..offset + 8].try_into().map_err(|_| denied())?;
            Ok(u64::from_le_bytes(bytes))
        };
        let bytes = word(56)?;
        if !(1..=MAX_WIRE_BYTES).contains(&bytes) {
            return Err(denied());
        }
        Ok(Self {
            operation, root: [word(24)?, word(32)?], file: [word(40)?, word(48)?], bytes,
            sha256: raw[64..].try_into().map_err(|_| denied())?,
        })
    }
}

pub struct PendingBackupArchive {
    root: PathBuf,
    wire: MigrationCopy,
    intent: Intent,
}

impl PendingBackupArchive {
    /// The supplied root is a previously verified local owner namespace, not
    /// a filename from HTTP. Reuse the caller's takeover across publication.
    pub fn stage(root: &Path, operation: ArchiveOperation, length: u64,
        mut input: impl Read, mut check_live: impl FnMut() -> io::Result<()>) -> io::Result<Self> {
        if !(1..=MAX_WIRE_BYTES).contains(&length) { return Err(denied()); }
        check_live()?;
        let mut wire = MigrationCopy::create(root, &operation.leaf("pending"))?;
        let mut digest = Sha256::new();
        wire.fill_archive(|out| {
            let mut remaining = length;
            let mut block = [0; 64 * 1024];
            while remaining != 0 {
                check_live()?;
                let size = remaining.min(block.len() as u64) as usize;
                input.read_exact(&mut block[..size])?;
                out.write_all(&block[..size])?;
                digest.update(&block[..size]);
                remaining -= size as u64;
            }
            if input.read(&mut [0])? != 0 { return Err(denied()); }
            Ok(())
        })?;
        check_live()?;
        let (root_identity, file) = wire.archive_binding()?;
        Ok(Self {
            root: root.to_owned(), wire,
            intent: Intent { operation, root: root_identity, file, bytes: length, sha256: digest.finalize().into() },
        })
    }

    pub fn handoff(&self) -> BackupArchiveHandoff {
        BackupArchiveHandoff::from_approved_intent(self.intent.operation, Sha256::digest(self.intent.encode()).into())
    }

    fn persist_intent(&self) -> io::Result<()> {
        let (parent, file) = self.wire.archive_binding()?;
        if parent != self.intent.root || file != self.intent.file || file_identity(&open_base(&self.root)?)? != parent {
            return Err(denied());
        }
        let op = self.intent.operation;
        let mut marker = MigrationCopy::create(&self.root, &op.leaf("intent-pending"))?;
        if marker.archive_binding()?.0 != self.intent.root { return Err(denied()); }
        marker.fill_archive(|out| out.write_all(&self.intent.encode()))?;
        marker.publish(&op.leaf("intent"))
    }

    /// Fsync intent first, then publish the final wire without clobbering.
    /// Do not drain unless reopen succeeds; an error may leave an intent
    /// only (or a final name after a late fsync failure). Never replay.
    pub fn publish(self, mut check_live: impl FnMut() -> io::Result<()>) -> io::Result<ReadyBackupArchive> {
        check_live()?;
        let handoff = self.handoff();
        self.persist_intent()?;
        check_live()?;
        self.wire.publish(&handoff.operation.leaf("archive"))?;
        let ready = ReadyBackupArchive::open(&self.root, handoff)?;
        check_live()?;
        Ok(ready)
    }
}

pub struct ReadyBackupArchive {
    wire: File,
    intent: Intent,
}

impl ReadyBackupArchive {
    /// Requires the digest separately admitted and retained by the native
    /// controller, not merely a matching operation-shaped filename.
    pub fn open(root: &Path, handoff: BackupArchiveHandoff) -> io::Result<Self> {
        let parent = open_base(root)?;
        let (intent, _) = read_intent(&parent, handoff)?;
        let wire = open_leaf(&parent, &handoff.operation.leaf("archive"))?;
        if check(&wire, 0o600, false)?.len() != intent.bytes || file_identity(&wire)? != intent.file {
            return Err(denied());
        }
        Ok(Self { wire, intent })
    }

    /// An isolated decoder may write only its own private disposable stage.
    /// It gets success only when the exact approved wire was actually consumed.
    pub fn consume<T>(self, decode: impl FnOnce(&mut dyn Read) -> io::Result<T>) -> io::Result<T> {
        let mut source = ApprovedRead {
            file: self.wire, digest: Sha256::new(), remaining: self.intent.bytes,
        };
        let result = decode(&mut source)?;
        if source.remaining != 0 || source.file.read(&mut [0])? != 0
            || <[u8; 32]>::from(source.digest.finalize()) != self.intent.sha256
        {
            return Err(denied());
        }
        Ok(result)
    }
}

impl BackupArchiveHandoff {
    /// Only the current coordinator may discard its exact admitted custody.
    /// An unrelated or ambiguous replacement is retained, never unlinked.
    pub fn discard(self, root: &Path) -> io::Result<()> {
        let parent = open_base(root)?;
        let (intent, record_id) = read_intent(&parent, self)?;
        for suffix in ["archive", "pending"] {
            remove_owned(&parent, &self.operation.leaf(suffix), intent.file, Some(intent.bytes))?;
        }
        remove_owned(&parent, &self.operation.leaf("intent"), record_id, Some(INTENT_BYTES as u64))?;
        parent.sync_all()
    }
}

struct ApprovedRead {
    file: File,
    digest: Sha256,
    remaining: u64,
}

impl Read for ApprovedRead {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let limit = self.remaining.min(bytes.len() as u64) as usize;
        let size = self.file.read(&mut bytes[..limit])?;
        self.digest.update(&bytes[..size]);
        self.remaining -= size as u64;
        Ok(size)
    }
}

fn file_identity(file: &File) -> io::Result<[u64; 2]> {
    let metadata = file.metadata()?;
    Ok([metadata.dev(), metadata.ino()])
}

fn open_leaf(parent: &File, leaf: &str) -> io::Result<File> {
    let leaf = CString::new(leaf).map_err(|_| denied())?;
    named_file(parent.as_raw_fd(), &leaf, libc::O_RDONLY, 0)
}

fn read_intent(parent: &File, handoff: BackupArchiveHandoff) -> io::Result<(Intent, [u64; 2])> {
    let mut marker = open_leaf(parent, &handoff.operation.leaf("intent"))?;
    if check(&marker, 0o600, false)?.len() != INTENT_BYTES as u64 {
        return Err(denied());
    }
    let mut bytes = [0; INTENT_BYTES];
    marker.read_exact(&mut bytes)?;
    if marker.read(&mut [0])? != 0 || <[u8; 32]>::from(Sha256::digest(bytes)) != handoff.intent_sha256 {
        return Err(denied());
    }
    let intent = Intent::decode(&bytes, handoff.operation)?;
    if intent.root != file_identity(parent)? {
        return Err(denied());
    }
    Ok((intent, file_identity(&marker)?))
}

fn remove_owned(parent: &File, leaf: &str, expected: [u64; 2], size: Option<u64>) -> io::Result<()> {
    let file = match open_leaf(parent, leaf) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let metadata = check(&file, 0o600, false)?;
    let identity = Identity::from_metadata(&metadata);
    let name = CString::new(leaf).map_err(|_| denied())?;
    if [metadata.dev(), metadata.ino()] != expected || size.is_some_and(|size| size != metadata.len())
        || !identity.still_names(parent.as_raw_fd(), &name)
    {
        return Err(denied());
    }
    // SAFETY: pinned parent, validated leaf and exact still-named owner file.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
#[path = "unix_backup_archive_tests.rs"]
mod tests;
