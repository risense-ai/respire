//! Backup payload for an authenticated host exchange. No transport registration.
use super::*;
use respire_spawn::owner_credential::{backup_grant::{vault_observation, verify_vault_observation}, OwnerRecord};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    pub operation: [u8; 16],
    pub challenge: [u8; 16],
    pub instance: [u8; 16],
    pub space: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Witness {
    request: Request,
    generation: u64,
    epoch: [u8; 16],
    account: [u8; 32],
    profile: [u8; 32],
    target: [u8; 32],
    file: [u8; 32],
    tag: [u8; 32],
}

impl Witness {
    fn payload(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(&("respire/backup/selected-restart/v1", &self.request,
            self.generation, self.epoch, self.account, self.profile, self.target, self.file))?)
    }

    pub(super) fn verify(&self, request: &Request, owner: &OwnerRecord, generation: u64,
        account: [u8;32], profile: [u8;32], target: [u8;32], file: [u8;32]) -> Result<()> {
        verify_vault_observation(owner, &request.challenge, &self.payload()?, &self.tag)?;
        ensure!(&self.request == request && self.generation == generation && self.epoch != [0;16]
            && self.account == account && self.profile == profile && self.target == target && self.file == file,
            "selected restart observation differs from retained admission");
        Ok(())
    }
}

pub(super) fn capture(ctx: &rest_dispatch::LibraryContext<'_>, request: Request,
    owner: &OwnerRecord) -> V1Result<Witness> {
    if request.instance != *ctx.scope.instance.as_bytes() || !ctx.may_commit() || request.space.is_empty() {
        return Err(transport::unauthorized());
    }
    reads::with_memory_store(ctx, Some(request.space.clone()), |store, _| {
        let pin = store.pin_backup_target().map_err(|_| transport::unavailable())?;
        let mut proof = Witness { generation: ctx.scope.generation, epoch: *ctx.scope.vault_epoch.as_bytes(),
            account: Sha256::digest(ctx.scope.account.as_bytes()).into(),
            profile: Sha256::digest(ctx.scope.canonical_profile.as_os_str().as_encoded_bytes()).into(),
            target: Sha256::digest(pin.path().as_os_str().as_encoded_bytes()).into(),
            file: pin.identity_digest().map_err(|_| transport::unavailable())?, request, tag: [0;32] };
        if !ctx.may_commit() { return Err(transport::unauthorized()); }
        proof.tag = vault_observation(owner, &proof.request.challenge,
            &proof.payload().map_err(|_| transport::unavailable())?).map_err(|_| transport::unavailable())?;
        Ok(proof)
    })
}
