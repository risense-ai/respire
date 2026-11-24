use super::*;

#[test]
fn foreground_identity_accepts_standalone_serve_not_cli_serve() {
    for args in [
        vec!["/tmp/rsrs-runtime", "serve", "--port", "15169"],
        vec!["/tmp/path with spaces/rsrs-runtime", "--json", "serve"],
        vec!["/tmp/rsrs", "runtime", "run"],
    ] {
        assert!(foreground_runtime_arguments(&args), "{args:?}");
    }
    for args in [
        vec!["/tmp/rsrs", "serve"],
        vec!["/tmp/unrelated", "serve"],
        vec!["/tmp/rsrs-runtime", "--status"],
    ] {
        assert!(!foreground_runtime_arguments(&args), "{args:?}");
    }
}

#[test]
fn foreground_identity_accepts_standalone_serving_modes_and_option_values() {
    for args in [
        vec!["/tmp/rsrs-runtime", "serve", "--port", "15169"],
        vec!["/tmp/path with spaces/rsrs-runtime", "--json", "serve"],
        vec!["/tmp/rsrs-runtime"],
        vec!["/tmp/rsrs-runtime", "--json", "runtime", "start"],
        vec!["/tmp/rsrs-runtime", "--port", "15169", "serve"],
        vec!["/tmp/rsrs-runtime", "serve", "--data-dir", "--status"],
        vec![
            "/tmp/rsrs-runtime",
            "serve",
            "--data-dir",
            "/tmp/profile",
            "--supervised",
        ],
        vec![
            "/tmp/rsrs-runtime",
            "serve",
            "--data-dir",
            "/tmp/profile",
            "--host",
            "127.0.0.1",
            "--port",
            "15169",
        ],
        vec!["/tmp/rsrs", "runtime", "run"],
        vec!["/tmp/rsrs", "--runtime-internal"],
        vec!["/tmp/rsrs", "web", "--internal"],
    ] {
        assert!(
            runtime_role_arguments(Path::new(args[0]), &args),
            "{args:?}"
        );
    }
}

#[test]
fn foreground_identity_rejects_conflicting_or_incomplete_standalone_arguments() {
    for args in [
        vec!["/tmp/rsrs", "serve"],
        vec!["/tmp/unrelated", "serve"],
        vec!["/tmp/rsrs-runtime", "--status"],
        vec!["/tmp/rsrs-runtime", "serve", "--status"],
        vec!["/tmp/rsrs-runtime", "serve", "--stop"],
        vec!["/tmp/rsrs-runtime", "serve", "--version"],
        vec!["/tmp/rsrs-runtime", "runtime", "start", "--stop"],
        vec!["/tmp/rsrs-runtime", "runtime", "status"],
        vec!["/tmp/rsrs-runtime", "runtime"],
        vec!["/tmp/rsrs-runtime", "serve", "--data-dir"],
        vec!["/tmp/rsrs-runtime", "serve", "--host"],
        vec!["/tmp/rsrs-runtime", "serve", "--host", ""],
        vec!["/tmp/rsrs-runtime", "serve", "--port"],
        vec!["/tmp/rsrs-runtime", "serve", "--port", "0"],
        vec!["/tmp/rsrs-runtime", "serve", "--port", "65536"],
        vec!["/tmp/rsrs-runtime", "serve", "--port", "--status"],
        vec!["/tmp/rsrs-runtime", "serve", "--direct"],
        vec!["/tmp/rsrs-runtime", "serve", "--core-worker-internal"],
        vec!["/tmp/rsrs-runtime", "serve", "--runtime-internal"],
        vec!["/tmp/rsrs-runtime", "--runtime-internal"],
        vec!["/tmp/rsrs-runtime", "web", "--internal"],
        vec!["/tmp/rsrs-runtime", "--status", "--data-dir", "serve"],
        vec!["/tmp/rsrs-runtime", "serve", "unknown"],
    ] {
        assert!(
            !runtime_role_arguments(Path::new(args[0]), &args),
            "{args:?}"
        );
    }
}

#[test]
fn background_identity_requires_the_exact_internal_launcher_shape() {
    for image in ["/tmp/rsrs-runtime", "/tmp/rsrs-runtime (deleted)"] {
        assert!(runtime_role_arguments(Path::new(image),
            &["rsrs-runtime", "--runtime-internal", "--runtime-background-internal", "--port", "15169"]));
    }
    for args in [
        vec!["rsrs-runtime", "--runtime-background-internal"],
        vec!["rsrs-runtime", "--runtime-internal", "--runtime-background-internal", "--port"],
        vec!["rsrs-runtime", "--runtime-internal", "--runtime-background-internal", "--port", "0"],
        vec!["rsrs-runtime", "--runtime-internal", "--runtime-background-internal", "--port", "15169", "--status"],
        vec!["rsrs-runtime", "--runtime-internal", "--runtime-background-internal", "--data-dir", "15169"],
    ] {
        assert!(!runtime_role_arguments(Path::new("/tmp/rsrs-runtime"), &args));
    }
    assert!(!runtime_role_arguments(Path::new("/tmp/foreign"),
        &["rsrs-runtime", "--runtime-internal", "--runtime-background-internal", "--port", "15169"]));
}

#[test]
fn runtime_role_uses_the_executable_not_argv_zero() -> Result<()> {
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;
    #[cfg(windows)]
    use std::os::windows::process::ExitStatusExt;
    let output = |name: &[u8]| std::process::Output {
        status: std::process::ExitStatus::from_raw(0),
        stdout: name.to_vec(),
        stderr: b"private stderr must not be reported".to_vec(),
    };
    for name in ["rsrs", "/private/path with spaces/rsrs-runtime"] {
        darwin_runtime_name(Ok(output(name.as_bytes())))?;
    }
    for (name, class) in [("", "empty_image"),
        ("/private/secret-name/foreign --private-argument", "image_name_mismatch")] {
        let error = darwin_runtime_name(Ok(output(name.as_bytes())))
            .err().context("unverified image was accepted")?.to_string();
        assert!(error.contains("stage=process_name") && error.contains(class));
        assert!(!error.contains("secret-name") && !error.contains("private-argument"));
    }
    let mut failed = output(b"/private/rsrs-runtime");
    failed.status = std::process::ExitStatus::from_raw(256);
    let error = darwin_runtime_name(Ok(failed))
        .err().context("failed ps was accepted")?.to_string();
    assert!(error.contains("class=ps_failed") && error.contains("ps_status="));
    assert!(!error.contains("private"));
    let error = darwin_runtime_name(Err(std::io::Error::from_raw_os_error(13)))
        .err().context("failed inspection was accepted")?.to_string();
    assert!(error.contains("class=ps_spawn_failed") && error.contains("os_kind=")
        && error.contains("os_code=Some(13)"));
    assert!(!runtime_role_arguments(
        Path::new("/tmp/rsrs"),
        &["rsrs-runtime", "serve"]
    ));
    assert!(!runtime_role_arguments(
        Path::new("/tmp/foreign"),
        &["rsrs", "--runtime-internal"]
    ));
    assert!(!runtime_role_arguments(
        Path::new("/tmp/rsrs-runtime"),
        &["rsrs", "--runtime-internal"]
    ));
    assert!(runtime_role_arguments(
        Path::new("/tmp/rsrs-runtime"),
        &["rsrs", "serve"]
    ));
    assert!(runtime_role_arguments(
        Path::new("/tmp/rsrs-runtime (deleted)"),
        &["rsrs-runtime", "serve"]
    ));
    assert!(runtime_role_arguments(
        Path::new("/tmp/rsrs (deleted)"),
        &["rsrs", "runtime", "run"]
    ));
    assert!(!runtime_role_arguments(
        Path::new("/tmp/foreign (deleted)"),
        &["rsrs-runtime", "serve"]
    ));
    assert!(!runtime_role_arguments(
        Path::new("/tmp/rsrs-runtime (deleted) suffix"),
        &["rsrs-runtime", "serve"]
    ));
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn procfs_arguments_preserve_empty_values_and_reject_malformed_input() -> Result<()> {
    assert_eq!(
        linux_process_arguments(b"rsrs-runtime\0serve\0")?,
        ["rsrs-runtime", "serve"]
    );
    assert_eq!(
        linux_process_arguments(b"rsrs-runtime\0--host\0\0")?,
        ["rsrs-runtime", "--host", ""]
    );
    assert!(!runtime_role_arguments(
        Path::new("rsrs-runtime"),
        &linux_process_arguments(b"rsrs-runtime\0--host\0\0")?
    ));
    assert_eq!(
        linux_process_arguments(b"rsrs-runtime\0serve\0--data-dir\0\0")?,
        ["rsrs-runtime", "serve", "--data-dir", ""]
    );
    assert!(runtime_role_arguments(
        Path::new("rsrs-runtime"),
        &linux_process_arguments(b"rsrs-runtime\0serve\0--data-dir\0\0")?
    ));
    assert!(!runtime_role_arguments(
        Path::new("rsrs-runtime"),
        &linux_process_arguments(b"rsrs-runtime\0serve\0\0")?
    ));
    assert!(linux_process_arguments(b"").is_err());
    assert!(linux_process_arguments(b"rsrs-runtime\0serve").is_err());
    assert!(linux_process_arguments(b"rsrs-runtime\0\xff\0").is_err());
    Ok(())
}
#[cfg(target_os = "linux")]
#[test]
fn procfs_liveness_keeps_pid_and_state_checks_strict() -> Result<()> {
    assert!(linux_stat_is_running("123 (rsrs) S 1 2 3", 123)?);
    assert!(linux_stat_is_running(
        "123 (name with ) parentheses) R 1 2 3",
        123
    )?);
    assert!(!linux_stat_is_running("123 (rsrs) Z 1 2 3", 123)?);
    for invalid in [
        "124 (rsrs) S 1",
        "123 rsrs S",
        "123 (rsrs)",
        "123 (rsrs) ? 1",
    ] {
        assert!(linux_stat_is_running(invalid, 123).is_err());
    }
    assert!(process_is_running(std::process::id())?);
    assert!(process_is_running(0).is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn exited_child_is_not_a_running_library_holder() -> Result<()> {
    let mut child = std::process::Command::new("sh")
        .args(["-c", "exit 0"])
        .spawn()?;
    let exited = wait_until_exited(child.id());
    child.wait()?;
    exited
}
