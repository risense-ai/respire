use super::*;

#[test]
fn clean_stop_uses_current_generation_and_rejects_changed_evidence() -> Result<()> {
    for change in ["none", "old-generation", "foreign-instance", "bytes", "claim", "owner"] {
        let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize()?)?;
        let other = tempfile::tempdir_in(std::env::temp_dir().canonicalize()?)?;
        let owner = respire_app::lock::LibraryLock::acquire(directory.path(), Duration::from_secs(1))?;
        let wrong_owner = respire_app::lock::LibraryLock::acquire(other.path(), Duration::from_secs(1))?;
        let path = directory.path().join("endpoint.json");
        let receipt = directory.path().join("launch.claimed");
        // These expected values model current process state, not parsed disk input.
        let current = json!({"pid":std::process::id(),"instance_id":"same-live-publisher","generation":2});
        let claimed = Some(b"original claimed receipt".to_vec());
        std::fs::write(&receipt, claimed.as_ref().context("synthetic claim absent")?)?;
        std::fs::write(&path, serde_json::to_vec_pretty(&current)?)?;
        let expected = current_publication(path.clone(),current.clone(),receipt.clone(),claimed.clone())?;
        match change {
            "old-generation" => {
                let mut old=current.clone(); old["generation"]=json!(1);
                std::fs::write(&path,serde_json::to_vec_pretty(&old)?)?;
            }
            "foreign-instance" => {
                let mut foreign=current.clone(); foreign["instance_id"]=json!("foreign");
                std::fs::write(&path,serde_json::to_vec_pretty(&foreign)?)?;
            }
            "bytes" => std::fs::write(&path,b"foreign bytes")?,
            "claim" => std::fs::write(&receipt,b"changed claim")?,
            _ => {}
        }
        let before=endpoint_record_bytes(&path)?;
        let before_claim=endpoint_record_bytes(&receipt)?;
        let result=expected.revoke_matching(if change == "owner" { &wrong_owner } else { &owner },
            &path,&current,&receipt,&claimed);
        if change == "none" {
            result?;
            assert!(!path.try_exists()?);
        } else {
            assert!(result.is_err(),"accepted changed {change}");
            assert_eq!(endpoint_record_bytes(&path)?,before);
        }
        assert_eq!(endpoint_record_bytes(&receipt)?,before_claim);
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn clean_stop_does_not_follow_endpoint_symlink() -> Result<()> {
    let directory=tempfile::tempdir_in(std::env::temp_dir().canonicalize()?)?;
    let owner=respire_app::lock::LibraryLock::acquire(directory.path(),Duration::from_secs(1))?;
    let target=directory.path().join("foreign.json");
    let path=directory.path().join("endpoint.json");
    let receipt=directory.path().join("launch.claimed");
    let identity=json!({"generation":2});
    let bytes=serde_json::to_vec_pretty(&identity)?;
    std::fs::write(&target,&bytes)?;
    std::os::unix::fs::symlink(&target,&path)?;
    let expected=current_publication(path.clone(),identity.clone(),receipt.clone(),None)?;
    assert!(expected.revoke_matching(&owner,&path,&identity,&receipt,&None).is_err());
    assert_eq!(std::fs::read(target)?,bytes);
    assert!(std::fs::symlink_metadata(path)?.file_type().is_symlink());
    Ok(())
}

#[test]
fn failed_start_revokes_only_unchanged_owned_publication() -> Result<()> {
    for change in ["none", "no-claim", "bytes", "claim", "missing-claim", "new-claim", "path", "receipt-path", "lock", "missing"] {
        let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize()?)?;
        let other = tempfile::tempdir_in(std::env::temp_dir().canonicalize()?)?;
        let owner = respire_app::lock::LibraryLock::acquire(directory.path(), Duration::from_secs(1))?;
        let wrong_owner = respire_app::lock::LibraryLock::acquire(other.path(), Duration::from_secs(1))?;
        let path = directory.path().join("endpoint.json");
        let receipt = directory.path().join("launch.claimed");
        let identity = json!({"pid": std::process::id(), "instance_id": "owned-startup"});
        let bytes = serde_json::to_vec(&identity)?;
        let receipt_bytes = if matches!(change, "no-claim" | "new-claim") {
            None
        } else {
            Some(b"consumed; never reusable".to_vec())
        };
        std::fs::write(&path, &bytes)?;
        if let Some(bytes) = receipt_bytes.as_ref() {
            std::fs::write(&receipt, bytes)?;
        }
        let published = EndpointPublication {
            path: path.clone(), bytes, identity: identity.clone(),
            receipt: receipt.clone(), receipt_bytes: receipt_bytes.clone(),
        };
        match change {
            "bytes" => std::fs::write(&path, b"foreign endpoint")?,
            "claim" | "new-claim" => std::fs::write(&receipt, b"changed claim")?,
            "missing-claim" => std::fs::remove_file(&receipt)?,
            "missing" => std::fs::remove_file(&path)?,
            _ => {}
        }
        let before_endpoint = endpoint_record_bytes(&path)?;
        let before_claim = endpoint_record_bytes(&receipt)?;
        let result = published.revoke_matching(
            if change == "lock" { &wrong_owner } else { &owner },
            if change == "path" { &receipt } else { &path },
            &identity,
            if change == "receipt-path" { &path } else { &receipt },
            &receipt_bytes,
        );
        if matches!(change, "none" | "no-claim") {
            result?;
            assert!(!path.try_exists()?);
        } else {
            assert!(result.is_err(), "accepted changed {change}");
            assert_eq!(endpoint_record_bytes(&path)?, before_endpoint);
        }
        assert_eq!(endpoint_record_bytes(&receipt)?, before_claim);
    }
    Ok(())
}

#[test]
fn failed_start_retains_endpoint_on_any_identity_change() -> Result<()> {
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize()?)?;
    let owner = respire_app::lock::LibraryLock::acquire(directory.path(), Duration::from_secs(1))?;
    let path = directory.path().join("endpoint.json");
    let receipt = directory.path().join("launch.claimed");
    let identity = json!({
        "schema_version": 2, "server": "respire", "v": PROTOCOL_V,
        "pid": std::process::id(), "bin": "test", "url": "http://127.0.0.1:1",
        "runtime_version": "test", "exe": "image", "image_path": "image",
        "image_sha256": "digest", "installation_root": "root", "data_dir": "profile",
        "installation_id": "installation", "instance_id": "instance",
        "process_start_id": "birth", "account_id": "account", "generation": 1,
        "managed_id": null, "launch_receipt_sha256": "claim-digest"
    });
    let bytes = serde_json::to_vec(&identity)?;
    std::fs::write(&path, &bytes)?;
    let published = EndpointPublication {
        path: path.clone(), bytes: bytes.clone(), identity: identity.clone(),
        receipt: receipt.clone(), receipt_bytes: None,
    };
    for key in identity.as_object().context("identity object")?.keys() {
        let mut changed = identity.clone();
        changed[key] = json!("changed");
        assert!(published.revoke_matching(&owner, &path, &changed, &receipt, &None).is_err(),
            "accepted changed {key}");
        assert_eq!(std::fs::read(&path)?, bytes);
    }
    Ok(())
}
