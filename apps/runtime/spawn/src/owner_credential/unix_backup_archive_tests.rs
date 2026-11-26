use super::*;
use super::super::tests::Scratch;
use std::{fs, os::unix::fs::{symlink, PermissionsExt}};

const WIRE: &[u8] = b"owner-only disposable archive wire";

fn operation(tag: u8) -> io::Result<ArchiveOperation> {
    let mut bytes = [tag; 16];
    bytes[6] = 0x40;
    bytes[8] = 0x80;
    ArchiveOperation::from_v4_bytes(bytes)
}

fn stage(root: &Path, op: ArchiveOperation) -> io::Result<PendingBackupArchive> {
    PendingBackupArchive::stage(root, op, WIRE.len() as u64, WIRE, || Ok(()))
}

#[test]
fn intent_and_pending_wire_do_not_establish_readiness() -> io::Result<()> {
    let root = Scratch::model_fixture()?;
    let path = root.path();
    let op = operation(7)?;
    let pending = stage(path, op)?;
    let handoff = pending.handoff();
    assert!(ReadyBackupArchive::open(path, handoff).is_err());
    pending.persist_intent()?;
    assert!(path.join(op.leaf("intent")).is_file());
    assert!(path.join(op.leaf("pending")).is_file());
    assert!(!path.join(op.leaf("archive")).exists());
    assert!(ReadyBackupArchive::open(path, handoff).is_err());
    handoff.discard(path)?;
    drop(pending);
    assert_eq!(fs::read_dir(path)?.count(), 0);
    Ok(())
}

#[test]
fn exact_final_can_reopen_but_cannot_be_clobbered_or_misbound() -> io::Result<()> {
    let root = Scratch::model_fixture()?;
    let foreign = Scratch::model_fixture()?;
    let path = root.path();
    let op = operation(7)?;
    let pending = stage(path, op)?;
    let handoff = pending.handoff();
    drop(pending.publish(|| Ok(()))?);
    let wrong = BackupArchiveHandoff::from_approved_intent(op, [0; 32]);
    let moved = BackupArchiveHandoff::from_approved_intent(operation(8)?, handoff.intent_sha256());
    assert!(ReadyBackupArchive::open(path, wrong).is_err());
    assert!(ReadyBackupArchive::open(path, moved).is_err());
    assert!(ReadyBackupArchive::open(foreign.path(), handoff).is_err());
    for leaf in [op.leaf("intent"), op.leaf("archive")] {
        let target = foreign.path().join(&leaf);
        fs::copy(path.join(&leaf), &target)?;
        fs::set_permissions(target, fs::Permissions::from_mode(0o600))?;
    }
    assert!(ReadyBackupArchive::open(foreign.path(), handoff).is_err(), "copied bytes do not retain root identity");
    assert!(stage(path, op)?.publish(|| Ok(())).is_err(), "custody may not replace an original");
    assert!(!path.join(op.leaf("pending")).exists());
    let wire = ReadyBackupArchive::open(path, handoff)?.consume(|input| {
        let mut wire = Vec::new();
        input.read_to_end(&mut wire)?;
        Ok(wire)
    })?;
    assert_eq!(wire, WIRE);
    assert!(ReadyBackupArchive::open(path, handoff)?.consume(|_| Ok(())).is_err());
    handoff.discard(path)?;
    assert_eq!(fs::read_dir(path)?.count(), 0);
    Ok(())
}

#[test]
fn same_length_changed_payload_and_unrelated_name_are_not_accepted() -> io::Result<()> {
    let root = Scratch::model_fixture()?;
    let path = root.path();
    let op = operation(7)?;
    let pending = stage(path, op)?;
    let handoff = pending.handoff();
    drop(pending.publish(|| Ok(()))?);
    let final_file = path.join(op.leaf("archive"));
    fs::write(&final_file, vec![b'x'; WIRE.len()])?;
    let result = ReadyBackupArchive::open(path, handoff)?.consume(|input| {
        io::copy(input, &mut io::sink())
    });
    assert!(result.is_err(), "digest compares bytes consumed, not just the inode");

    let held = path.join("kept-original");
    fs::rename(&final_file, &held)?;
    fs::write(&final_file, WIRE)?;
    fs::set_permissions(&final_file, fs::Permissions::from_mode(0o600))?;
    assert!(ReadyBackupArchive::open(path, handoff).is_err());
    assert!(handoff.discard(path).is_err());
    assert_eq!(fs::read(&final_file)?, WIRE, "do not clean a distinct replacement");
    fs::remove_file(&final_file)?;
    symlink(&held, &final_file)?;
    assert!(ReadyBackupArchive::open(path, handoff).is_err());
    assert!(handoff.discard(path).is_err());
    assert!(fs::symlink_metadata(final_file)?.is_symlink());
    Ok(())
}

#[test]
fn rejected_length_revocation_and_publication_keep_only_exact_durable_state() -> io::Result<()> {
    let root = Scratch::model_fixture()?;
    let path = root.path();
    let op = operation(7)?;
    for length in [0, MAX_WIRE_BYTES + 1, WIRE.len() as u64 + 1, WIRE.len() as u64 - 1] {
        assert!(PendingBackupArchive::stage(path, op, length, WIRE, || Ok(())).is_err());
    }
    let reject = || Err(io::Error::new(io::ErrorKind::PermissionDenied, "fixture scope retired"));
    assert!(PendingBackupArchive::stage(path, op, WIRE.len() as u64, WIRE, reject).is_err());
    assert_eq!(fs::read_dir(path)?.count(), 0);
    let pending = stage(path, op)?;
    let handoff = pending.handoff();
    let mut calls = 0;
    assert!(pending.publish(|| {
        calls += 1;
        if calls == 2 { return reject(); }
        Ok(())
    }).is_err());
    assert!(!path.join(op.leaf("archive")).exists());
    assert!(ReadyBackupArchive::open(path, handoff).is_err());
    handoff.discard(path)?;
    assert_eq!(fs::read_dir(path)?.count(), 0);
    Ok(())
}

fn publish_terminal_fixture(root: &Path, op: ArchiveOperation, bytes: &[u8]) -> io::Result<()> {
    let mut fixture = MigrationCopy::create(root, &op.leaf("terminal-fixture"))?;
    fixture.fill_archive(|output| output.write_all(bytes))?;
    fixture.publish(&op.leaf("terminal"))
}

#[test]
fn terminal_record_reads_exact_bounded_owner_file_for_that_operation() -> io::Result<()> {
    let root = Scratch::model_fixture()?;
    let path = root.path();
    let op = operation(7)?;
    let other = operation(8)?;
    assert!(read_backup_terminal(path, op)?.is_none());
    let record = vec![b'x'; TERMINAL_LIMIT as usize];
    publish_terminal_fixture(path, op, &record)?;
    assert_eq!(read_backup_terminal(path, op)?.as_deref(), Some(record.as_slice()));
    assert!(read_backup_terminal(path, other)?.is_none());
    fs::set_permissions(path.join(op.leaf("terminal")), fs::Permissions::from_mode(0o640))?;
    assert!(read_backup_terminal(path, op).is_err());
    Ok(())
}

#[test]
fn terminal_reader_denies_oversize_and_does_not_follow_substitute_links() -> io::Result<()> {
    let root = Scratch::model_fixture()?;
    let path = root.path();
    let op = operation(7)?;
    publish_terminal_fixture(path, op, &vec![b'x'; TERMINAL_LIMIT as usize + 1])?;
    assert!(read_backup_terminal(path, op).is_err());
    let name = path.join(op.leaf("terminal"));
    let unrelated = path.join("unrelated");
    fs::rename(&name, &unrelated)?;
    symlink(&unrelated, &name)?;
    assert!(read_backup_terminal(path, op).is_err());
    assert!(fs::symlink_metadata(name)?.is_symlink());
    assert!(unrelated.is_file());
    Ok(())
}
