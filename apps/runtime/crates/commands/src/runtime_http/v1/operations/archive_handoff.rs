//! Receiver-bound handoff for the directly supervising native archive controller.
use super::*;
use respire_spawn::owner_credential::{backup_grant, OwnerRecord};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    challenge: [u8; 32],
    supervisor_pid: u32,
    supervisor_birth: u64,
}

pub(super) struct Armed {
    scope: backup_grant::Scope,
    challenge: [u8; 32],
    record: OwnerRecord,
}

fn supervisor_is_current(pid: u32, birth: u64) -> bool {
    use std::os::unix::fs::MetadataExt;
    let checked = || -> Option<bool> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let parent = status
            .lines()
            .find_map(|line| line.strip_prefix("PPid:"))?
            .trim()
            .parse::<u32>()
            .ok()?;
        if pid == 0 || pid != parent {
            return Some(false);
        }
        let directory = format!("/proc/{pid}");
        if std::fs::metadata(&directory).ok()?.uid() != std::fs::metadata("/proc/self").ok()?.uid()
        {
            return Some(false);
        }
        let stat = std::fs::read_to_string(format!("{directory}/stat")).ok()?;
        Some(
            stat.rsplit_once(')')?
                .1
                .split_whitespace()
                .nth(19)?
                .parse::<u64>()
                .ok()?
                == birth,
        )
    };
    checked() == Some(true)
}

impl Request {
    pub(super) fn arm(self, claim: ArchiveClaim, live: &OwnerBinding) -> V1Result<Armed> {
        if claim.query_only
            || claim.inspect_only
            || !supervisor_is_current(self.supervisor_pid, self.supervisor_birth)
        {
            return Err(transport::unauthorized());
        }
        let scope = backup_grant::Scope {
            operation: claim.operation,
            archive: claim.archive,
            target: claim.target,
            target_identity: claim.target_identity,
            instance: claim.instance,
            generation: claim.generation,
            account: Sha256::digest(live.account.as_bytes()).into(),
            profile: Sha256::digest(live.canonical_profile.as_os_str().as_encoded_bytes()).into(),
            supervisor_pid: self.supervisor_pid,
            supervisor_birth: self.supervisor_birth,
        };
        let record = respire_spawn::owner_credential::read_owner_credential(
            &crate::rpc_worker::runtime_dir(),
            &live.instance.to_string(),
        )
        .map_err(|_| transport::unauthorized())?;
        Ok(Armed {
            scope,
            challenge: self.challenge,
            record,
        })
    }
}

impl Armed {
    pub(super) fn binding(&self) -> [u8; 32] {
        backup_grant::request_binding(&self.challenge, &self.scope)
    }

    pub(super) fn current(&self) -> bool {
        supervisor_is_current(self.scope.supervisor_pid, self.scope.supervisor_birth)
    }

    pub(super) fn deliver(self, receipt: &mut Value, live: &OwnerBinding) -> V1Result<()> {
        if !self.current() {
            return Err(transport::unavailable());
        }
        let intent = digest(
            receipt["intent_sha256"]
                .as_str()
                .ok_or_else(transport::unavailable)?,
        )?;
        let epoch = live.vault_epoch.as_bytes();
        let tag = backup_grant::issue(&self.record, &self.challenge, &self.scope, &intent, epoch)
            .map_err(|_| transport::unavailable())?;
        receipt["grant"] = json!(tag);
        Ok(())
    }
}
