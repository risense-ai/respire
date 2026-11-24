pub(super) fn owner() -> Result<String> {
    let value = if cfg!(windows) {
        powershell("[Security.Principal.WindowsIdentity]::GetCurrent().User.Value".into())?
    } else {
        checked("id", vec!["-u".into()])?
    };
    let value = value.trim();
    anyhow::ensure!(
        !value.is_empty()
            && if cfg!(windows) {
                value.starts_with("S-")
                    && value
                        .chars()
                        .all(|c| c == 'S' || c == '-' || c.is_ascii_digit())
            } else {
                value.chars().all(|c| c.is_ascii_digit())
            },
        "could not identify current-user service owner"
    );
    Ok(value.to_owned())
}

pub(super) fn item(config: &Registration) -> PathBuf {
    match config.platform {
        Platform::Linux => config
            .user_home
            .join(".config/systemd/user")
            .join(format!("respire-runtime-{}.service", config.id)),
        Platform::Macos => config
            .user_home
            .join("Library/LaunchAgents")
            .join(format!("ai.risense.respire.runtime.{}.plist", config.id)),
        Platform::Windows => super::control_path(&config.installation).with_file_name("task.xml"),
    }
}

fn name(config: &Registration) -> String {
    match config.platform {
        Platform::Linux => format!("respire-runtime-{}.service", config.id),
        Platform::Macos => format!("ai.risense.respire.runtime.{}", config.id),
        Platform::Windows => format!("Respire-runtime-{}", config.id),
    }
}

fn domain(config: &Registration) -> String {
    format!("gui/{}", config.owner)
}

fn systemd_property(
    config: &Registration,
    interface: &str,
    property: &str,
) -> Result<serde_json::Value> {
    let lookup = checked(
        "busctl",
        vec![
            "--user".into(),
            "--json=short".into(),
            "call".into(),
            "org.freedesktop.systemd1".into(),
            "/org/freedesktop/systemd1".into(),
            "org.freedesktop.systemd1.Manager".into(),
            "GetUnit".into(),
            "s".into(),
            name(config),
        ],
    )?;
    let object: serde_json::Value = serde_json::from_str(&lookup)?;
    let path = object["data"][0]
        .as_str()
        .context("systemd unit object unavailable")?;
    anyhow::ensure!(
        path.starts_with("/org/freedesktop/systemd1/unit/"),
        "unexpected systemd unit object"
    );
    let value = checked(
        "busctl",
        vec![
            "--user".into(),
            "--json=short".into(),
            "get-property".into(),
            "org.freedesktop.systemd1".into(),
            path.into(),
            interface.into(),
            property.into(),
        ],
    )?;
    Ok(serde_json::from_str(&value)?)
}

fn verify_systemd_loaded(
    config: &Registration,
    exec: &serde_json::Value,
    dropins: &serde_json::Value,
) -> Result<()> {
    let overrides = dropins["data"]
        .as_array()
        .context("systemd loaded drop-in list unavailable")?;
    anyhow::ensure!(
        overrides.is_empty(),
        "unknown systemd drop-in override; refusing lifecycle action"
    );
    let entries = exec["data"]
        .as_array()
        .context("systemd loaded ExecStart unavailable")?;
    anyhow::ensure!(entries.len() == 1, "unknown systemd loaded ExecStart count");
    let row = entries[0]
        .as_array()
        .context("invalid systemd loaded ExecStart")?;
    let executable = config.executable.to_str().context("non-UTF8 executable")?;
    anyhow::ensure!(
        row.first().and_then(serde_json::Value::as_str) == Some(executable),
        "systemd loaded executable changed; refusing lifecycle action"
    );
    let argv = row
        .get(1)
        .and_then(serde_json::Value::as_array)
        .context("systemd loaded argv unavailable")?;
    let expected = [
        executable,
        "runtime",
        "supervise",
        "--data-dir",
        config
            .installation
            .to_str()
            .context("non-UTF8 installation")?,
    ];
    anyhow::ensure!(
        argv.len() == expected.len()
            && argv
                .iter()
                .zip(expected)
                .all(|(actual, expected)| actual.as_str() == Some(expected)),
        "systemd loaded arguments changed; refusing lifecycle action"
    );
    Ok(())
}
fn launchd_value(raw: &str) -> Result<String> {
    let raw = raw.trim();
    if raw.starts_with('"') {
        return serde_json::from_str(raw).context("unrecognized quoted launchd value");
    }
    plain_value(raw)?;
    Ok(raw.to_owned())
}

fn launchd_enabled(disabled: &str, name: &str) -> Result<bool> {
    anyhow::ensure!(
        disabled.contains("disabled services = {") && disabled.trim_end().ends_with('}'),
        "launchd disabled override status unavailable"
    );
    let key = format!("\"{name}\" => ");
    let mut matched = disabled
        .lines()
        .filter_map(|line| line.trim().strip_prefix(&key));
    let enabled = match matched.next() {
        None => true,
        Some("true" | "disabled") => false,
        Some("false" | "enabled") => true,
        Some(_) => bail!("unknown launchd disabled override value"),
    };
    anyhow::ensure!(matched.next().is_none(), "duplicate launchd override entry");
    Ok(enabled)
}

fn label(config: &Registration) -> String {
    format!("{}/{}", domain(config), name(config))
}

fn file_guard(config: &Registration) -> Result<()> {
    anyhow::ensure!(
        config.owner == owner()?,
        "autostart belongs to another OS user"
    );
    let path = item(config);
    if let Some(expected) = config.artifact_hashes.get("service") {
        let metadata = std::fs::symlink_metadata(&path)?;
        anyhow::ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= 65536,
            "autostart service file is not a bounded regular file"
        );
        anyhow::ensure!(
            hash(&std::fs::read(&path)?) == *expected,
            "autostart service file changed; refusing overwrite or removal"
        );
    } else {
        anyhow::ensure!(
            !path.try_exists()?,
            "unknown autostart service file; refusing replacement"
        );
    }
    Ok(())
}

// Windows argv quoting follows CommandLineToArgvW, including trailing slashes.
pub(super) fn windows_argument(value: &str) -> Result<String> {
    plain_value(value)?;
    let mut result = String::from("\"");
    let mut slashes = 0;
    for c in value.chars() {
        if c == '\\' {
            slashes += 1;
            continue;
        }
        result.push_str(&"\\".repeat(if c == '"' { slashes * 2 + 1 } else { slashes }));
        result.push(c);
        slashes = 0;
    }
    result.push_str(&"\\".repeat(slashes * 2));
    result.push('"');
    Ok(result)
}

fn arguments(config: &Registration) -> Result<String> {
    Ok(format!(
        "runtime supervise --data-dir {}",
        windows_argument(
            config
                .installation
                .to_str()
                .context("non-UTF8 installation")?
        )?
    ))
}

fn task_runlevel_guard() -> &'static str {
    // The scheduler omits default LeastPrivilege from serialized XML. Its
    // typed effective COM value is authoritative; unknown/Highest stays denied.
    r#"function RequireLimited($xmlValue,$effectiveValue){
 if($null -eq $effectiveValue -or $effectiveValue -isnot [int] -or $effectiveValue -ne 0 -or @('','LeastPrivilege') -cnotcontains $xmlValue){throw 'Unknown task run level; refusing lifecycle action'}
}"#
}

fn task_guard(config: &Registration) -> Result<String> {
    let task = ps_literal(&name(config))?;
    let owner = ps_literal(&config.owner)?;
    let exe = ps_literal(&windows_task_path(
        config.executable.to_str().context("non-UTF8 executable")?,
    )?)?;
    let root = ps_literal(&windows_task_path(
        config
            .installation
            .to_str()
            .context("non-UTF8 installation")?,
    )?)?;
    let args = ps_literal(&arguments(config)?)?;
    Ok(format!(
        r#"
{runlevel_guard}
function Sid($value){{try{{if($value -match '^S-\d-'){{return ([Security.Principal.SecurityIdentifier]::new($value)).Value}};return ([Security.Principal.NTAccount]::new($value)).Translate([Security.Principal.SecurityIdentifier]).Value}}catch{{return $null}}}}
$scheduler=[Activator]::CreateInstance([Type]::GetTypeFromProgID('Schedule.Service'));[Console]::Out.WriteLine('rsrs-control-stage:com-created');$scheduler.Connect();[Console]::Out.WriteLine('rsrs-control-stage:com-connected');$folder=$scheduler.GetFolder('\');$t=$null;
try{{$t=$folder.GetTask({task})}}catch{{$failure=$_.Exception;while($null -ne $failure.InnerException){{$failure=$failure.InnerException}};if($failure.HResult -ne -2147024894){{throw}}}}
[Console]::Out.WriteLine('rsrs-control-stage:task-looked-up');
if($null -ne $t){{
 if($t.Xml.Length -gt 65536){{throw 'Task XML exceeds ownership validation limit'}};
 $settings=[Xml.XmlReaderSettings]::new();$settings.DtdProcessing=[Xml.DtdProcessing]::Prohibit;$settings.XmlResolver=$null;
 $reader=[Xml.XmlReader]::Create([IO.StringReader]::new($t.Xml),$settings);
 $doc=[Xml.XmlDocument]::new();$doc.XmlResolver=$null;try{{$doc.Load($reader)}}finally{{$reader.Dispose()}};
 [Console]::Out.WriteLine('rsrs-control-stage:task-xml-loaded');
 $ns=[Xml.XmlNamespaceManager]::new($doc.NameTable);$ns.AddNamespace('t','http://schemas.microsoft.com/windows/2004/02/mit/task');
 function Text($node,$xpath){{$value=$node.SelectSingleNode($xpath,$ns);if($null -eq $value){{return ''}};return $value.InnerText}}
 $a=@($doc.SelectNodes('/t:Task/t:Actions/*',$ns));$g=@($doc.SelectNodes('/t:Task/t:Triggers/*',$ns));$p=@($doc.SelectNodes('/t:Task/t:Principals/*',$ns));
 [Console]::Out.WriteLine('rsrs-control-stage:task-shape');
 if($p.Count -ne 1 -or $a.Count -ne 1 -or $g.Count -ne 1 -or $a[0].LocalName -ne 'Exec' -or $g[0].LocalName -ne 'LogonTrigger'){{throw 'Unknown task principal/action/trigger count; refusing lifecycle action'}};
 [Console]::Out.WriteLine('rsrs-control-stage:principal-sid');if((Sid (Text $p[0] 't:UserId')) -cne {owner}){{throw 'Unknown task principal; refusing lifecycle action'}};
 [Console]::Out.WriteLine('rsrs-control-stage:principal-logon');if((Text $p[0] 't:LogonType') -cne 'InteractiveToken'){{throw 'Unknown task logon type; refusing lifecycle action'}};
 [Console]::Out.WriteLine('rsrs-control-stage:principal-runlevel');if((Text $p[0] 't:RunLevel') -eq '' -and $t.Definition.Principal.RunLevel -eq 0){{[Console]::Out.WriteLine('rsrs-control-stage:principal-runlevel-omitted-limited')}};
 RequireLimited (Text $p[0] 't:RunLevel') $t.Definition.Principal.RunLevel;
 [Console]::Out.WriteLine('rsrs-control-stage:trigger-sid');if((Sid (Text $g[0] 't:UserId')) -cne {owner}){{throw 'Unknown task trigger identity; refusing lifecycle action'}};
 $arguments=Text $a[0] 't:Arguments';
 [Console]::Out.WriteLine('rsrs-control-stage:action-executable');if(-not [StringComparer]::OrdinalIgnoreCase.Equals([IO.Path]::GetFullPath((Text $a[0] 't:Command')),[IO.Path]::GetFullPath({exe}))){{throw 'Unknown task executable; refusing lifecycle action'}};
 [Console]::Out.WriteLine('rsrs-control-stage:action-arguments');if($arguments -cne {args}){{throw 'Unknown task arguments; refusing lifecycle action'}};
 [Console]::Out.WriteLine('rsrs-control-stage:action-directory');if(-not [StringComparer]::OrdinalIgnoreCase.Equals([IO.Path]::GetFullPath((Text $a[0] 't:WorkingDirectory')),[IO.Path]::GetFullPath({root}))){{throw 'Unknown task directory; refusing lifecycle action'}};
 [Console]::Out.WriteLine('rsrs-control-stage:task-marker');
 if((Text $doc '/t:Task/t:RegistrationInfo/t:Description') -cne {description}){{throw 'Unknown task marker; refusing lifecycle action'}};
}}
[Console]::Out.WriteLine('rsrs-control-stage:task-validated');

"#,
        description = ps_literal(&format!("Respire current-user runtime {}", config.id))?,
        runlevel_guard = task_runlevel_guard()
    ))
}
