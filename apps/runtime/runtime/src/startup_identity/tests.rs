use super::*;

#[test]
fn completion_requires_exact_verified_marker_not_starting_or_paused() -> Result<()> {
    for phase in ["Paused", "Starting", "Unknown"] {
        let bytes = serde_json::to_vec(&json!({"phase":phase}))?;
        assert!(verify_completion_marker(&bytes, &hash(&bytes)).is_err());
    }
    let bytes = serde_json::to_vec(&json!({"phase":"Verified"}))?;
    verify_completion_marker(&bytes, &hash(&bytes))?;
    assert!(verify_completion_marker(&bytes, &"0".repeat(64)).is_err());
    assert!(verify_completion_marker(b"not-json", &hash(b"not-json")).is_err());
    Ok(())
}


fn fixture() -> (LaunchContext, LaunchGrant) {
    let context = LaunchContext {
        root: PathBuf::from("fixture-root"),
        profile: PathBuf::from("fixture-profile"),
        account_id: "a".repeat(64),
        sync_generation: 0,
        image_path: PathBuf::from("fixture-runtime"),
        image_sha256: "b".repeat(64),
        port: 19169,
    };
    let grant = LaunchGrant {
        schema_version: 1,
        protocol_version: respire_commands::rpc_worker::PROTOCOL_V,
        nonce: "12345678-1234-4234-8234-123456789abc".to_owned(),
        instance_id: "12345678-1234-4234-8234-123456789abc".to_owned(),
        installation_root: context.root.clone(),
        installation_id: hash(context.root.display().to_string().as_bytes()),
        profile: context.profile.clone(),
        account_id: context.account_id.clone(),
        sync_generation: 0,
        image_path: context.image_path.clone(),
        image_sha256: context.image_sha256.clone(),
        supervisor_pid: if std::process::id() == 1 { 2 } else { 1 },
        supervisor_birth: "fixture-birth".to_owned(),
        managed_id: None,
        registration_sha256: None,
        maintenance: None,
        port: context.port,
        issued_at_ms: 1000,
        expires_at_ms: 61_000,
    };
    (context, grant)
}

mod detached;

#[test]
fn detached_grant_accepts_only_its_exact_context() -> Result<()> {
    detached::run()
}

#[test]
fn managed_and_maintenance_grants_bind_exact_control_files() -> Result<()> {
    // Re-enter only this test: environment and consumed startup state stay process-local.
    if let Ok(selection) = std::env::var("RSRS_TEST_OFFLINE_SELECTION") {
        return offline_read_child(&selection)
            .map_err(|_| anyhow::anyhow!("offline-read diagnostic stage failed"));
    }
    let (context, mut grant) = fixture();
    let id = hash(
        format!(
            "respire-runtime-v1\0{}\0fixture-owner",
            context.root.display()
        )
        .as_bytes(),
    );
    let mut registration = json!({
        "schema": 1, "platform": std::env::consts::OS,
        "installation": context.root, "owner": "fixture-owner", "id": &id[..24],
        "enabled": true, "manually_stopped": false, "maintenance": false,
        "environment": { "RSRS_DATA_DIR": context.root, "RSRS_RPC_PORT": "19169" }
    });
    let bytes = serde_json::to_vec(&registration)?;
    grant.managed_id = Some(id[..24].to_owned());
    grant.registration_sha256 = Some(hash(&bytes));
    validate_grant(&grant, &context, true, &grant.nonce, 2000)?;
    validate_control(
        Some(&grant),
        &context,
        Some(&registration),
        Some(&bytes),
        None,
        None,
    )?;
    assert!(validate_control(
        None,
        &context,
        Some(&registration),
        Some(&bytes),
        None,
        None
    )
    .is_err());
    registration["enabled"] = json!(false);
    let changed = serde_json::to_vec(&registration)?;
    assert!(validate_control(
        Some(&grant),
        &context,
        Some(&registration),
        Some(&changed),
        None,
        None
    )
    .is_err());
    grant.registration_sha256 = Some(hash(&changed));
    assert!(validate_control(
        Some(&grant),
        &context,
        Some(&registration),
        Some(&changed),
        None,
        None
    )
    .is_err());
    registration["enabled"] = json!(true);
    let bytes = serde_json::to_vec(&registration)?;
    grant.registration_sha256 = Some(hash(&bytes));
    let mut marker = json!({
        "schema": 1, "id": &id[..24], "owner_pid": grant.supervisor_pid,
        "owner_birth": "controller-birth", "expected_version": "fixture-cli-version",
        "expected_profile": context.profile, "phase": "Starting", "token": "maintenance-secret"
    });
    let marker_bytes = serde_json::to_vec(&marker)?;
    grant.maintenance = Some(MaintenanceBinding {
        marker_sha256: hash(&marker_bytes),
        coordinator_id: id[..24].to_owned(),
        controller_pid: grant.supervisor_pid,
        controller_birth: "controller-birth".to_owned(),
        issuer_cli_version: "fixture-cli-version".to_owned(),
    });
    validate_control(
        Some(&grant),
        &context,
        Some(&registration),
        Some(&bytes),
        Some(&marker),
        Some(&marker_bytes),
    )?;
    assert!(validate_control(
        Some(&grant),
        &context,
        Some(&registration),
        Some(&bytes),
        None,
        None
    )
    .is_err());
    marker["phase"] = json!("Paused");
    let changed = serde_json::to_vec(&marker)?;
    assert!(validate_control(
        Some(&grant),
        &context,
        Some(&registration),
        Some(&bytes),
        Some(&marker),
        Some(&changed)
    )
    .is_err());
    grant.maintenance.as_mut().context("maintenance binding is missing")?.marker_sha256 = hash(&changed);
    assert!(validate_control(Some(&grant), &context, Some(&registration), Some(&bytes), Some(&marker), Some(&changed)).is_err());
    validate_control_for_mode(Some(&grant), &context, Some(&registration), Some(&bytes), Some(&marker), Some(&changed), true)?;
    marker["phase"] = json!("Starting");
    let changed = serde_json::to_vec(&marker)?;
    grant.maintenance.as_mut().context("maintenance binding is missing")?.marker_sha256 = hash(&changed);
    assert!(validate_control_for_mode(Some(&grant), &context, Some(&registration), Some(&bytes), Some(&marker), Some(&changed), true).is_err());
    marker["phase"] = json!("Paused");
    marker["expected_profile"] = json!("other-profile");
    let changed = serde_json::to_vec(&marker)?;
    grant.maintenance.as_mut().context("maintenance binding is missing")?.marker_sha256 = hash(&changed);
    assert!(validate_control_for_mode(Some(&grant), &context, Some(&registration), Some(&bytes), Some(&marker), Some(&changed), true).is_err());
    offline_read_registered_profiles()
        .map_err(|_| anyhow::anyhow!("offline-read registered-profile diagnostic failed"))?;
    Ok(())
}

fn offline_read_child(selection: &str) -> Result<()> {
    use respire_protocol::metadata_control::HostOfflineRead;
    let kind = HostOfflineRead::SnapshotPreflight;
    eprintln!("offline-read stage=input");
    let mut input = std::io::stdin().lock();
    let mut size = [0; 4];
    input.read_exact(&mut size)?;
    let size = u32::from_be_bytes(size) as usize;
    anyhow::ensure!(size > 0 && size <= 64 * 1024, "invalid diagnostic frame");
    let mut bytes = vec![0; size];
    input.read_exact(&mut bytes)?;
    let grant: LaunchGrant = serde_json::from_slice(&bytes)?;
    eprintln!("offline-read stage=config");
    respire_app::service::validate_client_config()?;
    eprintln!("offline-read stage=context");
    let context = offline_read_context(grant.port, kind)?;
    let root = PathBuf::from(std::env::var_os("RSRS_DATA_DIR").context("missing test root")?);
    let expected = match selection {
        "main" => root.clone(),
        "secondary" => root.join("accounts/secondary profile"),
        _ => anyhow::bail!("invalid diagnostic selection"),
    };
    let root_eq = context.root == root;
    let profile_eq = context.profile == expected && grant.profile == expected;
    let account_eq = context.account_id == hash(expected.display().to_string().as_bytes());
    let generation_eq = context.sync_generation == 0;
    eprintln!("offline-read root_eq={root_eq} profile_eq={profile_eq} account_eq={account_eq} generation_eq={generation_eq}");
    anyhow::ensure!(root_eq && profile_eq && account_eq && generation_eq,
        "diagnostic context comparison failed");
    // Path equality ignores trailing separators; identity hashes cannot ignore them.
    let representation_eq = context.root.as_os_str() == root.as_os_str()
        && context.profile.as_os_str() == expected.as_os_str();
    let repeated = respire_protocol::metadata_control::metadata_status_path(&context.profile)?;
    let normalization_stable = repeated.as_os_str() == expected.as_os_str();
    eprintln!("offline-read representation_eq={representation_eq} normalization_stable={normalization_stable}");
    anyhow::ensure!(representation_eq && normalization_stable,
        "diagnostic path representation changed");
    eprintln!("offline-read stage=grant");
    validate_grant(&grant, &context, true, &grant.nonce, now_ms()?)?;
    let mut changed = grant.clone();
    changed.profile = if selection == "main" { root.join("accounts/secondary profile") } else { root };
    anyhow::ensure!(validate_grant(&changed, &context, true, &grant.nonce, now_ms()?).is_err(),
        "changed profile accepted");
    eprintln!("offline-read stage=authority");
    verify_offline_read_authority(&grant, &context, kind)?;
    changed = grant.clone();
    changed.registration_sha256 = Some("0".repeat(64));
    anyhow::ensure!(verify_offline_read_authority(&changed, &context, kind).is_err(),
        "changed registration accepted");
    let binding = respire_protocol::ControlBinding {
        installation_root: grant.installation_root.clone(), profile: grant.profile.clone(),
        launch_id: grant.instance_id.clone(), expected_account_id: None,
    };
    eprintln!("offline-read stage=consume");
    authorize_offline_read(grant, kind)?;
    eprintln!("offline-read stage=binding");
    verify_offline_read_binding(&binding, kind)?;
    eprintln!("offline-read stage=complete");
    writeln!(std::io::stdout().lock(), "\noffline-read complete")?;
    Ok(())
}

fn offline_read_registered_profiles() -> Result<()> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let suffix = if cfg!(unix) { "data spaces % $ \" \\" } else { "data spaces % and $" };
    let root = std::env::temp_dir().join(format!("offline-read-{} {suffix}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join("accounts/secondary profile"))?;
    let root = root.canonicalize()?;
    let result = (|| -> Result<()> {
        let control = root.join("runtime/autostart");
        fs::create_dir_all(&control)?;
        let id = hash(format!("respire-runtime-v1\0{}\0offline-test-owner", root.display()).as_bytes());
        let registration = serde_json::to_vec_pretty(&json!({
            "schema": 1, "platform": std::env::consts::OS,
            "installation": root, "owner": "offline-test-owner", "id": &id[..24],
            "enabled": true, "manually_stopped": false, "maintenance": false,
            "environment": {"RSRS_DATA_DIR": root, "RSRS_RPC_PORT": "19169"}
        }))?;
        fs::write(control.join("registration.json"), &registration)?;
        let image = std::env::current_exe()?.canonicalize()?;
        let image_sha256 = hash_file(&image)?;
        for selection in ["main", "secondary"] {
            let profile = if selection == "main" { root.clone() } else { root.join("accounts/secondary profile") };
            // Main preflight runs before config writes the secondary selection.
            let config = if selection == "main" { json!({}) } else { json!({"data_dir": profile}) };
            fs::write(root.join("client.json"), serde_json::to_vec(&config)?)?;
            let (_, mut grant) = fixture();
            grant.installation_root = root.clone();
            grant.installation_id = hash(root.display().to_string().as_bytes());
            grant.account_id = hash(profile.display().to_string().as_bytes());
            grant.profile = profile;
            grant.image_path = image.clone();
            grant.image_sha256 = image_sha256.clone();
            grant.supervisor_pid = std::process::id();
            grant.supervisor_birth = process_birth(grant.supervisor_pid)?.context("missing test parent")?;
            grant.managed_id = Some(id[..24].to_owned());
            grant.registration_sha256 = Some(hash(&registration));
            grant.issued_at_ms = now_ms()?;
            grant.expires_at_ms = grant.issued_at_ms + 60_000;
            let bytes = serde_json::to_vec(&grant)?;
            let mut child = Command::new(&image)
                .args(["--exact", "startup_identity::tests::managed_and_maintenance_grants_bind_exact_control_files", "--nocapture"])
                .env("RSRS_TEST_OFFLINE_SELECTION", selection)
                .env("RSRS_DATA_DIR", &root).env("HOME", &root).env("USERPROFILE", &root)
                .env_remove("RSRS_DEFAULT_DATA_DIR").env_remove("RSRS_M3_DIR")
                .env_remove("RSRS_CLIENT_ONLY").env_remove("RSRS_NO_AUTOSTART")
                .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit())
                .spawn()?;
            let outcome = (|| -> Result<()> {
                let mut input = child.stdin.take().context("missing diagnostic pipe")?;
                input.write_all(&u32::try_from(bytes.len())?.to_be_bytes())?;
                input.write_all(&bytes)?;
                input.flush()?;
                let deadline = Instant::now() + Duration::from_secs(20);
                loop {
                    if let Some(status) = child.try_wait()? {
                        anyhow::ensure!(status.success(), "diagnostic child failed");
                        break;
                    }
                    anyhow::ensure!(Instant::now() < deadline, "diagnostic child timed out");
                    std::thread::sleep(Duration::from_millis(20));
                }
                let mut output = String::new();
                child.stdout.take().context("missing diagnostic output")?
                    .take(4097).read_to_string(&mut output)?;
                anyhow::ensure!(output.len() <= 4096 && output.contains("\noffline-read complete\n"),
                    "diagnostic child did not execute the selected test");
                // Retain the private input owner until authorization and binding finish.
                drop(input);
                Ok(())
            })();
            if outcome.is_err() {
                let _ = child.kill();
                let _ = child.wait();
            }
            outcome?;
        }
        Ok(())
    })();
    let cleanup = fs::remove_dir_all(&root);
    result?;
    cleanup?;
    Ok(())
}

#[test]
fn linux_birth_parser_handles_parentheses_and_dead_or_invalid_processes() -> Result<()> {
    let fields = (1..=20)
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(
        linux_process_birth(&format!("123 (owner ) name) S {fields}"))?,
        Some("19".to_owned())
    );
    assert!(linux_process_birth(&format!("123 (owner) Z {fields}"))?.is_none());
    assert!(linux_process_birth("invalid").is_err());
    assert!(linux_process_birth("123 (owner) S 1 2").is_err());
    Ok(())
}

#[test]
fn launch_claim_cannot_be_replayed() -> Result<()> {
    let root =
        std::env::temp_dir().join(format!("respire-launch-fixture-{}", uuid::Uuid::new_v4()));
    let directory = root.join("launch");
    fs::create_dir_all(&directory)?;
    let result = (|| -> Result<()> {
        let directory = directory.canonicalize()?;
        let nonce = "12345678-1234-4234-8234-123456789abc";
        let (_, grant) = fixture();
        let bytes = serde_json::to_vec(&grant)?;
        claim(&directory, nonce, &bytes)?;
        let receipt_path = directory.join(format!("launch-{nonce}.claimed"));
        let receipt_bytes = fs::read(&receipt_path)?;
        let receipt: Value = serde_json::from_slice(&receipt_bytes)?;
        assert_eq!(receipt["schema_version"], 2);
        assert_eq!(receipt["state"], "consumed");
        assert_eq!(receipt["grant_sha256"], hash(&bytes));
        assert_eq!(receipt["grant_json"], std::str::from_utf8(&bytes)?);
        assert!(receipt["registration_json"].is_null());
        assert_eq!(receipt["pid"], std::process::id());
        assert_eq!(receipt["process_start_id"], process_birth(std::process::id())?.context("test process exited")?);
        assert!(claim(&directory, nonce, &bytes).is_err());
        assert_eq!(fs::read(&receipt_path)?, receipt_bytes);
        Ok(())
    })();
    fs::remove_dir_all(&root)?;
    result
}
