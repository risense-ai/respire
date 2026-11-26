//! Durable completion for a released native archive restore.
use super::*;
use crate::net_rpc::Health;
use respire_app::snapshot::{operation_status, SnapshotOperation};
use respire_spawn::owner_credential::{
    backup_grant::{Scope, VerifiedGrant}, read_backup_terminal, ArchiveOperation, ArchiveTarget,
    BackupArchiveHandoff,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Completion {
    version: u8,
    operation: [u8; 16],
    fingerprint: String,
    snapshot: String,
    intent: [u8; 32],
    scope_account: [u8; 32],
    profile: [u8; 32],
    target: [u8; 32],
    restored_file: [u8; 32],
    space: String,
    admitted: [u8; 16],
    restarted: [u8; 16],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    release_proof: Option<(String, String)>,
}

pub struct Admitted {
    scope: Scope,
    fingerprint: String,
    custody: BackupArchiveHandoff,
}

fn hash(bytes: &[u8]) -> [u8; 32] { Sha256::digest(bytes).into() }

pub struct Restart {
    operation: [u8; 16],
    instance: Uuid,
    file: [u8; 32],
    space: String,
}

impl Admitted {
    pub fn observe_parent(&self, original: &Health, fresh: &Health, selected: &ArchiveTarget,
        snapshot: &str) -> Result<Restart> {
        use std::os::unix::fs::MetadataExt;
        let instance = self.restart_instance(original, fresh, selected)?;
        let profile = Path::new(&fresh.data_dir).canonicalize()?;
        ensure!(respire_app::service::database_path(&profile)?.canonicalize()? == selected.path()
            && fresh.snapshot_restore_id.as_deref() == Some(snapshot), "parent restore mismatch");
        let file = selected.identity_digest()?;
        let target = std::fs::metadata(selected.path())?;
        let opened = std::fs::read_dir(format!("/proc/{}/fd", fresh.pid))?.flatten()
            .filter_map(|entry| std::fs::metadata(entry.path()).ok())
            .any(|meta| (meta.dev(), meta.ino()) == (target.dev(), target.ino()));
        ensure!(opened && file == selected.identity_digest()? && self.restart_instance(original, fresh, selected)? == instance,
            "fresh Runtime did not retain the parent file");
        Ok(Restart { operation: self.scope.operation, instance, file, space: "main".into() })
    }

    fn restart_instance(&self, original: &Health, fresh: &Health, selected: &ArchiveTarget) -> Result<Uuid> {
        let instance: Uuid = fresh.instance_id.parse()?;
        let admitted = Uuid::from_bytes(self.scope.instance);
        crate::net_rpc::request_scope(fresh)?;
        ensure!(admitted != instance && original.instance_id == admitted.to_string()
            && original.generation == self.scope.generation && original.account_id == fresh.account_id
            && fresh.account_id == hex::encode(self.scope.account) && fresh.store_ready
            && original.installation_id == fresh.installation_id && original.image_sha256 == fresh.image_sha256
            && self.scope.profile == hash(Path::new(&fresh.data_dir).canonicalize()?.as_os_str().as_encoded_bytes())
            && self.scope.target == hash(selected.path().as_os_str().as_encoded_bytes()),
            "restart scope mismatch");
        let current = || -> Result<bool> { Ok(crate::net_rpc::instance_is_running(fresh)?
            && crate::net_rpc::pid_listening_on(crate::net_rpc::rpc_port()) == Some(fresh.pid)) };
        ensure!(current()?, "fresh Runtime identity changed");
        Ok(instance)
    }

    pub(super) fn observe_selected(&self, original: &Health, fresh: &Health, selected: &ArchiveTarget,
        request: &super::selected_receipt::Request, witness: &super::selected_receipt::Witness) -> Result<Restart> {
        let instance = self.restart_instance(original, fresh, selected)?;
        let file = selected.identity_digest()?;
        ensure!(request.operation == self.scope.operation && request.instance == *instance.as_bytes(),
            "different backup operation or Runtime");
        let owner = respire_spawn::owner_credential::read_owner_credential(&crate::rpc_worker::runtime_dir(), &fresh.instance_id)?;
        witness.verify(request, &owner, fresh.generation, self.scope.account, self.scope.profile, self.scope.target, file)?;
        Ok(Restart { operation: self.scope.operation, instance, file, space: request.space.clone() })
    }

    pub fn retain(grant: &VerifiedGrant) -> Result<Self> {
        grant.check_fresh()?;
        let scope = *grant.scope();
        let custody = BackupArchiveHandoff::from_approved_intent(
            ArchiveOperation::from_v4_bytes(scope.operation)?, grant.intent());
        Ok(Self { scope, fingerprint: grant.fingerprint(), custody })
    }

    /// Called under the selected library lock, after verifying the restarted
    /// serving process and committing the original restore journal.
    pub fn publish(&self, profile: &Path, account: &str, selected: &ArchiveTarget,
        space: &str, restart: &Restart, snapshot: &str,
        ownership: &respire_app::lock::LibraryLock, keys: &respire_app::SessionKeys, fence: &[u8]) -> Result<()> {
        let id = Uuid::from_bytes(self.scope.operation);
        let admitted = Uuid::from_bytes(self.scope.instance);
        let restarted = restart.instance;
        ensure!(restart.space == space && restart.operation == self.scope.operation && restart.file == selected.identity_digest()?
            && self.scope.account == hash(account.as_bytes())
            && self.scope.profile == hash(profile.as_os_str().as_encoded_bytes()), "restart scope mismatch");
        selected.verify()?;
        let root = selected.path().parent().ok_or_else(|| anyhow::anyhow!("missing library root"))?;
        ensure!(ownership.protects(root)? && self.scope.target == hash(selected.path().as_os_str().as_encoded_bytes()),
            "restore target mismatch");
        let operation = SnapshotOperation { operation_id: id.to_string(), request_fingerprint: self.fingerprint.clone() };
        let terminal = operation_status(root, account, &operation)?.ok_or_else(|| anyhow::anyhow!("missing journal"))?;
        ensure!(terminal.phase == "verified" && terminal.receipt.operation.as_ref() == Some(&operation)
            && terminal.receipt.restored == snapshot, "restore release mismatch");
        let mut record = Completion { version: 3, operation: *id.as_bytes(), fingerprint: self.fingerprint.clone(),
            snapshot: snapshot.into(), intent: self.custody.intent_sha256(), scope_account: self.scope.account,
            profile: self.scope.profile, target: self.scope.target, restored_file: selected.identity_digest()?,
            space: space.into(), admitted: *admitted.as_bytes(), restarted: *restarted.as_bytes(), release_proof: None };
        let key = respire_app::memory::crypto::derive_subkey(&keys.urk, b"respire/backup/release-marker/v1")?;
        record.release_proof = Some(respire_app::memory::crypto::encrypt_bytes(&key,
            &serde_json::to_vec(&(hash(&serde_json::to_vec(&record)?), hash(fence)))?)?);
        self.custody.publish_terminal(profile, self.scope.archive, &serde_json::to_vec(&record)?)?;
        Ok(())
    }

    pub fn resume_release(root: &Path, account: &str, keys: &respire_app::SessionKeys, id: Uuid,
        lock: &respire_app::lock::LibraryLock, fence: Option<&[u8]>) -> Result<()> {
        ensure!(lock.protects(root)?, "missing library ownership");
        let op = ArchiveOperation::from_v4_bytes(*id.as_bytes())?;
        let bytes = read_backup_terminal(root, op)?.ok_or_else(|| anyhow::anyhow!("missing release marker"))?;
        let mut record: Completion = serde_json::from_slice(&bytes)?;
        let (nonce, cipher) = record.release_proof.take().ok_or_else(|| anyhow::anyhow!("unverified marker"))?;
        let key = respire_app::memory::crypto::derive_subkey(&keys.urk, b"respire/backup/release-marker/v1")?;
        let (body, reservation): ([u8;32],[u8;32]) = serde_json::from_slice(&respire_app::memory::crypto::decrypt_bytes(&key,&cipher,&nonce)?)?;
        let pin = ArchiveTarget::pin(respire_app::service::database_path(root)?.canonicalize()?)?;
        ensure!(record.version == 3 && body == hash(&serde_json::to_vec(&record)?) && fence.is_none_or(|v| hash(v)==reservation)
            && record.operation == *id.as_bytes() && record.profile == hash(root.as_os_str().as_encoded_bytes())
            && record.scope_account == hash(account.as_bytes()) && record.target == hash(pin.path().as_os_str().as_encoded_bytes())
            && record.restored_file == pin.identity_digest()?, "different release marker");
        let operation = SnapshotOperation { operation_id:id.to_string(), request_fingerprint:record.fingerprint };
        let status = operation_status(root,account,&operation)?.ok_or_else(|| anyhow::anyhow!("missing journal"))?;
        ensure!(status.receipt.restored == record.snapshot && matches!(status.phase.as_str(),"verified"|"released"), "different restore");
        if status.cleanup_pending || status.phase == "verified" {
            ensure!(fence.is_some(), "missing restart reservation");
            respire_app::snapshot::release_restore(root,&status.receipt)?;
        }
        respire_app::snapshot::ensure_no_incomplete_restore(root)?;
        let custody = BackupArchiveHandoff::from_approved_intent(op,record.intent);
        if let Err(e) = custody.discard(root) {
            let prefix = format!(".backup-archive-{}.",id.simple());
            ensure!(e.kind()==std::io::ErrorKind::NotFound && ["intent","archive","pending"].iter()
                .all(|suffix| matches!(root.join(format!("{prefix}{suffix}")).symlink_metadata(), Err(e) if e.kind()==std::io::ErrorKind::NotFound)), "custody cleanup uncertain");
        }
        Ok(())
    }
}

pub(in crate::runtime_http::v1) async fn lookup(scope: RestScope, id: Uuid) -> V1Result<Option<serde_json::Value>> {
    let Ok(operation) = ArchiveOperation::from_v4_bytes(*id.as_bytes()) else { return Ok(None) };
    let live = scope.clone();
    let raw = super::super::owner_control::bounded_blocking(move || {
        transport::with_scope(&live, || Ok(()))?;
        let result = read_backup_terminal(&live.canonical_profile, operation);
        transport::with_scope(&live, || result.map_err(|_| transport::unavailable()))
    }).await?;
    let Some(raw) = raw else { return Ok(None) };
    let record: Completion = serde_json::from_slice(&raw).map_err(|_| transport::unavailable())?;
    if !matches!(record.version,2|3) || record.operation != *id.as_bytes() || record.admitted == record.restarted
        || record.scope_account != hash(scope.account.as_bytes())
        || record.profile != hash(scope.canonical_profile.as_os_str().as_encoded_bytes()) { return Ok(None); }
    let verified = rest_dispatch::submit(scope.clone(), Access::Read, move |ctx| {
        reads::with_memory_store(ctx, Some(record.space), |store, _| {
            let pin = store.pin_backup_target().map_err(|_| transport::unavailable())?;
            let op = SnapshotOperation { operation_id:id.to_string(),request_fingerprint:record.fingerprint };
            let released = operation_status(pin.path().parent().ok_or_else(transport::unavailable)?, &ctx.scope.account, &op)
                .map_err(|_| transport::unavailable())?.is_some_and(|s| s.phase=="released" && !s.cleanup_pending && s.receipt.restored==record.snapshot);
            Ok(released && record.target == hash(pin.path().as_os_str().as_encoded_bytes())
                && pin.identity_digest().map_err(|_| transport::unavailable())? == record.restored_file)
        })
    }).await?;
    transport::with_scope(&scope, || Ok(verified.then(|| serde_json::json!({
        "operation_id": id.to_string(), "kind":"backup_import", "state":"completed", "progress_percent":100
    }))))
}
