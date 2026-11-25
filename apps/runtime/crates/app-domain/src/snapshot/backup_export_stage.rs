//! Detached stages cannot be serialized. Only a verified RuntimeLibrary can
//! mint the two small proofs; it never lends Vault keys to the copying worker.
use super::{authenticated_header, Manifest, MAX_DATABASE};
use crate::transport::local::BackupExportCopy;
use anyhow::{ensure, Result};
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};
use std::{fs::File, io::{Cursor, Read, Write}};

const STREAM_CHUNK: usize = 64 * 1024;

pub struct BackupExportDraft {
    copy: BackupExportCopy,
    id: String,
    source: String,
}

/// An encrypted historical snapshot challenge, not a Vault key. Opaque and
/// inseparably tied to the selected source, export ID, and current account.
pub struct BackupSnapshotProof {
    id: String,
    source: String,
    account: String,
    nonce: String,
    ciphertext: String,
}

pub struct BackupExportReady {
    copy: BackupExportCopy,
    manifest: Manifest,
    key_check: (String, String),
}

/// Keeps the native staged database alive for the entire response.
pub struct BackupExportReader {
    file: File,
    copy: BackupExportCopy,
    manifest: Manifest,
    header: Cursor<Vec<u8>>,
    remaining: u64,
    digest: Sha256,
    finished: bool,
}

impl BackupExportDraft {
    /// Off-dispatch only. This stage contains a raw copy until stripping is done.
    pub fn new(copy: BackupExportCopy) -> Result<Self> {
        copy.checked_path()?;
        let source = super::super::fingerprint_path(copy.source_database())?;
        Ok(Self { copy, id: uuid::Uuid::new_v4().simple().to_string(), source })
    }

    /// Off-dispatch only. No borrowed RuntimeLibrary or Vault key crosses into
    /// SQLite sanitization, full integrity checks, or database hashing.
    pub fn prepare(
        self,
        proof: BackupSnapshotProof,
        mut check: impl FnMut() -> Result<()>,
    ) -> Result<BackupExportReady> {
        ensure!(self.id == proof.id && self.source == proof.source, "backup proof is not for this staged source");
        check()?;
        let path = self.copy.checked_path()?;
        {
            let mut db = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW)?;
            let mode: String = db.query_row("PRAGMA journal_mode=MEMORY", [], |row| row.get(0))?;
            ensure!(mode.eq_ignore_ascii_case("memory"), "unsafe backup stage journal mode");
            crate::service::sanitize_detached_backup(&mut db)?;
            check()?;
            super::super::install_snapshot_key_check(&db, &proof.nonce, &proof.ciphertext)?;
            let mode: String = db.query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))?;
            ensure!(mode.eq_ignore_ascii_case("delete"), "backup is not standalone");
            db.close().map_err(|(_, error)| error)?;
        }
        check()?;
        let manifest = super::super::describe_detached_snapshot(&path, self.id, "", self.source, proof.account)?;
        ensure!(manifest.bytes <= MAX_DATABASE, "sealed backup exceeds the stage budget");
        ensure!(manifest.witness_problem.is_none() && manifest.witness_intact != Some(false),
            "backup witness checkpoint is unverifiable");
        self.copy.checked_path()?;
        check()?;
        Ok(BackupExportReady {
            copy: self.copy,
            manifest,
            key_check: (proof.nonce, proof.ciphertext),
        })
    }
}

impl crate::auth::RuntimeLibrary<'_> {
    /// Called only after checking the entire captured HTTP scope against the
    /// currently unlocked library, not merely comparing an account string.
    pub fn prove_backup_snapshot(&self, draft: &BackupExportDraft) -> Result<BackupSnapshotProof> {
        let account = super::super::fingerprint_account(self.backup_account());
        let (nonce, ciphertext) = self.seal_backup_challenge(&super::super::snapshot_key_challenge(
            &draft.id, &draft.source, &account,
        ))?;
        Ok(BackupSnapshotProof { id: draft.id.clone(), source: draft.source.clone(), account, nonce, ciphertext })
    }

    pub fn authenticate_backup_export(&self, ready: BackupExportReady) -> Result<BackupExportReader> {
        let account = self.backup_account();
        let expected = super::super::snapshot_key_challenge(
            &ready.manifest.id, &ready.manifest.source_library, &ready.manifest.account_fingerprint,
        );
        ensure!(
            self.open_backup_challenge(&ready.key_check.1, &ready.key_check.0)? == expected,
            "backup Vault lineage changed"
        );
        let header = authenticated_header(
            account, &ready.manifest, |challenge| self.seal_backup_challenge(challenge),
        )?;
        let file = File::open(ready.copy.checked_path()?)?;
        ensure!(file.metadata()?.len() == ready.manifest.bytes, "backup stage length changed");
        Ok(BackupExportReader {
            file,
            copy: ready.copy,
            remaining: ready.manifest.bytes,
            manifest: ready.manifest,
            header: Cursor::new(header),
            digest: Sha256::new(),
            finished: false,
        })
    }
}

impl BackupExportReader {
    /// The transport must apply its exact captured scope before and after
    /// each bounded read. An error taints this reader and it cannot resume.
    pub fn next_verified(
        &mut self,
        mut check: impl FnMut() -> Result<()>,
    ) -> Result<Option<Vec<u8>>> {
        if self.finished {
            return Ok(None);
        }
        let result = self.read_next(&mut check);
        if !matches!(&result, Ok(Some(_))) {
            self.finished = true;
        }
        result
    }

    fn read_next(
        &mut self,
        check: &mut impl FnMut() -> Result<()>,
    ) -> Result<Option<Vec<u8>>> {
        check()?;
        self.copy.checked_path()?;
        let mut bytes = vec![0; STREAM_CHUNK];
        let header = self.header.read(&mut bytes)?;
        if header != 0 {
            bytes.truncate(header);
        } else {
            if self.remaining == 0 {
                return Ok(None);
            }
            bytes.truncate(self.remaining.min(STREAM_CHUNK as u64) as usize);
            self.file.read_exact(&mut bytes)?;
            self.remaining -= bytes.len() as u64;
            self.digest.update(&bytes);
            if self.remaining == 0 {
                let mut trailing = [0];
                ensure!(self.file.read(&mut trailing)? == 0, "trailing bytes in backup stage");
                let observed = std::mem::take(&mut self.digest).finalize();
                ensure!(
                    format!("{observed:x}") == self.manifest.database_sha256,
                    "backup stage digest changed"
                );
            }
        }
        self.copy.checked_path()?;
        check()?;
        Ok(Some(bytes))
    }

    pub fn write_verified(
        mut self,
        mut output: impl Write,
        mut check: impl FnMut() -> Result<()>,
    ) -> Result<()> {
        while let Some(bytes) = self.next_verified(&mut check)? {
            output.write_all(&bytes)?;
        }
        Ok(())
    }
}
