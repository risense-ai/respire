//! Detached stages cannot be serialized. Only a verified RuntimeLibrary can
//! mint the two small proofs; it never lends Vault keys to the copying worker.
use super::{Manifest, MAX_DATABASE};
use crate::transport::local::BackupExportCopy;
use anyhow::{ensure, Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use sha2::{Digest, Sha256};
use std::{fs::{self, File}, io::{Cursor, Read, Write}, path::Path};

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

/// The private wire header is bound to this exact detached manifest.
pub struct SealedBackupExportHeader {
    manifest_digest: [u8; 32],
    bytes: Vec<u8>,
}

impl SealedBackupExportHeader {
    pub(crate) fn for_manifest(manifest: &Manifest, bytes: Vec<u8>) -> Result<Self> {
        Ok(Self { manifest_digest: Sha256::digest(serde_json::to_vec(manifest)?).into(), bytes })
    }

    fn for_ready(self, ready: &BackupExportReady) -> Result<Vec<u8>> {
        let expected: [u8; 32] = Sha256::digest(serde_json::to_vec(&ready.manifest)?).into();
        ensure!(self.manifest_digest == expected, "sealed header is not for this detached backup");
        Ok(self.bytes)
    }
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
        let canonical = copy.source_database().to_str().context("backup source must be UTF-8")?;
        let source = format!("{:x}", Sha256::digest(canonical.as_bytes()));
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
            install_snapshot_key_check(&db, &proof.nonce, &proof.ciphertext)?;
            let mode: String = db.query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))?;
            ensure!(mode.eq_ignore_ascii_case("delete"), "backup is not standalone");
            db.close().map_err(|(_, error)| error)?;
        }
        check()?;
        let manifest = describe_detached_snapshot(&path, self.id, "", self.source, proof.account)?;
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

impl BackupExportDraft {
    pub(crate) fn prove(
        &self,
        keys: &crate::SessionKeys,
        account: &str,
    ) -> Result<BackupSnapshotProof> {
        let account = format!("{:x}", Sha256::digest(account.as_bytes()));
        let (nonce, ciphertext) = keys.encrypt_content(&snapshot_key_challenge(
            &self.id, &self.source, &account,
        ))?;
        Ok(BackupSnapshotProof {
            id: self.id.clone(), source: self.source.clone(), account, nonce, ciphertext,
        })
    }
}

impl BackupExportReady {
    pub(crate) fn verify_lineage(&self, keys: &crate::SessionKeys) -> Result<()> {
        let expected = snapshot_key_challenge(
            &self.manifest.id, &self.manifest.source_library, &self.manifest.account_fingerprint,
        );
        ensure!(
            keys.decrypt_content(&self.key_check.1, &self.key_check.0)? == expected,
            "backup Vault lineage changed"
        );
        Ok(())
    }

    pub(crate) fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Consumes a sealed header supplied by the scoped Vault operation. The
    /// caller must keep only its one matching stage, never client bytes.
    pub fn into_reader(self, header: super::SealedBackupExportHeader) -> Result<BackupExportReader> {
        let bytes = header.for_ready(&self)?;
        let file = self.copy.open_verified_reader(self.manifest.bytes)?;
        ensure!(file.metadata()?.len() == self.manifest.bytes, "backup stage length changed");
        Ok(BackupExportReader {
            file,
            copy: self.copy,
            remaining: self.manifest.bytes,
            manifest: self.manifest,
            header: Cursor::new(bytes),
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

fn snapshot_key_challenge(id: &str, library: &str, account: &str) -> String {
    format!("snapshot-key-check:v1:{id}:{library}:{account}")
}

fn install_snapshot_key_check(copy: &Connection, nonce: &str, ciphertext: &str) -> Result<()> {
    copy.execute_batch("DROP TABLE IF EXISTS snapshot_key_check; CREATE TABLE snapshot_key_check(singleton INTEGER PRIMARY KEY CHECK(singleton=1),nonce TEXT NOT NULL,ciphertext TEXT NOT NULL);")?;
    copy.execute(
        "INSERT INTO snapshot_key_check VALUES(1,?1,?2)",
        rusqlite::params![nonce, ciphertext],
    )?;
    Ok(())
}

fn describe_detached_snapshot(
    database: &Path,
    id: String,
    note: &str,
    source_library: String,
    account_fingerprint: String,
) -> Result<Manifest> {
    let (entries, sqlite_user_version) = super::super::inventory(database)?;
    let database_schema_sha256 = super::super::schema_digest(database)?;
    // Legacy diagnostic backups intentionally retain a damaged witness.
    // Transfer staging must separately refuse such a witness.
    let (witness, witness_problem) = match crate::transport::local::witness_checkpoint_file(database) {
        Ok(report) => (report, None),
        Err(_) => (None, Some("witness checkpoint is unverifiable; diagnostic copy only".to_owned())),
    };
    let copy = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let selection: Option<String> = copy.query_row(
        "SELECT value FROM meta WHERE key=?1",
        [crate::core_sdk::profiles::ACTIVE_SELECTION_KEY],
        |row| row.get(0),
    ).optional()?;
    let model = selection.map(|value| serde_json::from_str::<crate::core_sdk::profiles::Selection>(&value).map(|selection| selection.model))
        .transpose()?.unwrap_or_else(|| "m3".to_owned());
    drop(copy);
    Ok(Manifest {
        schema_version: 1,
        id,
        created_at: chrono::Utc::now().to_rfc3339(),
        note: note.to_owned(),
        database_sha256: super::super::digest_file(database)?,
        bytes: fs::metadata(database)?.len(),
        entries,
        source_library,
        account_fingerprint,
        database_schema_sha256,
        sqlite_user_version,
        sdk_version: crate::core_sdk::CORE_VERSION.to_owned(),
        sdk_abi: crate::core_sdk::ABI_VERSION,
        model,
        witness_head: witness.as_ref().map(|report| report.head.clone()),
        witness_count: witness.as_ref().map(|report| report.count),
        witness_intact: if witness_problem.is_some() { Some(false) } else { witness.as_ref().map(|report| report.intact) },
        witness_problem,
    })
}
