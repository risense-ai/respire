//! Registry state for a single separately verified private owner connection.
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, Instant},
};
use uuid::Uuid;

mod private_model;
mod replay;
use replay::ReplayWindow;

#[cfg(test)]
const PROOF_TTL: Duration = Duration::from_secs(30);
const LEASE_TTL: Duration = Duration::from_secs(300);
#[cfg(test)]
const MAX_PROOFS: usize = 64;
const MAX_LEASES: usize = 256;
#[cfg(test)]
const PROOF: &str = "rhp1_";
const LEASE: &str = "rhl1_";
type Fingerprint = [u8; 32];

#[derive(Clone, PartialEq, Eq)]
pub(super) struct OwnerBinding {
    pub(super) account: String,
    pub(super) canonical_profile: PathBuf,
    pub(super) instance: Uuid,
    pub(super) generation: u64,
    pub(super) vault_epoch: Uuid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OwnerAction {
    ReadGrants,
    RevokeGrant,
    ReadStats,
    ReadQueryLog,
    ReadReviews,
    ReadSyncConflicts,
}

impl OwnerAction {
    fn flag(self) -> u8 {
        1 << (self as u8)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct ActionSet(u8);

impl ActionSet {
    pub(super) fn try_from_unique(actions: &[OwnerAction]) -> Option<Self> {
        if actions.is_empty() || actions.len() > 6 {
            return None;
        }
        let mut set = 0;
        for &action in actions {
            let flag = action.flag();
            if set & flag != 0 {
                return None;
            }
            set |= flag;
        }
        Some(Self(set))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Denied {
    NoOwner,
    UnusableCredential,
    ActionNotAllowed,
    Busy,
}

struct Entry {
    scopes: ActionSet,
    expires: Instant,
}

/// Return only in the successful exchange. Do not log or derive Debug.
pub(super) struct IssuedLease {
    pub(super) token: String,
    pub(super) expires_in: u64,
}

/// A queued operation must not carry the raw HTTP bearer. This identifier
/// has no wire form and can only revalidate the exact previously checked action.
pub(super) struct LeaseTicket {
    fingerprint: Fingerprint,
    action: OwnerAction,
}

pub(super) struct OwnerLeaseRegistry {
    binding: OwnerBinding,
    allowed: ActionSet,
    revoked: bool,
    #[cfg(test)]
    proofs: HashMap<Fingerprint, Entry>,
    leases: HashMap<Fingerprint, Entry>,
    replay: ReplayWindow,
}

impl OwnerLeaseRegistry {
    #[cfg(target_os = "linux")]
    pub(super) fn observe_archive_clock(&mut self, now_ms: u64) -> bool {
        self.replay.observe(now_ms)
    }

    /// Only the verified serving-side native owner connection may construct this.
    pub(super) fn for_verified_connection(
        binding: OwnerBinding,
        allowed: ActionSet,
    ) -> Result<Self, Denied> {
        if binding.account.trim().is_empty()
            || !binding.canonical_profile.is_absolute()
            || binding.instance.is_nil()
            || binding.generation == 0
            || binding.vault_epoch.is_nil()
        {
            return Err(Denied::NoOwner);
        }
        Ok(Self {
            binding,
            allowed,
            revoked: false,
            #[cfg(test)]
            proofs: HashMap::new(),
            leases: HashMap::new(),
            replay: ReplayWindow::new(),
        })
    }

    pub(super) fn revoke(&mut self) {
        self.revoked = true;
        #[cfg(test)]
        self.proofs.clear();
        self.leases.clear();
        self.replay = ReplayWindow::new();
    }

    // The caller must derive live state from the Runtime under the identity
    // fence, not from a request. Disconnect/lock must also proactively revoke.
    fn check_live(&mut self, live: Option<&OwnerBinding>) -> Result<(), Denied> {
        if self.revoked || live != Some(&self.binding) {
            self.revoke();
            return Err(Denied::NoOwner);
        }
        Ok(())
    }

    /// Deliver only over the verified native connection.
    #[cfg(test)]
    pub(super) fn issue(
        &mut self,
        live: Option<&OwnerBinding>,
        scopes: ActionSet,
        now: Instant,
    ) -> Result<String, Denied> {
        self.check_live(live)?;
        if scopes.0 & self.allowed.0 != scopes.0 {
            return Err(Denied::ActionNotAllowed);
        }
        mint(&mut self.proofs, PROOF, MAX_PROOFS, scopes, now, PROOF_TTL)
    }

    /// Preflight only: exchange still removes and revalidates under the lock
    /// after the bounded request body has arrived.
    #[cfg(test)]
    pub(super) fn screen_proof(
        &mut self,
        live: Option<&OwnerBinding>,
        supplied: &str,
        now: Instant,
    ) -> Result<(), Denied> {
        self.check_live(live)?;
        let key = digest(PROOF, supplied).ok_or(Denied::UnusableCredential)?;
        self.proofs
            .get(&key)
            .filter(|proof| proof.expires > now)
            .map(|_| ())
            .ok_or(Denied::UnusableCredential)
    }

    /// Removing the proof and minting the lease happen in one exclusive operation.
    #[cfg(test)]
    pub(super) fn exchange(
        &mut self,
        live: Option<&OwnerBinding>,
        supplied: &str,
        requested: ActionSet,
        now: Instant,
    ) -> Result<IssuedLease, Denied> {
        self.check_live(live)?;
        let key = digest(PROOF, supplied).ok_or(Denied::UnusableCredential)?;
        let proof = self.proofs.remove(&key).ok_or(Denied::UnusableCredential)?;
        if proof.expires <= now || proof.scopes != requested {
            return Err(Denied::UnusableCredential);
        }
        let token = mint(
            &mut self.leases,
            LEASE,
            MAX_LEASES,
            proof.scopes,
            now,
            LEASE_TTL,
        )?;
        Ok(IssuedLease {
            token,
            expires_in: LEASE_TTL.as_secs(),
        })
    }

    /// Signed proof is verified before reading the body and checked again at
    /// redemption. The record is the current serving Runtime secret, never HTTP input.
    pub(super) fn screen_signed(
        &mut self,
        live: Option<&OwnerBinding>,
        record: &[u8; 48],
        supplied: &str,
        wall_ms: u64,
    ) -> Result<respire_spawn::owner_credential::proof::VerifiedProof, Denied> {
        self.check_live(live)?;
        if !self.replay.observe(wall_ms) {
            return Err(Denied::UnusableCredential);
        }
        let verified = respire_spawn::owner_credential::proof::verify(record, supplied, wall_ms)
            .ok_or(Denied::UnusableCredential)?;
        if !self.replay.available(&verified.nonce())
            || verified.scopes() & self.allowed.0 != verified.scopes()
        {
            return Err(Denied::UnusableCredential);
        }
        Ok(verified)
    }

    #[cfg(test)]
    pub(super) fn exchange_signed(
        &mut self,
        live: Option<&OwnerBinding>,
        record: &[u8; 48],
        supplied: &str,
        requested: ActionSet,
        wall_ms: u64,
        now: Instant,
    ) -> Result<IssuedLease, Denied> {
        self.exchange_signed_at(live, record, supplied, requested, wall_ms, || {
            Some((wall_ms, now))
        })
    }

    pub(super) fn exchange_signed_at(
        &mut self,
        live: Option<&OwnerBinding>,
        record: &[u8; 48],
        supplied: &str,
        requested: ActionSet,
        wall_ms: u64,
        final_clock: impl FnOnce() -> Option<(u64, Instant)>,
    ) -> Result<IssuedLease, Denied> {
        let verified = self.screen_signed(live, record, supplied, wall_ms)?;
        if requested.0 != verified.scopes() {
            return Err(Denied::UnusableCredential);
        }
        let expires = verified
            .issued_at_ms()
            .checked_add(respire_spawn::owner_credential::proof::PROOF_MAX_AGE_MS)
            .ok_or(Denied::UnusableCredential)?;
        let (current_wall, now) = final_clock().ok_or(Denied::UnusableCredential)?;
        if !self.replay.observe(current_wall)
            || current_wall < verified.issued_at_ms()
            || !self.replay.consume(verified.nonce(), expires)
        {
            return Err(Denied::UnusableCredential);
        }
        let token = mint(
            &mut self.leases,
            LEASE,
            MAX_LEASES,
            requested,
            now,
            LEASE_TTL,
        )?;
        Ok(IssuedLease {
            token,
            expires_in: LEASE_TTL.as_secs(),
        })
    }

    /// Repeat under the identity fence at execution and stream boundaries.
    #[cfg(test)]
    pub(super) fn authorize(
        &mut self,
        live: Option<&OwnerBinding>,
        supplied: &str,
        action: OwnerAction,
        now: Instant,
    ) -> Result<(), Denied> {
        self.ticket(live, supplied, action, now).map(|_| ())
    }

    pub(super) fn ticket(
        &mut self,
        live: Option<&OwnerBinding>,
        supplied: &str,
        action: OwnerAction,
        now: Instant,
    ) -> Result<LeaseTicket, Denied> {
        self.check_live(live)?;
        let fingerprint = digest(LEASE, supplied).ok_or(Denied::UnusableCredential)?;
        let ticket = LeaseTicket {
            fingerprint,
            action,
        };
        self.recheck_ticket(live, &ticket, action, now)?;
        Ok(ticket)
    }

    pub(super) fn recheck_ticket(
        &mut self,
        live: Option<&OwnerBinding>,
        ticket: &LeaseTicket,
        required: OwnerAction,
        now: Instant,
    ) -> Result<(), Denied> {
        self.check_live(live)?;
        if ticket.action != required {
            return Err(Denied::ActionNotAllowed);
        }
        let entry = self
            .leases
            .get(&ticket.fingerprint)
            .filter(|entry| entry.expires > now)
            .ok_or(Denied::UnusableCredential)?;
        if entry.scopes.0 & required.flag() == 0 {
            return Err(Denied::ActionNotAllowed);
        }
        Ok(())
    }
}

fn mint(
    entries: &mut HashMap<Fingerprint, Entry>,
    prefix: &str,
    cap: usize,
    scopes: ActionSet,
    now: Instant,
    ttl: Duration,
) -> Result<String, Denied> {
    entries.retain(|_, entry| entry.expires > now);
    if entries.len() >= cap {
        return Err(Denied::Busy);
    }
    let token = format!("{prefix}{}", respire_app::memory::crypto::random_hex(32));
    let key = Sha256::digest(token.as_bytes()).into();
    if entries.contains_key(&key) {
        return Err(Denied::Busy);
    }
    entries.insert(
        key,
        Entry {
            scopes,
            expires: now + ttl,
        },
    );
    Ok(token)
}

fn digest(prefix: &str, token: &str) -> Option<Fingerprint> {
    let body = token.strip_prefix(prefix)?;
    if body.len() != 64
        || !body
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return None;
    }
    Some(Sha256::digest(token.as_bytes()).into())
}

#[cfg(test)]
mod tests;
