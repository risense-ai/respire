pub(super) fn inspect(config: &Registration) -> Result<State> {
    file_guard(config)?;
    match config.platform {
        Platform::Linux => {
            let output = control(
                "systemctl",
                &[
                    "--user".into(),
                    "show".into(),
                    name(config),
                    "--no-pager".into(),
                    "--property=LoadState,ActiveState,MainPID,FragmentPath,UnitFileState".into(),
                ],
            )?;
            anyhow::ensure!(
                output.code == 0 || output.code == 4,
                "systemd user session unavailable; no system/linger fallback is performed"
            );
            let fields: std::collections::BTreeMap<_, _> = output
                .stdout
                .lines()
                .filter_map(|line| line.split_once('='))
                .collect();
            let registered = match fields.get("LoadState").copied() {
                Some("not-found") => false,
                Some("loaded") => true,
                _ => bail!("systemd unit load state is unavailable or unexpected; no lifecycle action taken"),
            };
            if registered {
                let fragment = fields
                    .get("FragmentPath")
                    .context("systemd unit path missing")?;
                // systemctl link can expose an installation-owned source via a
                // user-unit symlink. Require the exact same resolved file, not
                // merely matching bytes at an unrelated path.
                anyhow::ensure!(
                    Path::new(fragment).canonicalize()? == item(config).canonicalize()?,
                    "unknown systemd user unit path; refusing lifecycle action"
                );
                let directory = systemd_property(
                    config,
                    "org.freedesktop.systemd1.Service",
                    "WorkingDirectory",
                )?;
                anyhow::ensure!(
                    directory["data"].as_str().map(Path::new)
                        == Some(config.installation.as_path()),
                    "systemd loaded working directory differs from this installation"
                );
                verify_systemd_loaded(
                    config,
                    &systemd_property(config, "org.freedesktop.systemd1.Service", "ExecStart")?,
                    &systemd_property(config, "org.freedesktop.systemd1.Unit", "DropInPaths")?,
                )?;
            }
            Ok(State {
                registered,
                loaded: registered,
                running: fields.get("ActiveState") == Some(&"active"),
                enabled: fields
                    .get("UnitFileState")
                    .is_some_and(|v| matches!(*v, "enabled" | "enabled-runtime")),
            })
        }
        Platform::Macos => {
            let disabled = checked("launchctl", vec!["print-disabled".into(), domain(config)])?;
            let enabled = launchd_enabled(&disabled, &name(config))?;
            let output = control("launchctl", &["print".into(), label(config)])?;
            if output.code != 0 {
                // print-disabled above already verified the user domain; a
                // full domain print can exceed the deliberately bounded output.
                anyhow::ensure!(
                    [3, 113].contains(&output.code),
                    "LaunchAgent query failed; refusing lifecycle action"
                );
                return Ok(State {
                    registered: item(config).is_file(),
                    loaded: false,
                    running: false,
                    enabled,
                });
            }
            let property = |key: &str| -> Result<Option<String>> {
                output
                    .stdout
                    .lines()
                    .find_map(|line| line.trim().strip_prefix(&format!("{key} = ")))
                    .map(launchd_value)
                    .transpose()
            };
            anyhow::ensure!(
                property("path")?.is_some_and(|value| Path::new(&value).canonicalize().is_ok_and(
                    |actual| item(config)
                        .canonicalize()
                        .is_ok_and(|expected| actual == expected)
                )),
                "unknown loaded LaunchAgent path; refusing lifecycle action"
            );
            anyhow::ensure!(
                property("program")?.is_some_and(|value| Path::new(&value)
                    .canonicalize()
                    .is_ok_and(|actual| config
                        .executable
                        .canonicalize()
                        .is_ok_and(|expected| actual == expected))),
                "unknown loaded LaunchAgent program; refusing lifecycle action"
            );
            let args = output
                .stdout
                .split("arguments = {\n")
                .nth(1)
                .and_then(|s| s.split("\n\t}").next())
                .context("loaded LaunchAgent arguments unavailable")?;
            let actual: Vec<String> = args.lines().map(launchd_value).collect::<Result<_>>()?;
            let expected = [
                config.executable.to_str().context("non-UTF8 executable")?,
                "runtime",
                "supervise",
                "--data-dir",
                config
                    .installation
                    .to_str()
                    .context("non-UTF8 installation")?,
            ];
            anyhow::ensure!(
                actual.len() == expected.len()
                    && actual
                        .iter()
                        .zip(expected)
                        .all(|(actual, expected)| actual == expected),
                "loaded LaunchAgent arguments changed; refusing lifecycle action"
            );
            Ok(State {
                registered: true,
                loaded: true,
                running: property("state")?.as_deref() == Some("running"),
                enabled,
            })
        }
        Platform::Windows => {
            let text=powershell(format!("{};$registered=($null -ne $t);$running=($registered -and $t.State -eq 4);$enabled=($registered -and [bool]$t.Enabled);[Console]::Out.WriteLine($registered.ToString().ToLowerInvariant());[Console]::Out.WriteLine($running.ToString().ToLowerInvariant());[Console]::Out.WriteLine($enabled.ToString().ToLowerInvariant())",task_guard(config)?))?;
            let flags: Vec<bool> = text
                .lines()
                .map(str::parse)
                .collect::<std::result::Result<_, _>>()?;
            anyhow::ensure!(
                flags.len() == 3,
                "invalid scheduled task status field count"
            );
            Ok(State {
                registered: flags[0],
                loaded: flags[0],
                running: flags[1],
                enabled: flags[2],
            })
        }
    }
}

pub(super) fn install(config: &mut Registration) -> Result<()> {
    validate_executable(config.platform, &config.executable)?;
    let before = inspect(config)?;
    anyhow::ensure!(
        !before.registered || config.artifact_hashes.contains_key("service"),
        "unknown existing registration; refusing replacement"
    );
    let content = render(config)?;
    private_write(
        &item(config),
        content.as_bytes(),
        config.artifact_hashes.get("service").map(String::as_str),
    )?;
    config
        .artifact_hashes
        .insert("service".into(), hash(content.as_bytes()));
    match config.platform {
        Platform::Linux => {
            checked(
                "systemctl",
                vec![
                    "--user".into(),
                    "link".into(),
                    item(config).to_string_lossy().into_owned(),
                ],
            )?;
            checked("systemctl", vec!["--user".into(), "daemon-reload".into()])?;
            checked(
                "systemctl",
                vec!["--user".into(), "enable".into(), name(config)],
            )?;
        }
        Platform::Macos => {
            checked("launchctl", vec!["enable".into(), label(config)])?;
        }
        Platform::Windows => {
            powershell(format!("{};$xml=[IO.File]::ReadAllText({});[void]$folder.RegisterTask({},$xml,{},{},$null,3,$null)",task_guard(config)?,ps_literal(item(config).to_str().context("non-UTF8 task path")?)?,ps_literal(&name(config))?,if before.registered{6}else{2},ps_literal(&config.owner)?))?;
        }
    }
    Ok(())
}

pub(super) fn reset_backoff(config: &Registration) -> Result<()> {
    let state = inspect(config)?;
    if state.registered && config.platform == Platform::Linux {
        checked(
            "systemctl",
            vec!["--user".into(), "reset-failed".into(), name(config)],
        )?;
    }
    Ok(())
}

pub(super) fn start(config: &Registration) -> Result<()> {
    let current = inspect(config)?;
    anyhow::ensure!(
        current.registered,
        "owned autostart registration is missing"
    );
    match config.platform {
        Platform::Linux => {
            checked(
                "systemctl",
                vec!["--user".into(), "start".into(), name(config)],
            )?;
        }
        Platform::Macos => {
            if !current.loaded {
                checked(
                    "launchctl",
                    vec![
                        "bootstrap".into(),
                        domain(config),
                        item(config).to_string_lossy().into_owned(),
                    ],
                )?;
            } else if !current.running {
                checked("launchctl", vec!["kickstart".into(), label(config)])?;
            }
        }
        Platform::Windows => {
            powershell(format!(
                "{};if($null -eq $t){{throw 'Owned task disappeared'}};[void]$t.Run($null)",
                task_guard(config)?
            ))?;
        }
    }
    Ok(())
}

pub(super) fn stop(config: &Registration) -> Result<()> {
    let current = inspect(config)?;
    match config.platform {
        Platform::Linux => {
            if current.registered {
                checked(
                    "systemctl",
                    vec!["--user".into(), "stop".into(), name(config)],
                )?;
            }
        }
        Platform::Macos => {
            if current.loaded {
                checked("launchctl", vec!["bootout".into(), label(config)])?;
            }
        }
        Platform::Windows => {
            if current.running {
                powershell(format!(
                    "{};if($null -ne $t){{$t.Stop(0);$deadline=[DateTime]::UtcNow.AddSeconds(15);while($folder.GetTask({}).State -eq 4){{if([DateTime]::UtcNow -ge $deadline){{throw 'Owned task did not stop'}};[Threading.Thread]::Sleep(100)}}}}",
                    task_guard(config)?,ps_literal(&name(config))?
                ))?;
            }
        }
    }
    Ok(())
}

pub(super) fn disable(config: &Registration) -> Result<()> {
    let current = inspect(config)?;
    match config.platform {
        Platform::Linux => {
            if current.registered {
                checked(
                    "systemctl",
                    vec!["--user".into(), "disable".into(), name(config)],
                )?;
                // Disabling can remove the user-unit link for an external
                // owned source. Refresh cached FragmentPath before readback.
                checked("systemctl", vec!["--user".into(), "daemon-reload".into()])?;
            }
        }
        Platform::Macos => {
            checked("launchctl", vec!["disable".into(), label(config)])?;
        }
        Platform::Windows => {
            if current.registered {
                powershell(format!(
                    "{};if($null -ne $t){{$t.Enabled=$false}}",
                    task_guard(config)?
                ))?;
            }
        }
    }
    Ok(())
}

pub(super) fn remove(config: &Registration) -> Result<()> {
    let current = inspect(config)?;
    anyhow::ensure!(
        !current.running,
        "stop the owned runtime before removing its startup registration"
    );
    match config.platform {
        Platform::Linux => {
            disable(config)?;
        }
        Platform::Macos => {
            if current.loaded {
                stop(config)?;
            }
            disable(config)?;
        }
        Platform::Windows => {
            if current.registered {
                powershell(format!(
                    "{};if($null -ne $t){{$folder.DeleteTask({},0)}}",
                    task_guard(config)?,
                    ps_literal(&name(config))?
                ))?;
            }
        }
    }
    file_guard(config)?;
    if item(config).is_file() {
        std::fs::remove_file(item(config))?;
    }
    if config.platform == Platform::Linux {
        checked("systemctl", vec!["--user".into(), "daemon-reload".into()])?;
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
pub(super) mod test_adapter {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    #[derive(Default)]
    pub struct Fake {
        pub registered: bool,
        pub enabled: bool,
        pub running: bool,
        pub dropin: bool,
        pub fragment: Option<PathBuf>,
        pub fail: Option<String>,
        pub changes: Vec<String>,
    }
    pub struct Scope;
    impl Drop for Scope {
        fn drop(&mut self) {
            TEST_CONTROL.with(|slot| *slot.borrow_mut() = None);
        }
    }
    pub fn install(config: &Registration) -> (Scope, Rc<RefCell<Fake>>) {
        let state = Rc::new(RefCell::new(Fake::default()));
        let shared = state.clone();
        let config = config.clone();
        TEST_CONTROL.with(|slot| *slot.borrow_mut() = Some(Box::new(move |name,args| {
            let mut state = shared.borrow_mut();
            let ok = |stdout:String| Ok(Output{code:0,stdout});
            match name {
                "id" => ok(config.owner.clone()),
                "systemctl" => {
                    anyhow::ensure!(args.first().map(String::as_str) == Some("--user"), "fake refuses non-user operation");
                    let operation = args.get(1).context("manager operation missing")?;
                    if state.fail.as_ref() == Some(operation) { return Ok(Output{code:1,stdout:String::new()}); }
                    if operation == "show" { return ok(format!("LoadState={}\nActiveState={}\nMainPID=0\nFragmentPath={}\nUnitFileState={}\n", if state.registered {"loaded"}else{"not-found"},if state.running {"active"}else{"inactive"},state.fragment.clone().unwrap_or_else(||item(&config)).display(),if state.enabled {"enabled"}else{"disabled"})); }
                    state.changes.push(operation.clone());
                    match operation.as_str() {
                        "link" => state.registered = true,
                        "enable" => state.enabled = true,
                        "disable" => state.enabled = false,
                        "start" => state.running = true,
                        "stop" => state.running = false,
                        "daemon-reload" => {if !item(&config).exists() {state.registered=false;}},
                        "reset-failed" => {},
                        _ => bail!("unexpected fake operation"),
                    }
                    ok(String::new())
                },
                "busctl" => {
                    if args.iter().any(|v|v == "GetUnit") { return ok(serde_json::json!({"data":["/org/freedesktop/systemd1/unit/test"]}).to_string()); }
                    match args.last().map(String::as_str) {
                        Some("DropInPaths") => ok(serde_json::json!({"data":if state.dropin {vec!["/tmp/unknown.conf"]}else{vec![]}}).to_string()),
                        Some("WorkingDirectory") => ok(serde_json::json!({"data":config.installation}).to_string()),
                        Some("ExecStart") => ok(serde_json::json!({"data":[[config.executable,[config.executable,"runtime","supervise","--data-dir",config.installation],false,0,0,0,0,0,0,0]]}).to_string()),
                        _ => bail!("unexpected fake property"),
                    }
                },
                _ => bail!("unexpected fake program"),
            }
        })));
        (Scope, state)
    }
}
