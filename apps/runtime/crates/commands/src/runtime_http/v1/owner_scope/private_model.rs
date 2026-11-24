use super::{Denied, OwnerBinding, OwnerLeaseRegistry};
use respire_spawn::owner_credential::model_proof::{self, ModelAction, ModelClaim};

impl OwnerLeaseRegistry {
    /// The caller supplies the live, fenced binding and current server wall
    /// clock. Redemption is exact and one-shot; it never issues an HTTP lease.
    pub(in crate::runtime_http::v1) fn consume_private_model(
        &mut self,
        live: Option<&OwnerBinding>,
        record: &[u8; 48],
        action: ModelAction,
        model_id: &str,
        proof: &str,
        now_ms: u64,
    ) -> Result<(), Denied> {
        self.check_live(live)?;
        let live = live.ok_or(Denied::NoOwner)?;
        if !self.replay.observe(now_ms) {
            return Err(Denied::UnusableCredential);
        }
        let claim = ModelClaim {
            action,
            model_id,
            instance: *live.instance.as_bytes(),
            generation: live.generation,
        };
        let verified =
            model_proof::verify(record, claim, proof, now_ms).ok_or(Denied::UnusableCredential)?;
        if !self.replay.consume(verified.nonce, verified.expires_at_ms) {
            return Err(Denied::UnusableCredential);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_http::v1::owner_scope::{ActionSet, OwnerAction};

    #[test]
    fn private_model_proof_one_shot_never_mints_a_lease() {
        let root = tempfile::tempdir().unwrap();
        let live = OwnerBinding {
            account: "synthetic".into(),
            canonical_profile: root.path().canonicalize().unwrap(),
            instance: uuid::Uuid::new_v4(),
            generation: 8,
            vault_epoch: uuid::Uuid::new_v4(),
        };
        let allowed = ActionSet::try_from_unique(&[OwnerAction::ReadGrants]).unwrap();
        let mut registry = OwnerLeaseRegistry::for_verified_connection(live.clone(), allowed)
            .unwrap_or_else(|_| panic!("invalid test owner"));
        let record = [8; 48];
        let claim = ModelClaim {
            action: ModelAction::Remove,
            model_id: "m3",
            instance: *live.instance.as_bytes(),
            generation: live.generation,
        };
        let proof = model_proof::issue(&record, claim, 100).unwrap();
        assert!(matches!(
            registry.consume_private_model(
                Some(&live),
                &record,
                ModelAction::Publish,
                "m3",
                proof.as_str(),
                101
            ),
            Err(Denied::UnusableCredential)
        ));
        assert!(registry
            .consume_private_model(
                Some(&live),
                &record,
                ModelAction::Remove,
                "m3",
                proof.as_str(),
                102
            )
            .is_ok());
        assert!(registry.leases.is_empty());
        assert!(matches!(
            registry.consume_private_model(
                Some(&live),
                &record,
                ModelAction::Remove,
                "m3",
                proof.as_str(),
                103
            ),
            Err(Denied::UnusableCredential)
        ));
        let changed = OwnerBinding {
            vault_epoch: uuid::Uuid::new_v4(),
            ..live
        };
        assert!(matches!(
            registry.consume_private_model(
                Some(&changed),
                &record,
                ModelAction::Remove,
                "m3",
                proof.as_str(),
                104
            ),
            Err(Denied::NoOwner)
        ));
    }
}
