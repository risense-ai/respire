//! Candidate custody seam for the offline host, not authority to restore.
use super::{AuthenticatedBackupHeader, MAX_WIRE};
use anyhow::{ensure, Result};
use respire_spawn::owner_credential::{MigrationCopy, MigrationSource};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs::File, io::{self, Read, Seek, Write}, path::Path};
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ImportWireIntent {
    operation: String,
    stage_root: [u64; 2],
    file: [u64; 2],
    wire_bytes: u64,
    wire_sha256: [u8; 32],
    source: String,
}

pub(super) struct PendingImportWire {
    file: MigrationCopy,
    intent: ImportWireIntent,
}

struct DigestWriter<'a>(&'a mut dyn Write, &'a mut Sha256);

impl Write for DigestWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = self.0.write(bytes)?;
        self.1.update(&bytes[..count]);
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl PendingImportWire {
    pub(super) fn stage(
        operation: Uuid,
        authenticated: AuthenticatedBackupHeader,
        input: impl Read,
        root: &Path,
        selected_database: &Path,
        mut gate: impl FnMut() -> Result<()>,
    ) -> Result<Self> {
        gate()?;
        let source = super::super::fingerprint_path(selected_database)?;
        ensure!(authenticated.manifest.source_library == source, "wrong backup library");
        let wire_bytes = authenticated.manifest.bytes + authenticated.prefix.len() as u64;
        ensure!((1..=MAX_WIRE).contains(&wire_bytes), "invalid backup wire size");
        ensure!(operation.get_version_num() == 4, "invalid backup operation");
        let owner_root = MigrationSource::open(root, false)?;
        let leaf = format!(".backup-wire-{operation}.pending");
        let mut file = MigrationCopy::create(root, &leaf)?;
        let file_identity = file.source_identity(&leaf)?;
        let mut digest = Sha256::new();
        file.write_exact_stage(wire_bytes, |output| {
            authenticated.copy_envelope_to_stage(input, DigestWriter(output, &mut digest), &mut gate)
                .map(|_| ()).map_err(io::Error::other)
        })?;
        gate()?;
        ensure!(owner_root.file_identity(&leaf)? == file_identity, "backup stage root changed");
        Ok(Self { file, intent: ImportWireIntent {
            operation: operation.to_string(), stage_root: owner_root.identity()?, file: file_identity,
            wire_bytes, wire_sha256: digest.finalize().into(), source,
        } })
    }

    /// A host must persist these exact bytes before final-name publication.
    pub(super) fn intent_bytes(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(&self.intent)?)
    }

    /// Late errors may mean the final name is already visible. Reconcile
    /// from the authenticated intent instead of blindly retrying.
    pub(super) fn publish(self, persist_intent: impl FnOnce(&[u8]) -> Result<()>) -> Result<()> {
        let bytes = self.intent_bytes()?;
        persist_intent(&bytes)?;
        self.file.publish(&self.intent.final_leaf())?;
        Ok(())
    }
}

impl ImportWireIntent {
    fn final_leaf(&self) -> String {
        format!(".backup-wire-{}.archive", self.operation)
    }

    /// The expected intent digest comes from an independent host control
    /// grant, not the archive uploader. No pending file authorizes replay.
    pub(super) fn reopen(
        bytes: &[u8],
        intent_digest: [u8; 32],
        operation: Uuid,
        root: &Path,
        selected_database: &Path,
        mut gate: impl FnMut() -> Result<()>,
    ) -> Result<File> {
        gate()?;
        ensure!(bytes.len() <= 4096 && <[u8; 32]>::from(Sha256::digest(bytes)) == intent_digest,
            "backup intent digest differs");
        let intent: Self = serde_json::from_slice(bytes)?;
        ensure!(intent.operation == operation.to_string() && operation.get_version_num() == 4
            && (1..=MAX_WIRE).contains(&intent.wire_bytes)
            && super::super::fingerprint_path(selected_database)? == intent.source,
            "backup operation or library differs");
        let directory = MigrationSource::open(root, false)?;
        ensure!(directory.identity()? == intent.stage_root, "backup custody root differs");
        let mut file = directory.open_verified_file(&intent.final_leaf(), intent.file, intent.wire_bytes)?;
        let mut digest = Sha256::new();
        let mut count = 0u64;
        let mut block = [0; 64 * 1024];
        loop {
            gate()?;
            let size = file.read(&mut block)?;
            if size == 0 { break; }
            count = count.checked_add(size as u64).filter(|size| *size <= intent.wire_bytes)
                .ok_or_else(|| anyhow::anyhow!("backup wire exceeds expected size"))?;
            digest.update(&block[..size]);
        }
        ensure!(count == intent.wire_bytes && <[u8; 32]>::from(digest.finalize()) == intent.wire_sha256,
            "backup archive differs from durable intent");
        file.rewind()?;
        gate()?;
        Ok(file)
    }
}
