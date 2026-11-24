//! One space write; reconnects query its original Runtime journal admission.
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use respire_commands::output::{Item, ResultEnvelope, Status};
use respire_protocol::space_control::*;
use respire_protocol::{HostAuthIdentity, RpcRequest, RpcResponse};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Original {
    schema_version: u32,
    root: PathBuf,
    profile: PathBuf,
    operation_id: String,
    key: String,
    source_identity: HostAuthIdentity,
    operation: HostSpaceOperation,
    token: String,
    coordinator: String,
    was_serving: bool,
    store_ready: bool,
    #[serde(default)]
    credential_backend: Option<String>,
    #[serde(default)]
    legacy_factor: bool,
    sent: bool,
}

pub(crate) struct SpaceRecoveryRequired {
    pub operation_id: String,
    pub key: String,
    pub profile: PathBuf,
    pub operation: HostSpaceOperation,
    pub query_only: bool,
}
impl std::fmt::Debug for SpaceRecoveryRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { std::fmt::Display::fmt(self, f) }
}
impl std::fmt::Display for SpaceRecoveryRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "space_recovery_required operation_id={}; repeat the same space invite/join/kick command to query this operation; never replay the write", self.operation_id)
    }
}
impl std::error::Error for SpaceRecoveryRequired {}

impl Original {
    fn fingerprint(&self) -> Result<String> {
        Ok(super::hash(&serde_json::to_vec(&json!({"schema_version":HOST_SPACE_SCHEMA_V,
            "method":"control.space", "root":self.root, "profile":self.profile,
            "source_identity":self.source_identity, "operation":self.operation}))?))
    }
    fn recovery(&self) -> SpaceRecoveryRequired {
        SpaceRecoveryRequired { operation_id: self.operation_id.clone(), key: self.key.clone(),
            profile: self.profile.clone(), operation: self.operation.clone(), query_only: self.sent }
    }
}

include!("space_control/membership.rs");

fn account(identity: &HostAuthIdentity, profile: &Path) -> String {
    match identity { HostAuthIdentity::Bound { account_id, .. } => account_id.clone(),
        _ => super::hash(profile.display().to_string().as_bytes()) }
}

fn unchanged(original: &Original, expected: &HostAuthIdentity, binary: &Path,
    manifest: &crate::runtime_fetch::ManagedManifest) -> Result<()> {
    let (root, profile) = respire_client_host::config::control_roots()?;
    let (current, release) = super::model_control::installed_model_runtime()?;
    anyhow::ensure!(root.canonicalize()? == original.root && profile.canonicalize()? == original.profile
        && current == binary && release.version == manifest.version && release.sha256.eq_ignore_ascii_case(&manifest.sha256)
        && super::control::identity(&original.profile)? == *expected,
        "space write installation, profile, image or resulting identity changed");
    Ok(())
}

fn validate_response(response: RpcResponse<ResultEnvelope>, request: &RpcRequest, profile: &Path,
    pid: u32, original: &Original) -> Result<ResultEnvelope> {
    anyhow::ensure!(response.v == request.v && response.id == request.id && response.pid == Some(pid)
        && response.bin == "rsrs-runtime" && response.scope.is_none() && response.web_url.is_none()
        && response.operation_id.as_deref() == Some(original.operation_id.as_str())
        && response.idempotency_key.as_deref() == Some(original.key.as_str()), "space response binding mismatch");
    if let Some(path) = response.data_dir.as_deref() { anyhow::ensure!(Path::new(path).canonicalize()? == profile, "space response profile mismatch"); }
    if response.error.is_some() {
        let code = match response.code.as_deref() {
            Some("unauthorized") => "unauthorized",
            Some("scope_mismatch") => "scope_mismatch",
            Some("auth_ownership_unavailable") => "auth_ownership_unavailable",
            Some("control_unavailable") => "control_unavailable",
            Some("space_outcome_unknown") => "space_outcome_unknown",
            Some("control_outcome_unknown") => "control_outcome_unknown",
            _ => anyhow::bail!("invalid space control rejection code"),
        };
        let deadline = code == "control_outcome_unknown";
        let envelope = response.envelope.as_ref().context("space control rejection envelope missing")?;
        anyhow::ensure!(!response.ok && response.exit == 2
            && response.error.as_deref().is_some_and(|error| !error.is_empty())
            && envelope.command == "space-control" && matches!(envelope.status, Status::Fail)
            && envelope.details.is_null() && envelope.actions.is_empty() && envelope.related.is_empty()
            && envelope.items.len() == 2
            && envelope.items[0].name == "control_ready"
            && envelope.items[0].value == (if deadline { "unknown" } else { "false" })
            && (if deadline { matches!(envelope.items[0].status, Status::Skip) }
                else { matches!(envelope.items[0].status, Status::Fail) })
            && envelope.items[1].name == "store_ready" && envelope.items[1].value == "false"
            && matches!(envelope.items[1].status, Status::Skip)
            && (if deadline { envelope.summary == json!({"outcome":"unknown"}) && envelope.errors.is_empty() }
                else { envelope.summary == json!({}) && envelope.errors.len() == 1
                    && response.error.as_ref() == envelope.errors.first() }),
            "invalid space control rejection envelope");
        // Keep the original operation Paused; never echo a possibly secret diagnostic.
        anyhow::bail!("Runtime rejected space control ({code}); query the original operation without replaying");
    }
    let envelope = response.envelope.context("space receipt missing")?;
    anyhow::ensure!(envelope.command == "space-control" && envelope.details.is_null() && envelope.errors.is_empty(), "invalid space envelope");
    for (name,value) in [("control_ready","true"),("store_ready","false")] {
        let mut items = envelope.items.iter().filter(|item| item.name == name);
        anyhow::ensure!(items.next().is_some_and(|item| item.value == value && if name == "control_ready" {
            matches!(item.status,Status::Ok) } else { matches!(item.status,Status::Skip) }) && items.next().is_none(), "space readiness mismatch");
    }
    let receipt: HostSpaceResult = serde_json::from_value(envelope.summary.clone()).map_err(|_| anyhow::anyhow!("invalid space receipt"))?;
    anyhow::ensure!(receipt.operation_id == original.operation_id && receipt.fingerprint == original.fingerprint()?
        && receipt.operation == original.operation && receipt.profile == profile && receipt.identity == original.source_identity
        && super::control::identity(profile)? == receipt.identity, "space receipt differs from its original identity or intent");
    match receipt.outcome {
        HostSpaceOutcome::Committed => anyhow::ensure!(response.ok && response.exit == 0 && response.code.is_none()
            && response.error.is_none() && matches!(envelope.status,Status::Ok), "invalid committed space outcome"),
        HostSpaceOutcome::Pending => anyhow::ensure!(response.ok && response.exit == 0 && response.code.is_none()
            && response.error.is_none() && matches!(envelope.status,Status::Pending), "invalid pending space outcome"),
        HostSpaceOutcome::Unknown => anyhow::ensure!(!response.ok && response.exit == 2
            && response.code.as_deref() == Some("space_outcome_unknown") && matches!(envelope.status,Status::Fail), "invalid unknown space outcome"),
        HostSpaceOutcome::Rejected => anyhow::ensure!(!response.ok && response.exit == 2
            && response.code.as_deref() == Some("space_rejected") && response.error.is_none()
            && matches!(envelope.status,Status::Fail), "invalid rejected space outcome"),
    }
    if receipt.outcome == HostSpaceOutcome::Rejected {
        anyhow::ensure!(receipt.invite_code.is_none() && receipt.session_id.is_none() && receipt.target_profile.is_none()
            && receipt.target_account_id.is_none() && receipt.target_session_sha256.is_none()
            && receipt.kick.is_empty() && receipt.remaining.is_none(), "rejected space receipt claims effects");
        return Ok(envelope);
    }
    match &original.operation {
        HostSpaceOperation::Join { space,user,readonly, .. } => {
            let target = original.root.join("accounts").join(space);
            anyhow::ensure!(receipt.target_profile.as_deref() == Some(target.as_path()) && receipt.invite_code.is_none()
                && receipt.session_id.is_none() && receipt.kick.is_empty() && receipt.remaining.is_none(), "join target receipt mismatch");
            if receipt.outcome == HostSpaceOutcome::Committed {
                anyhow::ensure!(receipt.target_account_id.as_deref() == Some(super::hash(user.as_bytes()).as_str()), "join account receipt mismatch");
                verify_target_path(&target)?;
                let bytes = std::fs::read(target.join("session.json"))?;
                anyhow::ensure!(receipt.target_session_sha256.as_deref() == Some(super::hash(&bytes).as_str()), "join session digest mismatch");
                // Host checks only the gate bytes and identity digest, never unwraps keys.
                let gate = target.join("agent.json");
                if *readonly { anyhow::ensure!(std::fs::read(gate)? == br#"{"readonly":true,"readonly_team":true}"#, "join readonly_team gate mismatch"); }
                else { anyhow::ensure!(!gate.try_exists()?, "join unexpectedly changed the agent gate"); }
            } else {
                anyhow::ensure!(receipt.target_account_id.is_none() && receipt.target_session_sha256.is_none(), "pending join claims completion");
            }
        }
        HostSpaceOperation::Invite { readonly, .. } => {
            anyhow::ensure!(receipt.target_profile.is_none() && receipt.target_account_id.is_none()
                && receipt.target_session_sha256.is_none() && receipt.kick.is_empty(), "invite receipt contains unrelated effects");
            if receipt.outcome == HostSpaceOutcome::Committed || receipt.outcome == HostSpaceOutcome::Pending && receipt.invite_code.is_some() {
                let code = receipt.invite_code.as_deref().context("committed invite code missing")?;
                let value = respire::space::parse_invite(code)?;
                anyhow::ensure!(value["readonly"].as_bool() == Some(*readonly)
                    && value["session_id"].as_str() == receipt.session_id.as_deref()
                    && value["control_operation_id"].as_str() == Some(original.operation_id.as_str())
                    && value["control_fingerprint"].as_str() == Some(receipt.fingerprint.as_str()), "invite result binding mismatch");
                let user = value["user"].as_str().context("invite owner missing")?;
                anyhow::ensure!(matches!(&original.source_identity,HostAuthIdentity::Bound { account_id, .. }
                    if *account_id == super::hash(user.as_bytes())), "invite owner differs from the admitted source");
            } else { anyhow::ensure!(receipt.invite_code.is_none(), "pending invite exposed an uncommitted code"); }
        }
        HostSpaceOperation::Kick { session_id,all } => {
            anyhow::ensure!(receipt.invite_code.is_none() && receipt.session_id.is_none() && receipt.target_profile.is_none()
                && receipt.target_account_id.is_none() && receipt.target_session_sha256.is_none(), "kick receipt contains unrelated effects");
            let mut seen = std::collections::HashSet::new();
            for item in &receipt.kick {
                uuid::Uuid::parse_str(&item.session_id)?;
                anyhow::ensure!(seen.insert(&item.session_id) && (*all || session_id.as_deref() == Some(item.session_id.as_str())), "kick result target mismatch");
            }
            if receipt.outcome == HostSpaceOutcome::Committed {
                anyhow::ensure!(!receipt.kick.is_empty() && receipt.remaining.is_some() && receipt.kick.iter().all(|item|
                    matches!(item.state,HostSpaceKickState::Revoked | HostSpaceKickState::Absent | HostSpaceKickState::Rejected)), "committed kick has unresolved members");
            }
        }
    }
    Ok(envelope)
}

#[cfg(test)]
#[path = "space_control/tests.rs"]
mod tests;

fn verify_target_path(target: &Path) -> Result<()> {
    for path in target.join("session.json").ancestors() {
        let metadata = std::fs::symlink_metadata(path)?;
        anyhow::ensure!(!metadata.file_type().is_symlink(), "join target is a symlink");
        #[cfg(windows)] {
            use std::os::windows::fs::MetadataExt;
            anyhow::ensure!(metadata.file_attributes() & 0x400 == 0, "join target is a reparse point");
        }
    }
    let gate = target.join("agent.json");
    if gate.try_exists()? {
        let metadata = std::fs::symlink_metadata(gate)?;
        anyhow::ensure!(!metadata.file_type().is_symlink(), "join agent gate is a symlink");
        #[cfg(windows)] {
            use std::os::windows::fs::MetadataExt;
            anyhow::ensure!(metadata.file_attributes() & 0x400 == 0, "join agent gate is a reparse point");
        }
    }
    Ok(())
}

pub(crate) fn control_space_invite(note: Option<&str>, readonly: bool) -> Result<ResultEnvelope> {
    output(control_write(HostSpaceOperation::Invite { note: note.map(str::to_owned),readonly },None)?)
}

pub(crate) fn control_space_join(code: &str) -> Result<ResultEnvelope> {
    let value = respire::space::parse_invite(code)?;
    let space = value["space"].as_str().context("invite space missing")?.trim().to_owned();
    respire::space::validate_space_name(&space)?;
    let addr = value["addr"].as_str().context("invite address missing")?.trim().trim_end_matches('/').to_owned();
    let user = value["user"].as_str().context("invite user missing")?.trim().to_owned();
    let operation = HostSpaceOperation::Join { invite_sha256: super::hash(code.trim().as_bytes()),space,addr,user,
        readonly: value["readonly"].as_bool().unwrap_or(false) };
    output(control_write(operation,Some(code))?)
}

pub(crate) fn control_space_kick(session_id: Option<&str>, all: bool) -> Result<ResultEnvelope> {
    anyhow::ensure!(all || session_id.is_some_and(|id| !id.trim().is_empty()), "kick needs --session <id> or --all");
    output(control_write(HostSpaceOperation::Kick { session_id: session_id.map(|id| id.trim().to_owned()),all },None)?)
}

fn output(receipt: HostSpaceResult) -> Result<ResultEnvelope> {
    let complete = receipt.outcome == HostSpaceOutcome::Committed;
    let status = match receipt.outcome { HostSpaceOutcome::Committed => Status::Ok,
        HostSpaceOutcome::Pending => Status::Pending, HostSpaceOutcome::Unknown | HostSpaceOutcome::Rejected => Status::Fail };
    let mut envelope = ResultEnvelope::new("space",status,json!({"outcome":receipt.outcome,"operation_id":receipt.operation_id}),Vec::new());
    match &receipt.operation {
        HostSpaceOperation::Invite { readonly, .. } => {
            envelope.summary["action"] = json!("invite");
            envelope.details = json!({"session_id":receipt.session_id,"readonly":readonly,"code":receipt.invite_code});
            if let Some(code) = &receipt.invite_code {
                let value = respire::space::parse_invite(code)?;
                envelope.details["space"] = value["space"].clone();
                envelope.summary["space"] = value["space"].clone();
                envelope.items.push(Item::new("code",if *readonly { Status::Warn } else { Status::Ok },code));
                envelope.items.push(Item::new("session",Status::Ok,receipt.session_id.as_deref().unwrap_or("")));
                envelope.actions.push("space join <code>".to_owned());
                if *readonly { envelope.status = Status::Warn; }
            }
        }
        HostSpaceOperation::Join { space,addr,user,readonly, .. } => {
            envelope.summary["action"] = json!("join");
            envelope.summary["space"] = json!(space);
            envelope.details = json!({"space":space,"addr":addr,"user":user,"readonly":readonly,
                "dir":receipt.target_profile,"keyring":complete,"hint":if complete { Some(format!("space use {space}")) } else { None }});
            if complete {
                envelope.items.push(Item::new("space",if *readonly { Status::Warn } else { Status::Ok },space));
                envelope.actions.push(format!("space use {space}"));
                if *readonly { envelope.status = Status::Warn; }
            }
        }
        HostSpaceOperation::Kick { .. } => {
            let revoked: Vec<&str> = receipt.kick.iter().filter(|item| item.state == HostSpaceKickState::Revoked).map(|item| item.session_id.as_str()).collect();
            let failed: Vec<String> = receipt.kick.iter().filter(|item| item.state != HostSpaceKickState::Revoked).map(|item|
                format!("{} ({})",item.session_id,match item.state { HostSpaceKickState::Absent => "server has no such session",
                    HostSpaceKickState::Rejected => "rejected",HostSpaceKickState::Queued => "not sent",
                    _ => "outcome unknown; do not replay" })).collect();
            envelope.summary["action"] = json!("kick");
            envelope.summary["revoked"] = json!(revoked.len());
            envelope.summary["failed"] = json!(failed.len());
            envelope.summary["remaining"] = json!(receipt.remaining);
            for id in &revoked { envelope.items.push(Item::new("revoked",Status::Ok,*id)); }
            for error in &failed { envelope.items.push(Item::new("failed",Status::Warn,error)); }
            if complete && !failed.is_empty() { envelope.status = Status::Warn; }
            envelope.details = json!({"revoked":revoked,"failed":failed,"remaining":receipt.remaining,"items":receipt.kick});
        }
    }
    envelope.details["outcome"] = json!(receipt.outcome);
    envelope.details["operation_id"] = json!(receipt.operation_id);
    if receipt.outcome == HostSpaceOutcome::Rejected {
        envelope.details["error_type"] = json!("runtime");
        envelope.errors.push("space operation rejected; no change was committed".to_owned());
    }
    if !complete && receipt.outcome != HostSpaceOutcome::Rejected {
        envelope.actions.push("repeat exactly the same command to query the original operation; do not create a replacement".to_owned());
    }
    Ok(envelope)
}
