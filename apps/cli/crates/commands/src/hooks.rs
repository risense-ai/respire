//! Plugin hook runner.
//!
//! Config is `<data-dir>/plugins.json`:
//! ```jsonc
//! { "hooks": {
//!     "pre-remember":  [{ "cmd": "sensitive-scan", "timeout_ms": 3000, "on_error": "block" }],
//!     "post-remember": [{ "cmd": "auto-tagger", "timeout_ms": 5000, "on_error": "warn" }]
//! } }
//! ```
//! Semantics: event JSON is written to the plugin stdin (`{"event":"...","ts":"...","data":{...}}`);
//! a `pre-*` hook that exits 0 with stdout `{"verdict":"block","reason":"..."}` vetoes the action;
//! a non-zero exit follows `on_error` (block=veto / warn=log and continue / skip=silent);
//! `post-*` failures do not block the main flow. Timeouts attempt cleanup and report failure.
//! Safety: the child is stripped of all ONEMEMORY_* secrets; payloads are summaries, not keys.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Hook event. pre = before the action (can veto); post = after (observe only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    PreRemember,
    PostRemember,
    PostRecall,
    PostForget,
    PostSync,
}

impl HookEvent {
    pub fn as_str(&self) -> &'static str {
        match self {
            HookEvent::PreRemember => "pre-remember",
            HookEvent::PostRemember => "post-remember",
            HookEvent::PostRecall => "post-recall",
            HookEvent::PostForget => "post-forget",
            HookEvent::PostSync => "post-sync",
        }
    }
    pub fn parse(s: &str) -> Option<HookEvent> {
        Some(match s {
            "pre-remember" => HookEvent::PreRemember,
            "post-remember" => HookEvent::PostRemember,
            "post-recall" => HookEvent::PostRecall,
            "post-forget" => HookEvent::PostForget,
            "post-sync" => HookEvent::PostSync,
            _ => return None,
        })
    }
    /// Pre events may veto the action; post events only observe.
    pub fn is_pre(&self) -> bool {
        matches!(self, HookEvent::PreRemember)
    }
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct HookSpec {
    /// Plugin command (run via shell; may include args).
    pub cmd: String,
    /// Wait timeout in milliseconds (default 5000; `on_error` applies after cleanup).
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// Failure policy: block (pre may veto) / warn (stderr warning, action continues) / skip (silent).
    /// Pre defaults to warn; post defaults to skip.
    #[serde(default)]
    pub on_error: String,
}

fn default_timeout_ms() -> u64 {
    5000
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct HooksConfig {
    #[serde(default)]
    pub hooks: BTreeMap<String, Vec<HookSpec>>,
}

/// A missing config means no plugins; unreadable or invalid config is an error.
pub fn read_config_at(path: &Path) -> anyhow::Result<HooksConfig> {
    use anyhow::Context;
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HooksConfig::default()),
        Err(error) => return Err(error).with_context(|| format!("read plugin config {}", path.display())),
    };
    let config: HooksConfig = serde_json::from_str(&text)
        .with_context(|| format!("parse plugin config {}", path.display()))?;
    for (event, specs) in &config.hooks {
        anyhow::ensure!(HookEvent::parse(event).is_some(), "unknown plugin event {event}");
        for spec in specs {
            anyhow::ensure!(!spec.cmd.trim().is_empty(), "empty plugin command for {event}");
            anyhow::ensure!(matches!(spec.on_error.as_str(), "" | "block" | "warn" | "skip"),
                "unknown plugin failure policy {} for {event}", spec.on_error);
        }
    }
    Ok(config)
}

/// Result of one fire() call.
#[derive(Debug, Default)]
pub struct HookVerdict {
    /// Pre events: true = the action was vetoed.
    pub blocked: bool,
    pub reason: String,
    /// Warnings (`on_error=warn` or plugin stderr summary); the caller may display them.
    pub warnings: Vec<String>,
    /// Execution faults remain visible to acknowledged host event delivery, including skip policy.
    pub failures: Vec<String>,
}

impl HookVerdict {
    fn allow() -> HookVerdict {
        HookVerdict::default()
    }
}

/// Single-hook outcome: Ok=success / Blocked=explicit veto / Failed=handled per on_error.
enum HookOutcome {
    Ok(Vec<String>),
    Blocked(String),
    Failed(String),
}

/// Fire one event: run every hook for it in order.
/// payload is a summary-level business payload; combined with event/ts into JSON on plugin stdin.
pub fn fire_at(path: &Path, event: HookEvent, payload: serde_json::Value) -> anyhow::Result<HookVerdict> {
    Ok(fire_config(&read_config_at(path)?, event, payload))
}

/// Execute the validated configuration captured before submitting a core command.
pub fn fire_config(cfg: &HooksConfig, event: HookEvent, payload: serde_json::Value) -> HookVerdict {
    let specs = match cfg.hooks.get(event.as_str()) {
        Some(v) if !v.is_empty() => v,
        _ => return HookVerdict::allow(),
    };
    let mut verdict = HookVerdict::allow();
    let envelope = serde_json::json!({
        "event": event.as_str(),
        "ts": chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%z").to_string(),
        "data": payload,
    });
    for spec in specs {
        match run_hook(spec, &envelope) {
            HookOutcome::Ok(mut w) => verdict.warnings.append(&mut w),
            HookOutcome::Blocked(reason) => {
                // Explicit veto always applies, regardless of on_error - skip only covers faults, not verdicts.
                if event.is_pre() {
                    verdict.blocked = true;
                    verdict.reason = format!("hook {} blocked: {reason}", spec.cmd);
                } else {
                    verdict.warnings.push(format!("hook {} returned a veto after commit (ignored): {reason}", spec.cmd));
                }
            }
            HookOutcome::Failed(fail) => {
                verdict.failures.push(format!("hook {} failed: {fail}", spec.cmd));
                eprintln!("HOOK event={} command={} failed={fail}", event.as_str(), spec.cmd);
                let policy = if spec.on_error.is_empty() && event.is_pre() {
                    "warn"
                } else if spec.on_error.is_empty() {
                    "skip"
                } else {
                    spec.on_error.as_str()
                };
                match (event.is_pre(), policy) {
                    (true, "block") => {
                        verdict.blocked = true;
                        verdict.reason = format!("hook {} failed: {fail}", spec.cmd);
                    }
                    (true, "warn") | (_, "warn") => {
                        verdict
                            .warnings
                            .push(format!("hook {} failed (continuing): {fail}", spec.cmd));
                    }
                    _ => {} // skip: silent
                }
            }
        }
        if verdict.blocked {
            break; // vetoed; remaining hooks are not run
        }
    }
    verdict
}

/// Run a single plugin.
fn run_hook(spec: &HookSpec, envelope: &serde_json::Value) -> HookOutcome {
    let body = match serde_json::to_string(envelope) {
        Ok(body) => body,
        Err(e) => return HookOutcome::Failed(e.to_string()),
    };
    // Restore PATH/HOME and Windows SystemRoot, but no ONEMEMORY_* secrets.
    // New secret vars are also wiped by env_clear, so there is no "forgot to strip" hole.
    #[cfg(windows)]
    let mut spawn = {
        use std::os::windows::process::CommandExt;
        let mut command = Command::new("cmd.exe");
        command.args(["/D", "/S", "/C"]);
        command.raw_arg(&spec.cmd);
        command
    };
    #[cfg(not(windows))]
    let mut spawn = {
        use std::os::unix::process::CommandExt;
        let mut command = Command::new("sh");
        command.arg("-c").arg(&spec.cmd).process_group(0);
        command
    };
    let spawn = spawn
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear()
        .env("PATH", crate::env::var("PATH").unwrap_or_default())
        .env("HOME", crate::env::var("HOME").unwrap_or_default())
        .env("RSRS_HOOK_EVENT", envelope["event"].as_str().unwrap_or(""));
    #[cfg(windows)]
    if let Some(root) = crate::env::var_os("SystemRoot") {
        spawn.env("SystemRoot", root);
    }
    let mut child = match spawn.spawn() {
        Ok(child) => child,
        Err(e) => return HookOutcome::Failed(format!("failed to start ({e})")),
    };
    let (Some(mut stdin), Some(mut stdout), Some(mut stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        let mut errors = vec!["hook I/O pipes unavailable".to_owned()];
        errors.extend(stop_hook(&mut child));
        return HookOutcome::Failed(errors.join("; "));
    };
    let writer = std::thread::spawn(move || stdin.write_all(body.as_bytes()));
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });

    let timeout = Duration::from_millis(spec.timeout_ms.max(1));
    let started = Instant::now();
    let mut errors = Vec::new();
    let status = loop {
        if writer.is_finished() && stdout_reader.is_finished() && stderr_reader.is_finished() {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {}
                Err(e) => {
                    errors.push(format!("wait for plugin failed ({e})"));
                    break None;
                }
            }
        }
        if started.elapsed() >= timeout {
            errors.push(format!("timeout (>{}ms)", spec.timeout_ms));
            break None;
        }
        std::thread::sleep(
            Duration::from_millis(10).min(timeout.saturating_sub(started.elapsed())),
        );
    };
    if status.is_none() {
        errors.extend(stop_hook(&mut child));
        let started = Instant::now();
        while !(writer.is_finished() && stdout_reader.is_finished() && stderr_reader.is_finished())
            && started.elapsed() < Duration::from_secs(2)
        {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    let input = finish_hook_io(writer, "stdin write");
    let stdout = finish_hook_io(stdout_reader, "stdout read");
    let stderr = finish_hook_io(stderr_reader, "stderr read");
    if status.is_some() {
        if let Ok(stderr) = &stderr {
            let stderr = String::from_utf8_lossy(stderr);
            for line in stderr.lines().filter(|l| !l.trim().is_empty()).take(5) {
                eprintln!("HOOK command={} message={}", spec.cmd, line);
            }
        }
    }
    let (status, stdout, input_error) = match (status, input, stdout, stderr) {
        (Some(status), Ok(()), Ok(stdout), Ok(_)) if errors.is_empty() => (status, stdout, None),
        (Some(status), Err(error), Ok(stdout), Ok(_))
            if errors.is_empty()
                && status.success()
                && error.kind() == std::io::ErrorKind::BrokenPipe =>
        {
            // An early veto may close stdin before the full payload is written.
            (status, stdout, Some(error))
        }
        (_, input, stdout, stderr) => {
            if let Err(e) = input {
                errors.push(e.to_string());
            }
            if let Err(e) = stdout {
                errors.push(e.to_string());
            }
            if let Err(e) = stderr {
                errors.push(e.to_string());
            }
            return HookOutcome::Failed(errors.join("; "));
        }
    };
    if !status.success() {
        return HookOutcome::Failed(format!("{}", status));
    }
    let stdout = String::from_utf8_lossy(&stdout);
    // Pre: last stdout JSON line {"verdict":"block","reason":"..."} -> veto
    if let Some(last) = stdout.lines().rev().find(|l| !l.trim().is_empty()) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(last.trim()) {
            if v["verdict"] == "block" {
                return HookOutcome::Blocked(
                    v["reason"].as_str().unwrap_or("no reason given").to_owned(),
                );
            }
        }
    }
    if let Some(error) = input_error {
        return HookOutcome::Failed(error.to_string());
    }
    HookOutcome::Ok(Vec::new())
}

fn wait_for_child(
    child: &mut std::process::Child,
    timeout: Duration,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if started.elapsed() >= timeout {
            return Ok(None);
        }
        std::thread::sleep(
            Duration::from_millis(10).min(timeout.saturating_sub(started.elapsed())),
        );
    }
}

fn stop_hook(child: &mut std::process::Child) -> Vec<String> {
    let mut errors = Vec::new();
    // shortcut: escaped descendants can survive; use OS-enforced jobs for full tree ownership.
    #[cfg(windows)]
    let mut stop = {
        use std::os::windows::process::CommandExt;
        let mut command = Command::new("taskkill");
        command.args(["/PID", &child.id().to_string(), "/T", "/F"]);
        command.creation_flags(0x0800_0000);
        command
    };
    #[cfg(not(windows))]
    let mut stop = {
        let mut command = Command::new("kill");
        command.args(["-KILL", "--", &format!("-{}", child.id())]);
        command
    };
    match stop
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(mut stopper) => {
            let unfinished = match wait_for_child(&mut stopper, Duration::from_secs(2)) {
                Ok(Some(status)) if status.success() => false,
                Ok(Some(status)) => {
                    errors.push(format!("process tree termination failed ({status})"));
                    false
                }
                Ok(None) => {
                    errors.push("process tree termination timed out".to_owned());
                    true
                }
                Err(e) => {
                    errors.push(format!("waiting for process tree termination failed ({e})"));
                    true
                }
            };
            if unfinished {
                let kill_error = stopper.kill().err();
                match wait_for_child(&mut stopper, Duration::from_secs(2)) {
                    Ok(Some(_)) => {}
                    Ok(None) => errors.push(format!(
                        "termination helper was not reaped (kill: {kill_error:?})"
                    )),
                    Err(e) => errors.push(format!(
                        "termination helper could not be reaped ({e}; kill: {kill_error:?})"
                    )),
                }
            }
        }
        Err(e) => errors.push(format!("failed to start process tree termination ({e})")),
    }
    let kill_error = child.kill().err();
    match wait_for_child(child, Duration::from_secs(2)) {
        Ok(Some(_)) => {}
        Ok(None) => errors.push(format!("hook child was not reaped (kill: {kill_error:?})")),
        Err(e) => errors.push(format!(
            "hook child could not be reaped ({e}; kill: {kill_error:?})"
        )),
    }
    errors
}

fn finish_hook_io<T>(
    worker: std::thread::JoinHandle<std::io::Result<T>>,
    name: &str,
) -> std::io::Result<T> {
    if !worker.is_finished() {
        return Err(std::io::Error::other(format!("{name} thread did not stop")));
    }
    worker
        .join()
        .map_err(|_| std::io::Error::other(format!("{name} thread panicked")))?
        .map_err(|e| std::io::Error::new(e.kind(), format!("{name} failed ({e})")))
}

/// Full event-name table (plugin list / docs).
pub const EVENT_NAMES: &[&str] = &[
    "pre-remember",
    "post-remember",
    "post-recall",
    "post-forget",
    "post-sync",
];

// -- Event payload builders (summary-level: the smallest surface a hook needs) --

pub fn remember_payload(
    title: &str,
    importance: &str,
    kind: &str,
    project: &str,
    content: &str,
) -> serde_json::Value {
    serde_json::json!({
        "title": title,
        "importance": importance,
        "kind": kind,
        "project": project,
        "content_head": content.chars().take(200).collect::<String>(),
        "content_chars": content.chars().count(),
    })
}

pub fn remember_done_payload(id: &str, title: &str, importance: &str) -> serde_json::Value {
    serde_json::json!({ "id": id, "title": title, "importance": importance })
}

pub fn recall_payload(query: &str, project: &str, hits: &[(String, f32)]) -> serde_json::Value {
    serde_json::json!({
        "query": query,
        "project": project,
        "hits": hits.iter().map(|(id, s)| serde_json::json!({"id": id, "score": s})).collect::<Vec<_>>(),
    })
}

pub fn forget_payload(id: &str) -> serde_json::Value {
    serde_json::json!({ "id": id })
}

pub fn sync_payload(pulled: usize, pushed: usize) -> serde_json::Value {
    serde_json::json!({ "pulled": pulled, "pushed": pushed })
}

#[cfg(test)]
mod tests;
