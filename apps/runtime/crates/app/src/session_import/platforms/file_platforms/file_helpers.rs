// ----------------------------------------------------------------- helpers

/// Port of the finder's `_roots`: default roots plus each extra root and its
/// known child subdirectories, keeping existing paths only.
fn scan_roots(defaults: Vec<PathBuf>, extra: &[PathBuf], children: &[&str]) -> Vec<PathBuf> {
    let mut expanded = Vec::with_capacity(extra.len() * (children.len() + 1));
    for root in extra {
        expanded.push(root.clone());
        for &child in children {
            expanded.push(root.join(child));
        }
    }
    common::merge_roots(defaults, &expanded)
}

/// Walk `base` following a shallow `/`-separated glob pattern (final segment
/// matches files, earlier segments match directories).
fn glob_files(base: &Path, pattern: &str) -> Vec<PathBuf> {
    let segments: Vec<&str> = pattern.split('/').filter(|segment| !segment.is_empty()).collect();
    let mut out = Vec::new();
    walk_glob(base, &segments, &mut out);
    out
}

fn walk_glob(dir: &Path, segments: &[&str], out: &mut Vec<PathBuf>) {
    let Some((&segment, rest)) = segments.split_first() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = match entry.file_name().into_string() {
            Ok(name) => name,
            Err(_) => continue,
        };
        if !scan_util::match_name(&name, segment) {
            continue;
        }
        let path = entry.path();
        if rest.is_empty() {
            if path.is_file() {
                out.push(path);
            }
        } else if path.is_dir() {
            walk_glob(&path, rest, out);
        }
    }
}

/// Non-list element of a JSON array, as objects.
fn json_maps(value: &Value) -> Vec<&Map<String, Value>> {
    value
        .as_array()
        .map(|items| items.iter().filter_map(Value::as_object).collect())
        .unwrap_or_default()
}

/// First/last user prompt across `role`/`type`-tagged messages.
fn message_edges(messages: &[&Map<String, Value>], role_keys: &[&str]) -> (String, String) {
    let mut first = String::new();
    let mut last = String::new();
    for message in messages {
        let role = message.get("role").and_then(Value::as_str).unwrap_or_default();
        let kind = message.get("type").and_then(Value::as_str).unwrap_or_default();
        if !role_keys.contains(&role) && !role_keys.contains(&kind) {
            continue;
        }
        let prompt = content_of(message.get("content").unwrap_or(&Value::Null));
        if !prompt.is_empty() {
            if first.is_empty() {
                first = prompt.clone();
            }
            last = prompt;
        }
    }
    (first, last)
}

/// Content text, parsing embedded JSON strings first (finder's `_content`).
fn content_of(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        if let Ok(parsed) = serde_json::from_str::<Value>(text) {
            let extracted = scan_util::content_text(&parsed);
            if !extracted.is_empty() {
                return extracted;
            }
        }
        return text.to_owned();
    }
    scan_util::content_text(value)
}

/// Droid prompt text with `<system-reminder>` parts dropped.
fn without_system_reminders(value: &Value) -> String {
    let Some(items) = value.as_array() else {
        return content_of(value);
    };
    let mut parts: Vec<String> = Vec::new();
    for item in items {
        let Some(map) = item.as_object() else {
            continue;
        };
        let text = map
            .get("text")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .or_else(|| map.get("content").and_then(Value::as_str))
            .unwrap_or_default();
        if text.starts_with("<system-reminder>") {
            continue;
        }
        if !text.is_empty() {
            parts.push(text.to_owned());
        }
    }
    parts.join("\n")
}

/// Numeric fields of `tokenUsage` (finder's `_usage`).
fn token_usage(settings: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    if let Some(usage) = settings.get("tokenUsage").and_then(Value::as_object) {
        for (key, value) in usage {
            if value.is_number() {
                out.insert(key.clone(), value.clone());
            }
        }
    }
    out
}

/// First `model:` tag in an amp `tags` array.
fn tag_model(value: &Value) -> Option<String> {
    let items = value.as_array()?;
    for item in items {
        if let Some(tag) = item.as_str() {
            if let Some(rest) = tag.strip_prefix("model:") {
                return Some(rest.to_owned());
            }
        }
    }
    None
}

/// Convert a `file:` URI to a filesystem path (port of `_file_uri_path`).
fn file_uri_path(value: Option<&str>) -> Option<String> {
    let value = value?;
    let (scheme, rest) = match value.find(':') {
        Some(index)
            if index > 0
                && value[..index]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.') =>
        {
            (&value[..index], &value[index + 1..])
        }
        _ => ("", value),
    };
    if scheme != "file" {
        return Some(value.to_owned());
    }
    let (netloc, path) = match rest.strip_prefix("//") {
        Some(after) => match after.find('/') {
            Some(slash) => (&after[..slash], &after[slash..]),
            None => (after, ""),
        },
        None => ("", rest),
    };
    let path = percent_decode(path);
    if !netloc.is_empty() && netloc != "localhost" {
        return Some(format!("//{netloc}{path}"));
    }
    let bytes = path.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[2] == b':' && bytes[1].is_ascii_alphabetic() {
        return Some(path[1..].to_owned());
    }
    Some(path)
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex_value(bytes[index + 1]), hex_value(bytes[index + 2])) {
                out.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// First `#### ` prompt line inside an aider chat block.
fn first_aider_prompt(body: &str) -> String {
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("#### ") {
            return rest.trim().to_owned();
        }
    }
    String::new()
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_owned()
}

fn dir_name(path: &Path) -> String {
    path.parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_owned()
}
