fn opencode_state() -> Result<&'static str> {
    let dir = home()?.join(".config/opencode");
    let mut referenced = false;
    for name in ["opencode.json", "opencode.jsonc"] {
        let text = match std::fs::read_to_string(dir.join(name)) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Ok("manual"),
        };
        let cfg: serde_json::Value = match serde_json::from_str(&text) {
            Ok(cfg) => cfg,
            Err(_) => return Ok("manual"),
        };
        // A later jsonc overrides the former only when it declares instructions.
        if let Some(entries) = cfg.get("instructions") {
            referenced = entries.as_array().is_some_and(|entries| {
                entries
                    .iter()
                    .any(|entry| entry.as_str().is_some_and(same_entry))
            });
        }
    }
    if !referenced {
        return Ok("none");
    }
    check_state(entity_path()?, "reference")
}

fn inject_opencode() -> Result<bool> {
    let entity = entity_path()?;
    let mut changed = write_cover(entity)?;
    // Both opencode.json and opencode.jsonc are loaded (jsonc later can override instructions) — write both if present
    let dir = home()?.join(".config/opencode");
    for name in ["opencode.json", "opencode.jsonc"] {
        let path = dir.join(name);
        if name.ends_with(".jsonc") && !path.exists() {
            continue;
        }
        changed |= patch_instructions(&path, true)?;
    }
    // oh-my-openagent prompt_append (touch only if the field exists; idempotent)
    let oma = dir.join("oh-my-openagent.json");
    if oma.exists() {
        changed |= patch_prompt_append(&oma, true)?;
    }
    Ok(changed)
}

fn remove_opencode() -> Result<bool> {
    // The entity file is shared by OpenCode and WorkBuddy; uninstall only drops this target's reference.
    let mut changed = false;
    let dir = home()?.join(".config/opencode");
    for name in ["opencode.json", "opencode.jsonc"] {
        let path = dir.join(name);
        if path.exists() {
            changed |= patch_instructions(&path, false)?;
        }
    }
    let oma = dir.join("oh-my-openagent.json");
    if oma.exists() {
        changed |= patch_prompt_append(&oma, false)?;
    }
    Ok(changed)
}

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
