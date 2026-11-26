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

#[test]
fn large_sqlite_import_uses_exact_owner_stage_and_cleans_up_on_rejection() -> Result<()> {
    use crate::snapshot::backup_archive::BackupArchiveHeader;
    use rusqlite::{Connection, OpenFlags};

    let root = tempfile::tempdir()?;
    let profile = root.path().canonicalize()?;
    let store = detached_fixture(&profile)?;
    let database = profile.join("rsrs.db");
    let live = Connection::open(&database)?;
    live.execute_batch(
        "CREATE TABLE import_stage_fixture(payload BLOB NOT NULL);
         INSERT INTO import_stage_fixture VALUES (zeroblob(33554432));",
    )?;
    let keys = SessionKeys::from_urk([7; 32])?;
    let authority = RuntimeLibrary(&keys, "alice", &profile);
    let draft = BackupExportDraft::new(store.prepare_backup_export(&profile)?.copy(|| Ok(()))?)?;
    let proof = authority.prove_backup_snapshot(&draft)?;
    let export = authority.authenticate_backup_export(draft.prepare(proof, || Ok(()))?)?;
    let mut bytes = Vec::new();
    export.write_verified(&mut bytes, || Ok(()))?;
    let no_stage = || -> Result<bool> {
        for entry in fs::read_dir(&profile)? {
            if entry?.file_name().to_string_lossy().starts_with(".backup-import-") {
                return Ok(false);
            }
        }
        Ok(true)
    };
    let mut body = bytes.as_slice();
    let authenticated = authority.authenticate_backup_import_header(BackupArchiveHeader::read(&mut body)?)?;
    assert!(body.len() > 32 * 1024 * 1024);
    let staged = authenticated.copy_into_owner_stage(body, &profile, &database, || Ok(()))?;
    assert_eq!(staged.manifest().bytes, body.len() as u64);
    let checked_path = staged.checked_path()?.to_path_buf();
    let staged_db = Connection::open_with_flags(
        &checked_path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    assert_eq!(
        staged_db.query_row("SELECT length(payload) FROM import_stage_fixture", [], |row| row.get::<_, u64>(0))?,
        32 * 1024 * 1024,
    );
    drop(staged_db);
    drop(staged);
    assert!(no_stage()?);

    let mut recover_header = bytes.as_slice();
    let authenticated = authority.authenticate_backup_import_header(BackupArchiveHeader::read(&mut recover_header)?)?;
    assert!(authenticated.copy_into_owner_stage(&recover_header[..65_537], &profile, &database, || Ok(())).is_err());
    assert!(no_stage()?);

    let mut body = bytes.as_slice();
    let authenticated = authority.authenticate_backup_import_header(BackupArchiveHeader::read(&mut body)?)?;
    let mut checks = 0;
    assert!(authenticated.copy_into_owner_stage(body, &profile, &database, || {
        checks += 1;
        anyhow::ensure!(checks < 3, "fixture scope revoked after partial write");
        Ok(())
    }).is_err());
    assert_eq!(checks, 3);
    assert!(no_stage()?);

    let other = profile.join("other.db");
    fs::File::create(&other)?;
    let mut body = bytes.as_slice();
    let authenticated = authority.authenticate_backup_import_header(BackupArchiveHeader::read(&mut body)?)?;
    assert!(authenticated.copy_into_owner_stage(body, &profile, &other, || Ok(())).is_err());
    assert!(no_stage()?);
    assert_eq!(live.query_row("SELECT length(payload) FROM import_stage_fixture", [], |row| row.get::<_, u64>(0))?, 32 * 1024 * 1024);
    Ok(())
}
