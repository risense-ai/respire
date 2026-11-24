use super::*;

#[test]
fn login_recovery_failure_preserves_the_bound_runtime_diagnostic() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let profile = directory.path();
    let request = login_recovery_request(ControlBinding {
        installation_root: profile.into(), profile: profile.into(),
        launch_id: uuid::Uuid::new_v4().to_string(), expected_account_id: None,
    }, HostAuthIdentity::Absent, false)?;
    let response: RpcResponse<ResultEnvelope> = serde_json::from_value(serde_json::json!({
        "v":request.v,"id":request.id,"bin":"rsrs-runtime","pid":42,
        "ok":false,"exit":2,"code":"unauthorized",
        "error":"login recovery status authorization failed"
    }))?;
    let error = match validate_login_recovery_response(response.clone(), &request, profile, 42) {
        Err(error) => error.to_string(),
        Ok(_) => anyhow::bail!("failed recovery was accepted"),
    };
    assert!(error.contains("unauthorized") && error.contains("authorization failed")
        && error.contains("retain recovery state"));
    let mut foreign = response;
    foreign.pid = Some(43);
    let error = match validate_login_recovery_response(foreign, &request, profile, 42) {
        Err(error) => error.to_string(),
        Ok(_) => anyhow::bail!("foreign recovery response was accepted"),
    };
    assert!(error.contains("identity mismatch") && !error.contains("authorization failed"));
    Ok(())
}

#[test]
fn account_process_start_requires_exact_native_birth() -> Result<()> {
    let birth = "123456789";
    let prefix = match crate::autostart::Platform::current()? {
        crate::autostart::Platform::Windows => "windows-filetime-",
        crate::autostart::Platform::Linux => "linux-starttime-",
        crate::autostart::Platform::Macos => "darwin-lstart-",
    };
    let start_id = format!("{prefix}{birth}");
    assert!(account_process_start_matches(birth, &start_id)?);
    for invalid in [birth.to_owned(), format!("{prefix}987654321"),
        format!("{prefix}{start_id}"), format!("{start_id} "), String::new()] {
        assert!(!account_process_start_matches(birth, &invalid)?);
    }
    assert!(!account_process_start_matches("", prefix)?);
    assert!(!account_process_start_matches(&start_id, &start_id)?);
    for foreign in ["windows-filetime-", "linux-starttime-", "darwin-lstart-"] {
        if foreign != prefix {
            assert!(!account_process_start_matches(birth, &format!("{foreign}{birth}"))?);
        }
    }
    Ok(())
}
