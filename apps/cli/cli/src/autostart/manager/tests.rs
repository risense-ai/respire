use super::*;
#[test]
fn darwin_birth_distinguishes_live_exited_and_unavailable_processes() -> Result<()> {
    let birth = "Sat Nov  7 14:00:00 2026";
    assert_eq!(darwin_process_birth(0, &format!("S+ {birth}\n"))?, Some(birth.into()));
    for state in ["Z", "Z+"] {
        assert_eq!(darwin_process_birth(0, &format!("{state} {birth}"))?, None);
    }
    assert_eq!(darwin_process_birth(1, "")?, None);
    for (code, text) in [(2, ""), (0, ""), (0, "S"), (0, "S \n"), (0, "S first\nS second")] {
        assert!(darwin_process_birth(code, text).is_err());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn exited_unreaped_child_is_not_a_live_drain_owner() -> Result<()> {
    let mut child = Command::new("sh").args(["-c", "exit 0"]).spawn()?;
    let result: Result<()> = (|| {
        let deadline = Instant::now() + Duration::from_secs(5);
        while process_birth(child.id())?.is_some() {
            anyhow::ensure!(Instant::now() < deadline, "exited child remained a live owner");
            std::thread::sleep(Duration::from_millis(25));
        }
        Ok(())
    })();
    let status = child.wait()?;
    result?;
    anyhow::ensure!(status.success());
    Ok(())
}
#[test]
fn loaded_systemd_configuration_must_match_disk_identity_without_overrides() -> Result<()> {
    let config = super::super::tests::fixture(Platform::Linux);
    let row = serde_json::json!([
        config.executable,
        [
            config.executable,
            "runtime",
            "supervise",
            "--data-dir",
            config.installation
        ],
        false,
        0,
        0,
        0,
        0,
        0,
        0,
        0
    ]);
    let exec = serde_json::json!({"data":[row]});
    let clean = serde_json::json!({"data":[]});
    verify_systemd_loaded(&config, &exec, &clean)?;
    anyhow::ensure!(verify_systemd_loaded(
        &config,
        &exec,
        &serde_json::json!({"data":["/tmp/foreign.conf"]})
    )
    .is_err());
    let mut changed = exec.clone();
    changed["data"][0][0] = serde_json::json!("/tmp/foreign");
    anyhow::ensure!(verify_systemd_loaded(&config, &changed, &clean).is_err());
    changed = exec.clone();
    changed["data"][0][1][4] = serde_json::json!("another-installation");
    anyhow::ensure!(verify_systemd_loaded(&config, &changed, &clean).is_err());
    anyhow::ensure!(
        verify_systemd_loaded(&config, &serde_json::json!({"data":[]}), &clean).is_err()
    );
    Ok(())
}
#[test]
fn launchd_values_preserve_spaces_quotes_and_literal_backslashes() -> Result<()> {
    let value = "/tmp/path spaces % $ \" \\\\";
    anyhow::ensure!(launchd_value(value)? == value);
    anyhow::ensure!(launchd_value(&serde_json::to_string(value)?)? == value);
    anyhow::ensure!(launchd_value("\"unterminated").is_err());
    Ok(())
}
#[test]
fn launchd_status_uses_actual_disabled_override() -> Result<()> {
    anyhow::ensure!(launchd_enabled(
        "disabled services = {\n}",
        "ai.risense.test"
    )?);
    anyhow::ensure!(!launchd_enabled(
        "disabled services = {\n \"ai.risense.test\" => true\n}",
        "ai.risense.test"
    )?);
    anyhow::ensure!(launchd_enabled(
        "disabled services = {\n \"ai.risense.test\" => false\n}",
        "ai.risense.test"
    )?);
    anyhow::ensure!(launchd_enabled(
        "disabled services = {\n \"ai.risense.test\" => unknown\n}",
        "ai.risense.test"
    )
    .is_err());
    anyhow::ensure!(launchd_enabled(
        "disabled services = {\n \"ai.risense.test\" => enabled\n}",
        "ai.risense.test"
    )?);
    anyhow::ensure!(!launchd_enabled(
        "disabled services = {\n \"ai.risense.test\" => disabled\n}",
        "ai.risense.test"
    )?);
    anyhow::ensure!(launchd_enabled("unrecognized", "ai.risense.test").is_err());
    Ok(())
}
#[cfg(target_os = "linux")]
#[test]
fn user_manager_lifecycle_refuses_loaded_overrides_and_preserves_data() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let mut config = super::super::tests::fixture(Platform::Linux);
    config.installation = temporary.path().canonicalize()?;
    config.user_home = config.installation.join("home");
    config.executable = config.installation.join("bin/rsrs");
    let (_scope, state) = test_adapter::install(&config);
    anyhow::ensure!(!inspect(&config)?.registered);
    install(&mut config)?;
    anyhow::ensure!(inspect(&config)?.enabled);
    start(&config)?;
    anyhow::ensure!(inspect(&config)?.running);
    let changes = state.borrow().changes.len();
    let alias = config.installation.join("unit-link.service");
    std::os::unix::fs::symlink(item(&config), &alias)?;
    state.borrow_mut().fragment = Some(alias.clone());
    anyhow::ensure!(
        inspect(&config)?.registered,
        "exact resolved owned source should be recognized"
    );
    std::fs::remove_file(&alias)?;
    let foreign = config.installation.join("foreign.service");
    std::fs::copy(item(&config), &foreign)?;
    std::os::unix::fs::symlink(&foreign, &alias)?;
    anyhow::ensure!(
        inspect(&config).is_err(),
        "matching bytes at an unrelated source are not ownership"
    );
    state.borrow_mut().fragment = None;
    state.borrow_mut().dropin = true;
    anyhow::ensure!(stop(&config).is_err());
    anyhow::ensure!(remove(&config).is_err());
    anyhow::ensure!(state.borrow().changes.len() == changes && item(&config).exists());
    state.borrow_mut().dropin = false;
    anyhow::ensure!(
        remove(&config).is_err(),
        "running service must not be removed"
    );
    stop(&config)?;
    disable(&config)?;
    reset_backoff(&config)?;
    anyhow::ensure!(!inspect(&config)?.enabled && !inspect(&config)?.running);
    let preserved = config.installation.join("library.db");
    std::fs::write(&preserved, b"synthetic data")?;
    remove(&config)?;
    anyhow::ensure!(!item(&config).exists() && std::fs::read(preserved)? == b"synthetic data");
    config.artifact_hashes.clear();
    anyhow::ensure!(!inspect(&config)?.registered);
    Ok(())
}
#[cfg(unix)]
#[test]
fn manager_control_is_bounded_private_and_reaps_its_timed_out_child() -> Result<()> {
    let _lock = crate::TEST_ENV_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("test environment lock poisoned"))?;
    struct Restore(Option<std::ffi::OsString>);
    impl Drop for Restore {
        fn drop(&mut self) {
            match &self.0 {
                Some(v) => std::env::set_var("RSRS_SUPER", v),
                None => std::env::remove_var("RSRS_SUPER"),
            }
        }
    }
    let _restore = Restore(std::env::var_os("RSRS_SUPER"));
    std::env::set_var("RSRS_SUPER", "synthetic-not-for-child");
    let output = checked(
        "sh",
        vec![
            "-c".into(),
            "test -z \"${RSRS_SUPER+x}\" && printf safe".into(),
        ],
    )?;
    anyhow::ensure!(output == "safe");
    anyhow::ensure!(checked("sh", vec!["-c".into(), "exit 7".into()]).is_err());
    anyhow::ensure!(control("sh", &["-c".into(), "head -c 65537 /dev/zero".into()]).is_err());
    anyhow::ensure!(control("sh", &["-c".into(), "printf '\\377'".into()]).is_err());
    anyhow::ensure!(program("respire-missing-manager-fixture").is_err());
    let temp = tempfile::tempdir()?;
    let pid_file = temp.path().join("child.pid");
    let error = control(
        "sh",
        &[
            "-c".into(),
            "printf '%s' \"$$\" > \"$1\"; exec sleep 90".into(),
            "respire-fixture".into(),
            pid_file.to_string_lossy().into_owned(),
        ],
    )
    .err()
    .context("timeout was not enforced")?;
    anyhow::ensure!(error.to_string().contains("timed out"));
    let pid: u32 = std::fs::read_to_string(pid_file)?.parse()?;
    anyhow::ensure!(
        process_birth(pid)?.is_none(),
        "timed out manager child was not reaped"
    );
    Ok(())
}
#[test]
fn encoded_control_preserves_unicode_and_stages_never_echo_arbitrary_output() -> Result<()> {
    use base64::Engine as _;
    let script = "[Console]::WriteLine('\u{4f60}\u{597d} \" $ \\');";
    let bytes = base64::engine::general_purpose::STANDARD.decode(encoded_powershell(script))?;
    let words: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    anyhow::ensure!(String::from_utf16(&words)? == script);
    anyhow::ensure!(
        last_control_stage(b"secret arbitrary output\nrsrs-control-stage:com-connected\n")
            == "com-connected"
    );
    anyhow::ensure!(last_control_stage(b"rsrs-control-stage:secret-value") == "unreported");
    Ok(())
}
#[test]
fn scheduler_paths_keep_identity_without_nt_namespace_or_environment_interpretation(
) -> Result<()> {
    anyhow::ensure!(
        windows_task_path(r"\\?\C:\safe folder\rsrs.exe")? == r"C:\safe folder\rsrs.exe"
    );
    anyhow::ensure!(windows_task_path(r"\\?\UNC\server\share\data")? == r"\\server\share\data");
    anyhow::ensure!(windows_task_path(r"\\?\GLOBALROOT\Device").is_err());
    anyhow::ensure!(control_hresult("rsrs-control-hresult:80070005") == "80070005");
    anyhow::ensure!(control_hresult("rsrs-control-hresult:secret-material") == "unreported");
    anyhow::ensure!(control_xml_location("rsrs-control-xml-location:1,39") == "1,39");
    for unsafe_value in ["1,secret", "123456789,2", "1,2,3", ",2"] {
        anyhow::ensure!(
            control_xml_location(&format!("rsrs-control-xml-location:{unsafe_value}"))
                == "unreported"
        );
    }
    #[cfg(target_os = "linux")]
    {
        validate_executable(Platform::Linux, Path::new("/tmp/data spaces % $/rsrs"))?;
        for path in [
            "/tmp/bad\"/rsrs",
            "/tmp/bad'/rsrs",
            r"/tmp/bad\name/rsrs",
            "relative/rsrs",
        ] {
            anyhow::ensure!(validate_executable(Platform::Linux, Path::new(path)).is_err());
        }
    }
    Ok(())
}
#[cfg(windows)]
#[test]
fn scheduler_default_runlevel_requires_effective_limited_and_rejects_unknown_values(
) -> Result<()> {
    let script = format!(
        r#"{}
$cases=@(
 @{{xml='';value=0;allowed=$true}},@{{xml='LeastPrivilege';value=0;allowed=$true}},
 @{{xml='HighestAvailable';value=0;allowed=$false}},@{{xml='';value=1;allowed=$false}},
 @{{xml='LeastPrivilege';value=1;allowed=$false}},@{{xml='';value=$null;allowed=$false}},
 @{{xml='';value='0';allowed=$false}},@{{xml='';value=$false;allowed=$false}},
 @{{xml='unknown';value=0;allowed=$false}},@{{xml='LeastPrivilege';value=-1;allowed=$false}});
foreach($case in $cases){{$accepted=$true;try{{RequireLimited $case.xml $case.value}}catch{{$accepted=$false}};if($accepted -ne $case.allowed){{throw 'Effective task run-level contract failed'}}}};
[Console]::Out.WriteLine('limited-runlevel-contract:passed');"#,
        task_runlevel_guard()
    );
    anyhow::ensure!(powershell(script)?.trim() == "limited-runlevel-contract:passed");
    Ok(())
}

#[cfg(windows)]
#[test]
fn scheduler_validates_unicode_xml_variants_without_registering_a_task() -> Result<()> {
    use base64::Engine as _;
    let temporary = tempfile::tempdir()?;
    let mut config = super::super::tests::fixture(Platform::Windows);
    config.installation = temporary.path().canonicalize()?;
    config.executable = std::env::current_exe()?.canonicalize()?;
    config.owner = owner()?;
    config.id = super::super::identity(&config.installation, &config.owner);
    let xml = super::super::render(&config)?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(xml.as_bytes());
    // TASK_VALIDATE_ONLY=1 checks the identical payload without registering,
    // updating, enabling or running a task. Compare only the declaration.
    let script = format!(
        r#"{guard};if($null -ne $t){{throw 'Validation fixture task already exists'}};
$xml=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{encoded}'));
$utf8=$xml.Replace('<?xml version="1.0"?>','<?xml version="1.0" encoding="UTF-8"?>');
try{{[void]$folder.RegisterTask({task},$utf8,1,{owner},$null,3,$null);[Console]::Out.WriteLine('utf8:valid')}}catch{{$failure=$_.Exception;while($null -ne $failure.InnerException){{$failure=$failure.InnerException}};[Console]::Out.WriteLine('utf8:hresult:'+$failure.HResult.ToString('X8'))}};
[void]$folder.RegisterTask({task},$xml,1,{owner},$null,3,$null);[Console]::Out.WriteLine('neutral:valid');
{guard};if($null -ne $t){{throw 'Validation-only unexpectedly created a task'}};
"#,
        guard = task_guard(&config)?,
        task = ps_literal(&name(&config))?,
        owner = ps_literal(&config.owner)?
    );
    let result = powershell(script)?;
    anyhow::ensure!(
        result.lines().any(|line| line == "neutral:valid"),
        "encoding-neutral task XML did not validate"
    );
    // Fixed protocol fields only; never print XML, paths or arbitrary COM errors.
    println!("Task Scheduler validation-only comparison: {result}");
    Ok(())
}

#[test]
fn windows_quoting_and_task_guard_are_literal_and_current_user_only() -> Result<()> {
    anyhow::ensure!(windows_argument("C:\\user folder\\")? == "\"C:\\user folder\\\\\"");
    anyhow::ensure!(ps_literal("a';&b")? == "'a'';&b'");
    let config = super::super::tests::fixture(Platform::Windows);
    let script = task_guard(&config)?;
    anyhow::ensure!(
        script.contains("Interactive")
            && script.contains("LeastPrivilege")
            && script.contains("$arguments -cne")
    );
    anyhow::ensure!(
        !script.contains("RunLevel Highest") && !script.contains("ExecutionPolicy Bypass")
    );
    Ok(())
}
