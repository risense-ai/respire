//! Launch, attest, and drain only a directly owned Runtime child.
use super::*;

pub(super) struct Serving {
    pub(super) child: Child,
    pub(super) launch: Launch,
    pub(super) health: Health,
}
impl Drop for Serving {
    fn drop(&mut self) {
        let _ = self.child.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
pub(super) fn rotate(
    open: &Open,
    previous: Option<&Launch>,
    takeover: &LibraryLock,
    installation: LibraryLock,
    library: LibraryLock,
    grant: Launch,
) -> Result<Serving> {
    open.verify()?;
    let dir = grant.installation_root.join("runtime/autostart/launch");
    fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    let path = dir.join(format!("launch-{}.json", grant.nonce));
    let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
    file.write_all(&serde_json::to_vec(&grant)?)?;
    file.sync_all()?;
    respire_spawn::durable::sync_directory(&dir)?;
    backup_restart::reserve(&grant.installation_root, &grant, previous, takeover, &installation, &library)?;
    drop(library);
    drop(installation);
    // proc/self/exe pins the running image even during package replacement.
    let mut child = Command::new("/proc/self/exe")
        .env_clear()
        .env("RSRS_DATA_DIR", &grant.installation_root)
        .env("RSRS_RPC_PORT", grant.port.to_string())
        .env("LD_LIBRARY_PATH", std::env::var_os("LD_LIBRARY_PATH").unwrap_or_default())
        .env("HOME", std::env::var_os("HOME").context("missing owner home")?)
        .env("RSRS_RUNTIME_BOOT_TOKEN", format!("launch-v1:{}", grant.nonce))
        .args(["serve", "--host", "127.0.0.1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let started = (|| -> Result<Health> {
        let mut output = child.stdout.take().context("child output pipe")?;
        std::thread::Builder::new().name("backup-child-output".into()).spawn(move || {
            let _ = std::io::copy(&mut output, &mut std::io::sink());
        })?;
        let input = child.stdin.as_mut().context("child credential pipe")?;
        let unlock = open.unlock(&grant);
        let frame = if previous.is_none() { ChildLaunch::Stage(unlock) } else { ChildLaunch::Reopen(unlock) };
        serde_json::to_writer(&mut *input, &frame)?;
        input.write_all(b"\n")?;
        input.flush()?;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            ensure!(
                child.try_wait()?.is_none() && Instant::now() < deadline,
                "reserved Runtime startup failed"
            );
            if let Ok(h) = net_rpc::health() {
                if h.pid == child.id()
                    && h.store_ready
                    && h.instance_id == grant.instance_id
                    && h.image_sha256 == grant.image_sha256
                    && h.account_id == grant.account_id
                    && net_rpc::instance_is_running(&h)?
                    && net_rpc::pid_listening_on(grant.port) == Some(h.pid)
                {
                    net_rpc::request_scope(&h)?;
                    return Ok(h);
                }
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    })();
    match started {
        Ok(health) => Ok(Serving { child, launch: grant, health }),
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(e)
        }
    }
}
impl Serving {
    pub(super) fn verify(&mut self) -> Result<()> {
        ensure!(
            self.child.try_wait()?.is_none()
                && net_rpc::instance_is_running(&self.health)?
                && net_rpc::pid_listening_on(self.launch.port) == Some(self.child.id()),
            "serving owner changed"
        );
        Ok(())
    }
    pub(super) fn drain(&mut self, agent: &ureq::Agent, token: &str) -> Result<()> {
        self.verify()?;
        let h = &self.health;
        let stop: Value = agent
            .post(&format!("{}/api/runtime/stop", net_rpc::rpc_base_url()))
            .set("Authorization", &format!("Bearer {token}"))
            .set("X-Respire-Lifecycle", "coordinator-v1")
            .send_json(json!({"instance_id":h.instance_id,"installation_id":h.installation_id,
                "account_id":h.account_id,"generation":h.generation}))?
            .into_json()?;
        ensure!(
            stop["ok"] == true && stop["state"] == "accepted" && stop["operation_id"].is_string(),
            "drain refused"
        );
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = self.child.try_wait()? {
                ensure!(status.success(), "owned child drain failed");
                break;
            }
            ensure!(Instant::now() < until, "owned drain exceeded deadline");
            std::thread::sleep(Duration::from_millis(25));
        }
        let path = self.launch.installation_root.join("runtime").join(format!("stop-{}.json", h.instance_id));
        let rec: Value = serde_json::from_slice(
            &crate::startup_identity::read_private_file(&path)?
                .context("owned final drain receipt missing")?,
        )?;
        ensure!(
            rec["instance_id"] == h.instance_id
                && rec["pid"] == h.pid
                && rec["state"] == "drained"
                && rec["operation_id"] == stop["operation_id"]
                && rec["unfinished"].as_array().is_some_and(Vec::is_empty),
            "drain receipt mismatch"
        );
        Ok(())
    }
    pub(super) fn claim(
        &mut self,
        agent: &ureq::Agent,
        input: &Confirm,
        pin: &ArchiveTarget,
        account: &str,
    ) -> Result<Option<backup_grant::VerifiedGrant>> {
        self.verify()?;
        let h = &self.health;
        let root = &self.launch.profile;
        if !(input.action == "restore"
            && input.space == "main"
            && input.instance_id.to_string() == h.instance_id
            && input.target_sha256 == hex(&sha(pin.path().as_os_str().as_encoded_bytes()))
            && input.target_identity_sha256 == hex(&pin.identity_digest()?)
            && digest(&input.archive_sha256).is_ok())
        {
            return Ok(None);
        }
        let scope = Scope {
            operation: *input.operation_id.as_bytes(),
            archive: digest(&input.archive_sha256)?,
            target: digest(&input.target_sha256)?,
            target_identity: pin.identity_digest()?,
            instance: *input.instance_id.as_bytes(),
            generation: h.generation,
            account: sha(account.as_bytes()),
            profile: sha(root.as_os_str().as_encoded_bytes()),
            supervisor_pid: std::process::id(),
            supervisor_birth: self.launch.supervisor_birth.parse()?,
        };
        let owner = read_owner_credential(&root.join("runtime"), &h.instance_id)?;
        let rx = Receiver::arm(&owner, scope)?;
        let challenge = rx.challenge();
        let claim = ArchiveClaim {
            operation: scope.operation,
            archive: scope.archive,
            target: scope.target,
            target_identity: scope.target_identity,
            instance: scope.instance,
            generation: scope.generation,
            inspect_only: false,
            query_only: false,
            receiver_binding: Some(backup_grant::request_binding(&challenge, &scope)),
        };
        let proof = archive_proof::issue(
            owner.as_bytes(),
            claim,
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis().try_into()?,
        )
        .context("cannot sign backup claim")?;
        let response=agent.post(&format!("{}/api/runtime/private-backup/claim",net_rpc::rpc_base_url()))
            .set("x-respire-archive-proof",&proof)
            .send_json(json!({"schema_version":3,"handoff":{"supervisor_pid":scope.supervisor_pid,"supervisor_birth":scope.supervisor_birth,
                "challenge":challenge},"operation_id":input.operation_id.to_string(),"archive_sha256":input.archive_sha256,
                "target_sha256":input.target_sha256,"target_identity_sha256":input.target_identity_sha256}));
        let issued: Value = match response {
            Ok(response) => response.into_json()?,
            Err(ureq::Error::Status(401 | 404, _)) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        ensure!(
            issued["operation_id"] == input.operation_id.to_string() && issued["state"] == "claimed",
            "incorrect claim receipt"
        );
        let epoch: Uuid = issued["vault_epoch"].as_str().context("missing Vault epoch")?.parse()?;
        Ok(Some(rx.accept(
            digest(issued["intent_sha256"].as_str().context("missing intent")?)?,
            *epoch.as_bytes(),
            &serde_json::from_value(issued["grant"].clone())?,
        )?))
    }
}
