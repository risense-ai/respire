/// Export a subtree as a share payload (plaintext, for someone else to import).
///
/// Walk order = root then descendants (parent before child), so the payload is self-contained: the importer need not topo-sort.
/// Skip a bad row (corrupt ciphertext) rather than abort — half a tree is better than none.
///
/// Do not load BGE: share export only decrypts, never searches; loading the model for seconds is waste (2026-09-21).
pub fn export_subtree_payload(root_prefix: &str) -> Result<(crate::share::SharePayload, String)> {
    let keys = auth::load_local_session()
        .map_err(|e| anyhow!("session not unlocked ({e}) \u{2014} register/login or keygen first"))?;
    let store = open_store()?;
    let all = store.all(false)?;
    let root_id = resolve_prefix(&all, root_prefix)?;
    let members = subtree_members(&all, &root_id);
    // Adjacency list + preorder walk (parent before child)
    let mut children_of: HashMap<&str, Vec<&str>> = HashMap::new();
    for m in &all {
        if !m.deleted && !m.local_parent_id.is_empty() {
            children_of
                .entry(m.local_parent_id.as_str())
                .or_default()
                .push(m.id.as_str());
        }
    }
    let mut order: Vec<String> = Vec::with_capacity(members.len());
    let mut stack = vec![root_id.clone()];
    while let Some(cur) = stack.pop() {
        if !members.contains(&cur) {
            continue;
        }
        order.push(cur.clone());
        if let Some(kids) = children_of.get(cur.as_str()) {
            // Push reversed so pop yields original order (same as subtree_ids)
            stack.extend(kids.iter().rev().map(|s| s.to_string()));
        }
    }
    let by_id: HashMap<&str, &crate::memory::model::StoredMemory> =
        all.iter().map(|m| (m.id.as_str(), m)).collect();
    let lookup = |id: &str| -> Option<MemoryEntry> {
        by_id
            .get(id)
            .and_then(|s| MemoryEngine::open(&keys, s).ok())
            .map(|entry| MemoryEntry {
                metadata: entry.metadata,
                unknown_fields: entry.unknown_fields,
                supersedes: entry.supersedes,
                superseded_by: entry.superseded_by,
                see_also: entry.see_also,
                id: entry.id,
                kind: entry.kind,
                tags: entry.tags,
                title: entry.title,
                content: entry.content,
                user: entry.user,
                computer: entry.computer,
                project: entry.project,
                created_at: entry.created_at,
                updated_at: entry.updated_at,
                emotion: entry.emotion,
                parent_id: entry.parent_id,
                importance: entry.importance,
                device: entry.device,
                modified_by: entry.modified_by,
            })
    };
    let root_title = by_id
        .get(root_id.as_str())
        .map(|m| {
            if m.local_title.is_empty() {
                short_id(&m.id)
            } else {
                m.local_title.clone()
            }
        })
        .unwrap_or_else(|| short_id(&root_id));
    let items = crate::share::items_from(&root_id, &order, &lookup, &members);
    if items.is_empty() {
        anyhow::bail!("no shareable memories in this subtree ({root_id}) \u{2014} empty subtree or entries cannot be decrypted");
    }
    let payload = crate::share::SharePayload {
        v: 1,
        kind: "subtree".to_owned(),
        root_title,
        created_at: now_stamp(),
        device: device_tag(),
        source_user: current_user(),
        items,
    };
    let encoded = crate::share::encode(&payload)?;
    Ok((payload, encoded))
}

/// Read a share file → payload.
pub fn read_share_file(path: &std::path::Path) -> Result<crate::share::SharePayload> {
    let text = crate::input_error::read_file(path, "share file")?;
    crate::share::decode(&text)
}

/// For the AI to pick an attach point: payload vs nearby local entries (read-only, no write).
///
/// Return Core-proposed candidates and a suggested root when no candidate exists.
///
/// Rule from "AI picks the attach point": without --parent, only emit the bill; the attach decision stays with the AI (model lives in main).
pub fn share_candidates_with(
    payload: &crate::share::SharePayload,
    embedder: &BgeEmbedder,
) -> Result<serde_json::Value> {
    let keys = auth::load_local_session()
        .map_err(|e| anyhow!("session not unlocked ({e}) \u{2014} register/login or keygen first"))?;
    let store = open_store()?;
    let all = store.all(false)?;
    let query = crate::share::mount_query(payload);
    require_candidates_ready(&store, &all)?;
    let report = candidate_report(&keys, embedder, &all, &query)?;
    let mut out: Vec<serde_json::Value> = Vec::new();
    for (bucket, verdict) in [
        (&report.merge, "same-topic/merge"),
        (&report.parent, "attach-under"),
    ] {
        for s in bucket {
            out.push(serde_json::json!({
                "id": s.entry.id,
                "short_id": short_id(&s.entry.id),
                "title": s.entry.title,
                "score": (s.score * 100.0).round() / 100.0,
                "suggest": verdict,
            }));
        }
    }
    out.sort_by(|a, b| {
        b["score"]
            .as_f64()
            .partial_cmp(&a["score"].as_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(serde_json::json!({
        "root_title": payload.root_title,
        "count": payload.items.len(),
        "chars": payload.total_chars(),
        "from": payload.device,
        "candidates": out,
        "hint": "if a close candidate exists \u{2192} share-import <file> --parent <short_id> --go to attach under it; \
                 if none \u{2192} share-import <file> --go creates a same-titled root",
    }))
}

/// Write a share payload: always mint new ids, rebuild the parent chain, attach the root at `attach_parent` (empty = own root).
///
/// Before writing, ask Core to identify same-topic conflicts and refuse an implicit sibling write.
/// cannot rely on the AI reading the bill. Force needs explicit `force` (CLI `--force`).
pub fn import_share_payload(
    payload: &crate::share::SharePayload,
    attach_parent: &str,
    force: bool,
    embedder: &BgeEmbedder,
) -> Result<ImportReport> {
    let keys = auth::load_local_session()
        .map_err(|e| anyhow!("session not unlocked ({e}) \u{2014} register/login or keygen first"))?;
    let store = open_store()?;
    // Resolve the attach point first (prefix ok); fail loudly, do not silently become a root
    let parent = if attach_parent.trim().is_empty() {
        String::new()
    } else {
        let all = store.all(true)?;
        resolve_prefix(&all, attach_parent.trim())?
    };
    // Ask Core to identify conflicts before importing.
    // Block only when attach is not explicit: `--parent` means "I know where"; same-topic is then expected
    // (e.g. hanging the shared subtree under a same-topic local entry as a supplement) — same as attach skipping store-judge.
    if !force && parent.is_empty() {
        let all = store.all(false)?;
        require_candidates_ready(&store, &all)?;
        let mut conflicts: Vec<serde_json::Value> = Vec::new();
        for it in &payload.items {
            let q = if it.content.trim().is_empty() {
                it.title.clone()
            } else {
                format!("{}\n{}", it.title, it.content)
            };
            if q.trim().is_empty() {
                continue;
            }
            let report = candidate_report(&keys, embedder, &all, &q)?;
            for s in &report.merge {
                conflicts.push(serde_json::json!({
                    "incoming": it.title,
                    "existing_id": s.entry.id,
                    "existing_short": short_id(&s.entry.id),
                    "existing_title": s.entry.title,
                    "score": (s.score * 100.0).round() / 100.0,
                }));
            }
        }
        if !conflicts.is_empty() {
            // Dedup (one existing entry hit by several payload items)
            let mut seen = std::collections::HashSet::new();
            conflicts.retain(|c| {
                let k = format!(
                    "{}|{}",
                    c["incoming"].as_str().unwrap_or(""),
                    c["existing_short"].as_str().unwrap_or("")
                );
                seen.insert(k)
            });
            let mut lines: Vec<String> = conflicts
                .iter()
                .map(|c| {
                    format!(
                        "  {:.2}  `{}` \u{2248} existing [{}] {}",
                        c["score"].as_f64().unwrap_or(0.0),
                        c["incoming"].as_str().unwrap_or(""),
                        c["existing_short"].as_str().unwrap_or(""),
                        c["existing_title"].as_str().unwrap_or("")
                    )
                })
                .collect();
            lines.sort();
            anyhow::bail!(
                "import blocked: {} payload items match existing entries; writing them would create sibling duplicates.\n{}\n\
                 pick one explicitly:\n\
                 \x20 1) attach under the same-topic entry: share-import <file> --parent <short-id> --go\n\
                 \x20 2) merge into one (update > merge > attach > store):\n\
                 \x20      share-import <file> --go --force, then remember \"<merged>\" --merge-ids \"<old-id>,<new-id>\"\n\
                 \x20 3) confirm it is a new fact and should sit as a sibling: share-import <file> --go --force",
                conflicts.len(),
                lines.join("\n")
            );
        }
    }
    let new_ids: Vec<String> = payload
        .items
        .iter()
        .map(|_| uuid::Uuid::new_v4().to_string())
        .collect();
    let (parents, reattached, orphaned) =
        crate::share::remap_parents(&payload.items, &new_ids, &parent)?;
    let mut entries = Vec::with_capacity(payload.items.len());
    let mut skipped = 0usize;
    for (i, it) in payload.items.iter().enumerate() {
        if it.content.trim().is_empty() {
            skipped += 1;
            continue;
        }
        it.metadata.validate()?;
        crate::memory::model::metadata::validate_extra_fields(&it.unknown_fields)?;
        let kind = it.kind.trim().to_lowercase();
        let parsed_kind = Kind::try_from_str(&kind).ok_or_else(|| anyhow!("unsupported_metadata: unknown imported memory kind"))?;
        let importance = match it.importance.trim() {
            "important" => "important",
            _ => "trivial",
        };
        let created_at = if it.created_at.trim().is_empty() {
            now_stamp()
        } else {
            it.created_at.clone()
        };
        let updated_at = if it.updated_at.trim().is_empty() {
            created_at.clone()
        } else {
            it.updated_at.clone()
        };
        entries.push(MemoryEntry {
            metadata: crate::memory::model::MemoryMeta { verification: None, ..it.metadata.clone() },
            unknown_fields: it.unknown_fields.clone(),
            supersedes: it.supersedes.clone(),
            superseded_by: it.superseded_by.clone(),
            see_also: it.see_also.clone(),
            id: new_ids[i].clone(),
            kind: parsed_kind,
            tags: it.tags.clone(),
            title: it.title.clone(),
            content: it.content.clone(),
            // Own as this machine's account: import becomes local memory (do not inherit the sharer's user, or server routing breaks)
            user: current_user(),
            computer: String::new(),
            project: String::new(),
            device: device_tag(),
            modified_by: device_tag(),
            created_at,
            updated_at,
            emotion: -1.0,
            parent_id: parents[i].clone(),
            importance: importance.to_owned(),
        });
    }
    let relation_ids: HashMap<String,String> = payload.items.iter().zip(new_ids.iter())
        .filter(|(item,_)| !item.content.trim().is_empty()).map(|(item,id)| (item.id.clone(),id.clone())).collect();
    remap_relations(&mut entries,&relation_ids)?;
    let report = ImportReport {
        imported: entries.len(),
        skipped,
        duplicates: 0,
        reattached,
        orphaned,
    };
    if entries.is_empty() {
        return Ok(report);
    }
    for entry in &entries {
        crate::metadata_gate::ensure_write(&store, entry)?;
        crate::metadata_gate::ensure_sources(&keys, &store, entry)?;
    }
    let stored = entries
        .iter()
        .map(|e| MemoryEngine::seal(&keys, embedder, e, &e.user))
        .collect::<Result<Vec<_>>>()?;
    store.import_batch(&stored)?;
    // Auto-sync after write: same as App::auto_sync (existing env assembly entry)
    if autosync_active() && remote_configured() {
        if let Err(e) =
            crate::sync::build_remote_from_env().and_then(|remote| sync_all(&keys, &store, &remote))
        {
            eprintln!("warning: auto-sync failed (saved locally; retry later): {e}");
        }
    }
    Ok(report)
}

/// SQLite consistent snapshot, including committed WAL; dest must be a new file, never overwrite existing data.
pub fn backup_db(dest: &std::path::Path) -> Result<PathBuf> {
    crate::input_error::validate_output_parent(dest)?;
    let src = database_path(&data_dir())?;
    backup_database(&src, dest)
}

pub(crate) fn backup_database(src: &std::path::Path, dest: &std::path::Path) -> Result<PathBuf> {
    backup_database_policy(src,dest,true)
}

pub(crate) fn backup_database_policy(src: &std::path::Path, dest: &std::path::Path, strip_grants: bool) -> Result<PathBuf> {
    crate::input_error::validate_output_parent(dest)?;
    let connection =
        rusqlite::Connection::open_with_flags(src, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let target = dest
        .to_str()
        .ok_or_else(|| anyhow!("backup path must be valid UTF-8"))?;
    // Exclusively create an empty dest first; SQLite VACUUM INTO can write an empty file.
    // Reject same-path, existing backup, directories, so we never overwrite the live library or a user file.
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(dest)?;
    let snapshot = (|| -> Result<()> {
        connection.execute("VACUUM INTO ?1", rusqlite::params![target])?;
        let mut backup = rusqlite::Connection::open(dest)?;
        if strip_grants {
            sanitize_detached_backup(&mut backup)?;
        }
        Ok(())
    })();
    if let Err(error) = snapshot {
        if let Err(cleanup) = std::fs::remove_file(dest) {
            anyhow::bail!("backup failed: {error}; failed to clean incomplete backup: {cleanup}");
        }
        return Err(error);
    }
    Ok(dest.to_path_buf())
}

// This accepts only the detached, exclusively-created backup above, not the
// serving library. A rollback image deliberately takes the other policy lane.
pub(crate) fn sanitize_detached_backup(backup: &mut rusqlite::Connection) -> Result<()> {
    let has_grants = backup
        .prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name='access_grants'")?
        .exists([])?;
    if has_grants {
        backup.execute_batch("PRAGMA secure_delete=ON;")?;
        let transaction = backup.transaction()?;
        let before = transaction.total_changes();
        let removed = transaction.execute("DELETE FROM access_grants", [])? as u64;
        anyhow::ensure!(transaction.total_changes() - before == removed,
            "snapshot grant removal changed unrelated rows");
        transaction.commit()?;
        // Do not leave grant bytes in freelist pages of a transferable copy.
        backup.execute_batch("VACUUM;")?;
        anyhow::ensure!(!backup.prepare("SELECT 1 FROM access_grants LIMIT 1")?.exists([])?,
            "snapshot still contains access grants");
    }
    Ok(())
}

#[cfg(test)]
#[path = "backup_copy_tests.rs"]
mod backup_copy_tests;
