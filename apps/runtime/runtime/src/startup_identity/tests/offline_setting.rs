use super::*;
use respire_commands::output::ResultEnvelope;
use respire_commands::rpc_worker::PROTOCOL_V;
use respire_protocol::{RpcResponse, HOST_CONTROL_SCHEMA_V};
use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn private_offline_setting_owns_legacy_takeover_and_selected_profile() -> Result<()> {
    const CHILD: &str = "RSRS_TEST_OFFLINE_AGENT_MODE_CHILD";
    if std::env::var_os(CHILD).is_some() {
        std::process::exit(crate::host_control::run()?);
    }

    let root = std::env::temp_dir()
        .canonicalize()?
        .join(format!("offline-agent-mode-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root)?;
    let result = exercise(&root, CHILD);
    let cleanup = fs::remove_dir_all(&root);
    result?;
    cleanup?;
    Ok(())
}

fn exercise(root: &Path, child_env: &str) -> Result<()> {
    let image = std::env::current_exe()?.canonicalize()?;
    let profile = root.join("accounts").join("local");
    fs::create_dir_all(&profile)?;
    fs::write(
        root.join("client.json"),
        serde_json::to_vec(&json!({"data_dir":profile}))?,
    )?;
    let agent = profile.join("agent.json");
    let original = br#"{"diary_mode":"concise","preserve":true}"#;
    fs::write(&agent, original)?;
    let image_hash = hash_file(&image)?;
    let parent = std::process::id();
    let birth = process_birth(parent)?.context("missing parent identity")?;

    let call = |key: &str,
                value: &str,
                valid_binding: bool|
     -> Result<RpcResponse<ResultEnvelope>> {
        let launch = uuid::Uuid::new_v4().to_string();
        let now = now_ms()?;
        let grant = LaunchGrant {
            schema_version: HOST_CONTROL_SCHEMA_V,
            protocol_version: PROTOCOL_V,
            nonce: launch.clone(),
            instance_id: launch.clone(),
            installation_root: root.to_path_buf(),
            installation_id: hash(root.display().to_string().as_bytes()),
            profile: profile.clone(),
            account_id: hash(profile.display().to_string().as_bytes()),
            sync_generation: 0,
            image_path: image.clone(),
            image_sha256: image_hash.clone(),
            supervisor_pid: parent,
            supervisor_birth: birth.clone(),
            managed_id: None,
            maintenance: None,
            registration_sha256: None,
            port: 19169,
            issued_at_ms: now,
            expires_at_ms: now + 60_000,
        };
        let params = json!({
            "schema_version": HOST_CONTROL_SCHEMA_V,
            "binding": {
                "installation_root": root,
                "profile": if valid_binding { profile.as_path() } else { root },
                "launch_id": launch,
                "expected_account_id": null
            },
            "key": key,
            "value": value
        });
        let request = json!({
            "v":PROTOCOL_V, "id":launch, "method":"control.agent_config.set_offline_mode", "args":[params.to_string()]
        });
        let mut command = Command::new(&image);
        command.args(["--exact",
            "startup_identity::tests::offline_setting::private_offline_setting_owns_legacy_takeover_and_selected_profile",
            "--nocapture"])
            .env(child_env, "1")
            .env("RSRS_DATA_DIR", root).env("HOME", root).env("USERPROFILE", root)
            .env_remove("RSRS_DEFAULT_DATA_DIR").env_remove("RSRS_M3_DIR")
            .env_remove("RSRS_CLIENT_ONLY").env_remove("RSRS_NO_AUTOSTART");
        let mut child = respire_spawn::spawn_control_helper(&mut command)?;
        let result = (|| -> Result<_> {
            let mut stdin = child.stdin.take().context("missing control input pipe")?;
            for bytes in [serde_json::to_vec(&grant)?, serde_json::to_vec(&request)?] {
                stdin.write_all(&u32::try_from(bytes.len())?.to_be_bytes())?;
                stdin.write_all(&bytes)?;
            }
            stdin.flush()?;
            let deadline = Instant::now() + Duration::from_secs(20);
            while child.try_wait()?.is_none() {
                anyhow::ensure!(Instant::now() < deadline, "offline mode child timed out");
                std::thread::sleep(Duration::from_millis(20));
            }
            let mut output = Vec::new();
            child
                .stdout
                .take()
                .context("missing control output pipe")?
                .read_to_end(&mut output)?;
            let frame = output
                .strip_prefix(b"\nrunning 1 test\n")
                .context("missing offline mode child output")?;
            // Cargo's single-threaded libtest formatter emits the test name
            // before the child writes its actual binary response frame.
            let frame = frame.strip_prefix(
                b"test startup_identity::tests::offline_setting::private_offline_setting_owns_legacy_takeover_and_selected_profile ... ",
            ).unwrap_or(frame);
            let length = u32::from_be_bytes(
                frame
                    .get(..4)
                    .context("missing response size")?
                    .try_into()?,
            );
            let body = &frame[4..];
            assert_eq!(body.len(), usize::try_from(length)?);
            let response: RpcResponse<ResultEnvelope> = serde_json::from_slice(body)?;
            assert_eq!(response.id, launch);
            Ok(response)
        })();
        if result.is_err() {
            let _ = child.kill();
            let _ = child.wait();
        }
        result
    };

    let invalid = call("recall_mode", "verbose", true)?;
    assert_eq!(invalid.code.as_deref(), Some("invalid_request"));
    let mismatch = call("diary_mode", "verbose", false)?;
    assert_eq!(
        mismatch.code.as_deref(),
        Some("scope_mismatch"),
        "{:?}",
        mismatch.error
    );
    for dir in [root.join("runtime").join("takeover"), profile.clone()] {
        let legacy = respire_client_host::lock::LibraryLock::acquire(&dir, Duration::ZERO)?;
        let refused = call("diary_mode", "verbose", true)?;
        assert_eq!(
            refused.code.as_deref(),
            Some("offline_ownership_unavailable"),
            "{:?}",
            refused.error
        );
        assert_eq!(fs::read(&agent)?, original);
        drop(legacy);
    }
    let accepted = call("diary_mode", "verbose", true)?;
    assert!(accepted.ok, "{:?}", accepted.error);
    let summary = accepted.envelope.context("missing mode receipt")?.summary;
    assert_eq!(
        summary,
        json!({"action":"updated","key":"diary_mode","value":"verbose"})
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&agent)?)?,
        json!({"diary_mode":"verbose","preserve":true})
    );
    assert!(!root.join("agent.json").exists());
    assert!(!root.join("runtime/endpoint.json").exists());
    Ok(())
}
