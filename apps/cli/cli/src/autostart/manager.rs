//! Current-user OS adapters. The only accepted registrations have our exact owner,
//! installation-specific name, executable and arguments; never use elevation.
use super::{hash, plain_value, private_write, render, Platform, Registration};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Debug, Serialize)]
pub(super) struct State {
    pub registered: bool,
    pub loaded: bool,
    pub running: bool,
    pub enabled: bool,
}

struct Output {
    code: i32,
    stdout: String,
}

/// Creation identity only: never persist a controller command line, which may
/// contain login material. None means that the controller has exited.
pub(crate) fn process_birth(pid: u32) -> Result<Option<String>> {
    #[cfg(target_os = "linux")]
    {
        let raw = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(raw) => raw,
            // ESRCH (3) means this procfs task exited between open and read.
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    || error.raw_os_error() == Some(3) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        let fields = raw.rsplit_once(')').context("invalid process identity")?.1;
        if fields.split_whitespace().next() == Some("Z") {
            return Ok(None);
        }
        return Ok(Some(
            fields
                .split_whitespace()
                .nth(19)
                .context("process birth missing")?
                .into(),
        ));
    }
    #[cfg(windows)]
    {
        return Ok(respire_spawn::process_birth(pid)?.map(|value| value.to_string()));
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let output = control(
            "ps",
            &["-p".into(), pid.to_string(), "-o".into(), "stat=".into(), "-o".into(), "lstart=".into()],
        )?;
        darwin_process_birth(output.code, &output.stdout)
    }
}

#[cfg(any(test, all(unix, not(target_os = "linux"))))]
fn darwin_process_birth(code: i32, stdout: &str) -> Result<Option<String>> {
    if code == 1 && stdout.trim().is_empty() {
        return Ok(None);
    }
    anyhow::ensure!(code == 0, "process identity unavailable");
    let line = stdout.trim();
    anyhow::ensure!(!line.contains('\n'), "ambiguous process identity");
    let (state, birth) = line.split_once(char::is_whitespace)
        .context("process state or birth missing")?;
    anyhow::ensure!(!birth.trim().is_empty(), "process birth missing");
    // A zombie has already exited and released its resources. Waiting for its
    // parent (possibly this CLI) to reap it must not block a verified drain.
    if state.starts_with('Z') {
        return Ok(None);
    }
    Ok(Some(birth.trim().into()))
}

pub(super) fn validate_executable(platform: Platform, path: &Path) -> Result<()> {
    let value = path
        .to_str()
        .context("startup executable path is not UTF-8")?;
    plain_value(value)?;
    anyhow::ensure!(
        path.is_absolute(),
        "startup requires an absolute native path; use an absolute RSRS_DATA_DIR/RSRS_BIN_DIR"
    );
    if platform == Platform::Linux {
        anyhow::ensure!(!value.chars().any(|c|matches!(c,'\\'|'\"'|'\'')),
            "systemd does not support quotes/backslashes in executable paths; choose a safe absolute RSRS_BIN_DIR (the data directory may still contain these characters)");
    }
    Ok(())
}

pub(crate) fn windows_task_path(raw: &str) -> Result<String> {
    plain_value(raw)?;
    if let Some(unc) = raw.strip_prefix(r"\\?\UNC\") {
        return Ok(format!(r"\\{}", unc));
    }
    if let Some(dos) = raw.strip_prefix(r"\\?\") {
        let bytes = dos.as_bytes();
        anyhow::ensure!(
            bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && bytes[2] == b'\\',
            "unsupported Windows startup path namespace"
        );
        return Ok(dos.to_owned());
    }
    Ok(raw.to_owned())
}

fn program(name: &str) -> Result<PathBuf> {
    if name == "powershell" {
        let root = std::env::var_os("SystemRoot").context("Windows SystemRoot unavailable")?;
        let path = PathBuf::from(root).join("System32/WindowsPowerShell/v1.0/powershell.exe");
        anyhow::ensure!(path.is_file(), "Windows PowerShell unavailable");
        return Ok(path);
    }
    for base in ["/usr/bin", "/bin"] {
        let path = Path::new(base).join(name);
        if path.is_file() {
            return Ok(path);
        }
    }
    bail!("current-user service manager program unavailable: {name}")
}

#[cfg(test)]
type TestControl = Box<dyn FnMut(&str, &[String]) -> Result<Output>>;
#[cfg(test)]
thread_local! { static TEST_CONTROL: std::cell::RefCell<Option<TestControl>> = const { std::cell::RefCell::new(None) }; }

fn control(name: &str, arguments: &[String]) -> Result<Output> {
    #[cfg(test)]
    if let Some(output) = TEST_CONTROL.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .map(|handler| handler(name, arguments))
    }) {
        return output;
    }
    let mut command = Command::new(program(name)?);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_clear();
    for key in [
        "HOME",
        "USERPROFILE",
        "USER",
        "LOGNAME",
        "LANG",
        "LC_ALL",
        "SystemRoot",
        "WINDIR",
        "TEMP",
        "TMP",
        "APPDATA",
        "LOCALAPPDATA",
        "COMPUTERNAME",
        "COMSPEC",
        "PATHEXT",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramData",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    #[cfg(unix)]
    command.env("LC_ALL", "C");
    command.env(
        "PATH",
        if cfg!(windows) {
            std::env::var_os("SystemRoot")
                .map(|root| PathBuf::from(root).join("System32"))
                .unwrap_or_default()
        } else {
            PathBuf::from("/usr/bin:/bin")
        },
    );
    let mut child = respire_spawn::spawn_host_control(&mut command)
        .context("could not invoke current-user service manager")?;
    let stdout = child
        .stdout
        .take()
        .context("service manager stdout missing")?;
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut data = Vec::new();
        let result = stdout.take(65537).read_to_end(&mut data).map(|_| data);
        let _ = tx.send(result);
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            child
                .kill()
                .context("could not stop timed-out service manager client")?;
            child.wait()?;
            let partial = rx
                .recv_timeout(Duration::from_secs(1))
                .ok()
                .and_then(Result::ok)
                .unwrap_or_default();
            let stage = last_control_stage(&partial);
            bail!("current-user service manager request timed out at fixed stage {stage}; inspect its state before retrying");
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let bytes = rx
        .recv_timeout(Duration::from_secs(1))
        .context("service manager output did not close")??;
    anyhow::ensure!(
        bytes.len() <= 65536,
        "service manager response exceeded limit"
    );
    Ok(Output {
        code: status.code().unwrap_or(-1),
        stdout: String::from_utf8(bytes).context("service manager returned invalid UTF-8")?,
    })
}

const CONTROL_STAGES: &[&str] = &[
    "script",
    "com-created",
    "com-connected",
    "task-looked-up",
    "task-xml-loaded",
    "task-shape",
    "principal-sid",
    "principal-logon",
    "principal-runlevel",
    "principal-runlevel-omitted-limited",
    "trigger-sid",
    "action-executable",
    "action-arguments",
    "action-directory",
    "task-marker",
    "task-validated",
];
fn control_stage(line: &str) -> Option<&str> {
    let value = line
        .trim_start_matches('\u{feff}')
        .strip_prefix("rsrs-control-stage:")?;
    CONTROL_STAGES.contains(&value).then_some(value)
}
fn last_control_stage(bytes: &[u8]) -> String {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|value| value.lines().filter_map(control_stage).last())
        .unwrap_or("unreported")
        .into()
}
fn control_hresult(text: &str) -> &str {
    text.lines()
        .filter_map(|line| line.strip_prefix("rsrs-control-hresult:"))
        .filter(|value| value.len() == 8 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .last()
        .unwrap_or("unreported")
}

fn control_xml_location(text: &str) -> &str {
    text.lines()
        .filter_map(|line| line.strip_prefix("rsrs-control-xml-location:"))
        .filter(|value| {
            value.split_once(',').is_some_and(|(line, column)| {
                [line, column].into_iter().all(|part| {
                    !part.is_empty()
                        && part.len() <= 8
                        && part.bytes().all(|byte| byte.is_ascii_digit())
                })
            })
        })
        .last()
        .unwrap_or("unreported")
}

fn encoded_powershell(script: &str) -> String {
    use base64::Engine as _;
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn checked(name: &str, args: Vec<String>) -> Result<String> {
    let result = control(name, &args)?;
    if result.code != 0 && name == "powershell" {
        bail!("Windows current-user operation failed (exit {}, fixed stage {}, HRESULT {}, XML location {}); inspect the owned registration",result.code,last_control_stage(result.stdout.as_bytes()),control_hresult(&result.stdout),control_xml_location(&result.stdout));
    }
    anyhow::ensure!(
        result.code == 0,
        "{name} current-user operation failed (exit {}); inspect the owned registration",
        result.code
    );
    Ok(result.stdout)
}

fn ps_literal(value: &str) -> Result<String> {
    plain_value(value)?;
    Ok(format!("'{}'", value.replace('\'', "''")))
}

fn powershell(script: String) -> Result<String> {
    let operation = [
        ("WindowsIdentity", "query current user"),
        (".DeleteTask(", "remove owned task"),
        (".RegisterTask(", "register owned task"),
        (".Run($null)", "start owned task"),
        (".Stop(0)", "stop owned task"),
        ("$t.Enabled=$false", "disable owned task"),
        (".GetTask(", "inspect owned task"),
    ]
    .into_iter()
    .find_map(|(needle, label)| script.contains(needle).then_some(label))
    .unwrap_or("query process");
    let wrapped=format!("$ErrorActionPreference='Stop';$ProgressPreference='SilentlyContinue';[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false);[Console]::Out.WriteLine('rsrs-control-stage:script');try{{{script}}}catch{{$failure=$_.Exception;while($null -ne $failure.InnerException){{$failure=$failure.InnerException}};[Console]::Out.WriteLine('rsrs-control-hresult:'+$failure.HResult.ToString('X8'));$location=[regex]::Match($failure.Message,'\\(([0-9]{{1,8}}),([0-9]{{1,8}})\\)');if($location.Success){{[Console]::Out.WriteLine('rsrs-control-xml-location:'+$location.Groups[1].Value+','+$location.Groups[2].Value)}};exit 1}};exit 0");
    let text = checked(
        "powershell",
        vec![
            "-NoLogo".into(),
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-EncodedCommand".into(),
            encoded_powershell(&wrapped),
        ],
    )
    .with_context(|| format!("Windows current-user operation failed: {operation}"))?;
    Ok(text
        .trim_start_matches('\u{feff}')
        .lines()
        .filter(|line| control_stage(line).is_none())
        .collect::<Vec<_>>()
        .join("\n"))
}

include!("manager/registration.rs");

include!("manager/services.rs");

#[cfg(test)]
mod tests;
