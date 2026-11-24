//! Convert a normalized [`Session`] into a protocol `MemoryEntry`.
//!
//! New bodies use the canonical Cause, Action, and Result section headings.
//! Existing memory bodies are not migrated by this formatter. Credentials embedded in transcripts
//! are scrubbed best-effort before storage (agent policy requires inspecting
//! content before writing; this is a safety net, not a scanner guarantee).

use chrono::{SecondsFormat, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::memory::model::{Kind, MemoryEntry};

use super::Session;

/// Conversion knobs chosen by the CLI (`extract` flags).
#[derive(Debug, Clone)]
pub struct ConvertOptions {
    pub kind: Kind,
    /// `important` (reusable) or `trivial` (diary).
    pub importance: String,
    /// Optional causal parent for every imported memory.
    pub parent: String,
    pub user: String,
    /// Device tag (`host/os`).
    pub device: String,
    /// Explicit project override; empty → derived from the session cwd.
    pub project: String,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self {
            kind: Kind::Context,
            importance: "important".to_owned(),
            parent: String::new(),
            user: "local".to_owned(),
            device: String::new(),
            project: String::new(),
        }
    }
}

/// Build one `MemoryEntry` from a session.
pub fn session_entry(session: &Session, opts: &ConvertOptions) -> MemoryEntry {
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let created_at = session
        .created_at
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| now.clone());
    let updated_at = session
        .updated_at
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| created_at.clone());
    MemoryEntry {
        metadata: Default::default(),
        unknown_fields: Default::default(),
        supersedes: String::new(),
        superseded_by: String::new(),
        see_also: Vec::new(),
        id: session_id(&session.key()),
        kind: opts.kind,
        tags: vec!["session-import".to_owned(), session.platform.clone()],
        title: scrub(&derive_title(session)),
        content: build_body(session),
        user: opts.user.clone(),
        computer: opts.device.clone(),
        project: project_of(session, &opts.project),
        created_at,
        // These SQLite sources have no reliable per-session modification stamp:
        // WAL commits can leave the main file mtime unchanged. Their cursor uses
        // content instead, and a fresh import needs a fresh LWW write stamp.
        updated_at: if session.uses_content_cursor() {
            now
        } else {
            updated_at
        },
        emotion: -1.0,
        parent_id: opts.parent.clone(),
        importance: opts.importance.clone(),
        device: opts.device.clone(),
        modified_by: opts.device.clone(),
    }
}

/// Human-readable platform label.
fn platform_label(platform: &str) -> String {
    match platform {
        "codex" => "Codex".into(),
        "opencode" => "OpenCode".into(),
        "claude" => "Claude Code".into(),
        "senpi" => "Senpi".into(),
        "oh-my-pi" => "oh-my-pi".into(),
        "gajae-code" => "gajae-code".into(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => other.to_owned(),
            }
        }
    }
}

/// Session title: `<Platform> session <first request>` (concise, event-shaped).
pub fn derive_title(session: &Session) -> String {
    let platform = platform_label(&session.platform);
    let snippet = first_line(&scrub(&session.first_user_message));
    if snippet.is_empty() {
        format!("{platform} session {}", short_id(&session.id))
    } else {
        format!("{platform} session {snippet}")
    }
}

/// Assemble the English scaffold; user-supplied text keeps its original language.
pub fn build_body(session: &Session) -> String {
    let cwd = session.cwd.as_deref().unwrap_or("unknown");
    let model = match (session.provider.as_deref(), session.model.as_deref()) {
        (Some(provider), Some(model)) => format!("{provider}/{model}"),
        (Some(provider), None) => provider.to_owned(),
        (None, Some(model)) => model.to_owned(),
        (None, None) => "unknown".to_owned(),
    };
    let created = session.created_at.as_deref().unwrap_or("unknown");
    let updated = session.updated_at.as_deref().unwrap_or(created);

    let mut body = String::new();
    body.push_str(&format!(
        "\u{3010}Cause\u{3011}{} session {}. Started {}; working directory {}; model {}.",
        platform_label(&session.platform),
        short_id(&session.id),
        scrub(created),
        scrub(&cwd),
        scrub(&model)
    ));

    let first_raw = session.first_user_message.trim();
    let first = truncate_chars(&scrub(first_raw), 600);
    body.push_str("\n\u{3010}Action\u{3011}");
    if first.is_empty() {
        body.push_str("(no user message recorded)");
    } else {
        body.push_str(&first);
    }
    let last_raw = session.last_user_message.trim();
    if !last_raw.is_empty() && last_raw != first_raw {
        body.push_str(&format!(
            "\nLast request: {}",
            truncate_chars(&scrub(last_raw), 400)
        ));
    }

    body.push_str(&format!("\n\u{3010}Result\u{3011}Session updated at {}.", scrub(updated)));
    if let Some(line) = usage_line(&session.usage) {
        body.push(' ');
        body.push_str(&line);
    }
    if let Some(agent) = session.agent.as_deref().filter(|s| !s.is_empty()) {
        body.push_str(&format!(" Executed by subagent `{}`", scrub(agent)));
        if let Some(parent) = session.parent_id.as_deref().filter(|s| !s.is_empty()) {
            body.push_str(&format!(" (parent session {})", short_id(parent)));
        }
        body.push('.');
    }
    body
}

fn usage_line(usage: &serde_json::Map<String, Value>) -> Option<String> {
    if usage.is_empty() {
        return None;
    }
    let number = |key: &str| -> Option<f64> { usage.get(key).and_then(Value::as_f64) };
    let mut parts = Vec::new();
    let total = number("totalTokens").or_else(|| number("total_tokens"));
    if let Some(total) = total {
        parts.push(format!("tokens {total:.0}"));
    } else {
        if let Some(input) = number("input") {
            parts.push(format!("input {input:.0}"));
        }
        if let Some(output) = number("output") {
            parts.push(format!("output {output:.0}"));
        }
    }
    if let Some(cost) = number("cost_total") {
        parts.push(format!("cost {cost:.4}"));
    }
    if let Some(count) = number("message_count") {
        parts.push(format!("messages {count:.0}"));
    }
    if parts.is_empty() {
        None
    } else {
        Some(format!("{}.", parts.join("; ")))
    }
}

/// Resolve the project label: explicit override, else the cwd basename.
fn project_of(session: &Session, explicit: &str) -> String {
    if !explicit.trim().is_empty() {
        return scrub(explicit.trim());
    }
    session
        .cwd
        .as_deref()
        .and_then(|cwd| cwd.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next())
        .filter(|name| !name.is_empty())
        .map(scrub)
        .unwrap_or_default()
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| truncate_chars(&collapse_ws(line), 40))
        .unwrap_or_default()
}

fn collapse_ws(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let mut out: String = text.chars().take(max).collect();
        out.push('\u{2026}');
        out
    }
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Stable memory id for a session (UUIDv5-shaped from `platform:id`), so
/// re-importing the same session maps to the same row instead of a duplicate.
fn session_id(key: &str) -> String {
    let digest = Sha256::digest(key.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes).to_string()
}

/// Best-effort credential scrub: PEM blocks, `key=value` secrets and known
/// token prefixes are redacted so plaintext secrets are not stored.
pub fn scrub(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_pem = false;
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        if line.contains("-----BEGIN") && lower.contains("private key") {
            in_pem = true;
            out.push_str("[REDACTED PRIVATE KEY]");
            out.push('\n');
            continue;
        }
        if in_pem {
            if line.contains("-----END") && lower.contains("private key") {
                in_pem = false;
            }
            continue;
        }
        out.push_str(&redact_line(line));
        out.push('\n');
    }
    if !text.ends_with('\n') {
        out.pop();
    }
    out
}

const SECRET_KEYS: [&str; 17] = [
    "password",
    "passwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "access_key",
    "private_key",
    "authorization",
    "bearer",
    "credential",
    "client_secret",
    "\u{5bc6}\u{7801}",
    "\u{53e3}\u{4ee4}",
    "\u{5bc6}\u{94a5}",
    "\u{4ee4}\u{724c}",
    "\u{51ed}\u{8bc1}",
];

fn redact_line(line: &str) -> String {
    let masked = redact_key_value(line).unwrap_or_else(|| line.to_owned());
    let masked = redact_bearer(&masked);
    redact_prefixed_tokens(&masked)
}

/// Mask the value when a secret keyword is followed (after spaces/tabs and an
/// optional opening quote) by `=`/`:` (ASCII or full-width), or when the keyword
/// is a `--flag` whose value is whitespace-separated (`--password hunter2`). Prose
/// that merely mentions "token" or a timestamp is not truncated.
fn redact_key_value(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    let mut best: Option<usize> = None;
    for key in SECRET_KEYS {
        let mut from = 0;
        while let Some(relative) = lower[from..].find(key) {
            let pos = from + relative;
            let after = &line[pos + key.len()..];
            let trimmed = after.trim_start_matches([' ', '\t', '"', '\'']);
            let separator = trimmed
                .chars()
                .next()
                .filter(|c| matches!(*c, '=' | ':' | '\u{ff1a}'));
            let index = if let Some(separator) = separator {
                line.len() - (trimmed.len() - separator.len_utf8())
            } else if line[..pos].ends_with("--") {
                // CLI flag form `--password <value>`: value is whitespace-separated.
                let gap = after.len() - after.trim_start_matches([' ', '\t']).len();
                if gap == 0 {
                    from = pos + key.len();
                    continue;
                }
                pos + key.len()
            } else {
                from = pos + key.len();
                continue;
            };
            best = Some(best.map_or(index, |current| current.min(index)));
            from = pos + key.len();
        }
    }
    best.map(|index| {
        let (head, _) = line.split_at(index);
        format!("{head} ***")
    })
}

/// Mask the token in `Bearer <token>` even without a `=`/`:` separator.
fn redact_bearer(line: &str) -> String {
    let mut result = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(pos) = rest.to_ascii_lowercase().find("bearer") {
        let end = pos + "bearer".len();
        result.push_str(&rest[..end]);
        rest = &rest[end..];
        let space = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        let token = &rest[space..];
        let token_len = token
            .find(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | ',' | ';' | ')' | ']'))
            .unwrap_or(token.len());
        if space > 0 && token_len >= 8 {
            result.push_str(&rest[..space]);
            result.push_str("***");
            rest = &rest[space + token_len..];
        }
    }
    result.push_str(rest);
    result
}

const TOKEN_PREFIXES: [&str; 14] = [
    "sk-",
    "ghp_",
    "gho_",
    "ghs_",
    "ghu_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "xoxa-",
    "xoxb-",
    "xoxp-",
    "AKIA",
    "AIza",
    "eyJ",
];

fn redact_prefixed_tokens(line: &str) -> String {
    let mut result = String::new();
    let mut rest = line;
    loop {
        let mut earliest: Option<(usize, &str)> = None;
        for prefix in TOKEN_PREFIXES {
            if let Some(idx) = rest.find(prefix) {
                if earliest.map_or(true, |(best, _)| idx < best) {
                    earliest = Some((idx, prefix));
                }
            }
        }
        let Some((idx, prefix)) = earliest else {
            result.push_str(rest);
            break;
        };
        result.push_str(&rest[..idx]);
        let after = &rest[idx..];
        let run: String = after[prefix.len()..]
            .chars()
            .take_while(|c| {
                c.is_ascii_alphanumeric()
                    || *c == '-'
                    || *c == '_'
                    || (prefix == "eyJ" && *c == '.')
            })
            .collect();
        if run.len() >= 8 {
            result.push_str(prefix);
            result.push_str("***");
        } else {
            result.push_str(prefix);
            result.push_str(&run);
        }
        rest = &after[prefix.len() + run.len()..];
    }
    result
}

#[cfg(test)]
mod tests;
