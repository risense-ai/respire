//! Hold only the historical installation startup exclusion for a local host.
//! The same OS user already has access to this installation lock.
//! This grants no maintenance, launch, credential, or business authority.
mod hold;
#[cfg(test)]
mod tests;
mod wire;

use anyhow::{Context, Result};
use respire_client_host::{config::normalize_control_path, lock::LibraryLock};
use respire_commands::{net_rpc, rpc_worker};
use serde::{de::DeserializeOwned, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use wire::{Acquire, Endpoint, Finish, Held, Released, MAX_FRAME, SCHEMA};

const STARTUP_HOLD_TIMEOUT: Duration = Duration::from_secs(90);

pub(crate) fn run() -> Result<i32> {
    let request: Acquire = read_frame(&mut std::io::stdin().lock())?;
    validate_controller(&request)?;
    // Same-user coordination only. The supplied PID and birth are a liveness
    // check, not authenticated parent identity or a grant to touch the serving store.
    let pid = request.controller_pid;
    let birth = request.controller_birth.clone();
    std::thread::Builder::new()
        .name("startup-controller".into())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_millis(300));
            match super::startup_identity::process_birth(pid) {
                Ok(Some(actual)) if actual == birth => {}
                Ok(_) => {
                    eprintln!("startup controller exited or its PID was reused");
                    std::process::exit(2);
                }
                Err(error) => {
                    eprintln!("startup controller verification failed: {error:#}");
                    std::process::exit(2);
                }
            }
        })?;
    let (finish_send, finish_receive) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("startup-input".into())
        .spawn(
            move || match read_frame::<Finish>(&mut std::io::stdin().lock()) {
                Ok(finish) => {
                    let _ = finish_send.send(finish);
                }
                Err(error) => {
                    eprintln!("startup input ended or failed: {error:#}");
                    std::process::exit(2);
                }
            },
        )?;
    let (root, profile) = configured_scope()?;
    let (guard, root, profile) =
        begin_acquisition(&request, &root, &profile, Duration::from_secs(60))?;
    let exclusion = hold::BoundExclusion::start(guard, STARTUP_HOLD_TIMEOUT)?;
    let held = describe_acquisition(&root, &profile)?;
    exclusion.verify_directory(&takeover_directory(&root)?)?;
    validate_controller(&request)?;
    anyhow::ensure!(
        configured_scope()? == (root.clone(), profile.clone()),
        "startup configuration changed while waiting for exclusion"
    );
    anyhow::ensure!(
        !exclusion.remaining().is_zero(),
        "startup exclusion timed out"
    );
    write_frame(&mut std::io::stdout().lock(), &held)?;
    let finish = wait_for_finish(&finish_receive, exclusion.remaining())?;
    anyhow::ensure!(
        finish.guard_id == held.guard_id,
        "startup guard ID mismatch"
    );
    if let Some(expected) = finish.endpoint.as_ref() {
        verify_endpoint(expected, &held)?;
    }
    validate_controller(&request)?;
    anyhow::ensure!(
        configured_scope()? == (root.clone(), profile.clone()),
        "startup configuration changed while exclusion was held"
    );
    verify_scope(&root, &profile, &held)?;
    exclusion.verify_directory(&root.join("runtime/takeover"))?;
    exclusion.release()?;
    write_frame(
        &mut std::io::stdout().lock(),
        &Released {
            schema_version: SCHEMA,
            guard_id: held.guard_id,
            endpoint: finish.endpoint,
        },
    )?;
    Ok(0)
}

fn wait_for_finish(input: &std::sync::mpsc::Receiver<Finish>, timeout: Duration) -> Result<Finish> {
    match input.recv_timeout(timeout) {
        Ok(finish) => Ok(finish),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            anyhow::bail!("startup exclusion timed out")
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            anyhow::bail!("startup input disconnected")
        }
    }
}

fn validate_controller(request: &Acquire) -> Result<()> {
    anyhow::ensure!(
        request.schema_version == SCHEMA,
        "unsupported startup schema"
    );
    anyhow::ensure!(
        request.controller_pid > 0
            && request.controller_pid != std::process::id()
            && !request.controller_birth.is_empty()
            && super::startup_identity::process_birth(request.controller_pid)?.as_deref()
                == Some(request.controller_birth.as_str()),
        "startup controller identity mismatch"
    );
    Ok(())
}

fn configured_scope() -> Result<(PathBuf, PathBuf)> {
    respire_app::service::validate_client_config()?;
    Ok((
        checked_path(&respire_app::service::main_data_dir())?,
        checked_path(&respire_app::service::data_dir())?,
    ))
}

fn checked_path(path: &Path) -> Result<PathBuf> {
    anyhow::ensure!(path.is_absolute(), "startup scope must be absolute");
    // Verbatim Windows paths otherwise keep forward slashes inside a single component.
    #[cfg(windows)]
    let path = {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let units: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .map(|unit| {
                if unit == u16::from(b'/') {
                    u16::from(b'\\')
                } else {
                    unit
                }
            })
            .collect();
        PathBuf::from(std::ffi::OsString::from_wide(&units))
    };
    let normalized = normalize_control_path(path.as_ref())?;
    for ancestor in normalized.ancestors() {
        match ancestor.canonicalize() {
            Ok(resolved) => {
                let suffix = normalized.strip_prefix(ancestor)?;
                // An empty join adds a separator on Windows and changes Runtime identity hashes.
                return Ok(if suffix.as_os_str().is_empty() {
                    resolved
                } else {
                    resolved.join(suffix)
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        }
    }
    anyhow::bail!("startup scope has no existing ancestor")
}

fn takeover_directory(root: &Path) -> Result<PathBuf> {
    let root = checked_path(root)?;
    let directory = root.join("runtime/takeover");
    for name in ["lock.db", "lock.db-journal", "lock.db-wal", "lock.db-shm"] {
        let file = directory.join(name);
        anyhow::ensure!(checked_path(&file)? == file, "startup lock path changed");
        match fs::symlink_metadata(&file) {
            Ok(metadata) => anyhow::ensure!(metadata.is_file(), "startup lock is not a file"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(directory)
}

#[cfg(test)]
fn acquire(
    request: &Acquire,
    root: &Path,
    profile: &Path,
    wait: Duration,
) -> Result<(LibraryLock, Held)> {
    let (guard, root, profile) = begin_acquisition(request, root, profile, wait)?;
    let held = describe_acquisition(&root, &profile)?;
    verify_held(&guard, &root, &profile, &held)?;
    Ok((guard, held))
}

fn begin_acquisition(
    request: &Acquire,
    root: &Path,
    profile: &Path,
    wait: Duration,
) -> Result<(LibraryLock, PathBuf, PathBuf)> {
    anyhow::ensure!(
        request.schema_version == SCHEMA,
        "unsupported startup schema"
    );
    let root = checked_path(root)?;
    let profile = checked_path(profile)?;
    anyhow::ensure!(
        checked_path(&request.installation_root)? == root
            && checked_path(&request.profile)? == profile,
        "startup scope does not match configuration"
    );
    let directory = takeover_directory(&root)?;
    let guard = LibraryLock::acquire(&directory, wait).context("startup exclusion unavailable")?;
    Ok((guard, root, profile))
}

fn describe_acquisition(root: &Path, profile: &Path) -> Result<Held> {
    // A cold profile needs only its directory here; the serving child locks it independently.
    fs::create_dir_all(profile)?;
    anyhow::ensure!(
        root.canonicalize()? == root && profile.canonicalize()? == profile,
        "startup scope changed during acquisition"
    );
    let held = Held {
        schema_version: SCHEMA,
        guard_id: uuid::Uuid::new_v4().to_string(),
        account_id: account_id(profile)?,
        installation_root: root.to_owned(),
        profile: profile.to_owned(),
        helper_pid: std::process::id(),
        helper_birth: super::startup_identity::process_birth(std::process::id())?
            .context("startup helper birth unavailable")?,
    };
    verify_scope(root, profile, &held)?;
    Ok(held)
}

fn account_id(profile: &Path) -> Result<String> {
    match respire_client_host::identity::host_session_user_of_dir(profile, digest)? {
        Some(user) => Ok(digest(user.as_bytes())),
        None => Ok(digest(profile.display().to_string().as_bytes())),
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
fn verify_held(guard: &LibraryLock, root: &Path, profile: &Path, held: &Held) -> Result<()> {
    verify_scope(root, profile, held)?;
    anyhow::ensure!(
        guard.protects(&root.join("runtime/takeover"))?,
        "startup lock ownership changed"
    );
    Ok(())
}

fn verify_scope(root: &Path, profile: &Path, held: &Held) -> Result<()> {
    anyhow::ensure!(
        takeover_directory(root)? == root.join("runtime/takeover")
            && checked_path(profile)? == held.profile
            && root.canonicalize()? == held.installation_root
            && account_id(profile)? == held.account_id,
        "startup scope or lock ownership changed"
    );
    Ok(())
}

fn verify_endpoint(expected: &Endpoint, held: &Held) -> Result<()> {
    // Neither a caller-provided URL nor a caller-provided filesystem asset is opened.
    anyhow::ensure!(
        expected.url == net_rpc::rpc_base_url(),
        "startup endpoint URL mismatch"
    );
    let before = super::startup_identity::read_private_file(&rpc_worker::endpoint_path())?
        .context("startup endpoint publication missing")?;
    let published: serde_json::Value = serde_json::from_slice(&before)?;
    let health = net_rpc::health()?;
    let observed = Endpoint {
        pid: health.pid,
        process_start_id: health.process_start_id.clone(),
        image_path: PathBuf::from(&health.image_path),
        image_sha256: health.image_sha256.clone(),
        instance_id: health.instance_id.clone(),
        account_id: health.account_id.clone(),
        generation: health.generation,
        url: health.url.clone(),
    };
    anyhow::ensure!(
        net_rpc::pid_listening_on(net_rpc::rpc_port()) == Some(health.pid),
        "startup listener process mismatch or unavailable"
    );
    let process = net_rpc::runtime_process_identity(health.pid)?;
    anyhow::ensure!(
        Path::new(&health.exe).canonicalize()? == Path::new(&process.executable).canonicalize()?,
        "startup Runtime executable mismatch"
    );
    verify_observation(
        expected,
        &observed,
        held,
        &published,
        &health.installation_root,
        &health.data_dir,
        &health.installation_id,
        health.v,
        &process.executable,
        &process.started_at,
    )?;
    anyhow::ensure!(
        super::startup_identity::hash_file(Path::new(&process.executable))?
            == observed.image_sha256,
        "startup Runtime image digest mismatch"
    );
    anyhow::ensure!(
        super::startup_identity::read_private_file(&rpc_worker::endpoint_path())?.as_deref()
            == Some(before.as_slice()),
        "startup endpoint publication changed during verification"
    );
    anyhow::ensure!(
        net_rpc::runtime_process_identity(health.pid)? == process,
        "startup Runtime process changed during verification"
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn verify_observation(
    expected: &Endpoint,
    observed: &Endpoint,
    held: &Held,
    published: &serde_json::Value,
    root: &str,
    profile: &str,
    installation_id: &str,
    protocol: u32,
    executable: &str,
    birth: &str,
) -> Result<()> {
    anyhow::ensure!(
        expected == observed
            && observed.pid > 0
            && observed.generation > 0
            && observed.account_id == held.account_id
            && uuid::Uuid::parse_str(&observed.instance_id)
                .is_ok_and(|id| id.get_version_num() == 4 && id.to_string() == observed.instance_id)
            && observed.image_sha256.len() == 64
            && observed
                .image_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && protocol == rpc_worker::PROTOCOL_V
            && Path::new(root).canonicalize()? == held.installation_root
            && Path::new(profile).canonicalize()? == held.profile
            && installation_id == digest(held.installation_root.display().to_string().as_bytes())
            && observed.image_path.canonicalize()? == Path::new(executable).canonicalize()?
            && !birth.is_empty() && observed.process_start_id == wire_birth(birth),
        "startup Runtime receipt mismatch"
    );
    let fields = serde_json::to_value(observed)?;
    for (key, value) in fields.as_object().context("invalid startup receipt")? {
        anyhow::ensure!(
            published.get(key) == Some(value),
            "startup endpoint field mismatch: {key}"
        );
    }
    anyhow::ensure!(
        published["installation_root"] == root
            && published["data_dir"] == profile
            && published["installation_id"] == installation_id
            && published["v"] == protocol,
        "startup endpoint scope mismatch"
    );
    Ok(())
}

fn wire_birth(birth: &str) -> String {
    let prefix = if cfg!(windows) {
        "windows-filetime"
    } else if cfg!(target_os = "linux") {
        "linux-starttime"
    } else {
        "darwin-lstart"
    };
    format!("{prefix}-{birth}")
}

fn read_frame<T: DeserializeOwned>(input: &mut impl Read) -> Result<T> {
    let mut length = [0; 4];
    input
        .read_exact(&mut length)
        .context("missing startup control frame")?;
    let length = u32::from_be_bytes(length) as usize;
    anyhow::ensure!(
        length > 0 && length <= MAX_FRAME,
        "invalid startup control frame size"
    );
    let mut bytes = vec![0; length];
    input
        .read_exact(&mut bytes)
        .context("incomplete startup control frame")?;
    serde_json::from_slice(&bytes).context("invalid startup control JSON")
}

fn write_frame(output: &mut impl Write, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    anyhow::ensure!(
        bytes.len() <= MAX_FRAME,
        "startup control response too large"
    );
    output.write_all(&(bytes.len() as u32).to_be_bytes())?;
    output.write_all(&bytes)?;
    output.flush()?;
    Ok(())
}
