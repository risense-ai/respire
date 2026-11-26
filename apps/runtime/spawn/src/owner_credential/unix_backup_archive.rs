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
const TERMINAL_LIMIT: u64 = 4096;
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

struct BoundedArchiveWriter<'a> {
    out: &'a mut File,
    digest: &'a mut Sha256,
    remaining: u64,
}

impl Write for BoundedArchiveWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.remaining { return Err(denied()); }
        let written = self.out.write(bytes)?;
        self.digest.update(&bytes[..written]);
        self.remaining -= written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> { self.out.flush() }
}

impl PendingBackupArchive {
    /// The supplied root is a previously verified local owner namespace, not
    /// a filename from HTTP. Reuse the caller's takeover across publication.
    pub fn stage(root: &Path, operation: ArchiveOperation, length: u64,
        mut input: impl Read, check_live: impl FnMut() -> io::Result<()>) -> io::Result<Self> {
        Self::stage_verified(root, operation, length, check_live, |out, check_live| {
            let mut remaining = length;
            let mut block = [0; 64 * 1024];
            while remaining != 0 {
                check_live()?;
                let size = remaining.min(block.len() as u64) as usize;
                input.read_exact(&mut block[..size])?;
                out.write_all(&block[..size])?;
                remaining -= size as u64;
            }
            if input.read(&mut [0])? != 0 { return Err(denied()); }
            Ok(())
        })
    }

    /// Charge the single owner-root import slot before copying any archive bytes.
    /// Surviving custody from an earlier process counts against the same slot.
    pub fn stage_reserved(
        root: &Path, operation: ArchiveOperation, length: u64,
        mut check_live: impl FnMut() -> io::Result<()>,
        copy: impl FnOnce(&mut dyn Write, &mut dyn FnMut() -> io::Result<()>) -> io::Result<()>,
    ) -> io::Result<Self> {
        if !(1..=MAX_WIRE_BYTES).contains(&length) { return Err(denied()); }
        check_live()?;
        let directory = lock_custody(root)?;
        let entries = std::fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))?;
        let mut terminals = 0;
        for (index, entry) in entries.enumerate() {
            if index >= 65536 { return Err(io::ErrorKind::WouldBlock.into()); }
            let name = entry?.file_name();
            let Some(suffix) = name.as_encoded_bytes().strip_prefix(b".backup-archive-") else { continue };
            let Some(id) = suffix.strip_suffix(b".terminal") else { return Err(io::ErrorKind::WouldBlock.into()) };
            terminals += 1;
            if id.len() != 32 || !id.iter().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b)) || terminals >= 128 {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let terminal = open_leaf(&directory, name.to_str().ok_or_else(denied)?)?;
            if !(1..=TERMINAL_LIMIT).contains(&check(&terminal, 0o600, false)?.len()) {
                return Err(io::ErrorKind::WouldBlock.into());
            }
        }
        let wire = MigrationCopy::create(root, &operation.leaf("pending"))?;
        if file_identity(&directory)? != wire.archive_binding()?.0 { return Err(denied()); }
        directory.sync_all()?;
        drop(directory);
        Self::fill_wire(root, operation, length, wire, check_live, copy)
    }

    /// Put the exact complete wire into custody while a typed decoder
    /// verifies the source. Returning the pending wire conveys no authority
    /// to admit it to maintenance or publish a terminal receipt.
    pub fn stage_verified(
        root: &Path, operation: ArchiveOperation, length: u64,
        mut check_live: impl FnMut() -> io::Result<()>,
        copy: impl FnOnce(&mut dyn Write, &mut dyn FnMut() -> io::Result<()>) -> io::Result<()>,
    ) -> io::Result<Self> {
        if !(1..=MAX_WIRE_BYTES).contains(&length) { return Err(denied()); }
        check_live()?;
        let wire = MigrationCopy::create(root, &operation.leaf("pending"))?;
        Self::fill_wire(root, operation, length, wire, check_live, copy)
    }

    fn fill_wire(
        root: &Path, operation: ArchiveOperation, length: u64, mut wire: MigrationCopy,
        mut check_live: impl FnMut() -> io::Result<()>,
        copy: impl FnOnce(&mut dyn Write, &mut dyn FnMut() -> io::Result<()>) -> io::Result<()>,
    ) -> io::Result<Self> {
        check_live()?;
        wire.reserve_archive_bytes(length)?;
        let mut digest = Sha256::new();
        wire.fill_archive(|out| {
            let mut bounded = BoundedArchiveWriter { out, digest: &mut digest, remaining: length };
            copy(&mut bounded, &mut check_live)?;
            if bounded.remaining != 0 { return Err(denied()); }
            Ok(())
        })?;
        check_live()?;
        let (root_identity, file) = wire.archive_binding()?;
        Ok(Self {
            root: root.to_owned(), wire,
            intent: Intent { operation, root: root_identity, file, bytes: length, sha256: digest.finalize().into() },
        })
    }

    /// Compare the authenticated receiver's ticket to this exact staged file.
    pub fn matches_ticket(&self, root: &Path, operation: ArchiveOperation, digest: [u8; 32]) -> bool {
        self.root == root && self.intent.operation == operation && self.intent.sha256 == digest
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
    pub fn publish(self, check_live: impl FnMut() -> io::Result<()>) -> io::Result<ReadyBackupArchive> {
        self.publish_observed(check_live, || ())
    }

    pub fn publish_claim(self, check_live: impl FnMut() -> io::Result<()>) -> Result<ReadyBackupArchive, ArchivePublishError> {
        let root = self.root.clone();
        let handoff = self.handoff();
        let mut renamed = false;
        self.publish_observed(check_live, || renamed = true).map_err(|error| {
            if renamed { return ArchivePublishError::AfterRename(error); }
            // Only the exact marker of this consumed ticket can be removed.
            let _ = handoff.discard_unpublished_intent(&root);
            ArchivePublishError::BeforeRename(error)
        })
    }

    /// The observer runs immediately after the successful no-replace wire rename.
    fn publish_observed(self, mut check_live: impl FnMut() -> io::Result<()>, renamed: impl FnOnce()) -> io::Result<ReadyBackupArchive> {
        let _directory = lock_custody(&self.root)?;
        check_live()?;
        let handoff = self.handoff();
        self.persist_intent()?;
        check_live()?;
        self.wire.publish_observed(&handoff.operation.leaf("archive"), renamed)?;
        let ready = ReadyBackupArchive::open(&self.root, handoff)?;
        check_live()?;
        Ok(ready)
    }
}

#[derive(Debug)]
pub enum ArchivePublishError {
    BeforeRename(io::Error),
    AfterRename(io::Error),
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

    pub fn require_archive_digest(self, expected: [u8; 32]) -> io::Result<Self> {
        if self.intent.sha256 != expected { return Err(denied()); }
        Ok(self)
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
    fn discard_unpublished_intent(self, root: &Path) -> io::Result<()> {
        let parent = open_base(root)?;
        let (_, record) = read_intent(&parent, self)?;
        remove_owned(&parent, &self.operation.leaf("intent"), record, Some(INTENT_BYTES as u64))?;
        parent.sync_all()
    }

    /// Publish an immutable, bounded record while exact retained custody is present.
    /// A retry after an uncertain rename must supply the identical record.
    pub fn publish_terminal(self, root: &Path, archive: [u8; 32], bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() || bytes.len() as u64 > TERMINAL_LIMIT { return Err(denied()); }
        let directory = lock_custody(root)?;
        let (intent, _) = read_intent(&directory, self)?;
        ReadyBackupArchive::open(root, self)?.require_archive_digest(archive)?;
        if let Some(previous) = read_backup_terminal(root, self.operation)? {
            if previous != bytes { return Err(denied()); }
            return directory.sync_all();
        }
        let mut pending = MigrationCopy::create(root, &self.operation.leaf("terminal-pending"))?;
        if pending.archive_binding()?.0 != intent.root { return Err(denied()); }
        pending.fill_archive(|file| file.write_all(bytes))?;
        pending.publish(&self.operation.leaf("terminal"))?;
        if read_backup_terminal(root, self.operation)?.as_deref() != Some(bytes) { return Err(denied()); }
        Ok(())
    }

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

/// Read the opaque terminal record at a fixed operation-owned name. The
/// caller must validate its schema and bind it to the freshly authorized
/// owner and selected library before projecting any state. Custody checks
/// here do not grant authority to publish, restore, or reinterpret a receipt.
pub fn read_backup_terminal(root: &Path, operation: ArchiveOperation) -> io::Result<Option<Vec<u8>>> {
    let parent = open_base(root)?;
    let leaf = operation.leaf("terminal");
    let mut file = match open_leaf(&parent, &leaf) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = check(&file, 0o600, false)?;
    let length = metadata.len();
    if !(1..=TERMINAL_LIMIT).contains(&length) {
        return Err(denied());
    }
    let mut result = Vec::with_capacity(length as usize);
    (&mut file).take(TERMINAL_LIMIT + 1).read_to_end(&mut result)?;
    let name = CString::new(leaf).map_err(|_| denied())?;
    if result.len() as u64 != length
        || check(&file, 0o600, false)?.len() != length
        || !Identity::from_metadata(&metadata).still_names(parent.as_raw_fd(), &name)
        || file_identity(&open_base(root)?)? != file_identity(&parent)?
    {
        return Err(denied());
    }
    Ok(Some(result))
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

fn lock_custody(root: &Path) -> io::Result<File> {
    let directory = open_base(root)?;
    let flags = libc::LOCK_EX | libc::LOCK_NB;
    // SAFETY: the descriptor belongs to this live directory handle.
    if unsafe { libc::flock(directory.as_raw_fd(), flags) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(directory)
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
