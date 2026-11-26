//! Reserve one restart using the exact existing private serving grant.
use anyhow::{ensure, Context, Result};
use respire_app::lock::LibraryLock;
use respire_protocol::HostControlLaunchGrant as Grant;
use rustix::{
    fs::OFlags,
    process::{getppid, getuid},
};
use std::{
    fs,
    io::{ErrorKind, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};
use uuid::Uuid;

fn directory(root: &Path) -> PathBuf {
    root.join("runtime/autostart/launch")
}
const NAME: &str = "backup-restart";

fn read(path: &Path) -> Result<Option<Grant>> {
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits() as i32)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let parent = path.parent().context("grant directory")?;
    ensure!(
        parent.canonicalize()? == parent,
        "noncanonical backup grant directory"
    );
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && meta.uid() == getuid().as_raw() && meta.mode() & 0o077 == 0 && meta.len() <= 65536,
        "invalid private backup restart grant"
    );
    let mut bytes = Vec::new();
    file.take(65537).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 65536, "oversized backup restart grant");
    Ok(Some(serde_json::from_slice(&bytes)?))
}

fn birth(pid: u32) -> Result<Option<String>> {
    ensure!(pid > 0, "invalid backup parent");
    let text = match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut fields = text
        .rsplit_once(')')
        .context("invalid process stat")?
        .1
        .split_whitespace();
    if matches!(fields.next(), Some("Z" | "X")) {
        return Ok(None);
    }
    Ok(Some(
        fields
            .nth(18)
            .context("missing process birth")?
            .parse::<u64>()?
            .to_string(),
    ))
}

fn identical(a: &Grant, b: &Grant) -> Result<bool> {
    Ok(serde_json::to_vec(a)? == serde_json::to_vec(b)?)
}

/// Publication and recovery serialize with installations and the selected library.
/// Call before yielding library ownership to the replacement.
pub fn reserve(
    root: &Path,
    grant: &Grant,
    previous: Option<&Grant>,
    takeover: &LibraryLock,
    installation: &LibraryLock,
    library: &LibraryLock,
) -> Result<()> {
    ensure!(
        root == grant.installation_root
            && takeover.protects(&root.join("runtime/takeover"))?
            && installation.protects(&root.join("runtime"))?
            && library.protects(&grant.profile)?
            && grant.supervisor_pid == std::process::id()
            && birth(grant.supervisor_pid)?.as_ref() == Some(&grant.supervisor_birth),
        "backup restart owner mismatch"
    );
    let nonce = Uuid::parse_str(&grant.nonce)?;
    ensure!(
        nonce.get_version_num() == 4 && nonce.to_string() == grant.nonce && grant.instance_id == grant.nonce,
        "invalid backup replacement nonce"
    );
    let dir = directory(root);
    for ancestor in dir.ancestors().take_while(|p| p.starts_with(root)) {
        let metadata = fs::symlink_metadata(ancestor)?;
        ensure!(
            metadata.is_dir() && metadata.uid() == getuid().as_raw() && metadata.mode() & 0o022 == 0,
            "backup launch directory is not owner-controlled"
        );
    }
    ensure!(
        !dir.join(format!("launch-{nonce}.claimed")).try_exists()?,
        "replacement grant previously consumed"
    );
    let source = dir.join(format!("launch-{nonce}.json"));
    let issued = read(&source)?.context("missing private launch grant")?;
    ensure!(identical(&issued, grant)?, "different launch grant");
    if let Some(previous) = previous {
        let current = read(&dir.join(NAME))?.context("missing prior reservation")?;
        ensure!(
            identical(previous, &current)?
                && previous.supervisor_pid == grant.supervisor_pid
                && previous.supervisor_birth == grant.supervisor_birth
                && previous.profile == grant.profile
                && previous.installation_root == root
                && previous.nonce != grant.nonce
                && previous.image_sha256 == grant.image_sha256
                && previous.account_id == grant.account_id
                && previous.port == grant.port,
            "backup reservation rotation differs"
        );
        ensure!(
            !dir.join(format!("launch-{}.json", previous.nonce)).try_exists()?
                && dir.join(format!("launch-{}.claimed", previous.nonce)).is_file(),
            "previous launch was not consumed"
        );
    }
    // Freeze the checked grant so later writes to the launch file cannot
    // change which child the reservation permits.
    let mut stage = tempfile::NamedTempFile::new_in(&dir)?;
    stage.write_all(&serde_json::to_vec(&issued)?)?;
    stage.as_file().sync_all()?;
    if previous.is_some() {
        respire_spawn::durable::replace_file(stage.path(), &dir.join(NAME))?;
    } else {
        respire_spawn::durable::publish_new_file(stage.path(), &dir.join(NAME))?;
    }
    Ok(())
}

/// This only restricts launch. Normal launch authority and consumption still run.
pub fn check(root: &Path, candidate: Option<&Grant>) -> Result<()> {
    let Some(reserved) = read(&directory(root).join(NAME))? else {
        return Ok(());
    };
    let grant = candidate.context("backup restart reserved for an exact replacement")?;
    ensure!(
        identical(&reserved, grant)?
            && getppid().is_some_and(|p| p.as_raw_nonzero().get() as u32 == grant.supervisor_pid)
            && birth(grant.supervisor_pid)?.as_ref() == Some(&grant.supervisor_birth),
        "backup replacement or native parent differs"
    );
    Ok(())
}

/// Clear a dead parent's reservation only under installation and library ownership and the
/// snapshot journal permits normal access. Ambiguous installation stays fenced.
pub fn recover_stale(
    root: &Path,
    profile: &Path,
    takeover: &LibraryLock,
    installation: &LibraryLock,
    library: &LibraryLock,
) -> Result<()> {
    ensure!(
        takeover.protects(&root.join("runtime/takeover"))?
            && installation.protects(&root.join("runtime"))?
            && library.protects(profile)?,
        "missing recovery ownership"
    );
    let dir = directory(root);
    let path = dir.join(NAME);
    let grant = read(&path)?.context("missing backup reservation")?;
    ensure!(
        grant.installation_root == root
            && grant.profile == profile
            && birth(grant.supervisor_pid)?.as_ref() != Some(&grant.supervisor_birth),
        "backup parent is live or recovery scope differs"
    );
    respire_app::snapshot::ensure_no_incomplete_restore(profile)?;
    let nonce = Uuid::parse_str(&grant.nonce)?;
    ensure!(
        nonce.get_version_num() == 4 && nonce.to_string() == grant.nonce,
        "invalid backup nonce"
    );
    ensure!(
        !dir.join(format!("launch-{nonce}.claimed")).try_exists()?,
        "consumed restart requires reconciliation"
    );
    let unconsumed = dir.join(format!("launch-{nonce}.json"));
    if let Some(issued) = read(&unconsumed)? {
        ensure!(identical(&issued, &grant)?, "launch grant replaced");
        fs::remove_file(unconsumed)?;
        respire_spawn::durable::sync_directory(&dir)?;
    }
    fs::remove_file(path)?;
    respire_spawn::durable::sync_directory(&dir)?;
    Ok(())
}

/// Verify the exact reservation is still owned by its live original issuer.
pub fn require_held(root: &Path, grant: &Grant, takeover: &LibraryLock) -> Result<()> {
    ensure!(
        takeover.protects(&root.join("runtime/takeover"))?
            && grant.installation_root == root
            && grant.supervisor_pid == std::process::id()
            && birth(grant.supervisor_pid)?.as_ref() == Some(&grant.supervisor_birth),
        "missing backup parent ownership"
    );
    let dir = directory(root);
    let reserved = read(&dir.join(NAME))?.context("missing reservation")?;
    ensure!(identical(&reserved, grant)?, "backup reservation changed");
    Ok(())
}

/// Release after the original parent observes its owned replacement serving the exact grant.
pub fn release(
    root: &Path,
    grant: &Grant,
    child: &mut std::process::Child,
    takeover: &LibraryLock,
) -> Result<()> {
    use crate::net_rpc;
    require_held(root, grant, takeover)?;
    let dir = directory(root);
    ensure!(child.try_wait()?.is_none(), "replacement changed");
    ensure!(
        grant.port == net_rpc::rpc_port() && net_rpc::pid_listening_on(grant.port) == Some(child.id()),
        "wrong serving child"
    );
    let health = net_rpc::health()?;
    ensure!(
        health.pid == child.id()
            && health.instance_id == grant.instance_id
            && health.installation_id == grant.installation_id
            && health.account_id == grant.account_id
            && health.image_sha256 == grant.image_sha256
            && Path::new(&health.data_dir) == grant.profile
            && health.generation == crate::rpc_worker::generation_on_wire(grant.sync_generation)
            && health.store_ready
            && net_rpc::instance_is_running(&health)?
            && child.try_wait()?.is_none(),
        "replacement has not reopened"
    );
    fs::remove_file(dir.join(NAME))?;
    respire_spawn::durable::sync_directory(&dir)?;
    Ok(())
}

/// Initial grant consumption can precede a long lock wait. Check the launch
/// window once more immediately before binding, never on normal serving traffic.
pub(crate) fn before_bind(root: &Path, instance: &str) -> Result<()> {
    let Some(grant) = read(&directory(root).join(NAME))? else {
        return Ok(());
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis();
    ensure!(
        instance == grant.instance_id
            && (u128::from(grant.issued_at_ms)..=u128::from(grant.expires_at_ms)).contains(&now),
        "reserved launch expired or changed while waiting for ownership"
    );
    Ok(())
}

static SUPERVISED: std::sync::OnceLock<()> = std::sync::OnceLock::new();

pub fn owned_by_supervisor(root: &Path, grant: &Grant) -> Result<bool> {
    if read(&directory(root).join(NAME))?.is_none() {
        return Ok(false);
    }
    check(root, Some(grant))?;
    let parent = fs::metadata(format!("/proc/{}/exe", grant.supervisor_pid))?;
    let this = fs::metadata("/proc/self/exe")?;
    if (parent.dev(), parent.ino()) != (this.dev(), this.ino()) {
        return Ok(false);
    }
    // The parent must run the dedicated supervisor mode of this installed image.
    let mut argv = Vec::new();
    fs::File::open(format!("/proc/{}/cmdline", grant.supervisor_pid))?
        .take(8193)
        .read_to_end(&mut argv)?;
    ensure!(argv.len() <= 8192, "oversized backup supervisor arguments");
    let mut args = argv
        .strip_suffix(&[0])
        .context("invalid supervisor arguments")?
        .split(|v| *v == 0);
    Ok(args.next().is_some()
        && args.next() == Some(b"--backup-supervisor".as_slice())
        && args.next().is_none())
}

pub fn enable_private_backup_control(grant: &Grant) -> Result<()> {
    ensure!(
        owned_by_supervisor(&grant.installation_root, grant)?,
        "no reserved backup supervisor"
    );
    SUPERVISED
        .set(())
        .map_err(|_| anyhow::anyhow!("backup control already initialized"))
}

pub(crate) fn private_backup_control() -> bool {
    SUPERVISED.get().is_some()
}
