/// Probe all targets (shown in the UI before inject).
pub fn targets() -> Result<Vec<Target>> {
    let h = home()?;
    let probe = [
        ("dsh", h.join(".dsh").is_dir()),
        ("opencode", h.join(".config/opencode").is_dir()),
        ("codex", h.join(".codex").is_dir()),
        ("claude", h.join(".claude").is_dir()),
        ("codebuddy", h.join(".codebuddy").is_dir()),
        ("workbuddy", h.join(".workbuddy").is_dir()),
        ("kylinbot", h.join(".kylinbot").is_dir()),
        ("pi", h.join(".pi").is_dir()),
        (
            "zigcode",
            h.join(".zcode").is_dir() || h.join(".zigcode").is_dir(),
        ),
        ("deepseek", h.join(".deepseek").is_dir()),
        (
            "qwen",
            h.join(".qwen").is_dir() || h.join(".config/qwen").is_dir(),
        ),
        (
            "doubao",
            h.join(".doubao").is_dir() || h.join(".config/doubao").is_dir(),
        ),
        ("grok", h.join(".grok").is_dir()),
        ("generic", true),
    ];
    let mut out = Vec::new();
    for (id, likely) in probe {
        let name: &'static str;
        let path: String;
        let mode: &'static str;
        match id {
            "dsh" => {
                name = "dsh";
                path = "~/.dsh/AGENTS.md".to_owned();
                mode = "inject";
            }
            "opencode" => {
                name = "opencode";
                path = entity_display();
                mode = "reference";
            }
            "codex" => {
                name = "codex";
                path = "~/.codex/AGENTS.md".to_owned();
                mode = "block";
            }
            "claude" => {
                name = "claude code";
                path = "~/.claude/CLAUDE.md".to_owned();
                mode = "block";
            }
            "codebuddy" => {
                name = "codebuddy";
                path = "~/.codebuddy/AGENTS.md".to_owned();
                mode = "block";
            }
            "workbuddy" => {
                name = "workbuddy";
                path = "~/.workbuddy/MEMORY.md".to_owned();
                mode = "ref-block";
            }
            "pi" => {
                name = "pi agent";
                path = "~/.pi/AGENTS.md".to_owned();
                mode = "block";
            }
            "zigcode" => {
                name = "zig code";
                path = "~/.zcode/AGENTS.md".to_owned();
                mode = "block";
            }
            "deepseek" => {
                name = "deepseek harness";
                path = "~/.deepseek/AGENTS.md".to_owned();
                mode = "block";
            }
            "qwen" => {
                name = "Qwen assistant";
                path = "~/.qwen/AGENTS.md".to_owned();
                mode = "block";
            }
            "doubao" => {
                name = "Doubao office";
                path = "~/.doubao/AGENTS.md".to_owned();
                mode = "block";
            }
            "grok" => {
                name = "grok build";
                path = "~/.grok/AGENTS.md".to_owned();
                mode = "block";
            }
            "kylinbot" => {
                name = "kylinbot";
                path = "~/.kylinbot/workspace/AGENTS.md".to_owned();
                mode = "block";
            }
            _ => {
                name = "generic AGENTS.md";
                path = "~/AGENTS.md".to_owned();
                mode = "block";
            }
        }
        let state = if id == "opencode" {
            opencode_state()?
        } else {
            let state = check_state(expand(&path), mode)?;
            if mode == "ref-block"
                && state == "fresh"
                && check_state(entity_path()?, "reference")? != "fresh"
            {
                "stale"
            } else {
                state
            }
        };
        out.push(Target {
            id,
            name,
            path,
            mode,
            likely_installed: likely,
            state,
        });
    }
    Ok(out)
}

/// Current state: absent (no file) / fresh (up to date) / stale (injected but not current) / none (file exists, not injected).
fn check_state(path: std::path::PathBuf, mode: &str) -> Result<&'static str> {
    let instructions = instructions_md()?;
    Ok(match std::fs::read_to_string(&path) {
        Err(_) => "absent",
        Ok(text) => match mode {
            // Cover/reference: the target file IS the inject source; compare the whole text
            "inject" | "reference" => {
                if text.trim_end() == instructions.trim_end() {
                    "fresh"
                } else {
                    "stale"
                }
            }
            // Block: no markers → not injected; markers present → compare inner content
            _ => {
                if mode == "ref-block" && text.chars().count() > WORKBUDDY_MAX_CHARS {
                    return Ok("stale");
                }
                if text.contains(LEGACY_BLOCK_BEGIN) || text.contains(LEGACY_BLOCK_END) {
                    return Ok("stale");
                }
                let ref_md = wb_ref_md(&entity_abs().unwrap_or_default())?;
                let expected = match mode {
                    "ref-block" => ref_md.trim_end(),
                    _ => instructions.trim_end(),
                };
                match extract_block(&text) {
                    None => "none",
                    Some(inner) => {
                        if inner == expected.replace("\r\n", "\n") {
                            "fresh"
                        } else {
                            "stale"
                        }
                    }
                }
            }
        },
    })
}

fn extract_block(text: &str) -> Option<String> {
    let start = text.find(BLOCK_BEGIN)? + BLOCK_BEGIN.len();
    let end = text.find(BLOCK_END)?;
    if end < start {
        return None;
    }
    Some(text[start..end].trim().replace("\r\n", "\n"))
}

/// Inject one target. Returns whether a write actually happened.
pub fn inject_one(id: &str) -> Result<bool> {
    match id {
        "dsh" => write_cover(expand("~/.dsh/AGENTS.md")),
        "opencode" => inject_opencode(),
        "codex" => write_block(expand("~/.codex/AGENTS.md")),
        "claude" => write_block(expand("~/.claude/CLAUDE.md")),
        "codebuddy" => write_block(expand("~/.codebuddy/AGENTS.md")),
        "workbuddy" => inject_workbuddy(),
        "pi" => write_block(expand("~/.pi/AGENTS.md")),
        "zigcode" => write_block(expand("~/.zcode/AGENTS.md")),
        "deepseek" => write_block(expand("~/.deepseek/AGENTS.md")),
        "qwen" => write_block(expand("~/.qwen/AGENTS.md")),
        "doubao" => write_block(expand("~/.doubao/AGENTS.md")),
        "grok" => write_block(expand("~/.grok/AGENTS.md")),
        "kylinbot" => write_block(expand("~/.kylinbot/workspace/AGENTS.md")),
        "generic" => write_block(expand("~/AGENTS.md")),
        other => anyhow::bail!("unknown inject target: {other}"),
    }
}

/// Uninstall one target. Returns whether a write actually happened.
pub fn remove_one(id: &str) -> Result<bool> {
    match id {
        "dsh" => remove_cover(expand("~/.dsh/AGENTS.md")),
        "opencode" => remove_opencode(),
        "codex" => remove_block(expand("~/.codex/AGENTS.md")),
        "claude" => remove_block(expand("~/.claude/CLAUDE.md")),
        "codebuddy" => remove_block(expand("~/.codebuddy/AGENTS.md")),
        "workbuddy" => remove_block(expand("~/.workbuddy/MEMORY.md")),
        "pi" => remove_block(expand("~/.pi/AGENTS.md")),
        "zigcode" => remove_block(expand("~/.zcode/AGENTS.md")),
        "deepseek" => remove_block(expand("~/.deepseek/AGENTS.md")),
        "qwen" => remove_block(expand("~/.qwen/AGENTS.md")),
        "doubao" => remove_block(expand("~/.doubao/AGENTS.md")),
        "grok" => remove_block(expand("~/.grok/AGENTS.md")),
        "kylinbot" => remove_block(expand("~/.kylinbot/workspace/AGENTS.md")),
        "generic" => remove_block(expand("~/AGENTS.md")),
        other => anyhow::bail!("unknown inject target: {other}"),
    }
}

// ── cover mode (dsh): target file = inject source ──

fn write_cover(path: std::path::PathBuf) -> Result<bool> {
    let instructions = instructions_md()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Drop a symlink first — fs::write would follow it and overwrite the repo source
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        if meta.file_type().is_symlink() {
            std::fs::remove_file(&path)?;
        }
    }
    let changed = match std::fs::read_to_string(&path) {
        Ok(old) if old == instructions => false,
        _ => {
            std::fs::write(&path, instructions)?;
            true
        }
    };
    Ok(changed)
}

fn remove_cover(path: std::path::PathBuf) -> Result<bool> {
    let instructions = instructions_md()?;
    match std::fs::read_to_string(&path) {
        // Identical to the inject source → treat as a pure inject file and delete it
        Ok(text) if text == instructions || text == INSTRUCTIONS_MD => {
            std::fs::remove_file(&path)?;
            Ok(true)
        }
        // User-owned content → strip the block only
        Ok(_) => remove_block(path),
        Err(_) => Ok(false),
    }
}

// ── reference mode (opencode): entity file + opencode.json instructions ──
