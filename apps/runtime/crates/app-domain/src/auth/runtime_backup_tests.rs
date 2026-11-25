use super::*;
use crate::snapshot::backup_archive::{open_to_stage, BackupExportDraft};
use anyhow::Context;
use std::{fs, path::Path};

fn detached_fixture(profile: &Path) -> Result<LocalStore> {
    let path = profile.join("rsrs.db");
    let store = LocalStore::open(&path)?;
    let live = rusqlite::Connection::open(&path)?;
    live.execute_batch(
        "INSERT INTO meta(key,value) VALUES ('export-fixture','committed-WAL');
         INSERT INTO access_grants(id,label,root_id,token_hash,created_at)
            VALUES ('fixture','fixture','root','unexportable-grant-token','2026-11-25');",
    )?;
    Ok(store)
}

#[test]
fn old_snapshot_policy_and_current_vault_authenticate_detached_native_export() -> Result<()> {
    let root = tempfile::tempdir()?;
    let profile = root.path().canonicalize()?;
    let store = detached_fixture(&profile)?;
    let keys = SessionKeys::from_urk([7; 32])?;
    let authority = RuntimeLibrary(&keys, "alice", &profile);
    let draft = BackupExportDraft::new(store.prepare_backup_export(&profile)?.copy(|| Ok(()))?)?;
    let proof = authority.prove_backup_snapshot(&draft)?;
    let pending = draft.prepare(proof, || Ok(()))?;
    let stream = authority.authenticate_backup_export(pending)?;
    let mut bytes = Vec::new();
    stream.write_verified(&mut bytes, || Ok(()))?;
    assert!(!bytes.windows(b"unexportable-grant-token".len())
        .any(|window| window == b"unexportable-grant-token"));
    let database = profile.join("export-verification.db");
    let manifest = open_to_stage(&keys, "alice", bytes.as_slice(), fs::File::create(&database)?, || Ok(()))?;
    assert_eq!(manifest.schema_version, 1);
    let read = rusqlite::Connection::open(&database)?;
    let value: String = read.query_row("SELECT value FROM meta WHERE key='export-fixture'", [], |row| row.get(0))?;
    assert_eq!(value, "committed-WAL");
    assert_eq!(read.query_row("SELECT count(*) FROM access_grants", [], |row| row.get::<_, u64>(0))?, 0);
    assert!(fs::read_dir(&profile)?.all(|entry| entry.is_ok_and(|entry| !entry.file_name().to_string_lossy().starts_with(".backup-export-"))));
    Ok(())
}

#[test]
fn wrong_vault_cannot_authorize_prepared_bytes_even_for_the_same_account() -> Result<()> {
    let root = tempfile::tempdir()?;
    let profile = root.path().canonicalize()?;
    let store = detached_fixture(&profile)?;
    let keys = SessionKeys::from_urk([7; 32])?;
    let other = SessionKeys::from_urk([6; 32])?;
    let first = RuntimeLibrary(&keys, "alice", &profile);
    let changed = RuntimeLibrary(&other, "alice", &profile);
    let draft = BackupExportDraft::new(store.prepare_backup_export(&profile)?.copy(|| Ok(()))?)?;
    let proof = first.prove_backup_snapshot(&draft)?;
    let pending = draft.prepare(proof, || Ok(()))?;
    let _error = changed.authenticate_backup_export(pending).err().context("changed Vault unexpectedly authorized archive")?;
    assert!(fs::read_dir(&profile)?.all(|entry| entry.is_ok_and(|entry| !entry.file_name().to_string_lossy().starts_with(".backup-export-"))));
    Ok(())
}

#[test]
fn revocation_before_chunk_release_cannot_resume_or_leak_the_chunk() -> Result<()> {
    let root = tempfile::tempdir()?;
    let profile = root.path().canonicalize()?;
    let store = detached_fixture(&profile)?;
    let keys = SessionKeys::from_urk([7; 32])?;
    let authority = RuntimeLibrary(&keys, "alice", &profile);
    let draft = BackupExportDraft::new(store.prepare_backup_export(&profile)?.copy(|| Ok(()))?)?;
    let proof = authority.prove_backup_snapshot(&draft)?;
    let mut stream = authority.authenticate_backup_export(draft.prepare(proof, || Ok(()))?)?;
    let head = stream.next_verified(|| Ok(()))?.context("missing archive header")?;
    assert!(head.starts_with(b"RSRSBKP"));
    let mut checks = 0;
    assert!(stream.next_verified(|| {
        checks += 1;
        anyhow::ensure!(checks == 1, "fixture revoked before DB chunk release");
        Ok(())
    }).is_err());
    assert!(stream.next_verified(|| Ok(()))?.is_none());
    drop(stream);
    assert!(fs::read_dir(&profile)?.all(|entry| entry.is_ok_and(|entry| !entry.file_name().to_string_lossy().starts_with(".backup-export-"))));
    Ok(())
}

#[test]
fn split_import_header_requires_live_lineage_before_copying() -> Result<()> {
    use crate::snapshot::backup_archive::BackupArchiveHeader;

    let root = tempfile::tempdir()?;
    let profile = root.path().canonicalize()?;
    let store = detached_fixture(&profile)?;
    let keys = SessionKeys::from_urk([7; 32])?;
    let changed = SessionKeys::from_urk([8; 32])?;
    let authority = RuntimeLibrary(&keys, "alice", &profile);
    let draft = BackupExportDraft::new(store.prepare_backup_export(&profile)?.copy(|| Ok(()))?)?;
    let proof = authority.prove_backup_snapshot(&draft)?;
    let export = authority.authenticate_backup_export(draft.prepare(proof, || Ok(()))?)?;
    let mut bytes = Vec::new();
    export.write_verified(&mut bytes, || Ok(()))?;
    let denied = RuntimeLibrary(&changed, "alice", &profile);
    assert!(denied.authenticate_backup_import_header(BackupArchiveHeader::read(bytes.as_slice())?).is_err());

    let mut body = bytes.as_slice();
    let authenticated = authority.authenticate_backup_import_header(BackupArchiveHeader::read(&mut body)?)?;
    let mut output = Vec::new();
    let attempt = authenticated.copy_to_stage(body, &mut output, || {
        anyhow::bail!("fixture scope expired before write")
    });
    assert!(attempt.is_err());
    assert!(output.is_empty());

    let mut body = bytes.as_slice();
    let authenticated = authority.authenticate_backup_import_header(BackupArchiveHeader::read(&mut body)?)?;
    let manifest = authenticated.copy_to_stage(body, &mut output, || Ok(()))?;
    assert_eq!(output.as_slice(), body);
    assert_eq!(manifest.bytes, body.len() as u64);
    Ok(())
}
