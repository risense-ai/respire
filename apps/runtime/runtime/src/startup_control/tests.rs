use super::*;
use serde_json::json;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Result<Self> {
        let root = std::env::temp_dir()
            .canonicalize()?
            .join(format!("runtime-startup-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root)?;
        Ok(Self(root.canonicalize()?))
    }

    fn request(&self, profile: PathBuf) -> Acquire {
        Acquire {
            schema_version: SCHEMA,
            installation_root: self.0.clone(),
            profile,
            controller_pid: 0,
            controller_birth: String::new(),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0) {
            eprintln!("startup fixture cleanup failed: {error}");
        }
    }
}

#[test]
fn cold_scope_uses_exact_old_cli_exclusion_and_preserves_profile_lock() -> Result<()> {
    let fixture = Fixture::new()?;
    let root = fixture.0.join("cold root with spaces");
    let profile = root.join("accounts/synthetic profile");
    let mut request = fixture.request(profile.clone());
    request.installation_root = root.clone();
    let (held, receipt) = acquire(&request, &root, &profile, Duration::ZERO)?;
    assert_eq!(receipt.installation_root, root);
    assert_eq!(receipt.profile, profile);
    assert!(receipt.helper_pid > 0 && !receipt.helper_birth.is_empty());
    assert!(LibraryLock::acquire(&root.join("runtime/takeover"), Duration::ZERO).is_err());
    // The Runtime child owns this second lock independently of the startup helper.
    let child = respire_app::lock::LibraryLock::acquire(&profile, Duration::ZERO)?;
    drop(held);
    let old_cli = LibraryLock::acquire(&root.join("runtime/takeover"), Duration::ZERO)?;
    assert!(respire_app::lock::LibraryLock::acquire(&profile, Duration::ZERO).is_err());
    drop(child);
    drop(old_cli);
    let _next_child = respire_app::lock::LibraryLock::acquire(&profile, Duration::ZERO)?;
    Ok(())
}

#[test]
fn older_cli_waits_then_new_helper_acquires_same_lock() -> Result<()> {
    let fixture = Fixture::new()?;
    let root = &fixture.0;
    let old_cli = LibraryLock::acquire(&root.join("runtime/takeover"), Duration::ZERO)?;
    let request = fixture.request(root.clone());
    assert!(acquire(&request, root, root, Duration::ZERO).is_err());
    let contender_root = root.clone();
    let (ready, waiting) = std::sync::mpsc::channel();
    let contender = std::thread::spawn(move || -> Result<()> {
        ready.send(())?;
        let (_guard, _) = acquire(
            &request,
            &contender_root,
            &contender_root,
            Duration::from_secs(5),
        )?;
        Ok(())
    });
    waiting.recv()?;
    drop(old_cli);
    contender
        .join()
        .map_err(|_| anyhow::anyhow!("startup contender panicked"))??;
    Ok(())
}

#[test]
fn scope_mismatch_and_invalid_paths_have_no_lock_side_effects() -> Result<()> {
    let fixture = Fixture::new()?;
    let root = &fixture.0;
    let mut request = fixture.request(root.join("wrong profile"));
    assert!(acquire(&request, root, root, Duration::ZERO).is_err());
    assert!(!root.join("runtime").exists());
    request.profile = root.clone();
    request.installation_root = root.join("wrong installation");
    assert!(acquire(&request, root, root, Duration::ZERO).is_err());
    assert!(checked_path(Path::new("relative")).is_err());
    // PathBuf::join normalizes parent traversal away for verbatim Windows roots.
    let traversal = PathBuf::from(format!("{}/parent/../escape", root.display()));
    assert!(checked_path(&traversal).is_err());
    fs::create_dir_all(root.join("runtime/takeover/lock.db"))?;
    assert!(takeover_directory(root).is_err());
    Ok(())
}

#[test]
fn held_scope_rejects_account_change() -> Result<()> {
    let fixture = Fixture::new()?;
    let root = &fixture.0;
    let request = fixture.request(root.clone());
    let (guard, held) = acquire(&request, root, root, Duration::ZERO)?;
    fs::write(
        root.join("session.json"),
        br#"{"user":"synthetic-other-account"}"#,
    )?;
    assert!(verify_held(&guard, root, root, &held).is_err());
    fs::write(root.join("session.json"), b"invalid session JSON")?;
    assert!(account_id(root).is_err());
    Ok(())
}

#[test]
fn migrated_local_profile_keeps_existing_checked_identity_projection() -> Result<()> {
    let fixture = Fixture::new()?;
    let root = &fixture.0;
    fs::write(
        root.join("session.json"),
        serde_json::to_vec(&json!({
            "vault_version":4, "kdf_salt":"synthetic-salt", "wrapped_urk":"rsrs:v1:synthetic-wrap",
            "urk_nonce":"synthetic-nonce", "keyring_account":"legacy-0000000000000000",
            "keyring_backend":"native", "crypto_namespace":"rsrs:v1:"
        }))?,
    )?;
    fs::write(
        root.join(".rsrs-migration.json"),
        serde_json::to_vec(&json!({
            "schema":2, "complete":true, "source_identity":"0".repeat(64),
            "source":root, "destination":root, "keyring_backend":"native",
            "namespace_migrated":true, "original_preserved":true
        }))?,
    )?;
    let request = fixture.request(root.clone());
    let (guard, held) = acquire(&request, root, root, Duration::ZERO)?;
    assert_eq!(
        held.account_id,
        digest(root.display().to_string().as_bytes())
    );
    verify_held(&guard, root, root, &held)?;
    fs::write(root.join(".rsrs-migration.json"), b"invalid receipt")?;
    assert!(verify_held(&guard, root, root, &held).is_err());
    Ok(())
}

#[test]
fn frame_shape_and_size_fail_explicitly() -> Result<()> {
    let fixture = Fixture::new()?;
    let request = fixture.request(fixture.0.clone());
    let mut frame = Vec::new();
    write_frame(&mut frame, &request)?;
    let decoded: Acquire = read_frame(&mut frame.as_slice())?;
    assert_eq!(decoded.installation_root, request.installation_root);
    assert!(read_frame::<Acquire>(&mut frame[..frame.len() - 1].as_ref()).is_err());
    assert!(read_frame::<Acquire>(&mut 0u32.to_be_bytes().as_slice()).is_err());
    assert!(read_frame::<Acquire>(&mut ((MAX_FRAME + 1) as u32).to_be_bytes().as_slice()).is_err());
    let mut extra = serde_json::to_value(&request)?;
    extra["operation"] = json!("unrelated");
    let mut frame = Vec::new();
    write_frame(&mut frame, &extra)?;
    assert!(read_frame::<Acquire>(&mut frame.as_slice()).is_err());
    assert!(validate_controller(&request).is_err());
    Ok(())
}

#[test]
fn endpoint_receipt_requires_same_process_profile_account_and_publication() -> Result<()> {
    let fixture = Fixture::new()?;
    let root = &fixture.0;
    let request = fixture.request(root.clone());
    let (_guard, held) = acquire(&request, root, root, Duration::ZERO)?;
    let image = root.join("synthetic-runtime-image");
    fs::write(&image, b"synthetic receipt fixture only")?;
    let endpoint = Endpoint {
        pid: 42,
        process_start_id: wire_birth("100"),
        image_path: image.clone(),
        image_sha256: "a".repeat(64),
        instance_id: uuid::Uuid::new_v4().to_string(),
        account_id: held.account_id.clone(),
        generation: 1,
        url: "http://127.0.0.1:19169".into(),
    };
    let mut published = serde_json::to_value(&endpoint)?;
    let root_text = root.to_str().context("fixture path is not UTF-8")?;
    let install_id = digest(root_text.as_bytes());
    published["installation_root"] = json!(root_text);
    published["data_dir"] = json!(root_text);
    published["installation_id"] = json!(install_id);
    published["v"] = json!(rpc_worker::PROTOCOL_V);
    let image_text = image.to_str().context("fixture image is not UTF-8")?;
    let verify =
        |observed: &Endpoint, publication: &serde_json::Value, profile: &str, birth: &str| {
            verify_observation(
                &endpoint,
                observed,
                &held,
                publication,
                root_text,
                profile,
                &install_id,
                rpc_worker::PROTOCOL_V,
                image_text,
                birth,
            )
        };
    verify(&endpoint, &published, root_text, "100")?;
    for field in [
        "pid",
        "process_start_id",
        "image_path",
        "image_sha256",
        "instance_id",
        "account_id",
        "generation",
        "url",
        "installation_root",
        "data_dir",
        "installation_id",
        "v",
    ] {
        let mut changed = published.clone();
        changed[field] = serde_json::Value::Null;
        assert!(
            verify(&endpoint, &changed, root_text, "100").is_err(),
            "accepted changed {field}"
        );
    }
    let mut changed = endpoint.clone();
    changed.instance_id = uuid::Uuid::new_v4().to_string();
    assert!(verify(&changed, &published, root_text, "100").is_err());
    assert!(verify(&endpoint, &published, root_text, "101").is_err());
    let other = root.join("other profile");
    fs::create_dir_all(&other)?;
    assert!(verify(
        &endpoint,
        &published,
        other.to_str().context("fixture path")?,
        "100"
    )
    .is_err());
    Ok(())
}

#[test]
fn finish_wait_expires_with_a_connected_controller() {
    let (_active_sender, receiver) = std::sync::mpsc::sync_channel(1);
    let error = super::wait_for_finish(&receiver, Duration::ZERO).expect_err("held wait timed out");
    assert!(error.to_string().contains("exclusion timed out"));
}

#[test]
fn deadline_releases_without_controller_io() -> Result<()> {
    let fixture = Fixture::new()?;
    let root = &fixture.0;
    let request = fixture.request(root.clone());
    let (guard, _) = acquire(&request, root, root, Duration::ZERO)?;
    let exclusion = super::hold::BoundExclusion::start(guard, Duration::from_millis(80))?;
    // No signal is sent: this is the ownership state during any stalled pipe,
    // controller or endpoint. A genuine second SQLite connection can get in
    // after the timed owner releases; a late receipt must still fail.
    let contender = LibraryLock::acquire(&root.join("runtime/takeover"), Duration::from_secs(2))?;
    assert!(
        exclusion.release().is_err(),
        "accepted a late release receipt"
    );
    drop(contender);
    Ok(())
}
