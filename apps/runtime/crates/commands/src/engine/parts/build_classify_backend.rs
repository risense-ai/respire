/// Configuration and credentials stay under the selected profile's local gate.
pub fn build_classify_backend(
    api_base: Option<&str>,
    model: &str,
    dry_run: bool,
    ds: Option<&str>,
    backend: Option<&str>,
) -> Result<classify::Backend> {
    respire_app::credential_io::ensure_provider_credentials_available()?;
    use classify::Backend;
    // -- backend pick --
    // --backend (jev|ds) and --ds collapse: jev plus --ds is a conflict;
    // An explicit flag takes precedence over the stored default selection.
    let be = backend
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            if ds.is_none() {
                respire_app::keystore::load_classify_backend()
            } else {
                None
            }
        });
    let ds_sel: Option<String> = match (&be, ds) {
        (Some(b), _) if b == "jev" || b == "typesafe" => {
            if ds.is_some() {
                anyhow::bail!(
                    "--backend jev conflicts with --ds - jev is the default backend; drop --ds"
                );
            }
            None
        }
        (Some(b), Some(k)) if b == "ds" || b == "deepseek" => Some(k.to_owned()),
        (Some(b), None) if b == "ds" || b == "deepseek" => Some(String::new()),
        (Some(b), _) => anyhow::bail!("unknown backend \"{b}\" - choose: jev | ds"),
        (None, k) => k.map(|s| s.to_owned()),
    };
    let ds = ds_sel.as_deref();
    let backend = if let Some(dsk) = ds {
        // Endpoint first: key-ring slot is per host - official DS and b.ai keys are not interchangeable
        let base = match api_base {
            Some(b) => b.to_owned(),
            None => respire_app::keystore::load_ds_last_base()
                .unwrap_or_else(|| classify::DEFAULT_DS_BASE.to_owned()),
        };
        let host = respire_app::keystore::host_of(&base);
        let slot = format!("ds@{host}");
        // dry-run hits no network, so no key is required (same as the TypeSafe backend)
        let key = if dry_run {
            respire_app::keystore::load_classify_key(&slot).unwrap_or_default()
        } else if dsk.trim().is_empty() {
            // `--ds` with no value: load the stored key for this endpoint; else print a setup hint
            respire_app::keystore::load_classify_key(&slot).ok_or_else(|| {
                anyhow!(
                    "endpoint {host} has no key yet:\n  \
                     rsrs classify --ds <your-key>                      # run now and store in the system keyring\n  \
                     rsrs classify --ds <key> --api-base <endpoint>     # another endpoint (e.g. b.ai)\n  \
                     or  export DS_API_KEY=<your-key>                      # env bypass\n\
                     get a key: https://platform.deepseek.com/api_keys"
                )
            })?
        } else {
            let k = dsk.trim().to_owned();
            // dry-run is inspect-only - do not write the keyring or remember the endpoint (tests must not pollute real config)
            if !dry_run {
                match respire_app::keystore::save_classify_key(&slot, &k) {
                    Ok(()) => {
                        // remember the endpoint (path included) so the next `--ds` needs no extra flags
                        let _ = respire_app::keystore::save_ds_last_base(&base);
                        if !json_mode() {
                            eprintln!("KEY key stored in the system keyring (slot {slot}; endpoint {base} remembered)");
                        }
                    }
                    Err(e) => {
                        if !json_mode() {
                            eprintln!("WARN could not store the key in the keyring ({e}) - this run works; next time pass it again or set DS_API_KEY");
                        }
                    }
                }
            }
            k
        };
        let model = if model == classify::DEFAULT_MODEL {
            // model not given: prefer the last model used on this host (relays differ), else the official default
            respire_app::keystore::load_ds_model(&host)
                .unwrap_or_else(|| classify::DEFAULT_DS_MODEL.to_owned())
        } else {
            model.to_owned()
        };
        if !dry_run && model != classify::DEFAULT_MODEL {
            let _ = respire_app::keystore::save_ds_model(&host, &model);
        }
        Backend {
            name: "ds",
            key,
            model,
            endpoint: classify::ds_endpoint(&base),
            base_shown: base,
        }
    } else {
        let key = if dry_run {
            String::new()
        } else {
            classify_api_key()?
        };
        let base = api_base.unwrap_or(classify::DEFAULT_API_BASE);
        Backend {
            name: "typesafe",
            key,
            model: model.to_owned(),
            endpoint: base.to_owned(),
            base_shown: base.to_owned(),
        }
    };
    Ok(backend)
}

/// Compare decision inputs without read-only access/heat counters or derived indexes.
pub fn classify_source_revision(memories: &[respire_app::StoredMemory]) -> serde_json::Value {
    let mut ordered: Vec<&respire_app::StoredMemory> = memories.iter().collect();
    ordered.sort_by(|left, right| left.id.cmp(&right.id));
    serde_json::Value::Array(ordered.into_iter().map(|memory| serde_json::json!({
        "id": memory.id, "user": memory.user, "ciphertext": memory.ciphertext,
        "nonce": memory.nonce, "updated_at": memory.updated_at, "deleted": memory.deleted,
        "parent_id": memory.local_parent_id, "title": memory.local_title,
        "kind": memory.local_kind, "tags": memory.local_tags,
        "content_head": memory.local_content_head, "importance": memory.local_importance,
    })).collect())
}

/// JEV API key: TYPESAFE_API_KEY env first, then api_key in <data-dir>/classify.json.
pub fn classify_api_key() -> Result<String> {
    respire_app::credential_io::ensure_provider_credentials_available()?;
    if let Ok(k) = respire_app::env::var("TYPESAFE_API_KEY") {
        let k = k.trim().to_owned();
        if !k.is_empty() {
            return Ok(k);
        }
    }
    // The GUI "AI backend" dropdown stores the key in the system keyring (slot typesafe) - file and keyring both count
    if let Some(k) = respire_app::keystore::load_classify_key("typesafe") {
        return Ok(k);
    }
    let p = respire_app::service::data_dir().join("classify.json");
    if let Ok(text) = std::fs::read_to_string(&p) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(k) = v.get("api_key").and_then(|x| x.as_str()) {
                if !k.trim().is_empty() {
                    return Ok(k.trim().to_owned());
                }
            }
        }
    }
    anyhow::bail!(
        "JEV API key is not set: export TYPESAFE_API_KEY, or write {{\"api_key\":\"...\"}} into {} (apply: console.typesafe.ai/keys, waitlist).\nHint: without a TypeSafe account, use the DS backend - `rsrs classify --ds <key>`",
        p.display()
    )
}


pub fn run_audit(json_flag: bool) -> Result<()> {
    if json_flag {
        set_json_mode(true);
    }
    let store = build_local()?;
    let all = store.all(true)?;
    let by_id: std::collections::HashMap<&str, &respire_app::StoredMemory> = all
        .iter()
        .filter(|m| !m.deleted)
        .map(|m| (m.id.as_str(), m))
        .collect();
    let active: Vec<&respire_app::StoredMemory> = all.iter().filter(|m| !m.deleted).collect();

    // 1. orphans
    let orphans: Vec<&&respire_app::StoredMemory> = active
        .iter()
        .filter(|m| {
            !m.local_parent_id.is_empty() && !by_id.contains_key(m.local_parent_id.as_str())
        })
        .collect();
    // 2. illegal importance
    let bad_imp: Vec<&&respire_app::StoredMemory> = active
        .iter()
        .filter(|m| {
            !matches!(
                m.local_importance.as_str(),
                "important" | "normal" | "trivial"
            )
        })
        .collect();
    // 3. duplicate-title groups
    let mut by_title: std::collections::HashMap<&str, Vec<&str>> = std::collections::HashMap::new();
    for m in &active {
        let t = m.local_title.trim();
        if !t.is_empty() {
            by_title.entry(t).or_default().push(m.id.as_str());
        }
    }
    let same_title: Vec<(&str, usize)> = by_title
        .iter()
        .filter(|(_, v)| v.len() > 1)
        .map(|(k, v)| (*k, v.len()))
        .collect();
    // 4. Truncated titles: title is a prefix of the body AND the cut is **mid-word** (next char is not a separator).
    //    "title is a body prefix" alone false-positives - tree roots (CATALOG title + gist) and
    //    entries whose body starts with the title are both prefix-matches with a complete title.
    const TITLE_SEP: &str = "\u{ff1a}:\u{ff0c},\u{3002}.\u{3001}\u{ff1b};\u{ff1f}\u{ff01}!\u{ff09})\u{3011}\u{300d}\u{300f}\"\u{201c}\u{201d} \n\t--/|\u{ff08}(\u{3010}\u{300a}\u{300c}\u{300e}";
    let truncated: Vec<&&respire_app::StoredMemory> = active
        .iter()
        .filter(|m| {
            let t = m.local_title.trim();
            if t.is_empty() || t.chars().count() > 20 {
                return false;
            }
            let head = &m.local_content_head;
            if !head.starts_with(t) {
                return false;
            }
            if head.chars().count() <= t.chars().count() + 10 {
                return false;
            }
            match head[t.len()..].chars().next() {
                Some(c) => !TITLE_SEP.contains(c),
                None => false,
            }
        })
        .collect();
    // 5. deep chains
    let depth = |id: &str| -> usize {
        let mut d = 0usize;
        let mut cur = Some(id.to_owned());
        while let Some(c) = cur {
            if d > 30 {
                break;
            }
            if let Some(pp) = by_id.get(c.as_str()) {
                let pid = pp.local_parent_id.clone();
                if pid.is_empty() {
                    break;
                }
                cur = Some(pid);
                d += 1;
            } else {
                break;
            }
        }
        d
    };
    let max_depth = active.iter().map(|m| depth(&m.id)).max().unwrap_or(0);
    // 6. Procedure template gaps: skill entries should read as a manual, not an essay.
    //    Needs the body, so decryption is best-effort: a locked store reports `skipped` instead of failing audit.
    let skills: Vec<&&respire_app::StoredMemory> = active
        .iter()
        .filter(|m| m.local_kind == "skill")
        .collect();
    // 7. Hot but untyped: frequently recalled, not a skill, and reads like a manual.
    //    Advisory for tidy only - the AI decides whether it is worth an upgrade.
    let hot_candidates: Vec<&&respire_app::StoredMemory> = active
        .iter()
        .filter(|m| {
            m.local_kind != "skill" && m.local_recall_count >= skill::HOT_RECALL_THRESHOLD
        })
        .collect();
    let bodies = if skills.is_empty() && hot_candidates.is_empty() {
        None
    } else {
        build_session().ok()
    };
    let mut skill_gaps: Vec<(String, String, Vec<&'static str>)> = Vec::new();
    let mut hot_untyped: Vec<(String, String, i64)> = Vec::new();
    if let Some(session) = &bodies {
        for m in &skills {
            if let Ok(full) = MemoryEngine::open(session, m) {
                let missing = skill::missing_sections(&full.content);
                if !missing.is_empty() {
                    skill_gaps.push((m.id.clone(), m.local_title.clone(), missing));
                }
            }
        }
        for m in &hot_candidates {
            if let Ok(full) = MemoryEngine::open(session, m) {
                if skill::looks_operational(&full.content) {
                    hot_untyped.push((
                        m.id.clone(),
                        m.local_title.clone(),
                        m.local_recall_count,
                    ));
                }
            }
        }
    }
    let skill_gaps_skipped = !skills.is_empty() && bodies.is_none();
    let hot_untyped_skipped = !hot_candidates.is_empty() && bodies.is_none();
    // 8. Procedures whose latest three runs all failed: doc says "keep it" but it is broken.
    let failing_skills = store.failing_skills(3)?;

    // M2 (2026-09-20 audit): previously only the local --json flag was honored;
    // RSRS_JSON=1 (what the GUI bridge sets) still printed prose, breaking cli-api.md "equivalent" output.
    if json_mode() || json_flag {
        let summary = serde_json::json!({
            "total_active": active.len(),
            "orphans": orphans.len(), "invalid_importance": bad_imp.len(),
            "same_title_groups": same_title.len(), "truncated_titles": truncated.len(),
            "max_depth": max_depth,
            "skill_template_gaps": skill_gaps.len(),
            "skill_template_gaps_skipped": skill_gaps_skipped,
            "failing_skills": failing_skills.len(),
            "hot_untyped": hot_untyped.len(),
            "hot_untyped_skipped": hot_untyped_skipped,
        });
        let mut result = ResultEnvelope::new(
            "audit",
            if orphans.is_empty()
                && bad_imp.is_empty()
                && truncated.is_empty()
                && skill_gaps.is_empty()
                && failing_skills.is_empty()
            {
                OutputStatus::Ok
            } else {
                OutputStatus::Warn
            },
            summary,
            Vec::new(),
        );
        result.actions.push("audit --json".into());
        let details = serde_json::json!({
            "orphans": orphans.iter().map(|m| serde_json::json!({"id": m.id, "title": m.local_title, "missing_parent": m.local_parent_id})).collect::<Vec<_>>(),
            "invalid_importance": bad_imp.iter().map(|m| serde_json::json!({"id": m.id, "title": m.local_title, "importance": m.local_importance})).collect::<Vec<_>>(),
            "same_title_groups": same_title.iter().map(|(t, n)| serde_json::json!({"title": t, "count": n})).collect::<Vec<_>>(),
            "truncated_titles": truncated.iter().map(|m| serde_json::json!({"id": m.id, "title": m.local_title})).collect::<Vec<_>>(),
            "skill_template_missing": skill_gaps.iter().map(|(id, title, missing)| serde_json::json!({"id": id, "title": title, "missing": missing})).collect::<Vec<_>>(),
            "failing_skills": failing_skills.iter().map(|(id, streak)| serde_json::json!({"id": id, "fail_streak": streak})).collect::<Vec<_>>(),
            "hot_untyped": hot_untyped.iter().map(|(id, title, count)| serde_json::json!({"id": id, "title": title, "recall_count": count})).collect::<Vec<_>>(),
        });
        result.details = details;
        emit_result(result)?;
        return Ok(());
    }
    let status = if orphans.is_empty()
        && bad_imp.is_empty()
        && truncated.is_empty()
        && skill_gaps.is_empty()
        && failing_skills.is_empty()
    {
        OutputStatus::Ok
    } else {
        OutputStatus::Warn
    };
    let mut items = vec![
        OutputItem::new("active", OutputStatus::Ok, active.len().to_string()),
        OutputItem::new(
            "orphans",
            if orphans.is_empty() {
                OutputStatus::Ok
            } else {
                OutputStatus::Warn
            },
            orphans.len().to_string(),
        ),
        OutputItem::new(
            "illegal importance",
            if bad_imp.is_empty() {
                OutputStatus::Ok
            } else {
                OutputStatus::Warn
            },
            bad_imp.len().to_string(),
        ),
        OutputItem::new(
            "duplicate titles",
            if same_title.is_empty() {
                OutputStatus::Ok
            } else {
                OutputStatus::Warn
            },
            same_title.len().to_string(),
        ),
        OutputItem::new(
            "truncated titles",
            if truncated.is_empty() {
                OutputStatus::Ok
            } else {
                OutputStatus::Warn
            },
            truncated.len().to_string(),
        ),
        OutputItem::new("max depth", OutputStatus::Ok, max_depth.to_string()),
        OutputItem::new(
            "skill template gaps",
            if skill_gaps.is_empty() {
                OutputStatus::Ok
            } else {
                OutputStatus::Warn
            },
            if skill_gaps_skipped {
                "skipped (locked)".to_owned()
            } else {
                skill_gaps.len().to_string()
            },
        ),
        OutputItem::new(
            "failing skills",
            if failing_skills.is_empty() {
                OutputStatus::Ok
            } else {
                OutputStatus::Warn
            },
            failing_skills.len().to_string(),
        ),
        OutputItem::new(
            "hot untyped",
            OutputStatus::Ok,
            if hot_untyped_skipped {
                "skipped (locked)".to_owned()
            } else {
                hot_untyped.len().to_string()
            },
        ),
    ];
    items.extend(orphans.iter().map(|m| {
        OutputItem::new(
            "orphan",
            OutputStatus::Warn,
            format!(
                "{} parent {}",
                respire_app::service::short_id(&m.id),
                m.local_parent_id
            ),
        )
        .action("resort")
    }));
    items.extend(bad_imp.iter().map(|m| {
        OutputItem::new(
            "illegal importance",
            OutputStatus::Warn,
            format!(
                "{} {}",
                respire_app::service::short_id(&m.id),
                m.local_importance
            ),
        )
    }));
    items.extend(same_title.iter().map(|(t, n)| {
        OutputItem::new(
            "duplicate title",
            OutputStatus::Warn,
            format!("{} count {}", t, n),
        )
    }));
    items.extend(skill_gaps.iter().map(|(id, title, missing)| {
        OutputItem::new(
            "skill template gap",
            OutputStatus::Warn,
            format!(
                "{} {} missing {}",
                respire_app::service::short_id(id),
                title,
                missing.join("/")
            ),
        )
        .action("update")
    }));
    items.extend(failing_skills.iter().map(|(id, streak)| {
        OutputItem::new(
            "failing skill",
            OutputStatus::Warn,
            format!("{} {} consecutive fail", respire_app::service::short_id(id), streak),
        )
        .action("run-log")
    }));
    items.extend(hot_untyped.iter().map(|(id, title, count)| {
        OutputItem::new(
            "hot untyped",
            OutputStatus::Ok,
            format!(
                "{} {} {count} hits",
                respire_app::service::short_id(id),
                title
            ),
        )
        .action("review: upgrade to skill only with evidence (run-log ok or user sign-off); keeping it as-is is a valid verdict")
    }));
    let mut result = ResultEnvelope::new(
        "audit",
        status,
        serde_json::json!({"total_active":active.len(),"orphans":orphans.len(),"invalid_importance":bad_imp.len(),"same_title_groups":same_title.len(),"truncated_titles":truncated.len(),"max_depth":max_depth,"skill_template_gaps":skill_gaps.len(),"skill_template_gaps_skipped":skill_gaps_skipped,"failing_skills":failing_skills.len()}),
        items,
    );
    result.actions.push("audit --json".into());
    emit_result(result)?;
    Ok(())
}

pub fn run_repack() -> Result<()> {
    let session = build_session()?;
    let store = build_local()?;
    let fixed = store.write_transaction(|| {
        let mut pending = Vec::new();
        for stored in store.all(true)? {
            let payload_parent = MemoryEngine::payload_parent(&session, &stored)?;
            if payload_parent != stored.local_parent_id {
                respire_app::metadata_gate::ensure_write(&store, &MemoryEngine::open(&session, &stored)?)?;
                pending.push(stored);
            }
        }
        for stored in &pending {
            let out = MemoryEngine::reseal_parent(
                &session,
                stored,
                &stored.local_parent_id,
                &stored.updated_at,
            )?;
            store.put_inner(&out, true)?;
        }
        Ok(pending.len())
    })?;
    auto_sync(&session, &store);
    emit_result(ResultEnvelope::new(
        "repack",
        OutputStatus::Ok,
        serde_json::json!({"fixed":fixed}),
        vec![OutputItem::new(
            "resealed",
            OutputStatus::Ok,
            fixed.to_string(),
        )],
    ))
}


pub fn emit_diary_fragments(command: &str, fragments: respire_app::diary::FragmentResult) -> Result<()> {
    if !json_mode() {
        println!("{}", serde_json::to_string(&fragments)?);
        return Ok(());
    }
    let mut result = ResultEnvelope::new(command, OutputStatus::Ok,
        serde_json::json!({"count":fragments.items.len(), "semantic_ready":fragments.semantic_ready,
            "pending_fragments":fragments.pending_fragments,"truncated":fragments.truncated,
            "max_chars":fragments.max_chars,"offset_unit":fragments.offset_unit}), Vec::new());
    result.details = serde_json::to_value(fragments)?;
    emit_result(result)
}

