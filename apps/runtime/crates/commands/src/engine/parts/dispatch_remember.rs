{
            let explicit_kind = r#type.is_some();
            let r#type = r#type.unwrap_or_else(|| "context".to_owned());
            let metadata_patch = metadata.patch();
            let requested_kind = Kind::try_from_str(&r#type.trim().to_lowercase())
                .ok_or_else(|| anyhow!("unsupported_metadata: unknown memory kind"))?;
            let new_kind = matches!(requested_kind, Kind::Hypothesis | Kind::Reflection | Kind::SelfNote);
            if metadata_patch.is_some() || new_kind { respire_app::metadata_gate::require_support()?; }
            anyhow::ensure!((supersedes.is_none() && see_also.is_empty()) || importance == "important", "associations require important memory");
            // importance enum check: an illegal value (e.g. a kind name "task" by mistake) cannot be filtered by audit/UI after write
            // 2026-09-19 two-tier: normal is retired - new writes are important/trivial only (stock normal still reads)
            if !matches!(importance.as_str(), "important" | "trivial") {
                anyhow::bail!("importance must be important/trivial (normal is retired; normal is logged as diary trivia), got \"{}\"", importance);
            }
            // Procedure template: a skill is a manual, not an essay. Warn at write time; `rsrs audit` lists the gaps.
            if r#type == "skill" && importance == "important" {
                let missing = skill::missing_sections(&content);
                if !missing.is_empty() {
                    eprintln!(
                        "WARN skill template missing {} - add {} (see `rsrs audit`)",
                        missing.join("/"),
                        skill::SECTIONS.join("/")
                    );
                }
            }
            let (session, profile_owner) = build_session_with_owner()?;
            let store = build_local()?;
            if metadata_patch.is_some() || new_kind {
                anyhow::ensure!(store.memory_metadata_v1_enabled()?, "memory_metadata_v1_disabled");
            }
            let stamp = now_stamp();
            let mut id = Uuid::new_v4().to_string();
            // Auto-fill device tag: explicit --computer first, else hostname/platform - so recall across devices can tell whose paths they are
            let computer = if computer.trim().is_empty() {
                respire_app::service::device_tag()
            } else {
                computer
            };

            // -- Trivia = diary, two modes (agent.json diary_mode; the AI reads it from the inject source):
            //    concise (default) = untitled events append to one daily track;
            //    an explicit title names an independent entry in either mode.
            //    verbose = one diary entry each (old behavior) --
            let (mut content, mut title) = (content, title);
            if importance == "trivial" && merge_ids.is_none() && metadata_patch.is_none() && !new_kind {
                if respire_app::service::diary_mode() != "verbose" && title.trim().is_empty() {
                    // Stable identity survives retitling; dated titles remain a fallback
                    // for legacy tracks created before deterministic IDs were used.
                    let now_local = chrono::Local::now();
                    let day = now_local.format("%Y-%m-%d").to_string();
                    let track_title = format!("\u{6d3b}\u{52a8}\u{8f68}\u{8ff9} {day}");
                    let line = format!("\u{3010}{}\u{3011}{}", now_local.format("%H:%M"), content.trim());
                    let all = store.all(false)?;
                    let user = current_user();
                    let track_id = uuid::Uuid::new_v5(
                        &uuid::Uuid::NAMESPACE_URL,
                        format!("activity-track/{user}/{day}").as_bytes(),
                    )
                    .to_string();
                    let tracks = select_diary_tracks(&all, &track_id, &track_title, |memory| {
                        let author = MemoryEngine::open(&session, memory)?.user;
                        Ok(author == user || profile_owner.as_deref() == Some(author.as_str()))
                    })?;
                    if !tracks.is_empty() {
                        // Prefer the stable ID; otherwise keep the earliest legacy track.
                        // The source-checked transaction still protects every merged row.
                        let main = tracks[0];
                        let mut merged = String::new();
                        for t in &tracks {
                            let full = MemoryEngine::open(&session, t)?;
                            for row in full.content.lines() {
                                if row.trim() == format!("\u{3010}\u{6d3b}\u{52a8}\u{8f68}\u{8ff9}\u{3011}{day}")
                                    || row.trim().is_empty()
                                {
                                    continue;
                                }
                                merged.push_str(row.trim_end());
                                merged.push('\n');
                            }

                        }
                        merged.push_str(&line);
                        // Appending must preserve a user's title and other metadata.
                        let mut e = MemoryEngine::open(&session, main)?;
                        e.content = merged;
                        e.updated_at = store.edit_stamp(&main.id)?;
                        e.modified_by = respire_app::service::device_tag();
                        let metadata_receipt = metadata::prepare(&session, &store, &e)?;
                        let stored = MemoryEngine::seal(&session, embedder!(), &e, &main.user)?;
                        anyhow::ensure!(commit_prepared_write(short_write, write_generation, || store.put_diary_checked(&stored, &main.ciphertext, &tracks[1..]))?, "diary was not saved");
                        crate::hooks::record(
                            crate::hooks::HookEvent::PostRemember,
                            crate::hooks::remember_done_payload(&stored.id, &e.title, &e.importance),
                        );
                        // Return the same decrypted public entry that show exposes,
                        // never the sealed StoredMemory persistence representation.
                        let mut result = ResultEnvelope::new(
                            "remember",
                            OutputStatus::Ok,
                            serde_json::json!({
                                "action": "diary_appended",
                                "index_state": if stored.local_artifact.is_empty() { "pending" } else { "ready" },
                                "id": stored.id,
                                "title": e.title,
                            }),
                            vec![OutputItem::new(
                                "write",
                                OutputStatus::Ok,
                                format!("id={}", stored.id),
                            )],
                        );
                        metadata::add_receipt(&mut result, metadata_receipt);
                        result.details = serde_json::to_value(&e)?;
                        emit_result(result)?;
                        return Ok(());
                    }
                    // No track for today: create with a deterministic id (same id across devices that day; sync converges), then the generic create path
                    id = track_id;
                    content = format!("\u{3010}\u{6d3b}\u{52a8}\u{8f68}\u{8ff9}\u{3011}{day}\n{line}");
                    title = track_title;
                } else {
                    content = respire_app::taxonomy::diary_content(&content);
                }
            }
            // -- Untitled fallback: first 20 chars of the first body line (tree view must not show a blank title; an explicit --title is untouched) --
            if title.trim().is_empty() {
                title = respire_app::service::derive_title(&content);
            }

            // -- Judge-then-store (dedup first, then choose): unless --force/--merge-ids/--parent (explicit attach is intent)
            //    run an internal recall of similar candidates; diary trivia skips dedup (daily log is not a duplicate event; it goes on the time chain) --
            if !force && merge_ids.is_none() && supersedes.is_none() && see_also.is_empty() && parent.is_empty() && importance != "trivial" {
                let (candidates_all, pending, lease) = respire_app::resident::recall_catalog(&session, &store)?;
                let _resident = lease.enter();
                if pending > 0 { rpc::kick_index(); }
                let q = MemoryQuery::new(&content).limit(5);
                let candidates:respire_app::core_sdk::RememberCandidates=respire_app::core_sdk::execute("remember_candidates",serde_json::json!({
                    "model":embedder!().model_name(),"snapshots":respire_app::memory::engine::snapshots(&session,&candidates_all)?,
                    "query":q,"query_settings":respire_app::feedback::query_settings(&store,&q,&candidates_all)?
                }))?;
                let merge_cand = candidates.merge;
                let parent_cand = candidates.parent;

                if !merge_cand.is_empty() {
                    let mut result = ResultEnvelope::new(
                        "remember",
                        OutputStatus::Pending,
                        serde_json::json!({
                            "state": "candidates",
                            "merge_count": merge_cand.len(),
                            "parent_count": parent_cand.len(),
                            "written": false,
                        }),
                        merge_cand
                            .iter()
                            .take(3)
                            .map(|(score, entry)| {
                                OutputItem::new(
                                    respire_app::service::short_id(&entry.id),
                                    OutputStatus::Pending,
                                    format!(
                                        "similarity={:.0}% title={}",
                                        score * 100.0,
                                        entry.title
                                    ),
                                )
                                .action("merge or attach")
                            })
                            .collect(),
                    );
                    let ids = merge_cand
                        .iter()
                        .take(3)
                        .map(|(_, e)| e.id.clone())
                        .collect::<Vec<_>>()
                        .join(",");
                    result.actions.push(format!(
                        "rsrs remember \"<combined content>\" --merge-ids \"{ids}\""
                    ));
                    if !parent_cand.is_empty() {
                        result
                            .actions
                            .push("rsrs remember \"<content>\" --parent <candidate id>".into());
                    }
                    result
                        .actions
                        .push("Read candidate bodies; choose update, merge or a real --parent, or explicitly defer. Do not bypass candidate review with bare --force.".into());
                    result.actions.extend(respire_app::service::write_decision_actions().iter().map(|action| (*action).to_owned()));
                    emit_result(result)?;
                    return Ok(());
                }
                if !parent_cand.is_empty() {
                    let mut result = ResultEnvelope::new(
                        "remember",
                        OutputStatus::Pending,
                        serde_json::json!({
                            "state": "parent_candidates",
                            "written": false,
                            "count": parent_cand.len(),
                        }),
                        parent_cand
                            .iter()
                            .take(3)
                            .map(|(score, entry)| {
                                OutputItem::new(
                                    respire_app::service::short_id(&entry.id),
                                    OutputStatus::Pending,
                                    format!(
                                        "similarity={:.0}% title={}",
                                        score * 100.0,
                                        entry.title
                                    ),
                                )
                                .action("attach with --parent")
                            })
                            .collect(),
                    );
                    result
                        .actions
                        .push("rsrs remember \"<content>\" --parent <candidate id>".into());
                    result
                        .actions
                        .push("Read candidate bodies; choose update, merge or a real --parent, or explicitly defer. Do not bypass candidate review with bare --force.".into());
                    result.actions.extend(respire_app::service::write_decision_actions().iter().map(|action| (*action).to_owned()));
                    emit_result(result)?;
                    return Ok(());
                }
            }

            // -- --merge-ids: merge path - tombstone the listed old memories (children reattach to their cause), store a combined new entry (inherits the first cause chain) --
            if let Some(ids) = merge_ids {
                let to_merge: Vec<String> = ids
                    .split(',')
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .collect();
                let kind = if explicit_kind {
                    requested_kind
                } else {
                    let all = store.all(false)?;
                    let first = to_merge.first().ok_or_else(|| anyhow!("merge needs at least one live entry"))?;
                    let full = respire_app::service::resolve_prefix(&all, first)?;
                    let row = all.iter().find(|row| row.id == full).ok_or_else(|| anyhow!("missing merge source"))?;
                    MemoryEngine::open(&session, row)?.kind
                };
                let mut entry = respire_app::MemoryEntry {
                    metadata: Default::default(),
                    unknown_fields: Default::default(),
                    supersedes: String::new(),
                    superseded_by: String::new(),
                    see_also: Vec::new(),
                    id,
                    kind,
                    tags: tags
                        .split(',')
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                        .map(ToOwned::to_owned)
                        .collect(),
                    title,
                    content,
                    user: current_user(),
                    computer,
                    project,
                    created_at: stamp.clone(),
                    updated_at: stamp,
                    emotion,
                    parent_id: String::new(),
                    importance,
                    device: respire_app::service::device_tag(),
                    modified_by: respire_app::service::device_tag(),
                };
                let (merged_full, record_revision) = respire_app::service::merge_entries_with_metadata_revision(
                    &session, &store, embedder!(), &mut entry, &to_merge, &parent, true,
                    metadata_patch.as_ref(),
                )?;
                let metadata_receipt = record_revision.as_deref().map(|revision| metadata::receipt(&entry, revision))
                    .unwrap_or_else(|| serde_json::json!({}));
                let dropped = merged_full.len();
                report_orphans(&store, "merge");
                crate::hooks::record(
                    crate::hooks::HookEvent::PostRemember,
                    crate::hooks::remember_done_payload(
                        &entry.id,
                        &entry.title,
                        &entry.importance,
                    ),
                );
                // Adoption: a merge adopts the old entries (they were the preferred recall candidates)
                for mid in &merged_full {
                    let _ = store.mark_adopted(mid);
                }
                let hint = note_write_maintenance();
                let mut result = ResultEnvelope::new(
                    "remember",
                    OutputStatus::Ok,
                    serde_json::json!({
                        "action": "merged",
                        "dropped": dropped,
                        "id": entry.id,
                        "parent": entry.parent_id,
                        "maintenance_hint": hint,
                    }),
                    vec![OutputItem::new(
                        "merge",
                        OutputStatus::Ok,
                        format!("dropped={dropped} id={}", entry.id),
                    )],
                );
                if let Some(h) = hint {
                    result.actions.push(h);
                }
                metadata::add_receipt(&mut result, metadata_receipt);
                result.details = serde_json::to_value(&entry)?;
                emit_result(result)?;
                schedule_autosync(&session, &store);
                return Ok(());
            }

            // -- Causal attach: --parent is the cause (full id first; unique 8-char prefix also works) --
            // Unresolvable -> warn and actually clear (own cause), so we never warn "stored independently" then print "attached".
            let mut issues: Vec<String> = Vec::new();
            let parent_id = if !parent.is_empty() {
                let all = store.all(true)?;
                match respire_app::service::resolve_prefix(&all, &parent) {
                    Ok(full) => Some(full),
                    Err(e) => {
                        issues.push(format!(
                            "parent (cause) cannot be resolved ({e}); stored independently"
                        ));
                        None
                    }
                }
            } else {
                None
            };
            let parent_id = parent_id.unwrap_or_default();
            let mut entry = respire_app::MemoryEntry {
                metadata: Default::default(),
                unknown_fields: Default::default(),
                supersedes: String::new(),
                superseded_by: String::new(),
                see_also: Vec::new(),
                id,
                kind: Kind::try_from_str(&r#type.trim().to_lowercase()).ok_or_else(|| anyhow!("unsupported_metadata: unknown memory kind"))?,
                tags: tags
                    .split(',')
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(ToOwned::to_owned)
                    .collect(),
                title,
                content,
                user: current_user(),
                computer,
                project,
                created_at: stamp.clone(),
                updated_at: stamp,
                emotion,
                parent_id: parent_id.clone(),
                importance: importance.clone(),
                device: respire_app::service::device_tag(),
                modified_by: respire_app::service::device_tag(),
            };
            metadata.apply(&mut entry);
            let metadata_receipt = metadata::prepare(&session, &store, &entry)?;
            let skill_missing = if entry.kind == Kind::Skill && entry.importance == "important" {
                let missing = skill::missing_sections(&entry.content);
                if !missing.is_empty() {
                    issues.push(format!(
                        "skill template missing {}; add {}",
                        missing.join("/"),
                        skill::SECTIONS.join("/")
                    ));
                }
                missing
            } else {
                Vec::new()
            };
            let stored = if supersedes.is_some() || !see_also.is_empty() {
                respire_app::service::store_related(&session,&store,embedder!(),&mut entry,supersedes.as_deref(),&see_also)?
            } else {
                let sealed = MemoryEngine::seal(&session, embedder!(), &entry, &entry.user)?;
                anyhow::ensure!(commit_prepared_write(short_write, write_generation, || store.put_checked(&sealed, None))?, "memory was not saved");
                sealed
            };
            crate::hooks::record(
                crate::hooks::HookEvent::PostRemember,
                crate::hooks::remember_done_payload(&stored.id, &entry.title, &entry.importance),
            );
            // Adoption: explicit attach (--parent) adopts that cause node; --force with no hang is not adoption
            if !parent_id.is_empty() {
                let _ = store.mark_adopted(&parent_id);
            }
            let hint = note_write_maintenance();
            let status = if issues.is_empty() {
                OutputStatus::Ok
            } else {
                OutputStatus::Warn
            };
            let mut result = ResultEnvelope::new(
                "remember",
                status,
                serde_json::json!({
                    "action": "created",
                    "index_state": if stored.local_artifact.is_empty() { "pending" } else { "ready" },
                    "id": entry.id,
                    "parent": parent_id,
                    "maintenance_hint": hint,
                    "skill_template_missing": skill_missing,
                }),
                vec![OutputItem::new(
                    "write",
                    OutputStatus::Ok,
                    format!("id={}", entry.id),
                )],
            );
            if let Some(h) = hint {
                result.actions.push(h);
            }
            result.errors.extend(issues);
            metadata::add_receipt(&mut result, metadata_receipt);
            result.details = serde_json::to_value(&entry)?;
            emit_result(result)?;
            schedule_autosync(&session, &store);
}
