//! Receiver-bound authorization for offline installation of one claimed archive.
use super::OwnerRecord;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::{
    io,
    time::{Duration, Instant},
};

const LIFETIME: Duration = Duration::from_secs(30);

#[derive(Clone, Copy)]
pub struct Scope {
    pub operation: [u8; 16],
    pub archive: [u8; 32],
    pub target: [u8; 32],
    pub target_identity: [u8; 32],
    pub account: [u8; 32],
    pub profile: [u8; 32],
    pub instance: [u8; 16],
    pub generation: u64,
    pub supervisor_pid: u32,
    pub supervisor_birth: u64,
}

impl Scope {
    fn commitment(&self, intent: &[u8; 32], epoch: &[u8; 16]) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(b"respire/offline-backup/restore/v1\0");
        hash.update(self.operation);
        hash.update(self.archive);
        hash.update(self.target);
        hash.update(self.target_identity);
        hash.update(self.account);
        hash.update(self.profile);
        hash.update(self.instance);
        hash.update(self.generation.to_be_bytes());
        hash.update(self.supervisor_pid.to_be_bytes());
        hash.update(self.supervisor_birth.to_be_bytes());
        hash.update(epoch);
        hash.update(intent);
        hash.finalize().into()
    }
}

fn authenticator(owner: &OwnerRecord, challenge: &[u8; 32]) -> io::Result<Hmac<Sha256>> {
    let record = owner.as_bytes();
    let mut mac = Hmac::<Sha256>::new_from_slice(&record[..32]).map_err(io::Error::other)?;
    mac.update(b"respire/native-backup/confirmation/v1\0");
    mac.update(&record[32..]);
    mac.update(challenge);
    Ok(mac)
}

pub fn issue(
    owner: &OwnerRecord,
    challenge: &[u8; 32],
    scope: &Scope,
    intent: &[u8; 32],
    epoch: &[u8; 16],
) -> io::Result<[u8; 32]> {
    let mut mac = authenticator(owner, challenge)?;
    mac.update(&scope.commitment(intent, epoch));
    Ok(mac.finalize().into_bytes().into())
}

/// Held in memory by the surviving native coordinator. Restart loses the challenge.
pub struct Receiver {
    scope: Scope,
    challenge: [u8; 32],
    acquired: Instant,
    mac: Hmac<Sha256>,
}

impl Receiver {
    pub fn arm(owner: &OwnerRecord, scope: Scope) -> io::Result<Self> {
        if scope.operation[6] >> 4 != 4
            || scope.operation[8] & 0xc0 != 0x80
            || scope.instance == [0; 16]
            || scope.generation == 0
            || scope.supervisor_pid == 0
            || scope.supervisor_birth == 0
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let mut challenge = [0; 32];
        getrandom::fill(&mut challenge).map_err(|_| io::ErrorKind::Other)?;
        Ok(Self {
            scope,
            challenge,
            acquired: Instant::now(),
            mac: authenticator(owner, &challenge)?,
        })
    }

    pub fn challenge(&self) -> [u8; 32] {
        self.challenge
    }

    pub fn accept(
        mut self,
        intent: [u8; 32],
        epoch: [u8; 16],
        tag: &[u8; 32],
    ) -> io::Result<VerifiedGrant> {
        if epoch == [0; 16] {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let fingerprint = self.scope.commitment(&intent, &epoch);
        self.mac.update(&fingerprint);
        self.mac
            .verify_slice(tag)
            .map_err(|_| io::ErrorKind::PermissionDenied)?;
        let grant = VerifiedGrant {
            scope: self.scope,
            epoch,
            intent,
            acquired: self.acquired,
            fingerprint,
        };
        grant.check_fresh()?;
        Ok(grant)
    }
}

pub struct VerifiedGrant {
    epoch: [u8; 16],
    scope: Scope,
    intent: [u8; 32],
    acquired: Instant,
    fingerprint: [u8; 32],
}
impl VerifiedGrant {
    pub fn scope(&self) -> &Scope {
        &self.scope
    }
    pub fn vault_epoch(&self) -> [u8; 16] {
        self.epoch
    }
    pub fn intent(&self) -> [u8; 32] {
        self.intent
    }
    pub fn fingerprint(&self) -> String {
        self.fingerprint
            .iter()
            .map(|v| format!("{v:02x}"))
            .collect()
    }
    pub fn check_fresh(&self) -> io::Result<()> {
        if self.acquired.elapsed() >= LIFETIME {
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn altered_scope_epoch_expiry_and_new_receiver_refuse_original_tag() -> io::Result<()> {
        let owner = OwnerRecord::from_bytes([7; 48]);
        let mut op = [3; 16];
        op[6] = 0x40;
        op[8] = 0x80;
        let scope = Scope {
            operation: op,
            archive: [4; 32],
            target: [5; 32],
            target_identity: [6; 32],
            account: [7; 32],
            profile: [8; 32],
            instance: [9; 16],
            generation: 10,
            supervisor_pid: 11,
            supervisor_birth: 12,
        };
        let intent = [13; 32];
        let epoch = [14; 16];
        let arm = || Receiver::arm(&owner, scope);
        let receiver = arm()?;
        let sign = |r: &Receiver| issue(&owner, &r.challenge(), &scope, &intent, &epoch);
        let tag = sign(&receiver)?;
        assert!(arm()?.accept(intent, epoch, &tag).is_err());
        assert!(receiver.accept(intent, epoch, &tag).is_ok());
        for altered in [
            Scope {
                target_identity: [1; 32],
                ..scope
            },
            Scope {
                generation: 11,
                ..scope
            },
            Scope {
                instance: [2; 16],
                ..scope
            },
            Scope {
                supervisor_birth: 13,
                ..scope
            },
        ] {
            let r = arm()?;
            let tag = issue(&owner, &r.challenge(), &altered, &intent, &epoch)?;
            assert!(r.accept(intent, epoch, &tag).is_err());
        }
        let r = arm()?;
        let tag = sign(&r)?;
        assert!(r.accept(intent, [15; 16], &tag).is_err());
        let mut r = arm()?;
        let tag = sign(&r)?;
        r.acquired -= LIFETIME;
        assert!(r.accept(intent, epoch, &tag).is_err());
        Ok(())
    }
}

fn observation_mac(
    owner: &OwnerRecord,
    challenge: &[u8; 16],
    observation: &[u8],
) -> io::Result<Hmac<Sha256>> {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(&owner.as_bytes()[..32]).map_err(io::Error::other)?;
    mac.update(b"respire/offline-backup/vault-observation/v1\0");
    mac.update(&owner.as_bytes()[32..]);
    mac.update(challenge);
    mac.update(observation);
    Ok(mac)
}

/// Authenticate a Vault observation delivered to the surviving native receiver.
pub fn vault_observation(
    owner: &OwnerRecord,
    challenge: &[u8; 16],
    observation: &[u8],
) -> io::Result<[u8; 32]> {
    Ok(observation_mac(owner, challenge, observation)?
        .finalize()
        .into_bytes()
        .into())
}

pub fn verify_vault_observation(
    owner: &OwnerRecord,
    challenge: &[u8; 16],
    observation: &[u8],
    tag: &[u8; 32],
) -> io::Result<()> {
    observation_mac(owner, challenge, observation)?
        .verify_slice(tag)
        .map_err(|_| io::ErrorKind::PermissionDenied.into())
}

fn request_mac(owner: &OwnerRecord, challenge: &[u8; 32], scope: &Scope) -> io::Result<Hmac<Sha256>> {
    let mut mac = authenticator(owner, challenge)?;
    mac.update(b"request-offline-grant/v1\0");
    mac.update(&scope.commitment(&[0; 32], &[0; 16]));
    Ok(mac)
}

pub fn sign_request(owner: &OwnerRecord, challenge: &[u8; 32], scope: &Scope) -> io::Result<[u8; 32]> {
    Ok(request_mac(owner, challenge, scope)?.finalize().into_bytes().into())
}
pub fn verify_request(owner: &OwnerRecord, challenge: &[u8; 32], scope: &Scope, tag: &[u8; 32]) -> io::Result<()> {
    request_mac(owner, challenge, scope)?.verify_slice(tag).map_err(|_| io::ErrorKind::PermissionDenied.into())
}
