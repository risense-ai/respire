use super::*;

pub fn process_is_running(pid: u32) -> Result<bool> {
    #[cfg(target_os = "linux")]
    {
        anyhow::ensure!(pid != 0, "invalid runtime process identity");
        let own = std::fs::read_to_string("/proc/self/stat")
            .context("runtime process inspection requires procfs")?;
        anyhow::ensure!(
            linux_stat_is_running(&own, std::process::id())?,
            "cannot verify current process namespace"
        );
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => linux_stat_is_running(&stat, pid),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error).context("runtime process inspection failed"),
        }
    }
    #[cfg(windows)]
    {
        // Query-limited kernel handles avoid spawning tasklist on every drain
        // poll and retain fail-closed errors for inaccessible process identity.
        Ok(respire_spawn::process_birth(pid)?.is_some())
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let output = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "pid=", "-o", "stat="])
            .output()
            .context("cannot check stopped runtime process")?;
        let text = String::from_utf8_lossy(&output.stdout);
        anyhow::ensure!(
            output.status.success() || output.stderr.is_empty(),
            "runtime process inspection failed"
        );
        let mut fields = text.split_whitespace();
        // Zombies have released their listener, native session and database locks.
        Ok(fields.next() == Some(pid.to_string().as_str())
            && fields.next().is_some_and(|state| !state.starts_with('Z')))
    }
}

#[cfg(target_os = "linux")]
fn linux_stat_is_running(stat: &str, pid: u32) -> Result<bool> {
    anyhow::ensure!(
        stat.starts_with(&format!("{pid} (")),
        "runtime process PID mismatch"
    );
    let (_, fields) = stat
        .rsplit_once(')')
        .context("runtime process name delimiter missing")?;
    let state = fields
        .split_whitespace()
        .next()
        .context("runtime process state missing")?;
    anyhow::ensure!(
        state.len() == 1 && "RSDZTtXxKWPI".contains(state),
        "runtime process state is invalid"
    );
    // Zombies have exited and released library locks. Unknown states fail closed.
    Ok(state != "Z")
}

#[derive(Debug, PartialEq, Eq)]
pub struct RuntimeProcessIdentity {
    pub started_at: String,
    pub executable: String,
}

/// An endpoint PID is only a locator. Verify runtime arguments and creation time
/// before waiting, then compare again before forcefully terminating the process.
fn foreground_runtime_arguments(args: &[&str]) -> bool {
    let standalone = args.first().is_some_and(|program| {
        Path::new(program).file_name().and_then(|name| name.to_str()) == Some("rsrs-runtime")
    });
    let mut args = args.iter().copied().skip(1);
    let command = loop {
        match args.next() {
            Some("--json" | "--direct") => continue,
            other => break other,
        }
    };
    (standalone && command == Some("serve"))
        || (command == Some("runtime") && args.next() == Some("run"))
}

fn runtime_role_arguments(executable: &Path, args: &[&str]) -> bool {
    let image_name = executable
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.strip_suffix(" (deleted)").unwrap_or(name));
    let standalone = image_name == Some("rsrs-runtime");
    if standalone {
        if args.get(1) == Some(&"--runtime-internal") {
            return matches!(&args[2..], ["--runtime-background-internal", "--port", port]
                if port.parse::<u16>().is_ok_and(|port| port > 0));
        }
        let mut args = args.iter().copied().skip(1);
        while let Some(argument) = args.next() {
            match argument {
                "--json" | "--supervised" | "serve" => {}
                "runtime" if args.next() == Some("start") => {}
                "--host" if args.next().is_some_and(|value| !value.is_empty()) => {}
                "--port"
                    if args
                        .next()
                        .and_then(|value| value.parse::<u16>().ok())
                        .is_some_and(|port| port > 0) => {}
                "--data-dir" if args.next().is_some() => {}
                _ => return false,
            }
        }
        // Bare invocation serves too; consumed option values are never modes.
        return true;
    }
    if image_name != Some("rsrs") {
        return false;
    }
    if args.contains(&"--runtime-internal")
        || (args.contains(&"web") && args.contains(&"--internal"))
    {
        return true;
    }
    combined_foreground_runtime_arguments(args)
}

fn combined_foreground_runtime_arguments(args: &[&str]) -> bool {
    let mut args = args.iter().copied().skip(1);
    let command = loop {
        match args.next() {
            Some("--json" | "--direct") => continue,
            other => break other,
        }
    };
    command == Some("runtime") && args.next() == Some("run")
}

#[cfg(target_os = "linux")]
fn linux_process_arguments(command: &[u8]) -> Result<Vec<&str>> {
    command
        .strip_suffix(&[0])
        .context("runtime command line is not terminated")?
        .split(|byte| *byte == 0)
        .map(std::str::from_utf8)
        .collect::<std::result::Result<_, _>>()
        .context("runtime command line is not UTF-8")
}

#[cfg(any(test, all(unix, not(target_os = "linux"))))]
fn darwin_runtime_name(output: std::io::Result<std::process::Output>) -> Result<()> {
    let output = output.map_err(|error| anyhow::anyhow!(
        "stage=process_name class=ps_spawn_failed os_kind={:?} os_code={:?}",
        error.kind(), error.raw_os_error()
    ))?;
    anyhow::ensure!(output.status.success(),
        "stage=process_name class=ps_failed ps_status={}", output.status);
    let text = String::from_utf8_lossy(&output.stdout);
    let name = text.trim();
    anyhow::ensure!(!name.is_empty(), "stage=process_name class=empty_image");
    anyhow::ensure!(name == "rsrs" || name.ends_with("/rsrs")
        || name == "rsrs-runtime" || name.ends_with("/rsrs-runtime"),
        "stage=process_name class=image_name_mismatch");
    Ok(())
}

pub fn runtime_process_identity(pid: u32) -> Result<RuntimeProcessIdentity> {
    #[cfg(any(windows, target_os = "linux"))]
    anyhow::ensure!(
        pid != std::process::id() && pid_is_respire(pid),
        "cannot verify runtime process {pid}"
    );
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        anyhow::ensure!(pid != 0 && pid != std::process::id(),
            "cannot verify runtime process {pid}: stage=process_name class=invalid_pid");
        darwin_runtime_name(std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "comm="])
            .output()).with_context(|| {
                // This observation explains a refusal; it never authorizes cleanup or exit.
                let observation = match process_is_running(pid) {
                    Ok(true) => "running".to_owned(),
                    Ok(false) => "not_observed_running".to_owned(),
                    Err(error) => format!("unconfirmed observation_error={error:#}"),
                };
                format!("cannot verify runtime process {pid}: exit_observation={observation}")
            })?;
    }
    #[cfg(windows)]
    {
        let process = respire_spawn::runtime_process_identity(pid)?
            .context("runtime process disappeared or could not be identified")?;
        let executable = process
            .executable
            .to_str()
            .context("runtime process image is not UTF-8")?
            .to_owned();
        Ok(RuntimeProcessIdentity {
            started_at: process.birth.to_string(),
            executable,
        })
    }
    #[cfg(target_os = "linux")]
    {
        let directory = std::path::PathBuf::from(format!("/proc/{pid}"));
        let command = std::fs::read(directory.join("cmdline"))?;
        let args = linux_process_arguments(&command)?;
        let executable = std::fs::read_link(directory.join("exe"))?;
        anyhow::ensure!(
            runtime_role_arguments(&executable, &args),
            "process {pid} is not a runtime; restart refused"
        );
        let stat = std::fs::read_to_string(directory.join("stat"))?;
        let started_at = stat
            .rsplit_once(')')
            .and_then(|(_, fields)| fields.split_whitespace().nth(19))
            .context("runtime process creation time missing")?
            .to_owned();
        let executable = executable.to_string_lossy().into_owned();
        Ok(RuntimeProcessIdentity {
            started_at,
            executable,
        })
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let output = std::process::Command::new("ps")
            .args(["-ww", "-p", &pid.to_string(), "-o", "lstart=", "-o", "args="])
            .env("LC_ALL", "C")
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "cannot inspect runtime process {pid}"
        );
        let text = String::from_utf8(output.stdout)?;
        let text = text.trim_end();
        let started_at = text
            .get(..24)
            .context("runtime process creation time missing")?
            .to_owned();
        let command = text
            .get(24..)
            .context("runtime process command missing")?
            .trim();
        let image = std::process::Command::new("ps")
            .args(["-ww", "-p", &pid.to_string(), "-o", "comm="])
            .env("LC_ALL", "C")
            .output()?;
        anyhow::ensure!(image.status.success(), "cannot inspect runtime process image");
        let executable = String::from_utf8(image.stdout)?.trim().to_owned();
        let tail = command
            .strip_prefix(&executable)
            .filter(|tail| tail.is_empty() || tail.starts_with(char::is_whitespace))
            .context("runtime command does not match its process image")?;
        let mut args = vec![executable.as_str()];
        args.extend(tail.split_whitespace());
        anyhow::ensure!(
            args.contains(&"--runtime-internal")
                || (args.contains(&"web") && args.contains(&"--internal"))
                || foreground_runtime_arguments(&args),
            "process {pid} is not a runtime; restart refused"
        );
        Ok(RuntimeProcessIdentity {
            started_at,
            executable,
        })
    }
}

pub fn kill_runtime(pid: u32, identity: &RuntimeProcessIdentity) -> Result<()> {
    crate::runtime_policy::require_host("runtime recovery")?;
    anyhow::ensure!(
        pid != std::process::id() && pid_is_respire(pid),
        "refusing to terminate an unverified runtime process {pid}"
    );
    anyhow::ensure!(
        &runtime_process_identity(pid)? == identity,
        "runtime process {pid} changed during shutdown; termination refused"
    );
    #[cfg(windows)]
    {
        let expected = respire_spawn::ProcessIdentity {
            birth: identity
                .started_at
                .parse()
                .context("invalid verified process creation time")?,
            executable: std::path::PathBuf::from(&identity.executable),
        };
        respire_spawn::terminate_runtime(pid, &expected)?;
    }
    #[cfg(target_os = "linux")]
    {
        let expected = respire_spawn::ProcessIdentity {
            birth: identity
                .started_at
                .parse()
                .context("invalid verified process creation time")?,
            executable: std::path::PathBuf::from(&identity.executable),
        };
        respire_spawn::terminate_runtime(pid, &expected)?;
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let status = std::process::Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status()?;
        anyhow::ensure!(
            status.success(),
            "failed to terminate runtime process {pid}"
        );
    }
    Ok(())
}

pub fn kill_pid(pid: u32) {
    if pid == 0 || pid == std::process::id() {
        return;
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status();
    }
}

/// Used by tests and doctor: ensure the token file exists without starting HTTP.
#[cfg(test)]
mod tests;
