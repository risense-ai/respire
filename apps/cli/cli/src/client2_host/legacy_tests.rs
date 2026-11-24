//! Regression fixtures for the retired Client2 compatibility transport.
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

const READY_LIMIT: u64 = 64 * 1024;

fn read_ready(reader: impl Read) -> Result<Value> {
    let mut bytes = Vec::new();
    BufReader::new(reader.take(READY_LIMIT + 1)).read_until(b'\n', &mut bytes)?;
    anyhow::ensure!(bytes.len() <= READY_LIMIT as usize && bytes.last() == Some(&b'\n'),
        "incomplete Runtime readiness frame");
    let ready: Value = serde_json::from_slice(&bytes)?;
    anyhow::ensure!(ready["startup_complete"] == true
        || (ready["startup_complete"] == false && ready["error"].is_string()),
        "Runtime startup is incomplete");
    Ok(ready)
}

#[cfg(any(test, target_os = "macos"))]
fn guarded_host_pid(ready: &Value, guardian: u32, executable: &Path, root: &Path, port: u16) -> Result<u32> {
    let pid = ready["pid"].as_u64().and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0 && *pid != guardian).context("invalid guarded host PID")?;
    anyhow::ensure!(ready["guardian_pid"] == guardian && ready["startup_complete"] == true
        && ready["bin"] == crate::VERSION && ready["port"] == port
        && Path::new(ready["exe"].as_str().context("guarded host image missing")?).canonicalize()? == executable.canonicalize()?
        && Path::new(ready["installation_root"].as_str().context("guarded host root missing")?).canonicalize()? == root.canonicalize()?,
        "guarded host readiness does not match this launch");
    Ok(pid)
}

fn prepare_legacy_rpc(body: &[u8], health: &crate::net_rpc::Health)
    -> Result<(Vec<u8>, Option<respire_protocol::RpcRequest>)>
{
    let original: respire_protocol::RpcRequest = serde_json::from_slice(body)?;
    if original.scope.is_some() { return Ok((body.to_vec(), None)); }
    anyhow::ensure!(original.operation_id.is_none() && original.idempotency_key.is_none()
        && original.parent_operation_id.is_none(), "scope_required");
    let mut request = original.clone();
    request.scope = Some(crate::net_rpc::request_scope(health)?);
    if request.method != "runtime.result" {
        request.operation_id = Some(request.id.clone());
        request.idempotency_key = Some(request.id.clone());
    }
    Ok((serde_json::to_vec(&request)?, Some(original)))
}

fn project_legacy_rpc(body: &[u8], original: &respire_protocol::RpcRequest,
    health: &crate::net_rpc::Health) -> Result<Vec<u8>>
{
    type Reply = respire_protocol::RpcResponse<Value>;
    let same_actor = |reply: &Reply| reply.v == 1 && reply.scope.as_ref().is_some_and(|scope|
        scope.account_id == health.account_id && scope.instance_id == health.instance_id);
    let strip = |reply: &mut Reply| {
        reply.scope = None; reply.operation_id = None; reply.idempotency_key = None;
    };
    let mut reply: Reply = serde_json::from_slice(body)?;
    anyhow::ensure!(reply.id == original.id && same_actor(&reply),
        "legacy result identity differs from the owned Runtime");
    if original.method == "runtime.result" {
        if let Some(summary) = reply.envelope.as_mut().and_then(|envelope| envelope.get_mut("summary"))
            .and_then(Value::as_object_mut) {
            if summary.get("state").and_then(Value::as_str) == Some("failed") {
                summary.insert("state".into(), json!("completed"));
            }
            if let Some(value) = summary.get_mut("response").filter(|value| !value.is_null()) {
                let mut child: Reply = serde_json::from_value(value.clone())?;
                anyhow::ensure!(original.args.first() == Some(&child.id) && same_actor(&child),
                    "legacy cached result identity differs from the original request");
                strip(&mut child);
                *value = serde_json::to_value(child)?;
            }
            for field in ["scope", "operation_id", "idempotency_key"] { summary.remove(field); }
            summary.insert("request_id".into(), json!(original.args.first()));
            summary.insert("retention".into(), json!("last 64 completed requests in this runtime; unknown is not proof of non-commit; never replay a write automatically"));
        }
    }
    strip(&mut reply);
    Ok(serde_json::to_vec(&reply)?)
}

fn validate_request(method: &str, path: &str, loopback: bool,
    headers: &[(String, String)], port: u16) -> Result<()> {
    anyhow::ensure!(loopback, "non-loopback peer rejected");
    anyhow::ensure!(matches!((method, path), ("GET", "/api/health")
        | ("POST", "/api/rpc" | "/api/runtime/stop")
        | ("GET" | "POST", "/mcp" | "/sse")), "unsupported compatibility route");
    for name in ["Host", "Origin", "Accept", "X-Respire-Health", "X-Respire-Lifecycle"] {
        let values: Vec<_> = headers.iter().filter(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str()).collect();
        anyhow::ensure!(values.len() <= 1, "duplicate compatibility header");
        if name == "Host" {
            anyhow::ensure!(values.len() == 1 && (values[0] == format!("127.0.0.1:{port}")
                || values[0] == format!("localhost:{port}")), "foreign Host rejected");
        } else if name == "Origin" {
            anyhow::ensure!(values.is_empty() || (!values[0].is_empty()
                && crate::net_rpc::origin_ok(values[0], &format!("http://127.0.0.1:{port}"))),
                "foreign Origin rejected");
        }
    }
    Ok(())
}

fn host_process(pid: u32) -> Result<(PathBuf, String)> {
    let before = crate::autostart::process_birth(pid)?
        .context("compatibility host process is not running")?;
    #[cfg(windows)]
    let executable = respire_spawn::process_identity(pid)?
        .context("compatibility host process disappeared")?.executable;
    #[cfg(target_os = "linux")]
    let executable = std::fs::read_link(format!("/proc/{pid}/exe"))?;
    #[cfg(all(unix, not(target_os = "linux")))]
    let executable = {
        let output = Command::new("/bin/ps").args(["-ww", "-p", &pid.to_string(), "-o", "comm="])
            .env("LC_ALL", "C").output()?;
        anyhow::ensure!(output.status.success(), "cannot inspect compatibility host image");
        PathBuf::from(String::from_utf8(output.stdout)?.trim())
    };
    anyhow::ensure!(crate::autostart::process_birth(pid)?.as_deref() == Some(before.as_str()),
        "compatibility host changed during OS identity inspection");
    Ok((executable.canonicalize()?, before))
}

// Bind the exact IPv4 loopback endpoint, not an unrelated interface with the
// same port. Missing OS tooling and ambiguous ownership both fail closed.
#[cfg(target_os = "linux")]
fn loopback_listener(port: u16, pid: u32) -> Result<u32> {
    let loopback = format!("0100007F:{port:04X}");
    let wildcard = format!("00000000:{port:04X}");
    let mut inodes = std::collections::BTreeSet::new();
    for line in std::fs::read_to_string("/proc/net/tcp")?.lines().skip(1) {
        let fields: Vec<_> = line.split_whitespace().collect();
        anyhow::ensure!(fields.len() >= 10, "invalid OS TCP listener record");
        if fields[3] == "0A" && (fields[1] == loopback || fields[1] == wildcard) {
            anyhow::ensure!(fields[9] != "0", "OS listener inode is missing");
            inodes.insert(fields[9].to_owned());
        }
    }
    anyhow::ensure!(inodes.len() == 1, "compatibility loopback listener is unavailable or ambiguous");
    let inode = inodes.into_iter().next().context("compatibility listener is missing")?;
    let socket = format!("socket:[{inode}]");
    // Inspect only the claimed process: the kernel socket inode, not the health
    // response, must prove its ownership. No scan of inaccessible foreign PIDs.
    for entry in std::fs::read_dir(format!("/proc/{pid}/fd"))? {
        let entry = entry?;
        match std::fs::read_link(entry.path()) {
            Ok(target) if target.as_os_str() == std::ffi::OsStr::new(&socket) => return Ok(pid),
            Ok(_) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(error.into()),
        }
    }
    anyhow::bail!("claimed compatibility host does not own the OS listener")
}

#[cfg(windows)]
fn windows_listener_owner(output: &[u8], port: u16) -> Result<u32> {
    let local = format!("127.0.0.1:{port}");
    let wildcard = format!("0.0.0.0:{port}");
    let mut owners = std::collections::BTreeSet::new();
    for line in output.split(|byte| *byte == b'\n') {
        let fields: Vec<_> = line.split(u8::is_ascii_whitespace)
            .filter(|field| !field.is_empty()).collect();
        // netstat headers use the system code page; TCP records must be ASCII.
        if fields.first().copied() != Some(b"TCP".as_slice()) { continue; }
        anyhow::ensure!(line.is_ascii() && fields.len() == 5, "invalid OS TCP listener record");
        if fields[3] == b"LISTENING" && (fields[1] == local.as_bytes() || fields[1] == wildcard.as_bytes()) {
            anyhow::ensure!(fields[4].iter().all(u8::is_ascii_digit), "invalid OS TCP listener PID");
            owners.insert(std::str::from_utf8(fields[4])?.parse::<u32>()?);
        }
    }
    anyhow::ensure!(owners.len() == 1 && !owners.contains(&0),
        "compatibility loopback listener ownership is unavailable or ambiguous");
    owners.into_iter().next().context("compatibility listener is missing")
}

#[cfg(not(target_os = "linux"))]
fn loopback_listener(port: u16, _pid: u32) -> Result<u32> {
    #[cfg(unix)]
    let mut owners = std::collections::BTreeSet::new();
    #[cfg(windows)]
    {
        let output = Command::new("netstat").args(["-ano", "-p", "tcp"]).output()?;
        anyhow::ensure!(output.status.success(), "cannot inspect compatibility listener");
        return windows_listener_owner(&output.stdout, port);
    }
    #[cfg(unix)]
    {
        let output = Command::new("/usr/sbin/lsof")
            .args(["-nP", &format!("-i4TCP:{port}"), "-sTCP:LISTEN", "-Fpn"]).output()?;
        anyhow::ensure!(output.status.success(), "cannot inspect compatibility listener");
        let local = format!("127.0.0.1:{port}");
        let wildcard = format!("*:{port}");
        let mut pid = None;
        for line in String::from_utf8(output.stdout)?.lines() {
            if let Some(value) = line.strip_prefix('p') { pid = Some(value.parse::<u32>()?); }
            if let Some(value) = line.strip_prefix('n') {
                if value == local || value == wildcard {
                    owners.insert(pid.context("listener process identity is missing")?);
                }
            }
        }
    }
    #[cfg(unix)]
    {
        anyhow::ensure!(owners.len() == 1 && !owners.contains(&0),
            "compatibility loopback listener ownership is unavailable or ambiguous");
        owners.into_iter().next().context("compatibility listener is missing")
    }
}

fn listener_matches(public: &Value, port: u16, listener: Option<u32>) -> Result<u32> {
    let pid = public["pid"].as_u64().and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0).context("compatibility host PID is invalid")?;
    anyhow::ensure!(listener == Some(pid),
        "compatibility host is not the verified OS listener; inspect before retry");
    anyhow::ensure!(public["public_port"] == port, "compatibility host port changed");
    Ok(pid)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guardian_readiness_is_bound_to_the_owned_launch() -> Result<()> {
        let executable = std::env::current_exe()?.canonicalize()?;
        let root = std::env::temp_dir().canonicalize()?;
        let ready = json!({"startup_complete":true, "pid":123, "guardian_pid":456,
            "exe":executable, "installation_root":root, "bin":crate::VERSION, "port":15169});
        assert_eq!(guarded_host_pid(&ready, 456, &executable, &root, 15169)?, 123);
        let frame = format!("{ready}\n");
        assert_eq!(read_ready(frame.as_bytes())?, ready);
        assert!(read_ready(b"".as_slice()).is_err());
        let failed = json!({"startup_complete":false, "error":"public listener bind (io::AddrInUse)"});
        let frame = format!("{failed}\n");
        assert_eq!(read_ready(frame.as_bytes())?, failed);
        for (key, value) in [("pid", json!(0)), ("pid", json!(456)),
            ("pid", json!(u64::MAX)), ("guardian_pid", json!(789)),
            ("startup_complete", json!(false)), ("bin", json!("foreign")),
            ("port", json!(15170)), ("exe", json!(root)),
            ("installation_root", json!(executable))] {
            let mut wrong = ready.clone();
            wrong[key] = value;
            assert!(guarded_host_pid(&wrong, 456, &executable, &root, 15169).is_err(), "{key}");
        }
        Ok(())
    }

    #[test]
    fn legacy_rpc_identity_is_supplied_and_projected_by_the_host() -> Result<()> {
        let _guard = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let root = tempfile::tempdir()?;
        let previous = std::env::var_os("RSRS_DATA_DIR");
        std::env::set_var("RSRS_DATA_DIR", root.path());
        let result = (|| -> Result<()> {
        let health: crate::net_rpc::Health = serde_json::from_value(json!({
            "server":"respire","bin":"fixture","v":1,"pid":42,"account_id":"account",
            "instance_id":"instance","generation":7,"installation_root":root.path()
        }))?;
        let raw = serde_json::to_vec(&json!({"v":1,"id":"original","method":"cli.exec","args":["--json","status"]}))?;
        let (bound, original) = prepare_legacy_rpc(&raw, &health)?;
        let bound: respire_protocol::RpcRequest = serde_json::from_slice(&bound)?;
        assert_eq!(bound.scope, Some(crate::net_rpc::request_scope(&health)?));
        assert_eq!(bound.operation_id.as_deref(), Some("original"));
        let original = original.context("missing legacy projection")?;
        let response = json!({"v":1,"id":"original","ok":true,"exit":0,"bin":"fixture",
            "scope":bound.scope,"operation_id":"original","idempotency_key":"original"});
        let projected: Value = serde_json::from_slice(&project_legacy_rpc(&serde_json::to_vec(&response)?, &original, &health)?)?;
        for field in ["scope","operation_id","idempotency_key"] { assert!(projected.get(field).is_none()); }
        let mut changed = response; changed["scope"]["instance_id"] = json!("foreign");
        assert!(project_legacy_rpc(&serde_json::to_vec(&changed)?, &original, &health).is_err());
        let mut mixed = original; mixed.operation_id = Some("foreign".into());
        assert!(prepare_legacy_rpc(&serde_json::to_vec(&mixed)?, &health).is_err());
        Ok(())
        })();
        match previous {
            Some(value) => std::env::set_var("RSRS_DATA_DIR", value),
            None => std::env::remove_var("RSRS_DATA_DIR"),
        }
        result
    }

    #[test]
    fn compatibility_boundary_rejects_foreign_and_ambiguous_requests() -> Result<()> {
        let base = vec![("Host".into(), "127.0.0.1:15169".into())];
        assert!(validate_request("POST", "/api/rpc", true, &base, 15169).is_ok());
        for path in ["//other/api/rpc", "/api/rpc?target=other", "http://other/api/rpc", "/admin"] {
            assert!(validate_request("POST", path, true, &base, 15169).is_err());
        }
        assert!(validate_request("POST", "/api/rpc", false, &base, 15169).is_err());
        assert!(validate_request("GET", "/api/runtime/stop", true, &base, 15169).is_err());
        assert!(validate_request("GET", "/mcp", true, &base, 15169).is_ok());
        assert!(validate_request("POST", "/mcp", true, &base, 15169).is_ok());
        for extra in [("Host", "127.0.0.1:15169"), ("Origin", "null"),
            ("Origin", "http://evil.example"), ("Origin", "http://127.0.0.1:15170")] {
            let mut headers = base.clone(); headers.push((extra.0.into(), extra.1.into()));
            assert!(validate_request("POST", "/api/rpc", true, &headers, 15169).is_err());
        }
        // An untrusted endpoint cannot borrow the identity of this real process.
        // OS listener ownership must independently match, even with a valid PID.
        let pid = std::process::id();
        let (image, birth) = host_process(pid)?;
        assert_eq!(image, std::env::current_exe()?.canonicalize()?);
        assert!(!birth.is_empty());
        assert_eq!(host_process(pid)?, (image, birth));
        assert!(host_process(0).is_err());
        #[cfg(target_os = "linux")]
        {
            let listener = TcpListener::bind(("127.0.0.1", 0))?;
            let port = listener.local_addr()?.port();
            assert_eq!(loopback_listener(port, pid)?, pid);
            assert!(loopback_listener(port, 0).is_err());
            // A genuine socket in this process on another loopback address
            // cannot prove ownership of the public 127.0.0.1 endpoint.
            let other = TcpListener::bind(("127.0.0.2", port))?;
            drop(listener);
            assert!(loopback_listener(port, pid).is_err());
            drop(other);
        }
        #[cfg(windows)]
        {
            let header = b"\r\n\xbb\xee\xb6\xaf\xc1\xac\xbd\xd3\r\n";
            let row = b"  TCP  127.0.0.1:15169  0.0.0.0:0  LISTENING  123\r\n";
            let mut output = header.to_vec(); output.extend_from_slice(row);
            assert_eq!(windows_listener_owner(&output, 15169)?, 123);
            assert_eq!(windows_listener_owner(&[output.as_slice(), row].concat(), 15169)?, 123);
            assert_eq!(windows_listener_owner(b"TCP 0.0.0.0:15169 0.0.0.0:0 LISTENING 123\n", 15169)?, 123);
            for invalid in [
                b"TCP 127.0.0.2:15169 0.0.0.0:0 LISTENING 123\n".as_slice(),
                b"TCP 127.0.0.1:151690 0.0.0.0:0 LISTENING 123\n",
                b"TCP 127.0.0.1:15169 0.0.0.0:0 ESTABLISHED 123\n",
                b"TCP 127.0.0.1:15169 0.0.0.0:0 LISTENING 0\n",
                b"TCP 127.0.0.1:15169 0.0.0.0:0 LISTENING +123\n",
                b"TCP 127.0.0.1:15169 0.0.0.0:0 LISTENING 4294967296\n",
                b"TCP 127.0.0.1:15169 0.0.0.0:0 LISTENING \xff\n",
                b"TCP 127.0.0.1:15169 0.0.0.0:0 LISTENING\n",
                b"TCP 127.0.0.1:15169 0.0.0.0:0 LISTENING 123 extra\n",
            ] { assert!(windows_listener_owner(invalid, 15169).is_err()); }
            let other = b"TCP 0.0.0.0:15169 0.0.0.0:0 LISTENING 456\n";
            assert!(windows_listener_owner(&[row.as_slice(), other].concat(), 15169).is_err());
            assert!(windows_listener_owner(header, 15169).is_err());
            let listener = TcpListener::bind(("127.0.0.1", 0))?;
            assert_eq!(loopback_listener(listener.local_addr()?.port(), pid)?, pid);
        }
        let claimed = json!({"pid":pid, "public_port":15169,
            "compatibility_host":"client2-v0256"});
        assert!(listener_matches(&claimed, 15169, None).is_err());
        assert!(listener_matches(&claimed, 15169, Some(pid.saturating_add(1))).is_err());
        assert!(listener_matches(&claimed, 15170, Some(pid)).is_err());
        assert!(listener_matches(&json!({"pid":0,"public_port":15169}), 15169, Some(0)).is_err());
        assert!(read_ready(&b"{}\n"[..]).is_err());
        assert!(read_ready(&b"{\"startup_complete\":true}"[..]).is_err());
        Ok(())
    }
}
