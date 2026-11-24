//! Durable, secret-free continuation for an explicitly requested offline keygen.
//! Only Runtime Start may move a committed original operation toward serving.
use anyhow::{Context, Result};
use respire_commands::output::{Item, ResultEnvelope, Status};
use respire_protocol::{HostAuthIdentity, HostAuthOutcome, HostAuthResult};
use serde::{Deserialize, Serialize};
use std::{io::Read, path::{Path, PathBuf}};
use super::super::{hash, private_write, lifecycle};
use super::launch_authorization::control;
mod launch;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Stage { Prepared, Sent, Committed, Starting, Verified, Finished, HandedOff, RolledBack, RollbackStarting, RollbackFinished, RollbackHandedOff }
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Original {
    schema: u32, operation_id: String, force: bool,
    root: PathBuf, source: PathBuf, profile: PathBuf,
    binary: PathBuf, image_sha256: String,
    token: String, coordinator: String, marker_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_marker_sha256: Option<String>,
    original_identity: HostAuthIdentity, session_sha256: Option<String>, stage: Stage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    material: Option<serde_json::Value>,
    actor: Option<RuntimeIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    final_actor: Option<RuntimeIdentity>,
    was_serving: bool, store_ready: bool,
}
fn path(root: &Path) -> PathBuf { root.join("runtime/autostart/offline-keygen.json") }
/// Generic restart/recovery must not replace this operation's private marker.
/// Even malformed, unreadable or dangling-link evidence blocks generic recovery.
pub(crate) fn reject_generic_recovery(root:&Path) -> Result<()> {
    let file=path(root); no_links(&file)?;
    match std::fs::symlink_metadata(file) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => anyhow::bail!("offline_keygen_handoff_pending: recovery evidence unreadable"),
        Ok(_) => anyhow::bail!("offline_keygen_handoff_pending: use the original operation's explicit handoff"),
    }
}
fn canonical_id(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| id.get_version_num() == 4 && id.to_string() == value)
}
fn no_links(path: &Path) -> Result<()> {
    for component in path.ancestors() {
        match std::fs::symlink_metadata(component) {
            Ok(meta) => {
                anyhow::ensure!(!meta.file_type().is_symlink(), "offline keygen path is a link");
                #[cfg(windows)] {
                    use std::os::windows::fs::MetadataExt;
                    anyhow::ensure!(meta.file_attributes() & 0x400 == 0, "offline keygen path is a reparse point");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
fn load(root: &Path) -> Result<Option<(Original, String)>> {
    let file = path(root); no_links(&file)?;
    let meta = match std::fs::symlink_metadata(&file) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    anyhow::ensure!(meta.is_file() && meta.len() <= 16384, "invalid offline keygen record");
    let mut bytes=Vec::new();
    std::fs::File::open(file)?.take(16385).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= 16384, "offline keygen record exceeds bound");
    let record: Original=serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid offline keygen record"))?;
    // Existing evidence is bound to the OS canonical root (including Windows
    // verbatim prefixes). Missing roots remain a read-only first-use probe.
    let canonical_root = root.canonicalize()?;
    no_links(&path(&canonical_root))?;
    validate_record(&record,&canonical_root)?;
    Ok(Some((record,hash(&bytes))))
}
pub(crate) fn pending_for_selected_root() -> Result<bool> {
    let (root, _) = respire_client_host::config::control_roots()?;
    Ok(load(&root)?.is_some())
}

fn valid_digest(value:&str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
fn validate_record(record:&Original,root:&Path) -> Result<()> {
    if let Some(candidate)=record.material.as_ref() {
        anyhow::ensure!(candidate.as_object().is_some_and(|object| object.len() == 7)
            && candidate["vault_version"] == 4
            && candidate["auth_operation_id"].as_str() == Some(record.operation_id.as_str())
            && ["kdf_salt","wrapped_urk","urk_nonce","keyring_account","keyring_backend"]
                .iter().all(|field| candidate[*field].as_str().is_some_and(|value| !value.is_empty())),
            "offline keygen material does not belong to the original operation");
    }
    anyhow::ensure!(record.schema == 1 && canonical_id(&record.operation_id) && canonical_id(&record.token)
        && record.root == root && record.source == record.profile && record.profile.is_absolute()
        && record.binary.is_absolute() && !record.coordinator.is_empty()
        && valid_digest(&record.image_sha256) && valid_digest(&record.marker_sha256),
        "offline keygen original binding mismatch");
    anyhow::ensure!(record.pending_marker_sha256.as_deref().is_none_or(|digest|
        valid_digest(digest) && matches!(record.stage,Stage::Prepared|Stage::RolledBack)),
        "offline keygen marker intent is outside a pre-effect stage");
    let committed=matches!(record.stage,Stage::Committed|Stage::Starting|Stage::Verified|Stage::Finished|Stage::HandedOff);
    anyhow::ensure!(if committed { record.session_sha256.as_deref().is_some_and(valid_digest) }
        else { record.session_sha256.is_none() }, "offline keygen stage has inconsistent receipt evidence");
    let actor_required=matches!(record.stage,Stage::Verified|Stage::Finished|Stage::HandedOff)
        || (record.was_serving && matches!(record.stage,Stage::RollbackFinished|Stage::RollbackHandedOff));
    anyhow::ensure!(record.actor.is_some() == actor_required && (record.was_serving || !record.store_ready),
        "offline keygen stage has inconsistent live actor evidence");
    anyhow::ensure!(record.final_actor.is_none() || matches!(record.stage, Stage::HandedOff | Stage::RollbackHandedOff)
        && record.actor.is_some(), "offline keygen final actor precedes durable handoff");
    Ok(())
}
fn require_startable(stage:Stage) -> Result<()> {
    anyhow::ensure!(matches!(stage,Stage::Sent|Stage::Committed),
        "offline_keygen_handoff_pending: prior launch/completion requires independent recovery; no spawn retried");
    Ok(())
}
fn keygen_sends_original(stage:Stage,query_only:bool) -> Result<bool> {
    match stage {
        Stage::Prepared => {
            anyhow::ensure!(!query_only,
                "offline keygen original was not sent; repeat keygen with the original force to resume preparation");
            Ok(true)
        }
        Stage::Sent | Stage::Committed => Ok(false),
        _ => anyhow::bail!("offline keygen handoff requires explicit recovery; no write was repeated"),
    }
}
fn save(record: &Original, digest: &mut Option<String>) -> Result<()> {
    validate_record(record,&record.root)?;
    let file=path(&record.root); no_links(&file)?;
    if let Some((previous,observed))=load(&record.root)? {
        anyhow::ensure!(digest.as_deref() == Some(observed.as_str()), "offline keygen record changed");
        validate_transition(&previous,record)?;
    } else {
        anyhow::ensure!(digest.is_none() && record.stage == Stage::Prepared, "offline keygen admission missing");
    }
    let bytes=serde_json::to_vec(record)?;
    // A failed publish is not retried here, even if it may have taken effect.
    let published=private_write(&file,&bytes,digest.as_deref());
    #[cfg(test)]
    let published=published.and_then(|()| tests::journal_publish_result());
    if published.is_err() {
        // Observe a possible after-write error without retrying or overwriting.
        // Equal readback is not proof that the failed durability step succeeded.
        let published=load(&record.root).ok().flatten().is_some_and(|(_,actual)| actual == hash(&bytes));
        if published { anyhow::bail!("offline_keygen_record_durability_unknown: exact record visible; no subsequent action taken"); }
        anyhow::bail!("offline_keygen_record_publication_unknown: original evidence retained; no subsequent action taken");
    }
    *digest=Some(hash(&bytes)); Ok(())
}
fn validate_transition(previous:&Original,next:&Original) -> Result<()> {
    let mut binding=next.clone();
    binding.stage=previous.stage;
    binding.marker_sha256=previous.marker_sha256.clone();
    binding.pending_marker_sha256=previous.pending_marker_sha256.clone();
    binding.session_sha256=previous.session_sha256.clone();
    binding.actor=previous.actor.clone();
    binding.final_actor=previous.final_actor.clone();
    binding.material=previous.material.clone();
    if matches!(previous.stage,Stage::Prepared|Stage::RolledBack) {
        anyhow::ensure!(if next.stage == previous.stage {
            (next.marker_sha256 == previous.marker_sha256
                || previous.pending_marker_sha256.as_ref() == Some(&next.marker_sha256))
                && (next.pending_marker_sha256.is_some()
                    || previous.pending_marker_sha256.as_ref().is_none_or(|target| target == &next.marker_sha256))
        } else {
            previous.pending_marker_sha256.is_none() && next.pending_marker_sha256.is_none()
                && next.marker_sha256 == previous.marker_sha256
        }, "offline keygen marker intent must settle before any effect claim");
    } else {
        anyhow::ensure!(next.pending_marker_sha256 == previous.pending_marker_sha256,
            "offline keygen cannot create a marker intent from an effect-bearing stage");
    }
    anyhow::ensure!(if let Some(final_actor) = &previous.final_actor {
        next.final_actor.as_ref() == Some(final_actor)
    } else {
        next.final_actor.is_none() || matches!((previous.stage,next.stage),
            (Stage::Finished,Stage::HandedOff) | (Stage::RollbackFinished,Stage::RollbackHandedOff))
    }, "offline keygen final actor changed outside handoff");
    anyhow::ensure!(serde_json::to_value(&binding)? == serde_json::to_value(previous)?
        && (next.material == previous.material || previous.material.is_none()
            && matches!((previous.stage,next.stage),(Stage::Prepared,Stage::Prepared)))
        && previous.session_sha256.as_ref().is_none_or(|digest| next.session_sha256.as_ref() == Some(digest))
        && previous.actor.as_ref().is_none_or(|actor| next.actor.as_ref() == Some(actor)),
        "offline keygen original operation changed");
    anyhow::ensure!(matches!((previous.stage,next.stage),
        (Stage::Prepared,Stage::Prepared) | (Stage::Prepared,Stage::Sent)
        | (Stage::Sent,Stage::Sent) | (Stage::Sent,Stage::Committed)
        | (Stage::Committed,Stage::Committed) | (Stage::Sent,Stage::Starting)
        | (Stage::Committed,Stage::Starting) | (Stage::Starting,Stage::Verified)
        | (Stage::Verified,Stage::Finished) | (Stage::Finished,Stage::HandedOff)
        | (Stage::Sent,Stage::RolledBack) | (Stage::RolledBack,Stage::RolledBack)
        | (Stage::RolledBack,Stage::RollbackStarting)
        | (Stage::RollbackStarting,Stage::RollbackFinished)
        | (Stage::RollbackFinished,Stage::RollbackHandedOff)), "offline keygen phase cannot be replayed or regressed");
    Ok(())
}
fn verify_selection(record: &Original) -> Result<()> {
    let (root,profile)=respire_client_host::config::control_roots()?;
    let (binary,digest)=crate::runtime_fetch::require_offline_keygen_handoff()?;
    anyhow::ensure!(root.canonicalize()? == record.root && profile.canonicalize()? == record.profile
        && binary == record.binary && digest == record.image_sha256,
        "offline keygen installation, selection or image changed");
    Ok(())
}
fn update_marker(record: &mut Original, maintenance: &mut lifecycle::Maintenance) -> Result<()> {
    let binding=maintenance.auth_launch_binding(&record.profile)?.context("offline keygen requires Paused maintenance")?;
    anyhow::ensure!(binding["coordinator_id"].as_str() == Some(record.coordinator.as_str())
        && lifecycle::current_boot_token().as_deref() == Some(record.token.as_str()), "offline keygen marker binding changed");
    record.marker_sha256=binding["marker_sha256"].as_str().context("offline keygen marker digest missing")?.to_owned();
    Ok(())
}

/// A new controller may replace only one of the exact marker digests already
/// recorded for this original. Persist its target before publishing that marker.
fn record_marker_intent(record:&mut Original,digest:&mut Option<String>,observed:Option<&str>,target:&str) -> Result<()> {
    anyhow::ensure!(matches!(record.stage,Stage::Prepared|Stage::RolledBack)
        && match observed {
            Some(actual) => actual == record.marker_sha256 || record.pending_marker_sha256.as_deref() == Some(actual),
            None => record.stage == Stage::Prepared,
        }, "offline keygen rebind does not match its original marker intent");
    if let Some(actual)=observed { record.marker_sha256=actual.to_owned(); }
    record.pending_marker_sha256=Some(target.to_owned());
    save(record,digest)
}
fn resume_prelaunch_maintenance(record:&mut Original,digest:&mut Option<String>) -> Result<lifecycle::Maintenance> {
    anyhow::ensure!(matches!(record.stage,Stage::Prepared|Stage::RolledBack),
        "offline keygen cannot rebind an uncertain effect or launch");
    let root=record.root.clone(); let profile=record.profile.clone();
    let token=record.token.clone(); let coordinator=record.coordinator.clone();
    let marker=record.marker_sha256.clone(); let pending=record.pending_marker_sha256.clone();
    lifecycle::resume_offline_keygen_prelaunch_maintenance(&root,&profile,&token,&coordinator,
        &marker,pending.as_deref(),record.stage == Stage::Prepared,
        |observed,target| record_marker_intent(record,digest,observed,target))
}
fn settle_marker_intent(record:&mut Original,digest:&mut Option<String>,maintenance:&mut lifecycle::Maintenance) -> Result<()> {
    let target=record.pending_marker_sha256.clone().context("offline keygen marker rebind intent missing")?;
    update_marker(record,maintenance)?;
    anyhow::ensure!(record.marker_sha256 == target, "offline keygen rebound marker differs from durable intent");
    record.pending_marker_sha256=None;
    save(record,digest)
}

include!("offline_keygen/operation.rs");

include!("offline_keygen/completion.rs");

#[cfg(test)]
mod tests;
