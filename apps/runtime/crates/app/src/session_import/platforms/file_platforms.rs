//! File-based platform scanners: openclaw, droid, amp, gemini, kimi, qwen,
//! codebuff, roo-code, kilo-code, cline and aider.
//!
//! Ported from the reference finder's `file_scanners.py` (plus the shared
//! `transcript.py` / `jsonio.py` helpers): default store roots, glob patterns,
//! session-id derivation, timestamp units and usage/subagent extraction are
//! preserved. The finder shells out for nothing here; every scanner reads local
//! files only (aider reads bounded `.aider.chat.history.md` files and never
//! spawns the `aider` binary).

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use super::common::{self, PlatformDef};
use crate::session_import::{scan_util, ScanOptions, Session};

/// File-based scanner definitions (openclaw, droid, amp, gemini, kimi, qwen, codebuff, roo/kilo-code, cline, aider).
pub fn platforms() -> Vec<PlatformDef> {
    vec![
        PlatformDef {
            id: "openclaw",
            name: "OpenClaw",
            roots: openclaw_roots,
            scan: scan_openclaw,
        },
        PlatformDef {
            id: "droid",
            name: "Factory Droid",
            roots: droid_roots,
            scan: scan_droid,
        },
        PlatformDef {
            id: "amp",
            name: "Amp",
            roots: amp_roots,
            scan: scan_amp,
        },
        PlatformDef {
            id: "gemini",
            name: "Gemini",
            roots: gemini_roots,
            scan: scan_gemini,
        },
        PlatformDef {
            id: "kimi",
            name: "Kimi",
            roots: kimi_roots,
            scan: scan_kimi,
        },
        PlatformDef {
            id: "qwen",
            name: "Qwen",
            roots: qwen_roots,
            scan: scan_qwen,
        },
        PlatformDef {
            id: "codebuff",
            name: "Codebuff",
            roots: codebuff_roots,
            scan: scan_codebuff,
        },
        PlatformDef {
            id: "roo-code",
            name: "Roo Code",
            roots: roo_roots,
            scan: scan_roocode,
        },
        PlatformDef {
            id: "kilo-code",
            name: "Kilo Code",
            roots: kilo_roots,
            scan: scan_kilocode,
        },
        PlatformDef {
            id: "cline",
            name: "Cline",
            roots: cline_roots,
            scan: scan_cline,
        },
        PlatformDef {
            id: "aider",
            name: "Aider",
            roots: aider_roots,
            scan: scan_aider,
        },
    ]
}

// ---------------------------------------------------------------- openclaw
// `~/.openclaw/agents/*/sessions/*.jsonl`, `sessions/*.jsonl` and
// `session-backups/…`; JSONL transcript per file.

const ROO_EXTENSION: &str = "rooveterinaryinc.roo-cline";
const KILO_EXTENSION: &str = "kilocode.kilo-code";
const CLINE_EXTENSION: &str = "saoudrizwan.claude-dev";

fn openclaw_roots() -> Vec<PathBuf> {
    scan_util::existing(
        [scan_util::home().map(|home| home.join(".openclaw"))]
            .into_iter()
            .flatten(),
    )
}

fn scan_openclaw(opts: &ScanOptions) -> Vec<Session> {
    let roots = scan_roots(openclaw_roots(), &opts.roots, &[".openclaw"]);
    let mut paths = Vec::new();
    for root in &roots {
        paths.extend(glob_files(root, "agents/*/sessions/*.jsonl"));
        paths.extend(glob_files(root, "sessions/*.jsonl"));
        paths.extend(glob_files(root, "session-backups/*/*.jsonl"));
        paths.extend(glob_files(root, "session-backups/*/sessions/*.jsonl"));
    }
    common::jsonl_sessions("openclaw", paths)
}

// ---------------------------------------------------------------- droid
// `~/.factory/sessions/*/*.jsonl` with adjacent `*.settings.json`.

fn droid_roots() -> Vec<PathBuf> {
    scan_util::existing(
        [scan_util::home().map(|home| home.join(".factory"))]
            .into_iter()
            .flatten(),
    )
}

fn scan_droid(opts: &ScanOptions) -> Vec<Session> {
    let roots = scan_roots(droid_roots(), &opts.roots, &[".factory"]);
    let mut paths = Vec::new();
    for root in &roots {
        paths.extend(glob_files(root, "sessions/*/*.jsonl"));
    }
    scan_util::recent(paths, scan_util::MAX_PLATFORM_FILES)
        .iter()
        .map(|path| droid_session(path))
        .collect()
}

fn droid_session(path: &Path) -> Session {
    let mut sid = file_stem(path);
    let mut cwd: Option<String> = None;
    let mut first_user: Option<String> = None;
    let mut last_user = String::new();
    let mut created: Option<String> = None;
    let mut updated: Option<String> = None;

    for data in scan_util::iter_jsonl(path) {
        let stamp = data.get("timestamp").and_then(Value::as_str);
        if created.is_none() {
            created = stamp.map(ToOwned::to_owned);
        }
        if let Some(stamp) = stamp {
            updated = Some(stamp.to_owned());
        }
        if data.get("type").and_then(Value::as_str) == Some("session_start") {
            if let Some(id) = data.get("id").and_then(Value::as_str) {
                sid = id.to_owned();
            }
            if cwd.is_none() {
                cwd = data.get("cwd").and_then(Value::as_str).map(ToOwned::to_owned);
            }
        }
        if let Some(message) = data.get("message").and_then(Value::as_object) {
            if message.get("role").and_then(Value::as_str) == Some("user") {
                let prompt = without_system_reminders(message.get("content").unwrap_or(&Value::Null));
                if !prompt.is_empty() {
                    if first_user.is_none() {
                        first_user = Some(prompt.clone());
                    }
                    last_user = prompt;
                }
            }
        }
    }

    let settings = scan_util::read_json(&path.with_extension("settings.json"))
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();

    let created_at = created.clone().or_else(|| scan_util::file_mtime_iso(path));
    let updated_at = updated.or(created).or_else(|| scan_util::file_mtime_iso(path));

    Session {
        platform: "droid".to_owned(),
        id: sid,
        path: path.display().to_string(),
        cwd,
        created_at,
        updated_at,
        provider: None,
        model: settings
            .get("model")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        first_user_message: first_user.unwrap_or_default(),
        last_user_message: last_user,
        usage: token_usage(&settings),
        parent_id: None,
        agent: None,
    }
}

// ---------------------------------------------------------------- amp
// `~/.local/share/amp/threads/T-*.json` (one thread file → one session).

fn amp_roots() -> Vec<PathBuf> {
    scan_util::existing(
        [scan_util::home().map(|home| home.join(".local").join("share").join("amp"))]
            .into_iter()
            .flatten(),
    )
}

fn scan_amp(opts: &ScanOptions) -> Vec<Session> {
    let roots = scan_roots(amp_roots(), &opts.roots, &["amp", ".local/share/amp"]);
    let mut paths = Vec::new();
    for root in &roots {
        paths.extend(glob_files(root, "threads/T-*.json"));
    }
    let mut sessions = Vec::new();
    for path in scan_util::recent(paths, scan_util::MAX_PLATFORM_FILES) {
        sessions.extend(amp_sessions(&path));
    }
    sessions
}

fn amp_sessions(path: &Path) -> Vec<Session> {
    let value = match scan_util::read_json(path) {
        Some(value) => value,
        None => return Vec::new(),
    };
    let data = match value.as_object() {
        Some(map) => map,
        None => return Vec::new(),
    };
    let messages = data.get("messages").map(json_maps).unwrap_or_default();
    let (first_user, last_user) = message_edges(&messages, &["user"]);
    let env = data.get("env").and_then(Value::as_object);
    let initial = env.and_then(|env| env.get("initial")).and_then(Value::as_object);
    let tree = initial
        .and_then(|initial| initial.get("trees"))
        .and_then(Value::as_array)
        .and_then(|trees| trees.first())
        .and_then(Value::as_object);

    let mut usage = Map::new();
    usage.insert("message_count".to_owned(), json!(messages.len()));

    vec![Session {
        platform: "amp".to_owned(),
        id: data
            .get("id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| file_stem(path)),
        path: path.display().to_string(),
        cwd: file_uri_path(tree.and_then(|tree| tree.get("uri")).and_then(Value::as_str)),
        created_at: data
            .get("created")
            .and_then(Value::as_i64)
            .and_then(scan_util::unix_millis)
            .or_else(|| scan_util::file_mtime_iso(path)),
        updated_at: scan_util::file_mtime_iso(path),
        provider: None,
        model: initial
            .and_then(|initial| initial.get("tags"))
            .and_then(tag_model),
        first_user_message: first_user,
        last_user_message: last_user,
        usage,
        parent_id: None,
        agent: None,
    }]
}

// ---------------------------------------------------------------- gemini
// `~/.gemini/tmp/*/chats/*.json`.

fn gemini_roots() -> Vec<PathBuf> {
    scan_util::existing(
        [scan_util::home().map(|home| home.join(".gemini"))]
            .into_iter()
            .flatten(),
    )
}

fn scan_gemini(opts: &ScanOptions) -> Vec<Session> {
    let roots = scan_roots(gemini_roots(), &opts.roots, &[".gemini"]);
    let mut paths = Vec::new();
    for root in &roots {
        paths.extend(glob_files(root, "tmp/*/chats/*.json"));
    }
    let mut sessions = Vec::new();
    for path in scan_util::recent(paths, scan_util::MAX_PLATFORM_FILES) {
        sessions.extend(gemini_sessions(&path));
    }
    sessions
}

fn gemini_sessions(path: &Path) -> Vec<Session> {
    let value = match scan_util::read_json(path) {
        Some(value) => value,
        None => return Vec::new(),
    };
    let data = match value.as_object() {
        Some(map) => map,
        None => return Vec::new(),
    };
    let messages = data.get("messages").map(json_maps).unwrap_or_default();
    let (first_user, last_user) = message_edges(&messages, &["user"]);

    let mut usage = Map::new();
    usage.insert("message_count".to_owned(), json!(messages.len()));

    vec![Session {
        platform: "gemini".to_owned(),
        id: data
            .get("sessionId")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| file_stem(path)),
        path: path.display().to_string(),
        cwd: data
            .get("projectHash")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        created_at: data
            .get("startTime")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| scan_util::file_mtime_iso(path)),
        updated_at: data
            .get("lastUpdated")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| scan_util::file_mtime_iso(path)),
        provider: Some("google".to_owned()),
        model: None,
        first_user_message: first_user,
        last_user_message: last_user,
        usage,
        parent_id: None,
        agent: None,
    }]
}

// ---------------------------------------------------------------- kimi
// `~/.kimi/sessions/*/*/wire.jsonl` (session id = parent directory name).

fn kimi_roots() -> Vec<PathBuf> {
    scan_util::existing(
        [scan_util::home().map(|home| home.join(".kimi"))]
            .into_iter()
            .flatten(),
    )
}

fn scan_kimi(opts: &ScanOptions) -> Vec<Session> {
    let roots = scan_roots(kimi_roots(), &opts.roots, &[".kimi"]);
    let mut paths = Vec::new();
    for root in &roots {
        paths.extend(glob_files(root, "sessions/*/*/wire.jsonl"));
    }
    scan_util::recent(paths, scan_util::MAX_PLATFORM_FILES)
        .iter()
        .map(|path| kimi_session(path))
        .collect()
}

fn kimi_session(path: &Path) -> Session {
    let mut first_user = String::new();
    let mut last_user = String::new();
    let mut created: Option<String> = None;
    let mut updated: Option<String> = None;

    for data in scan_util::iter_jsonl(path) {
        let stamp = data.get("timestamp").and_then(Value::as_f64);
        if created.is_none() {
            created = stamp.and_then(scan_util::unix_seconds);
        }
        if let Some(value) = stamp.and_then(scan_util::unix_seconds) {
            updated = Some(value);
        }
        if data.get("type").and_then(Value::as_str) == Some("TurnBegin") {
            let prompt = data
                .get("payload")
                .and_then(Value::as_object)
                .and_then(|payload| payload.get("user_input"))
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty());
            if let Some(prompt) = prompt {
                if first_user.is_empty() {
                    first_user = prompt.to_owned();
                }
                last_user = prompt.to_owned();
            }
        }
    }

    Session {
        platform: "kimi".to_owned(),
        id: dir_name(path),
        path: path.display().to_string(),
        cwd: None,
        created_at: created.or_else(|| scan_util::file_mtime_iso(path)),
        updated_at: updated.or_else(|| scan_util::file_mtime_iso(path)),
        provider: Some("moonshot".to_owned()),
        model: None,
        first_user_message: first_user,
        last_user_message: last_user,
        usage: Map::new(),
        parent_id: None,
        agent: None,
    }
}

include!("file_platforms/additional_platforms.rs");
include!("file_platforms/file_helpers.rs");
