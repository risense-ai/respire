fn new_registration() -> Result<Registration> {
    let root = respire_client_host::paths::main_data_dir();
    let environment_root = if root.is_absolute() {
        root.clone()
    } else {
        std::env::current_dir()?.join(&root)
    };
    fs::create_dir_all(&root)?;
    let root = root.canonicalize()?;
    let owner = manager::owner()?;
    let executable = crate::mcp::stable_bin_path();
    let home = dirs::home_dir()
        .context("current-user home unavailable")?
        .canonicalize()?;
    let mut environment = BTreeMap::new();
    for &key in ENVIRONMENT {
        if let Ok(value) = respire::env::var(key) {
            super::plain_value(&value)?;
            environment.insert(key.into(), value);
        }
    }
    environment.insert(
        "RSRS_DATA_DIR".into(),
        environment_root
            .to_str()
            .context("non-UTF8 installation")?
            .into(),
    );
    environment.insert(
        "RSRS_BIN_DIR".into(),
        executable
            .parent()
            .context("native executable parent missing")?
            .to_str()
            .context("non-UTF8 native directory")?
            .into(),
    );
    environment.insert(
        "RSRS_RPC_PORT".into(),
        crate::net_rpc::rpc_port().to_string(),
    );
    Ok(Registration {
        schema: 1,
        platform: Platform::current()?,
        id: identity(&root, &owner),
        installation: root,
        executable,
        user_home: home,
        owner,
        enabled: false,
        manually_stopped: false,
        maintenance: false,
        environment,
        artifact_hashes: BTreeMap::new(),
        digest: None,
    })
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Attempts {
    schema: u32,
    id: String,
    starts: Vec<u64>,
}

fn attempts_path(root: &Path) -> PathBuf {
    control_path(root).with_file_name("launch-attempts.json")
}
fn read_attempts(config: &Registration) -> Result<(Attempts, Option<String>)> {
    let path = attempts_path(&config.installation);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((
                Attempts {
                    schema: 1,
                    id: config.id.clone(),
                    starts: Vec::new(),
                },
                None,
            ))
        }
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= 4096,
        "invalid runtime launch budget"
    );
    let bytes = fs::read(path)?;
    let value: Attempts = serde_json::from_slice(&bytes)?;
    anyhow::ensure!(
        value.schema == 1 && value.id == config.id && value.starts.len() <= 3,
        "runtime launch budget belongs to an unknown registration"
    );
    Ok((value, Some(hash(&bytes))))
}
fn record_attempt(config: &Registration, now: u64) -> Result<bool> {
    let (mut attempts, digest) = read_attempts(config)?;
    attempts
        .starts
        .retain(|started| now.saturating_sub(*started) < 120);
    if attempts.starts.len() >= 3 {
        return Ok(false);
    }
    attempts.starts.push(now);
    private_write(
        &attempts_path(&config.installation),
        &serde_json::to_vec(&attempts)?,
        digest.as_deref(),
    )?;
    Ok(true)
}
pub(super) fn reset_attempts(config: &Registration) -> Result<()> {
    let (mut attempts, digest) = read_attempts(config)?;
    attempts.starts.clear();
    private_write(
        &attempts_path(&config.installation),
        &serde_json::to_vec(&attempts)?,
        digest.as_deref(),
    )?;
    manager::reset_backoff(config)
}
fn now_seconds() -> Result<u64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs())
}
/// Best-effort fixed stages help diagnose a stalled explicit restart without
/// exporting arguments, account data or raw OS errors. Never weaken a fence if
/// its diagnostic file cannot be updated.
pub(crate) fn restart_stage(name: &str) {
    if let Ok(Some(config)) = read_registration(&respire_client_host::paths::main_data_dir()) {
        let _ = event(&config, name);
    }
}

/// Supervisor events reuse the fixed lifecycle event names so the diagnostic
/// file stays bounded and never captures free-form text.
pub(super) fn record_supervise_event(config: &Registration, name: &str) {
    if let Err(error) = event(config, name) {
        eprintln!("supervisor event could not be recorded: {error}");
    }
}

fn event(config: &Registration, name: &str) -> Result<()> {
    // Fixed event names only. Never serialize command arguments, errors,
    // environment values, account identifiers or recovery material.
    anyhow::ensure!(
        [
            "starting",
            "paused",
            "crash_limit",
            "exit_ok",
            "exit_error",
            "restart_probe",
            "restart_managed",
            "restart_identity",
            "restart_stop",
            "restart_start",
            "restart_ready"
        ]
        .contains(&name),
        "unknown runtime event"
    );
    let path = control_path(&config.installation).with_file_name("events.jsonl");
    let previous = match fs::symlink_metadata(&path) {
        Ok(meta) => {
            anyhow::ensure!(
                meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= 65536,
                "invalid runtime event file"
            );
            Some(fs::read(&path)?)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let mut bytes = previous.clone().unwrap_or_default();
    let mut line = serde_json::to_vec(
        &json!({"schema":1,"id":config.id,"pid":std::process::id(),"version":env!("CARGO_PKG_VERSION"),"time":now_seconds()?,"event":name}),
    )?;
    line.push(b'\n');
    if bytes.len() + line.len() > 65536 {
        let backup = path.with_file_name("events.previous.jsonl");
        let prior_backup = match fs::symlink_metadata(&backup) {
            Ok(meta) => {
                anyhow::ensure!(
                    meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= 65536,
                    "invalid runtime event backup"
                );
                Some(fs::read(&backup)?)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        private_write(
            &backup,
            &bytes,
            prior_backup.as_ref().map(|v| hash(v)).as_deref(),
        )?;
        bytes.clear();
    }
    bytes.extend(line);
    private_write(&path, &bytes, previous.as_ref().map(|v| hash(v)).as_deref())
}
