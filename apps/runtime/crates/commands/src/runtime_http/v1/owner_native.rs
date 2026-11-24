//! Instance-owned native credential plus its in-memory scoped leases.
use respire_spawn::owner_credential::{
    create_owner_credential, read_owner_credential, OwnerCredential, OwnerRecord, OWNER_RECORD_LEN,
};
use std::{io, path::Path, time::Instant};

use super::owner_scope::{
    ActionSet, Denied, IssuedLease, LeaseTicket, OwnerAction, OwnerBinding, OwnerLeaseRegistry,
};

pub(super) struct NativeOwner {
    binding: OwnerBinding,
    registry: OwnerLeaseRegistry,
    credential: OwnerCredential,
    record: OwnerRecord,
}

impl NativeOwner {
    /// Call only while the authoritative serving identity and unlocked vault
    /// are fenced. The provider rejects unverified filesystem custody.
    pub(super) fn establish(base: &Path, live: OwnerBinding) -> io::Result<Self> {
        let registry = new_registry(live.clone())?;
        let record = fresh_record()?;
        let credential = create_owner_credential(base, &live.instance.to_string(), &record)?;
        Ok(Self {
            binding: live,
            registry,
            credential,
            record,
        })
    }

    /// The prior bearer and proof state are invalid before the provider can
    /// publish a fresh epoch, even if publishing fails.
    pub(super) fn synchronize(&mut self, live: &OwnerBinding) -> io::Result<()> {
        if &self.binding == live {
            return Ok(());
        }
        self.registry.revoke();
        if self.binding.instance != live.instance {
            return Err(io::Error::from(io::ErrorKind::PermissionDenied));
        }
        let registry = new_registry(live.clone())?;
        let record = fresh_record()?;
        self.credential.publish(&record)?;
        self.binding = live.clone();
        self.registry = registry;
        self.record = record;
        Ok(())
    }

    pub(super) fn matches(&self, live: &OwnerBinding) -> bool {
        &self.binding == live
    }

    pub(super) fn provider_live(&self, base: &Path) -> bool {
        read_owner_credential(base, &self.binding.instance.to_string())
            .is_ok_and(|record| record.as_bytes() == self.record.as_bytes())
    }

    pub(super) fn ticket(
        &mut self,
        live: &OwnerBinding,
        token: &str,
        action: OwnerAction,
    ) -> Result<LeaseTicket, Denied> {
        self.registry
            .ticket(Some(live), token, action, Instant::now())
    }

    pub(super) fn recheck(
        &mut self,
        live: &OwnerBinding,
        ticket: &LeaseTicket,
        action: OwnerAction,
    ) -> Result<(), Denied> {
        self.registry
            .recheck_ticket(Some(live), ticket, action, Instant::now())
    }

    pub(super) fn screen(
        &mut self,
        live: &OwnerBinding,
        proof: &str,
        wall_ms: u64,
    ) -> Result<(), Denied> {
        self.registry
            .screen_signed(Some(live), self.record.as_bytes(), proof, wall_ms)
            .map(|_| ())
    }

    pub(super) fn exchange(
        &mut self,
        live: &OwnerBinding,
        proof: &str,
        scopes: ActionSet,
        wall_ms: u64,
        final_clock: impl FnOnce() -> Option<(u64, Instant)>,
    ) -> Result<IssuedLease, Denied> {
        self.registry.exchange_signed_at(
            Some(live),
            self.record.as_bytes(),
            proof,
            scopes,
            wall_ms,
            final_clock,
        )
    }
}

fn fresh_record() -> io::Result<OwnerRecord> {
    let fresh = respire_app::memory::crypto::random_bytes(OWNER_RECORD_LEN);
    let bytes: [u8; OWNER_RECORD_LEN] = fresh
        .try_into()
        .map_err(|_| io::Error::other("owner credential unavailable"))?;
    Ok(OwnerRecord::from_bytes(bytes))
}

fn new_registry(binding: OwnerBinding) -> io::Result<OwnerLeaseRegistry> {
    use OwnerAction::*;
    let allowed = ActionSet::try_from_unique(&[
        ReadGrants,
        RevokeGrant,
        ReadStats,
        ReadQueryLog,
        ReadReviews,
        ReadSyncConflicts,
    ])
    .ok_or_else(|| io::Error::other("owner action set unavailable"))?;
    OwnerLeaseRegistry::for_verified_connection(binding, allowed)
        .map_err(|_| io::Error::from(io::ErrorKind::PermissionDenied))
}
