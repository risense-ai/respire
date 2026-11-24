/// Add/remove the inject path in the instructions array; keep other entries (the old impl replaced the whole list and deleted extras; fixed 2026-09-09).
fn patch_instructions(path: &std::path::Path, add: bool) -> Result<bool> {
    let entry_str = entity_abs()?;
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if !add {
                return Ok(false);
            }
            let mut obj = serde_json::Map::new();
            obj.insert(
                "$schema".to_owned(),
                serde_json::json!("https://opencode.ai/config.json"),
            );
            obj.insert("instructions".to_owned(), serde_json::json!([entry_str]));
            if let Some(p) = path.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::write(
                path,
                serde_json::to_string_pretty(&serde_json::Value::Object(obj))?,
            )?;
            return Ok(true);
        }
        Err(e) => return Err(anyhow!("failed to read {}: {e}", path.display())),
    };
    let mut cfg: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
        anyhow!("{} parse failed (for jsonc with comments, edit by hand: add/remove {} in the instructions array): {e}", path.display(), entry_str)
    })?;
    let obj = cfg.as_object_mut().ok_or_else(|| {
        anyhow!(
            "{} top-level value is not an object; inspect by hand",
            path.display()
        )
    })?;
    let entry = serde_json::Value::String(entry_str.clone());
    match obj.get_mut("instructions") {
        Some(serde_json::Value::Array(arr)) => {
            // Normalize: `~` form and absolute form are the same item; drop the old form then add the new (no duplicates)
            arr.retain(|x| !x.as_str().is_some_and(same_entry));
            let has = arr.iter().any(|x| x == &entry);
            if add && !has {
                arr.insert(0, entry);
            }
        }
        Some(_) => {
            return Err(anyhow!(
                "{} instructions is not an array; inspect by hand",
                path.display()
            ))
        }
        None if add => {
            obj.insert("instructions".to_owned(), serde_json::json!([entry_str]));
        }
        None => {}
    }
    let new_text = serde_json::to_string_pretty(&cfg)?;
    if new_text == text {
        return Ok(false);
    }
    std::fs::write(path, new_text)?;
    Ok(true)
}

/// Add/remove the inject path in each agent's prompt_append in oh-my-openagent.json (no-op if the field is missing).
fn patch_prompt_append(path: &std::path::Path, add: bool) -> Result<bool> {
    let text = std::fs::read_to_string(path)?;
    let mut cfg: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| anyhow!("{} parse failed: {e}", path.display()))?;
    let abs = entity_abs()?;
    let line = format!("- Memory rules (respire): {abs}");
    // Legacy Chinese line written by earlier releases, kept as escapes so the
    // source stays ASCII: migrate it on add and strip it on remove.
    let legacy = format!("- \u{8bb0}\u{5fc6}\u{94c1}\u{5f8b}\u{ff08}respire\u{ff09}\u{ff1a}{abs}");
    let mut changed = 0usize;
    walk_prompt_append(&mut cfg, add, &line, &legacy, &mut changed);
    if changed == 0 {
        return Ok(false);
    }
    let new_text = serde_json::to_string_pretty(&cfg)?;
    if new_text == text {
        return Ok(false);
    }
    std::fs::write(path, new_text)?;
    Ok(true)
}

/// Only complete managed lines outside fenced examples are ours. Preserve all
/// other prompt text and its line endings verbatim, even if it quotes our path.
fn rewrite_prompt_reference(text: &str, add: bool, line: &str, legacy: &str) -> String {
    let mut out = String::new();
    let mut seen = false;
    let mut fence: Option<(u8, usize)> = None;
    for piece in text.split_inclusive('\n') {
        let (body, ending) = if let Some(body) = piece.strip_suffix("\r\n") {
            (body, "\r\n")
        } else if let Some(body) = piece.strip_suffix('\n') {
            (body, "\n")
        } else {
            (piece, "")
        };
        let trimmed = body.trim_start_matches(' ');
        let indent = body.len() - trimmed.len();
        let bytes = trimmed.as_bytes();
        if indent <= 3 && bytes.first().is_some_and(|c| *c == b'`' || *c == b'~') {
            let ch = bytes[0];
            let count = bytes.iter().take_while(|c| **c == ch).count();
            if count >= 3 {
                match fence {
                    None => fence = Some((ch, count)),
                    Some((open, length)) if ch == open && count >= length && trimmed[count..].trim().is_empty() => fence = None,
                    _ => {}
                }
                out.push_str(piece);
                continue;
            }
        }
        if fence.is_none() && (body == line || body == legacy) {
            if add && !seen {
                out.push_str(line);
                out.push_str(ending);
                seen = true;
            }
        } else {
            out.push_str(piece);
        }
    }
    if add && !seen && fence.is_none() {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(if text.contains("\r\n") { "\r\n" } else { "\n" });
        }
        out.push_str(line);
    }
    out
}

fn walk_prompt_append(
    v: &mut serde_json::Value,
    add: bool,
    line: &str,
    legacy: &str,
    changed: &mut usize,
) {
    match v {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(s)) = map.get_mut("prompt_append") {
                let rewritten = rewrite_prompt_reference(s, add, line, legacy);
                if rewritten != *s {
                    *s = rewritten;
                    *changed += 1;
                }
            }
            for (_, child) in map.iter_mut() {
                walk_prompt_append(child, add, line, legacy, changed);
            }
        }
        serde_json::Value::Array(arr) => {
            for child in arr.iter_mut() {
                walk_prompt_append(child, add, line, legacy, changed);
            }
        }
        _ => {}
    }
}

// ── block mode (codex/claude/codebuddy/generic): markers wrap the insert ──

fn write_block(path: std::path::PathBuf) -> Result<bool> {
    apply_block(&path, false, None)
}

/// Exercise the same WorkBuddy budgeted writer independently in boundary tests.
#[cfg(test)]
fn write_block_with(path: &std::path::Path, content: &str) -> Result<bool> {
    apply_block_with_limit(path, false, None, content, Some(WORKBUDDY_MAX_CHARS))
}

/// workbuddy reference mode: entity file (shared with opencode) + MEMORY.md reference block.
fn inject_workbuddy() -> Result<bool> {
    let ref_md = wb_ref_md(&entity_abs()?);
    let path = expand("~/.workbuddy/MEMORY.md");
    // Check the entire resulting document before touching either target.
    let preview = block_preview_with(&path, false, &ref_md)?;
    check_document_budget(&preview, WORKBUDDY_MAX_CHARS)?;
    let entity = entity_path()?;
    let entity_changed = write_cover(entity.clone()).map_err(|error| anyhow!(
        "WorkBuddy shared policy update failed at {}; inspect its current contents before retrying; MEMORY.md update was not started: {error}", entity.display()
    ))?;
    finish_workbuddy_injection(&path, &entity, &ref_md, &preview.revision, entity_changed)
}

/// These files are not a transaction. Never roll back over concurrent edits.
fn finish_workbuddy_injection(
    path: &std::path::Path,
    entity: &std::path::Path,
    content: &str,
    expected: &str,
    entity_changed: bool,
) -> Result<bool> {
    let memory_changed = apply_block_with_limit(
        path, false, Some(expected), content, Some(WORKBUDDY_MAX_CHARS)
    ).map_err(|error| {
        let state = if entity_changed { "partially completed: shared policy was updated" } else { "incomplete: shared policy was unchanged" };
        anyhow!("WorkBuddy injection {state} at {}; MEMORY.md update failed at {}. Inspect both current files, preserve concurrent user edits and rerun rsrs inject --id workbuddy only after review to repeat validation; no automatic rollback was attempted: {error}", entity.display(), path.display())
    })?;
    Ok(entity_changed || memory_changed)
}

#[derive(Debug, Serialize)]
pub struct BlockPreview {
    pub path: String,
    pub before: String,
    pub after: String,
    pub revision: String,
    pub changed: bool,
}

fn block_preview(path: &std::path::Path, remove: bool) -> Result<BlockPreview> {
    block_preview_with(path, remove, instructions_md())
}

fn block_preview_with(path: &std::path::Path, remove: bool, content: &str) -> Result<BlockPreview> {
    use sha2::{Digest, Sha256};
    let before = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(anyhow!("failed to read {}: {e}", path.display())),
    };
    let mut bounds = Vec::new();
    for (begin, end_marker) in [
        (BLOCK_BEGIN, BLOCK_END),
        (LEGACY_BLOCK_BEGIN, LEGACY_BLOCK_END),
    ] {
        let starts: Vec<_> = before.match_indices(begin).map(|(i, _)| i).collect();
        let ends: Vec<_> = before.match_indices(end_marker).map(|(i, _)| i).collect();
        match (starts.as_slice(), ends.as_slice()) {
            ([], []) => {}
            ([start], [end]) if start < end => bounds.push((*start, end + end_marker.len())),
            _ => anyhow::bail!(
                "inject markers incomplete or duplicated; fix {} first",
                path.display()
            ),
        }
    }
    bounds.sort_unstable();
    anyhow::ensure!(
        bounds.windows(2).all(|pair| pair[0].1 <= pair[1].0),
        "inject markers overlap; fix {} first", path.display()
    );
    let newline = if before
        .find('\n')
        .is_some_and(|i| i > 0 && before.as_bytes()[i - 1] == b'\r')
    {
        "\r\n"
    } else {
        "\n"
    };
    let block = format!(
        "{BLOCK_BEGIN}{newline}{}{newline}{BLOCK_END}",
        content.trim_end()
    );
    let after = if bounds.is_empty() {
        if remove { before.clone() } else {
            let sep = if before.is_empty() {
                String::new()
            } else if before.ends_with("\n\n") {
                String::new()
            } else if before.ends_with('\n') {
                newline.to_owned()
            } else {
                format!("{newline}{newline}")
            };
            format!("{before}{sep}{block}{newline}")
        }
    } else {
        let mut after = String::new();
        let mut cursor = 0;
        for (index, (start, end)) in bounds.iter().copied().enumerate() {
            after.push_str(&before[cursor..start]);
            if index == 0 && !remove { after.push_str(&block); }
            cursor = end;
        }
        after.push_str(&before[cursor..]);
        after
    };
    let revision = hex::encode(Sha256::digest(before.as_bytes()));
    let changed = before != after;
    Ok(BlockPreview {
        path: path.display().to_string(),
        before,
        after,
        revision,
        changed,
    })
}

fn apply_block(path: &std::path::Path, remove: bool, expected: Option<&str>) -> Result<bool> {
    apply_block_with(path, remove, expected, instructions_md())
}

fn apply_block_with(
    path: &std::path::Path,
    remove: bool,
    expected: Option<&str>,
    content: &str,
) -> Result<bool> {
    apply_block_with_limit(path, remove, expected, content, None)
}

fn check_document_budget(preview: &BlockPreview, max_chars: usize) -> Result<()> {
    let count = preview.after.chars().count();
    anyhow::ensure!(count <= max_chars,
        "WorkBuddy MEMORY.md would contain {count} characters (maximum {max_chars}); review existing content before retrying; target was not written");
    Ok(())
}

fn apply_block_with_limit(
    path: &std::path::Path,
    remove: bool,
    expected: Option<&str>,
    content: &str,
    max_chars: Option<usize>,
) -> Result<bool> {
    let preview = block_preview_with(path, remove, content)?;
    if !remove {
        if let Some(limit) = max_chars { check_document_budget(&preview, limit)?; }
    }
    if expected.is_some_and(|value| value != preview.revision) {
        anyhow::bail!("target file changed; inspect and validate its current contents again before applying");
    }
    if !preview.changed {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, preview.after)?;
    Ok(true)
}

/// First desktop preview target: Codex. Reads config only; does not write.
pub fn preview_codex(remove: bool) -> Result<BlockPreview> {
    block_preview(&home()?.join(".codex/AGENTS.md"), remove)
}

pub fn apply_codex(remove: bool, expected: &str) -> Result<bool> {
    apply_block(&home()?.join(".codex/AGENTS.md"), remove, Some(expected))
}

fn remove_block(path: std::path::PathBuf) -> Result<bool> {
    apply_block(&path, true, None)
}

