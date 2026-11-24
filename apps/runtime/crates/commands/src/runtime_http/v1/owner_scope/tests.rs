use super::*;
use std::sync::{Arc, Barrier, Mutex};

fn scopes(actions: &[OwnerAction]) -> ActionSet {
    ActionSet::try_from_unique(actions).expect("valid synthetic action set")
}

fn binding() -> OwnerBinding {
    OwnerBinding {
        account: "synthetic-account".to_owned(),
        canonical_profile: std::env::temp_dir().join("respire-fake-profile"),
        instance: Uuid::new_v4(),
        generation: 1,
        vault_epoch: Uuid::new_v4(),
    }
}

#[test]
fn proof_exact_set_and_lease_exact_action_are_separate() {
    use OwnerAction::*;
    let now = Instant::now();
    let live = binding();
    let reads = scopes(&[ReadGrants]);
    let allowed = scopes(&[ReadGrants, ReadStats]);
    let mut registry = OwnerLeaseRegistry::for_verified_connection(live.clone(), allowed).unwrap();
    assert!(ActionSet::try_from_unique(&[ReadGrants, ReadGrants]).is_none());
    assert_eq!(
        registry.issue(Some(&live), scopes(&[RevokeGrant]), now),
        Err(Denied::ActionNotAllowed)
    );
    let denied = registry.issue(Some(&live), reads, now).unwrap();
    assert!(matches!(
        registry.exchange(Some(&live), &denied, allowed, now),
        Err(Denied::UnusableCredential)
    ));
    assert!(matches!(
        registry.exchange(Some(&live), &denied, reads, now),
        Err(Denied::UnusableCredential)
    ));

    let proof = registry.issue(Some(&live), reads, now).unwrap();
    assert_eq!(
        registry.authorize(Some(&live), &proof, ReadGrants, now),
        Err(Denied::UnusableCredential)
    );
    let lease = registry.exchange(Some(&live), &proof, reads, now).unwrap();
    assert_eq!(lease.expires_in, 300);
    assert!(matches!(
        registry.exchange(Some(&live), &lease.token, reads, now),
        Err(Denied::UnusableCredential)
    ));
    assert_eq!(
        registry.authorize(Some(&live), &lease.token, ReadGrants, now),
        Ok(())
    );
    assert_eq!(
        registry.authorize(Some(&live), &lease.token, RevokeGrant, now),
        Err(Denied::ActionNotAllowed)
    );
    assert_eq!(
        registry.authorize(Some(&live), "synthetic-discovery-token", ReadGrants, now),
        Err(Denied::UnusableCredential)
    );
}

#[test]
fn simultaneous_redemption_only_issues_one_lease() {
    let now = Instant::now();
    let live = binding();
    let allowed = scopes(&[OwnerAction::ReadStats]);
    let mut registry = OwnerLeaseRegistry::for_verified_connection(live.clone(), allowed).unwrap();
    let proof = registry.issue(Some(&live), allowed, now).unwrap();
    let registry = Arc::new(Mutex::new(registry));
    let rendezvous = Arc::new(Barrier::new(2));
    let successes = std::thread::scope(|scope| {
        let mut workers = Vec::new();
        for _ in 0..2 {
            let registry = Arc::clone(&registry);
            let rendezvous = Arc::clone(&rendezvous);
            let live = &live;
            let proof = &proof;
            workers.push(scope.spawn(move || {
                rendezvous.wait();
                registry
                    .lock()
                    .unwrap()
                    .exchange(Some(live), proof, allowed, now)
                    .is_ok()
            }));
        }
        workers
            .into_iter()
            .map(|worker| usize::from(worker.join().unwrap()))
            .sum::<usize>()
    });
    assert_eq!(successes, 1);
}

#[test]
fn proof_and_lease_expire_at_the_exclusive_boundary() {
    let now = Instant::now();
    let live = binding();
    let allowed = scopes(&[OwnerAction::ReadReviews]);
    let mut registry = OwnerLeaseRegistry::for_verified_connection(live.clone(), allowed).unwrap();
    let expired = registry.issue(Some(&live), allowed, now).unwrap();
    assert!(matches!(
        registry.exchange(Some(&live), &expired, allowed, now + PROOF_TTL),
        Err(Denied::UnusableCredential)
    ));

    let fresh = registry.issue(Some(&live), allowed, now).unwrap();
    let lease = registry
        .exchange(Some(&live), &fresh, allowed, now)
        .unwrap();
    assert_eq!(
        registry.authorize(
            Some(&live),
            &lease.token,
            OwnerAction::ReadReviews,
            now + LEASE_TTL - Duration::from_millis(1)
        ),
        Ok(())
    );
    assert_eq!(
        registry.authorize(
            Some(&live),
            &lease.token,
            OwnerAction::ReadReviews,
            now + LEASE_TTL
        ),
        Err(Denied::UnusableCredential)
    );
}

#[test]
fn identity_switch_or_owner_disconnect_destroys_in_flight_credentials() {
    let now = Instant::now();
    let live = binding();
    let allowed = scopes(&[OwnerAction::ReadSyncConflicts]);
    let mut registry = OwnerLeaseRegistry::for_verified_connection(live.clone(), allowed).unwrap();
    let first = registry.issue(Some(&live), allowed, now).unwrap();
    let lease = registry
        .exchange(Some(&live), &first, allowed, now)
        .unwrap();
    let pending = registry.issue(Some(&live), allowed, now).unwrap();
    let mut next = live.clone();
    next.generation += 1;
    assert_eq!(
        registry.authorize(
            Some(&next),
            &lease.token,
            OwnerAction::ReadSyncConflicts,
            now
        ),
        Err(Denied::NoOwner)
    );
    assert!(matches!(
        registry.exchange(Some(&live), &pending, allowed, now),
        Err(Denied::NoOwner)
    ));

    let mut reconnected =
        OwnerLeaseRegistry::for_verified_connection(next.clone(), allowed).unwrap();
    let fresh = reconnected.issue(Some(&next), allowed, now).unwrap();
    assert!(matches!(
        reconnected.exchange(Some(&next), &pending, allowed, now),
        Err(Denied::UnusableCredential)
    ));
    assert_eq!(reconnected.issue(None, allowed, now), Err(Denied::NoOwner));
    assert!(matches!(
        reconnected.exchange(Some(&next), &fresh, allowed, now),
        Err(Denied::NoOwner)
    ));
}

#[test]
fn account_switch_logout_lock_and_exit_end_all_prior_grants() {
    let now = Instant::now();
    let allowed = scopes(&[OwnerAction::ReadStats]);

    // These are synthetic observations at the authority boundary, not events
    // emitted by the existing Runtime or a test-only production bypass.
    for transition in ["account-switch", "logout", "vault-lock", "runtime-exit"] {
        let initial = binding();
        let mut registry =
            OwnerLeaseRegistry::for_verified_connection(initial.clone(), allowed).unwrap();
        let proof = registry.issue(Some(&initial), allowed, now).unwrap();
        let lease = registry
            .exchange(Some(&initial), &proof, allowed, now)
            .unwrap();
        let pending = registry.issue(Some(&initial), allowed, now).unwrap();
        assert!(registry
            .authorize(Some(&initial), &lease.token, OwnerAction::ReadStats, now)
            .is_ok());

        let ticket = registry
            .ticket(Some(&initial), &lease.token, OwnerAction::ReadStats, now)
            .expect("authorized fixture ticket");
        assert_eq!(
            registry.recheck_ticket(Some(&initial), &ticket, OwnerAction::RevokeGrant, now),
            Err(Denied::ActionNotAllowed)
        );

        let mut changed = initial.clone();
        changed.generation += 1;
        match transition {
            "account-switch" => {
                changed.account = "synthetic-other-account".to_owned();
                changed.canonical_profile = changed.canonical_profile.join("other");
                assert_eq!(
                    registry.authorize(Some(&changed), &lease.token, OwnerAction::ReadStats, now),
                    Err(Denied::NoOwner)
                );
            }
            "logout" | "vault-lock" => {
                assert_eq!(
                    registry.screen_proof(None, &pending, now),
                    Err(Denied::NoOwner)
                );
            }
            "runtime-exit" => {
                // The owning Runtime drops its process-local registry.
                drop(registry);
                changed.instance = Uuid::new_v4();
                registry =
                    OwnerLeaseRegistry::for_verified_connection(changed.clone(), allowed).unwrap();
            }
            _ => unreachable!("fixture transition"),
        }

        let current = if transition == "runtime-exit" {
            &changed
        } else {
            &initial
        };
        assert!(
            registry
                .recheck_ticket(Some(current), &ticket, OwnerAction::ReadStats, now)
                .is_err(),
            "{transition} must revoke already queued work"
        );
        assert!(
            registry
                .authorize(Some(current), &lease.token, OwnerAction::ReadStats, now)
                .is_err(),
            "{transition} must revoke the previously issued lease"
        );
        assert!(
            registry.screen_proof(Some(current), &pending, now).is_err(),
            "{transition} must revoke the still-pending proof"
        );
        assert!(
            registry
                .exchange(Some(current), &pending, allowed, now)
                .is_err(),
            "{transition} must not mint from a stale proof"
        );
    }
}

#[test]
fn signed_owner_proof_is_bound_to_scope_epoch_and_nonregressing_time() {
    let live = binding();
    let now = Instant::now();
    let allowed = scopes(&[OwnerAction::ReadStats, OwnerAction::ReadReviews]);
    let requested = scopes(&[OwnerAction::ReadStats]);
    let record = [7_u8; 48];
    let mut registry = OwnerLeaseRegistry::for_verified_connection(live.clone(), allowed).unwrap();
    let proof = respire_spawn::owner_credential::proof::issue(&record, requested.0, 1000)
        .expect("synthetic signed proof");
    assert!(registry
        .screen_signed(Some(&live), &record, proof.as_str(), 1000)
        .is_ok());
    // A caller may time out during validation. No replay receipt or lease may
    // be created once the final fenced clock rejects that detached request.
    assert!(registry
        .exchange_signed_at(
            Some(&live),
            &record,
            proof.as_str(),
            requested,
            1000,
            || None
        )
        .is_err());
    let wrong_record = [8_u8; 48];
    assert!(registry
        .screen_signed(Some(&live), &wrong_record, proof.as_str(), 1000)
        .is_err());
    assert!(registry
        .exchange_signed(Some(&live), &record, proof.as_str(), allowed, 1000, now)
        .is_err());
    assert!(registry
        .exchange_signed(Some(&live), &record, proof.as_str(), requested, 1000, now)
        .is_ok());
    assert!(registry
        .screen_signed(Some(&live), &record, proof.as_str(), 31_001)
        .is_err());
    assert!(registry
        .exchange_signed(Some(&live), &record, proof.as_str(), requested, 1000, now)
        .is_err());
}

#[test]
fn proof_flood_is_bounded_and_expired_capacity_is_reclaimed() {
    let now = Instant::now();
    let live = binding();
    let allowed = scopes(&[OwnerAction::ReadQueryLog]);
    let mut registry = OwnerLeaseRegistry::for_verified_connection(live.clone(), allowed).unwrap();
    for _ in 0..MAX_PROOFS {
        registry.issue(Some(&live), allowed, now).unwrap();
    }
    assert_eq!(registry.issue(Some(&live), allowed, now), Err(Denied::Busy));
    assert!(registry
        .issue(Some(&live), allowed, now + PROOF_TTL)
        .is_ok());
}
