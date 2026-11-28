//! Read the existing native login confirmation checkpoint under the host's locks.
use super::*;

pub fn verify_login_merge_offer(ownership: &AuthOwnership, identity: &HostAuthIdentity,
    id: &str, key: &str) -> Result<()> {
    crate::service::require_profile_change_host()?;
    ensure!(crate::service::main_data_dir().canonicalize()? == ownership.root
        && crate::service::data_dir().canonicalize()? == ownership.profile, "login selection changed");
    verify_locked_auth_identity(&ownership.root, &ownership.profile, identity)?;
    let journal = load_journal_metadata()?;
    let index = owned_account_index(&journal, &ownership.root, &ownership.profile, id, key)?;
    let record = &journal.operations[index];
    let active = journal.active.as_ref().context("no active login")?;
    let account = record.account.as_ref().context("no account operation")?;
    let login = account.login.as_ref().context("no login progress")?;
    let base = account_fingerprint(ownership, identity, false, &account.addr, &account.user)?;
    let intent = (false, false, true);
    ensure!(record.fingerprint == digest(&serde_json::to_vec(&(&base, intent))?)
        && account.authorization.is_some() && !login.migrate_vault, "login merge intent changed");
    ensure!(record.phase == AuthPhase::Authorized && !account.register
        && account.source_identity == *identity && record.profile == ownership.profile
        && active.operation_id.as_deref() == Some(id) && active.alias == record.alias
        && active.directory == ownership.profile && !active.retired
        && login.offer_local_merge && !login.migrate_local && login.merge_local.is_none()
        && login.merge_target.is_some() && login.required_inputs.as_slice() == ["merge_local"],
        "no pending login merge confirmation");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_owned_pending_login_offer_is_visible() -> Result<()> {
        let isolated = crate::test_lock::Isolate::new()?;
        let _keyring = crate::migration::memory_keyring_for_test();
        let root = isolated.path().canonicalize()?;
        let ownership = AuthOwnership::acquire(&root, &root)?;
        let identity = HostAuthIdentity::Absent;
        let id = uuid::Uuid::new_v4().to_string();
        let (mut journal, index, _) = admit_account_with_intent(&ownership, &identity, &id,
            "fixture-key", false, "http://127.0.0.1", "fixture", Some((false, false, true)))?;
        let check = |key| verify_login_merge_offer(&ownership, &identity, &id, key);
        assert!(check("fixture-key").is_err());
        save_authorization(&mut journal, index, json!({"user":"fixture","token":"synthetic-unused"}))?;
        progress(&mut journal, index)?.offer_merge(&root.join("target"))?;
        require_inputs(&mut journal, index, &["merge_local"])?;
        let before = std::fs::read(recovery_path())?;
        check("fixture-key")?;
        assert!(check("wrong-key").is_err());
        assert!(verify_login_merge_offer(&ownership, &identity, &uuid::Uuid::new_v4().to_string(), "fixture-key").is_err());
        let wrong_identity = HostAuthIdentity::Bound {
            account_id: digest(b"other account"),
            session_sha256: digest(b"other session"),
        };
        assert!(verify_login_merge_offer(&ownership, &wrong_identity, &id, "fixture-key").is_err());
        assert_eq!(std::fs::read(recovery_path())?, before);

        journal.operations[index].account.as_mut().context("account operation missing")?
            .source_identity = wrong_identity;
        save_journal(&mut journal)?;
        let mismatched = std::fs::read(recovery_path())?;
        assert!(check("fixture-key").is_err(), "accepted a mismatched receipt identity");
        assert_eq!(std::fs::read(recovery_path())?, mismatched);

        let mut journal: Journal = serde_json::from_slice(&before)?;
        require_inputs(&mut journal, index, &["super_password"])?;
        let changed = std::fs::read(recovery_path())?;
        assert!(check("fixture-key").is_err());
        assert_eq!(std::fs::read(recovery_path())?, changed);

        let cases: [(&str, fn(&mut Recovery, &mut LoginProgress)); 5] = [
            ("retired recovery", |active, _| active.retired = true),
            ("vault migration", |_, login| login.migrate_vault = true),
            ("existing merge decision", |_, login| login.merge_local = Some(false)),
            ("missing merge target", |_, login| login.merge_target = None),
            ("disabled merge offer", |_, login| login.offer_local_merge = false),
        ];
        for (case, mutate) in cases {
            let mut journal: Journal = serde_json::from_slice(&before)?;
            let active = journal.active.as_mut().context("active recovery missing")?;
            let login = journal.operations[index].account.as_mut().and_then(|account| account.login.as_mut())
                .context("login progress missing")?;
            mutate(active, login);
            save_journal(&mut journal)?;
            let before_check = std::fs::read(recovery_path())?;
            assert!(check("fixture-key").is_err(), "accepted {case}");
            assert_eq!(std::fs::read(recovery_path())?, before_check, "changed recovery for {case}");
        }
        Ok(())
    }
}
