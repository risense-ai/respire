impl PreparedLogin {
    pub fn prepare(addr: &str, authorized: &Value, super_password: String) -> Result<Self> {
        let vault = Self::fetch_vault(addr, authorized)?;
        Self::prepare_with_vault(addr, authorized, super_password, vault, None)
    }

    fn material_keys(&self) -> Result<crate::memory::SessionKeys> {
        login::raw_factor_keys(&self.session, self.login_password.as_deref(),
            Some(&self.super_password), self.legacy_secret.as_deref())
    }

    pub fn fetch_vault(addr: &str, authorized: &Value) -> Result<Value> {
        let token = authorized["token"].as_str().filter(|token| !token.is_empty()).context("authorization did not return a token")?;
        match ureq::AgentBuilder::new().redirects(0).timeout(std::time::Duration::from_secs(30)).build()
            .get(&format!("{}/api/self/vault", addr.trim().trim_end_matches('/')))
            .set("Authorization", &format!("Bearer {token}")).call() {
            Ok(response) => response.into_json().map_err(Into::into),
            Err(ureq::Error::Status(404, _)) => Ok(Value::Null),
            Err(error) => Err(error).context("could not fetch the account vault"),
        }
    }

    /// Publish only a fully migrated library's wrap, preserving its original factors.
    pub fn prepare_migration(addr: &str, authorized: &Value, password: &str, legacy_super: Option<&str>, secret_key: Option<&str>, new_super: Option<&str>) -> Result<Self> {
        crate::service::require_profile_change_host()?;
        let mut local = crate::auth::read_session_json()?;
        ensure!(local["user"] == authorized["user"], "select the migrated account before publishing its vault");
        ensure!(local["crypto_namespace"].as_str() == Some(crate::memory::crypto::RSRS_PREFIX),
            "run rsrs migrate --source <source> --account <new-account> first; changing only a vault wrap is not a full migration");
        let original = local.clone();
        let version = local["vault_version"].as_i64().context("local vault missing version")?;
        if let Some(secret) = secret_key.filter(|value| !value.is_empty()) {
            local[if version == 1 { "secret" } else { "secret_key" }] = json!(secret);
        }
        let user = authorized["user"].as_str().context("authorization did not return a user")?;
        let alias = local["keyring_account"].as_str().unwrap_or(user).to_owned();
        let super_password = if version == 1 {
            ensure!(new_super.is_none() && legacy_super.is_none(), "v1 uses the original login password and Account Secret, not a super Key");
            String::new()
        } else {
            legacy_super.filter(|value| !value.is_empty()).map(str::to_owned)
                .context("the original super Key is required; no replacement Key was generated")?
        };
        ensure!(new_super.is_none_or(|value| value == super_password),
            "migration preserves the original super Key; --new-super cannot replace it");
        let keys = login::raw_factor_keys(&local, Some(password), Some(&super_password), secret_key)?;
        let cloud = Self::fetch_vault(addr, authorized)?;
        if !cloud.is_null() {
            ensure!(cloud["version"] == local["vault_version"], "migration must preserve the original vault factors");
            let mut cloud_session = local.clone();
            for field in ["kdf_salt", "wrapped_urk", "urk_nonce"] { cloud_session[field] = cloud[field].clone(); }
            let cloud_keys = login::raw_factor_keys(&cloud_session, Some(password), Some(&super_password), secret_key)?;
            ensure!(cloud_keys.urk == keys.urk, "cloud and local vault data keys differ; original data was preserved");
        }
        let vault = json!({"version":local["vault_version"],"kdf_salt":local["kdf_salt"],"wrapped_urk":local["wrapped_urk"],"urk_nonce":local["urk_nonce"]});
        let mut factors = authorized.clone();
        factors["pass"] = json!(password);
        if version == 1 { factors["secret"] = local["secret"].clone(); }
        if version == 3 { factors["secret_key"] = local["secret_key"].clone(); }
        let mut prepared = Self::prepare_with_vault(addr, &factors, super_password, vault.clone(), Some((original, keys.urk)))?;
        if cloud != vault { prepared.migration_cloud = Some(cloud); }
        // Explicit cloud migration recalculates the authentication hash from the
        // same login password. Ordinary login never changes this stored salt.
        let salt = crate::memory::crypto::derive_auth_salt(user)?;
        let pass_hash = crate::memory::crypto::derive_pass_hash(password, &salt)?;
        prepared.session["auth_salt"] = json!(salt);
        prepared.login_password = Some(password.to_owned());
        prepared.migration_auth = Some(json!({"salt":salt,"pass_hash":pass_hash}));
        ureq::AgentBuilder::new().redirects(0).timeout(std::time::Duration::from_secs(30)).build()
            .get(&format!("{}/auth/salt", addr.trim().trim_end_matches('/'))).query("user", user).call()
            .context("cloud namespace migration requires a server supporting /auth/salt; original password was preserved")?;
        Ok(prepared)
    }

    pub fn migration_super_password(&self) -> Option<&str> {
        None
    }

    /// Publish the unchanged URK's new wrap only after host/runtime verification.
    pub fn finish_migration(&self, addr: &str, authorized: &Value) -> Result<()> {
        self.finish_migration_guarded(addr, authorized, |_, _| Ok(()))
    }

    fn finish_migration_guarded(&self, addr: &str, authorized: &Value,
        mut checkpoint: impl FnMut(&str, bool) -> Result<()>) -> Result<()> {
        if self.migration_cloud.is_none() && self.migration_auth.is_none() { return Ok(()); }
        let token = authorized["token"].as_str().context("authorization did not return a token")?;
        if let Some(original) = self.migration_cloud.as_ref() {
        ensure!(Self::fetch_vault(addr, authorized)? == *original, "cloud vault changed during migration; original local session was restored");
        let vault = json!({"version":self.session["vault_version"],"kdf_salt":self.session["kdf_salt"],"wrapped_urk":self.session["wrapped_urk"],"urk_nonce":self.session["urk_nonce"]});
        let mut request = ureq::AgentBuilder::new().redirects(0).timeout(std::time::Duration::from_secs(30)).build()
            .post(&format!("{}/api/self/vault", addr.trim().trim_end_matches('/')))
            .set("Authorization", &format!("Bearer {token}"));
        if original.is_null() { request = request.set("If-None-Match", "*"); }
        checkpoint("vault", false)?;
        let result = request.send_json(vault.clone());
        if let Err(error) = result {
            // A lost HTTP response can follow a committed write; inspect that exact wrap once.
            if !Self::fetch_vault(addr, authorized).is_ok_and(|current| current == vault) {
                return Err(anyhow!("legacy vault publication failed: {error}; retain the original super Key and library"));
            }
        }
        checkpoint("vault", true)?;
        }
        if let Some(authentication) = self.migration_auth.as_ref() {
            let agent = ureq::AgentBuilder::new().redirects(0).timeout(std::time::Duration::from_secs(30)).build();
            checkpoint("password", false)?;
            let result = agent.post(&format!("{}/api/self/password", addr.trim().trim_end_matches('/')))
                .set("Authorization", &format!("Bearer {token}")).send_json(authentication.clone());
            if let Err(error) = result {
                let confirmed: Value = agent.get(&format!("{}/auth/salt", addr.trim().trim_end_matches('/')))
                    .query("user", &self.user).call()?.into_json()?;
                ensure!(confirmed["salt"] == authentication["salt"],
                    "authentication namespace publication failed: {error}; retry explicit migration with the original password");
            }
            checkpoint("password", true)?;
        }
        Ok(())
    }

    pub fn prepare_with_vault(addr: &str, authorized: &Value, super_password: String, vault: Value, legacy: Option<(Value, [u8; 32])>) -> Result<Self> {
        Self::prepare_with_vault_inner(addr, authorized, super_password, vault, legacy, false)
    }

    fn prepare_with_vault_inner(addr: &str, authorized: &Value, super_password: String, vault: Value,
        legacy: Option<(Value, [u8; 32])>, registration_source: bool) -> Result<Self> {
        Self::prepare_candidate(addr, authorized, super_password, vault, legacy, registration_source, false, None)
    }

    // Only the owned fivekeys transaction may prepare an unauthenticated candidate.
    // Its caller verifies the kit first; an empty server keeps recovery offline.
    fn prepare_candidate(addr: &str, authorized: &Value, super_password: String, vault: Value,
        legacy: Option<(Value, [u8; 32])>, registration_source: bool, without_token: bool, fixed_directory: Option<&Path>) -> Result<Self> {
        Self::prepare_candidate_with_material(addr, authorized, super_password, vault, legacy,
            registration_source, without_token, fixed_directory, None)
    }

    fn prepare_candidate_with_material(addr: &str, authorized: &Value, super_password: String, vault: Value,
        legacy: Option<(Value, [u8; 32])>, registration_source: bool, without_token: bool, fixed_directory: Option<&Path>,
        original_material: Option<&respire_protocol::host_control::HostAuthSessionMaterial>) -> Result<Self> {
        crate::service::require_profile_change_host()?;
        let user = authorized["user"].as_str().context("authorization did not return a user")?.to_owned();
        let base = addr.trim().trim_end_matches('/');
        let unbound_recovery = without_token && addr.trim().is_empty() && user.is_empty();
        ensure!(!user.is_empty() || unbound_recovery, "authorization did not return a user");
        let token = authorized["token"].as_str().filter(|token| !token.is_empty());
        ensure!(without_token || token.is_some(), "authorization did not return a token");
        ensure!(!unbound_recovery || token.is_none(), "offline recovery cannot carry a token");
        let version = vault["version"].as_i64().context("vault missing version")?;
        ensure!((1..=4).contains(&version), "unsupported vault version; original factors were preserved");
        ensure!(!unbound_recovery || matches!(version, 1 | 2 | 4), "unsupported offline recovery kit version");
        let salt = vault["kdf_salt"].as_str().context("vault missing kdf_salt")?;
        let wrapped = vault["wrapped_urk"].as_str().context("vault missing wrapped_urk")?;
        let nonce = vault["urk_nonce"].as_str().context("vault missing urk_nonce")?;
        let current = crate::service::data_dir();
        let mut directory = if let Some(directory) = fixed_directory { directory.to_path_buf() }
        else if unbound_recovery {
            // Match the original fivekeys selection without inventing an account.
            if crate::service::session_user_of_dir(&current).is_empty() { current.clone() }
            else {
                let main = crate::service::main_data_dir();
                ensure!(crate::service::session_user_of_dir(&main).is_empty(),
                    "offline recovery cannot replace a named account; select its original local library");
                main
            }
        }
        else if (registration_source && crate::service::session_user_of_dir(&current).is_empty())
            || (legacy.is_some() && crate::service::session_user_of_dir(&current) == user) {
            current.clone()
        } else { login_directory(base, &user)? };
        let original = read_original(&directory)?;
        let mut session: Value = match original.as_deref() {
            Some(bytes) => serde_json::from_slice(bytes).context("target account session is invalid; original files were preserved")?,
            None => json!({}),
        };
        ensure!(session.is_object(), "target account session must be an object");
        let previous_alias = session["keyring_account"].as_str().unwrap_or(&user);
        let supplied_password = authorized["pass"].as_str().filter(|value| version == 1 || !value.is_empty()).map(str::to_owned);
        let login_password = supplied_password;
        let account_secret = authorized["secret"].as_str().filter(|value| !value.is_empty()).map(str::to_owned);
        let secret_key = authorized["secret_key"].as_str().filter(|value| !value.is_empty()).map(str::to_owned);
        let verified = match version {
            1 => crate::memory::SessionKeys::unlock(
                login_password.as_deref().context("original v1 login password is required")?,
                account_secret.as_deref().context("original v1 Account Secret is required")?, salt, wrapped, nonce),
            2 => crate::memory::SessionKeys::unlock_super(&super_password, salt, wrapped, nonce),
            3 => crate::memory::SessionKeys::unlock_vault(&super_password,
                secret_key.as_deref().context("original Secret Key is required; select the explicitly migrated profile")?,
                salt, wrapped, nonce),
            _ => crate::memory::SessionKeys::unlock_v4(&super_password, salt, wrapped, nonce),
        }.context("original decryption factors do not unlock this account; the original account was preserved")?;
        if !unbound_recovery && fixed_directory.is_none() && original.is_none() && legacy.is_none() {
            let plain = crate::service::account_dir(&user)?;
            let suffix = hex::encode(sha2::Sha256::digest(format!("{base}\n{user}").as_bytes()));
            let isolated = crate::service::account_dir(&format!("{user}-{}", &suffix[..16]))?;
            for candidate in [current, crate::service::main_data_dir(), plain, isolated] {
                if read_original(&candidate)?.is_none() && library_accepts_keys(&candidate, &verified)? {
                    directory = candidate;
                    break;
                }
            }
        }
        ensure!(session["user"].as_str().is_none_or(|owner| owner.is_empty() || owner == user), "destination belongs to another account");
        if let Some((original, urk)) = legacy.as_ref() {
            ensure!(session == *original && verified.urk == *urk, "legacy session changed or migration would replace its data key");
        }
        let database = crate::service::database_path(&directory)?;
        if database.is_file() {
            let database = rusqlite::Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            let has_memories: bool = database.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='memories')", [], |row| row.get(0))?;
            if has_memories {
                let count: i64 = database.query_row("SELECT count(*) FROM memories WHERE ciphertext <> ''", [], |row| row.get(0))?;
                if count > 0 {
                    let same_wrap = session["kdf_salt"] == vault["kdf_salt"] && session["wrapped_urk"] == vault["wrapped_urk"] && session["urk_nonce"] == vault["urk_nonce"];
                    if legacy.is_none() && !same_wrap {
                        // Full logout removes the wrap, and a super-password change can
                        // rewrap the same URK. Prove compatibility against local data.
                        if !library_accepts_keys(&directory, &verified)? {
                            let original_keys = login::supplied_keys(original.as_deref()
                                .context("original session missing; compatibility could not be verified")?, original_material)?;
                            ensure!(original_keys.urk != verified.urk
                                && library_accepts_keys(&directory, &original_keys)?,
                                "local key compatibility could not be verified; original data was preserved");
                            return Err(LocalKeyMaterialRejected { directory: directory.clone(),
                                original_sha256: digest(original.as_deref().context("original session missing")?) }.into());
                        }
                    }
                }
            }
        }
        let alias = format!("login-{}", uuid::Uuid::new_v4().simple());
        let legacy_secret = match version {
            1 => Some(account_secret.context("original v1 Account Secret is required")?),
            3 => Some(secret_key.context("original Secret Key is required")?),
            _ => None,
        };
        for field in ["pass", "super", "secret", "secret_key", "legacy_secret_backend"] {
            session.as_object_mut().map(|object| object.remove(field));
        }
        session["user"] = json!(user);
        session["addr"] = json!(base);
        if let Some(token) = token {
            session["token"] = json!(token);
            session["session_id"] = authorized["session_id"].clone();
        } else if let Some(object) = session.as_object_mut() {
            object.remove("token");
            object.remove("session_id");
        }
        let local_wrap = [session["kdf_salt"].clone(), session["wrapped_urk"].clone(), session["urk_nonce"].clone()];
        session["vault_version"] = json!(version);
        session["kdf_salt"] = vault["kdf_salt"].clone();
        session["wrapped_urk"] = vault["wrapped_urk"].clone();
        session["urk_nonce"] = vault["urk_nonce"].clone();
        if !unbound_recovery && session["crypto_namespace"].as_str() == Some(crate::memory::crypto::RSRS_PREFIX)
            && !wrapped.starts_with(crate::memory::crypto::RSRS_PREFIX) {
            // A local explicit namespace migration can precede cloud rewrapping.
            // Keep its current namespace after verifying the cloud URK above.
            ensure!(local_wrap[1].as_str().is_some_and(|value| value.starts_with(crate::memory::crypto::RSRS_PREFIX)),
                "migrated local wrap is missing; original account was preserved");
            session["kdf_salt"] = local_wrap[0].clone();
            session["wrapped_urk"] = local_wrap[1].clone();
            session["urk_nonce"] = local_wrap[2].clone();
        }
        if unbound_recovery {
            // Offline recovery carries only the supplied kit, never stale cloud identity.
            session.as_object_mut().context("offline recovery session must be an object")?.retain(|field, _|
                matches!(field.as_str(), "user" | "addr" | "vault_version" | "kdf_salt" | "wrapped_urk" | "urk_nonce")
);
        }
        session["keyring_account"] = json!(alias);
        Ok(Self { user, directory, original, session, super_password, login_password, legacy_secret, alias, recovery_written: false, migration_cloud: None, migration_auth: None })
    }

    pub fn commit(&mut self) -> Result<()> {
        ensure!(read_original(&self.directory)? == self.original, "target session changed during login; original account was preserved");
        let result = self.commit_inner();
        if let Err(error) = result {
            return match self.rollback() { Ok(()) => Err(error), Err(rollback) => Err(error.context(format!("login rollback failed: {rollback:#}"))) };
        }
        Ok(())
    }

    fn commit_inner(&mut self) -> Result<()> {
        self.commit_inner_for(None)
    }

    fn commit_inner_for(&mut self, operation_id: Option<&str>) -> Result<()> {
        let _legacy_journal_lock = if operation_id.is_none() { Some(auth_journal_lock()?) } else { None };
        let mut journal = load_journal()?;
        let account_index = match operation_id {
            Some(id) => {
                let index = journal.operations.iter().position(|record| record.operation_id == id
                    && record.account.is_some() && record.phase == AuthPhase::Authorized)
                    .context("account admission is missing")?;
                ensure!(journal.active.as_ref().and_then(|active| active.operation_id.as_deref()) == Some(id),
                    "account commit does not own recovery");
                Some(index)
            }
            None => { ensure!(journal.active.is_none(), "interrupted login must be recovered before another commit"); None }
        };
        let config = match std::fs::read(crate::service::client_config_path()) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        if let Some(active) = &journal.active {
            ensure!(active.backends.is_empty() && active.pass_backends.is_empty()
                && active.totp_backends.is_empty() && active.legacy_secret_backends.is_empty(),
                "host_recovery_required: historical credential cleanup metadata retained");
        }
        let staged_backends = if account_index.is_some() {
            journal.active.as_ref().context("account recovery is missing")?.backends.clone()
        } else { Vec::new() };
        let staged_pass_backends = if account_index.is_some() {
            journal.active.as_ref().context("account recovery is missing")?.pass_backends.clone()
        } else { Vec::new() };
        let staged_legacy_secret_backends = if account_index.is_some() {
            journal.active.as_ref().context("account recovery is missing")?.legacy_secret_backends.clone()
        } else { Vec::new() };
        let staged_totp_backends = if account_index.is_some() {
            journal.active.as_ref().context("account recovery is missing")?.totp_backends.clone()
        } else { Vec::new() };
        let directory_existed = if account_index.is_some() {
            journal.active.as_ref().context("account recovery is missing")?.directory_existed
        } else { self.directory.exists() };
        let original_backups = journal.active.as_ref().map(|active| active.original_backups.clone()).unwrap_or_default();
        journal.active = Some(Recovery { directory: self.directory.clone(), original: self.original.clone(), config,
            original_backups,
            alias: self.alias.clone(), backends: staged_backends, pass_backends: staged_pass_backends,
            legacy_secret_backends: staged_legacy_secret_backends, totp_backends: staged_totp_backends,
            directory_existed, operation_id: operation_id.map(str::to_owned), owns_config: account_index.is_some(), ..Recovery::default() });
        if let Some(index) = account_index {
            journal.operations[index].account.as_mut().context("account receipt is missing")?.target = Some(self.directory.clone());
            checkpoint_account_candidate(&mut journal, index, &self.session)?;
        }
        save_journal(&mut journal)?;
        self.recovery_written = true;
        let backend = self.session["keyring_backend"].as_str()
            .context("host must persist login credentials before session commit")?;
        require_persistent_auth_backend(backend)?;
        ensure!(self.session["keyring_account"] == self.alias,
            "host login credential owner changed before commit");
        ensure!(["pass", "super", "secret", "secret_key"].iter().all(|field| self.session.get(*field).is_none()),
            "host must normalize login candidate factors before commit");
        if let Some(index) = account_index {
            journal.operations[index].backend = Some(backend.to_owned());
        }
        save_journal(&mut journal)?;
        if let Some(index) = account_index {
            checkpoint_account_candidate(&mut journal, index, &self.session)?;
            journal.operations[index].phase = AuthPhase::Prepared;
            save_journal(&mut journal)?;
        }
        if !self.directory.exists() {
            let parent = self.directory.parent().context("account directory has no parent")?;
            std::fs::create_dir_all(parent)?;
            std::fs::create_dir(&self.directory)?;
        }
        private_write(&self.directory.join("session.json"), &serde_json::to_vec_pretty(&self.session)?)?;
        crate::service::set_data_dir(&self.directory.to_string_lossy())?;
        if let Some(index) = account_index {
            journal.operations[index].phase = AuthPhase::AwaitingVerification;
            save_journal(&mut journal)?;
        }
        Ok(())
    }

    pub fn rollback(&mut self) -> Result<()> {
        if self.recovery_written {
            recover_interrupted()?;
            self.recovery_written = false;
        }
        Ok(())
    }

    pub fn complete(&self) -> Result<()> {
        let _journal_lock = auth_journal_lock()?;
        let mut journal = load_journal()?;
        let active = journal.active.as_ref().context("login recovery is missing")?;
        ensure!(active.operation_id.is_none() && active.alias == self.alias && active.directory == self.directory,
            "login completion does not own the active transaction");
        retire_active(&mut journal).context("login verified but recovery record could not be retired")
    }
}
