pub fn model_task_text(task: &Value) -> String {
    let zh = crate::i18n::lang() == crate::i18n::Lang::Zh;
    let done = task["done"].as_u64().unwrap_or(0);
    let total = task["total"].as_u64().unwrap_or(0);
    let phase = task["phase"].as_str().unwrap_or("unknown");
    let progress = if total > 0 && phase == "download" {
        format!(
            "{:.1}% ({:.1}/{:.1} MB)",
            done as f64 / total as f64 * 100.0,
            done as f64 / 1_000_000.0,
            total as f64 / 1_000_000.0
        )
    } else if total > 0 {
        format!(
            "{:.1}% ({done}/{total})",
            done as f64 / total as f64 * 100.0
        )
    } else if matches!(
        phase,
        "queued" | "switch-verify" | "switch-load" | "switch-index" | "switch-diary"
            | "committed" | "index" | "index-load" | "prepare" | "load"
    ) {
        done.to_string()
    } else {
        if zh {
            "\u{7b49}\u{5f85}\u{8fdb}\u{5ea6}"
        } else {
            "waiting for progress"
        }
        .to_owned()
    };
    let elapsed = task["elapsed"]
        .as_u64()
        .or_else(|| task["elapsed_secs"].as_u64());
    let mut text = format!("{} {progress}", task_phase(phase));
    if let Some(item) = task["item"].as_str().filter(|item| !item.is_empty()) {
        text.push_str(&format!(" {item}"));
    }
    if let Some(elapsed) = elapsed {
        text.push_str(&format!(" {elapsed}{}", if zh { " \u{79d2}" } else { "s" }));
    }
    text
}

fn task_phase(phase: &str) -> String {
    if crate::i18n::lang() != crate::i18n::Lang::Zh {
        return match phase {
            "queued" => "Queued",
            "switch-verify" => "Verifying target model",
            "switch-load" => "Loading target model",
            "switch-index" => "Indexing memories",
            "switch-diary" => "Indexing diaries",
            "committed" => "Model switch committed",
            other => other,
        }
        .to_owned();
    }
    match phase {
        "download" => "\u{4e0b}\u{8f7d}\u{4e2d}",
        "connect" => "\u{8fde}\u{63a5}\u{4e0b}\u{8f7d}\u{6e90}",
        "prepare" => "\u{51c6}\u{5907}\u{6a21}\u{578b}",
        "index-load" => "\u{52a0}\u{8f7d}\u{7d22}\u{5f15}\u{6a21}\u{578b}",
        "verify" => "\u{6821}\u{9a8c}\u{4e2d}",
        "index" => "\u{91cd}\u{5efa}\u{7d22}\u{5f15}",
        "queued" => "\u{5df2}\u{6392}\u{961f}",
        "switch-verify" => "\u{6821}\u{9a8c}\u{76ee}\u{6807}\u{6a21}\u{578b}",
        "switch-load" => "\u{52a0}\u{8f7d}\u{76ee}\u{6807}\u{6a21}\u{578b}",
        "switch-index" => "\u{91cd}\u{5efa}\u{8bb0}\u{5fc6}\u{7d22}\u{5f15}",
        "switch-diary" => "\u{91cd}\u{5efa}\u{65e5}\u{8bb0}\u{7d22}\u{5f15}",
        "committed" => "\u{6a21}\u{578b}\u{5207}\u{6362}\u{5df2}\u{63d0}\u{4ea4}",
        "load" => "\u{52a0}\u{8f7d}\u{6a21}\u{578b}",
        "running" => "\u{5904}\u{7406}\u{4e2d}",
        "scheduled" | "pending" => "\u{7b49}\u{5f85}\u{5904}\u{7406}",
        "failed" => "\u{5931}\u{8d25}",
        "ready" => "\u{5c31}\u{7eea}",
        "unknown" => "\u{672a}\u{77e5}",
        other => other,
    }
    .to_owned()
}

pub fn related_value(related: &respire_protocol::RelatedMemory) -> String {
    use respire_protocol::RelationKind;
    let label = match related.relation {
        RelationKind::NewVersion => "new version",
        RelationKind::OldVersion => "old version",
        RelationKind::SeeAlso => "see also",
        RelationKind::CoRecall => "co-recall",
        RelationKind::Neighbor => "neighbor",
    };
    format!("\u{2248} {label} {}", related.title)
}

/// Remove persistence and credential material before a result crosses the
/// CLI boundary. StoredMemory is an internal shape and must never expose its
/// ciphertext, nonce or encrypted embeddings. Plaintext command payloads such
/// as recall/show content remain available; explicit secret/grant/share
/// commands retain the fields that are their declared result.
pub fn sanitize_details(command: &str, value: &Value) -> Value {
    fn sensitive_key(command: &str, key: &str) -> bool {
        let explicit = command.to_ascii_lowercase();
        matches!(
            key.to_ascii_lowercase().as_str(),
            "ciphertext"
                | "nonce"
                | "embedding"
                | "embedding_enc"
                | "local_embedding"
                | "wrapped_urk"
                | "kdf_salt"
                | "blob"
        ) || (matches!(key.to_ascii_lowercase().as_str(), "token" | "password")
            && explicit != "secret"
            && explicit != "grant")
            || (key.eq_ignore_ascii_case("secret") && explicit != "secret")
            || (key.eq_ignore_ascii_case("secret_key") && explicit != "secret")
            || (key.eq_ignore_ascii_case("grant") && explicit != "grant")
            || (key.eq_ignore_ascii_case("code")
                && explicit != "grant"
                && explicit != "space"
                && explicit != "review")
            || (key.eq_ignore_ascii_case("payload")
                && explicit != "share"
                && explicit != "share-import")
            || (key.eq_ignore_ascii_case("raw") && explicit != "share")
    }

    fn walk(command: &str, value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut out = serde_json::Map::new();
                for (key, child) in map {
                    if sensitive_key(command, key) {
                        continue;
                    }
                    out.insert(key.clone(), walk(command, child));
                }
                Value::Object(out)
            }
            Value::Array(values) => Value::Array(values.iter().map(|v| walk(command, v)).collect()),
            other => other.clone(),
        }
    }

    walk(command, value)
}

fn summary_rows(value: &Value) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    flatten_summary("", value, &mut rows, 0);
    rows
}

fn human_summary(command: &str, value: &Value) -> Vec<Vec<String>> {
    let fields: &[&str] = match command {
        "sync" => &[
            "pulled",
            "pushed",
            "local_alive",
            "remote_alive",
            "pending",
            "conflicts",
            "undecodable",
            "index_pending",
            "converged",
        ],
        "status" => &[
            "state",
            "workers",
            "session.user",
            "local_alive",
            "workspace",
            "server_addr",
            "autosync",
            "retrieval_index.state",
            "retrieval_index.error",
        ],
        _ => return summary_rows(value),
    };
    let mut rows: Vec<Vec<String>> = fields
        .iter()
        .filter_map(|field| {
            let mut child = value;
            for key in field.split('.') {
                child = child.get(key)?;
            }
            if child.is_null() {
                return None;
            }
            Some(vec![crate::i18n::field_label(field), compact_human(child)])
        })
        .collect();
    if command == "status" && value["model_operation"]["active"] == true {
        rows.push(vec![
            crate::i18n::field_label("model"),
            model_task_text(&value["model_operation"]),
        ]);
    }
    rows
}

fn humanize_value(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(text) = json_as_text(trimmed) {
        return text;
    }
    if let Some(text) = known_phrase(trimmed) {
        return text;
    }
    if let Some((total, active)) = split_ends(trimmed, " total / ", " active") {
        return if crate::i18n::lang() == crate::i18n::Lang::Zh {
            format!("\u{603b}\u{6570} {total} / \u{6709}\u{6548} {active}")
        } else {
            format!("total {total} / active {active}")
        };
    }
    if let Some(rest) = trimmed.strip_prefix("pulled ") {
        if let Some((pulled, pushed)) = rest.split_once(" / pushed ") {
            return if crate::i18n::lang() == crate::i18n::Lang::Zh {
                format!("\u{4e0b}\u{8f7d} {pulled} / \u{4e0a}\u{4f20} {pushed}")
            } else {
                format!("pulled {pulled} / pushed {pushed}")
            };
        }
    }
    if let Some(rest) = trimmed.strip_prefix("send ") {
        let parts: Vec<&str> = rest.split(" / ").collect();
        if parts.len() == 4 {
            let conflicts = parts[1].strip_prefix("conflicts ").unwrap_or(parts[1]);
            let decrypt = parts[2].strip_prefix("decrypt ").unwrap_or(parts[2]);
            let index = parts[3].strip_prefix("index ").unwrap_or(parts[3]);
            return if crate::i18n::lang() == crate::i18n::Lang::Zh {
                format!(
                    "\u{5f85}\u{63a8}\u{9001} {} / \u{51b2}\u{7a81} {conflicts} / \u{89e3}\u{5bc6} {decrypt} / \u{5d4c}\u{5165} {index}",
                    parts[0]
                )
            } else {
                format!(
                    "send {} / conflicts {conflicts} / decrypt {decrypt} / index {index}",
                    parts[0]
                )
            };
        }
    }
    raw.to_owned()
}

fn split_ends<'a>(text: &'a str, mid: &str, end: &str) -> Option<(&'a str, &'a str)> {
    let text = text.strip_suffix(end)?;
    text.split_once(mid)
}

fn json_as_text(text: &str) -> Option<String> {
    let object = text.starts_with('{') && text.ends_with('}');
    let array = text.starts_with('[') && text.ends_with(']');
    let quoted = text.starts_with('"') && text.ends_with('"');
    if !object && !array && !quoted {
        return None;
    }
    let value: Value = serde_json::from_str(text).ok()?;
    Some(compact_human(&value))
}

fn compact_human(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            keys.iter()
                .map(|key| {
                    format!(
                        "{}={}",
                        crate::i18n::field_label(key),
                        scalar_text(&map[*key])
                    )
                })
                .collect::<Vec<_>>()
                .join("; ")
        }
        Value::Array(items) => items
            .iter()
            .map(compact_human)
            .collect::<Vec<_>>()
            .join("; "),
        other => scalar_text(other),
    }
}

fn known_phrase(text: &str) -> Option<String> {
    let zh = crate::i18n::lang() == crate::i18n::Lang::Zh;
    Some(
        match text {
            "converged" => {
                if zh {
                    "\u{5df2}\u{5bf9}\u{9f50}"
                } else {
                    "converged"
                }
            }
            "not-converged" => {
                if zh {
                    "\u{672a}\u{5bf9}\u{9f50}"
                } else {
                    "not converged"
                }
            }
            "active-count-diff" => {
                if zh {
                    "\u{6709}\u{6548}\u{6761}\u{6570}\u{4e0d}\u{4e00}\u{81f4}"
                } else {
                    "active count differs"
                }
            }
            "cloud sync (auto)" => {
                if zh {
                    "\u{4e91}\u{540c}\u{6b65}\u{ff08}\u{81ea}\u{52a8}\u{ff09}"
                } else {
                    "cloud sync (auto)"
                }
            }
            "cloud sync (manual)" => {
                if zh {
                    "\u{4e91}\u{540c}\u{6b65}\u{ff08}\u{624b}\u{52a8}\u{ff09}"
                } else {
                    "cloud sync (manual)"
                }
            }
            "local authority store (offline)" => {
                if zh {
                    "\u{672c}\u{5730}\u{5e93}\u{ff08}\u{79bb}\u{7ebf}\u{ff09}"
                } else {
                    "local store (offline)"
                }
            }
            "reset" => {
                if zh {
                    "\u{5df2}\u{91cd}\u{7f6e}"
                } else {
                    "reset"
                }
            }
            "kept" => {
                if zh {
                    "\u{4fdd}\u{7559}"
                } else {
                    "kept"
                }
            }
            "true" => crate::i18n::yes_no(true),
            "false" => crate::i18n::yes_no(false),
            _ => return None,
        }
        .to_owned(),
    )
}

fn flatten_summary(name: &str, value: &Value, rows: &mut Vec<Vec<String>>, depth: usize) {
    match value {
        Value::Object(map) if depth < 2 => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for key in keys {
                let child = &map[key];
                let next = if name.is_empty() {
                    crate::i18n::field_label(key)
                } else {
                    format!("{name}.{}", crate::i18n::field_label(key))
                };
                flatten_summary(&next, child, rows, depth + 1);
            }
        }
        Value::Array(items) => {
            rows.push(vec![label_or_value(name), items.len().to_string()]);
        }
        other => {
            rows.push(vec![label_or_value(name), compact_human(other)]);
        }
    }
}

fn label_or_value(name: &str) -> String {
    if name.is_empty() {
        "value".to_owned()
    } else {
        name.to_owned()
    }
}

fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        Value::Bool(flag) => crate::i18n::yes_no(*flag).to_owned(),
        Value::Object(_) | Value::Array(_) => compact_human(value),
        other => other.to_string(),
    }
}

/// Render a compact, dependency-free table for TTY and redirected text output.
pub fn render_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    if headers.is_empty() {
        return String::new();
    }
    let rows = rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|cell| {
                    cell.chars()
                        .map(|c| if c.is_control() { ' ' } else { c })
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut widths: Vec<usize> = headers.iter().map(|h| UnicodeWidthStr::width(*h)).collect();
    for row in &rows {
        for (i, cell) in row.iter().enumerate().take(widths.len()) {
            widths[i] = widths[i].max(UnicodeWidthStr::width(cell.as_str()));
        }
    }
    let line = |row: &[String]| {
        row.iter()
            .enumerate()
            .take(widths.len())
            .map(|(i, cell)| {
                format!(
                    "{cell}{}",
                    " ".repeat(widths[i].saturating_sub(UnicodeWidthStr::width(cell.as_str())))
                )
            })
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_owned()
    };
    let head = headers.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let mut out = vec![line(&head)];
    out.extend(rows.iter().map(|row| line(row)));
    out.join("\n")
}

pub fn status_from_items(items: &[Item]) -> Status {
    if items.iter().any(|i| matches!(i.status, Status::Fail)) {
        Status::Fail
    } else if items
        .iter()
        .any(|i| matches!(i.status, Status::Warn | Status::Pending))
    {
        Status::Warn
    } else {
        Status::Ok
    }
}

pub fn status_label(status: Status) -> &'static str {
    crate::i18n::chrome(match status {
        Status::Ok => "pass",
        Status::Warn => "warn",
        Status::Fail => "fail",
        Status::Skip => "skip",
        Status::Pending => "pending",
    })
}
