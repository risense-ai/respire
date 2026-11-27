#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreReceipt {
    pub restored: String,
    pub before_restore: String,
    pub sync_quarantined: bool,
    #[serde(skip_serializing)]
    rollback_file: String,
    #[serde(skip_serializing)]
    rollback_sha256: String,
    #[serde(skip_serializing)]
    account_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<SnapshotOperation>,
    #[serde(default, skip_serializing)]
    legacy_journal_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotOperation {
    pub operation_id: String,
    pub request_fingerprint: String,
}

#[derive(Debug)]
pub struct OperationConflict;

impl std::fmt::Display for OperationConflict {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("snapshot operation idempotency conflict")
    }
}

impl std::error::Error for OperationConflict {}

#[derive(Debug, Clone)]
pub struct RestoreStatus {
    pub phase: String,
    pub receipt: RestoreReceipt,
    pub cleanup_pending: bool,
}

fn terminal_phase(phase: &str) -> bool {
    matches!(phase, "released" | "rolled_back")
}

#[derive(Serialize, Deserialize)]
struct RecoveryJournal {
    schema_version: u32,
    phase: String,
    receipt: RecoveryReceipt,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    history: Vec<TerminalReceipt>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TerminalReceipt {
    phase: String,
    restored: String,
    before_restore: String,
    sync_quarantined: bool,
    account_fingerprint: String,
    operation: SnapshotOperation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    legacy_journal_sha256: Option<String>,
}

// Never evict: one current operation plus its retained terminal predecessors.
const MAX_RECORDED_OPERATIONS: usize = 64;

fn next_history(previous: RecoveryJournal) -> Result<Vec<TerminalReceipt>> {
    anyhow::ensure!(terminal_phase(&previous.phase), "an earlier snapshot recovery is pending");
    let mut history = previous.history;
    if let Some(operation) = previous.receipt.operation {
        anyhow::ensure!(history.len() + 1 < MAX_RECORDED_OPERATIONS, "snapshot operation history is full; new restores are blocked");
        history.push(TerminalReceipt {
            phase: previous.phase,
            restored: previous.receipt.restored,
            before_restore: previous.receipt.before_restore,
            sync_quarantined: true,
            account_fingerprint: previous.receipt.account_fingerprint,
            operation,
            legacy_journal_sha256: previous.receipt.legacy_journal_sha256,
        });
    }
    Ok(history)
}
#[derive(PartialEq, Eq, Serialize, Deserialize)]
struct RecoveryReceipt {
    restored: String,
    before_restore: String,
    rollback_file: String,
    rollback_sha256: String,
    account_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    operation: Option<SnapshotOperation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    legacy_journal_sha256: Option<String>,
}
impl From<&RestoreReceipt> for RecoveryReceipt {
    fn from(value: &RestoreReceipt) -> Self {
        Self {
            restored: value.restored.clone(),
            before_restore: value.before_restore.clone(),
            rollback_file: value.rollback_file.clone(),
            rollback_sha256: value.rollback_sha256.clone(),
            account_fingerprint: value.account_fingerprint.clone(),
            operation: value.operation.clone(),
            legacy_journal_sha256: value.legacy_journal_sha256.clone(),
        }
    }
}
impl From<RecoveryReceipt> for RestoreReceipt {
    fn from(value: RecoveryReceipt) -> Self {
        Self {
            restored: value.restored,
            before_restore: value.before_restore,
            sync_quarantined: true,
            rollback_file: value.rollback_file,
            rollback_sha256: value.rollback_sha256,
            account_fingerprint: value.account_fingerprint,
            operation: value.operation,
            legacy_journal_sha256: value.legacy_journal_sha256,
        }
    }
}
const RECOVERY_FILE: &str = ".snapshot-recovery.json";
const MAINTENANCE_FILE: &str = ".snapshot-maintenance.json";

fn reject_live_symlink(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    anyhow::ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "live database symbolic links and non-regular files are not supported for snapshots"
    );
    Ok(())
}
fn write_journal(root: &Path, receipt: &RestoreReceipt, phase: &str) -> Result<()> {
    write_record(root, receipt, phase, RECOVERY_FILE)
}
fn write_record(root: &Path, receipt: &RestoreReceipt, phase: &str, name: &str) -> Result<()> {
    let history = match read_journal(root)? {
        Some(previous) if previous.receipt.rollback_file != receipt.rollback_file => next_history(previous)?,
        Some(previous) => previous.history,
        None => Vec::new(),
    };
    let bytes = serde_json::to_vec(&RecoveryJournal {
        schema_version: 1,
        phase: phase.to_owned(),
        receipt: receipt.into(),
        // The gate retains its existing role. History lives in RECOVERY_FILE;
        // read_journal merges the old terminal record during gate publication.
        history: if name == RECOVERY_FILE { history } else { Vec::new() },
    })?;
    anyhow::ensure!(bytes.len() < 65536, "snapshot recovery journal capacity exceeded");
    let temporary = root.join(format!(
        ".snapshot-journal-{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let mut file = private_new(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    respire_spawn::durable::replace_file(&temporary, &root.join(name))?;
    Ok(())
}
fn read_journal(root: &Path) -> Result<Option<RecoveryJournal>> {
    let recovery = read_record(root, RECOVERY_FILE)?.map(|(journal, _)| journal);
    let maintenance = read_record(root, MAINTENANCE_FILE)?.map(|(journal, _)| journal);
    resolve_journal(recovery, maintenance)
}

fn resolve_journal(
    recovery: Option<RecoveryJournal>,
    maintenance: Option<RecoveryJournal>,
) -> Result<Option<RecoveryJournal>> {
    match (recovery, maintenance) {
        (Some(recovery), Some(mut maintenance)) => {
            if recovery.receipt.rollback_file != maintenance.receipt.rollback_file {
                anyhow::ensure!(
                    terminal_phase(&recovery.phase) && maintenance.phase == "prepared",
                    "snapshot recovery and maintenance journals disagree"
                );
                // A new operation published its gate before replacing the last
                // terminal receipt. No live replacement can precede this gate.
                maintenance.history = next_history(recovery)?;
                return Ok(Some(maintenance));
            }
            // Binding legacy recovery publishes only RECOVERY_FILE. Its original
            // prepared gate remains unbound until terminal cleanup removes it.
            let legacy_binding = recovery.receipt.legacy_journal_sha256.is_some()
                && recovery.receipt.operation.is_some()
                && maintenance.phase == "prepared"
                && maintenance.receipt.operation.is_none()
                && maintenance.receipt.legacy_journal_sha256.is_none()
                && recovery.history.is_empty() && maintenance.history.is_empty();
            if legacy_binding {
                maintenance.receipt.operation = recovery.receipt.operation.clone();
                maintenance.receipt.legacy_journal_sha256 = recovery.receipt.legacy_journal_sha256.clone();
                anyhow::ensure!(recovery.receipt == maintenance.receipt,
                    "legacy recovery binding changed its original receipt");
            }
            anyhow::ensure!(
                recovery.receipt.rollback_sha256 == maintenance.receipt.rollback_sha256
                    && recovery.receipt.operation == maintenance.receipt.operation
                    && recovery.receipt.legacy_journal_sha256 == maintenance.receipt.legacy_journal_sha256,
                "snapshot recovery and maintenance journals disagree"
            );
            Ok(Some(recovery))
        }
        (Some(journal), None) | (None, Some(journal)) => Ok(Some(journal)),
        (None, None) => Ok(None),
    }
}

fn read_record(root: &Path, name: &str) -> Result<Option<(RecoveryJournal, Vec<u8>)>> {
    let path = root.join(name);
    let meta = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(
        meta.is_file() && !meta.file_type().is_symlink() && meta.len() < 65536,
        "invalid snapshot recovery journal file"
    );
    let bytes = fs::read(path)?;
    anyhow::ensure!(bytes.len() < 65536, "snapshot recovery journal exceeds size limit");
    let journal: RecoveryJournal = serde_json::from_slice(&bytes)?;
    anyhow::ensure!(
        journal.schema_version == 1
            && matches!(
                journal.phase.as_str(),
                "prepared" | "installed" | "verified" | "released" | "rolled_back"
            ),
        "unsupported snapshot recovery journal"
    );
    let name = &journal.receipt.rollback_file;
    let id = name
        .strip_prefix(".snapshot-rollback-")
        .and_then(|name| name.strip_suffix(".db"))
        .context("invalid snapshot rollback filename")?;
    anyhow::ensure!(valid_id(id), "invalid snapshot rollback identity");
    if let Some(digest) = journal.receipt.legacy_journal_sha256.as_deref() {
        anyhow::ensure!(journal.receipt.operation.is_some() && valid_digest(digest),
            "invalid legacy snapshot recovery binding");
    }
    anyhow::ensure!(
        !terminal_phase(&journal.phase) || journal.receipt.operation.is_some() || !journal.history.is_empty(),
        "terminal snapshot journal is missing its operation identity"
    );
    anyhow::ensure!(journal.history.len() < MAX_RECORDED_OPERATIONS, "snapshot operation history capacity exceeded");
    for (index, entry) in journal.history.iter().enumerate() {
        anyhow::ensure!(entry.legacy_journal_sha256.as_deref().is_none_or(valid_digest),
            "invalid historical legacy snapshot recovery binding");
        anyhow::ensure!(
            terminal_phase(&entry.phase)
                && valid_id(&entry.restored)
                && valid_id(&entry.before_restore)
                && !journal.history[..index].iter().any(|earlier| earlier.operation.operation_id == entry.operation.operation_id)
                && journal.receipt.operation.as_ref().is_none_or(|current| current.operation_id != entry.operation.operation_id),
            "invalid snapshot operation history"
        );
    }
    Ok(Some((journal, bytes)))
}

