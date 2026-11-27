// ── tools ──

/// Export all plaintext memories as JSON (backup/migrate).
pub fn export_json(path: &std::path::Path) -> Result<usize> {
    crate::input_error::validate_output_parent(path)?;
    let keys = auth::load_local_session()?;
    let store = open_store()?;
    let blobs = store.all(false)?;
    let mut items = Vec::new();
    for s in &blobs {
        let e = MemoryEngine::open(&keys, s)?;
        items.push(serde_json::to_value(&e)?);
    }
    std::fs::write(path, serde_json::to_string_pretty(&items)?)?;
    Ok(items.len())
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum ImportTags {
    List(Vec<String>),
    Text(String),
}

impl Default for ImportTags {
    fn default() -> Self {
        Self::List(Vec::new())
    }
}

#[derive(serde::Deserialize)]
struct ImportItem {
    #[serde(default, flatten)]
    metadata: crate::memory::model::MemoryMeta,
    #[serde(default, flatten)]
    unknown_fields: crate::memory::model::ExtraFields,
    #[serde(default, rename = "device")]
    _device: String,
    #[serde(default, rename = "modified_by")]
    _modified_by: String,
    #[serde(default)]
    supersedes: String,
    #[serde(default)]
    superseded_by: String,
    #[serde(default)]
    see_also: Vec<String>,
    #[serde(default)]
    id: String,
    #[serde(default, alias = "type")]
    kind: Option<String>,
    #[serde(default)]
    tags: ImportTags,
    #[serde(default)]
    title: String,
    content: String,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    computer: String,
    #[serde(default)]
    project: String,
    #[serde(default, alias = "createdAt")]
    created_at: Option<String>,
    #[serde(default, alias = "updatedAt")]
    updated_at: Option<String>,
    #[serde(default)]
    emotion: Option<f32>,
    #[serde(default, alias = "parentId")]
    parent_id: Option<String>,
    #[serde(default)]
    importance: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ImportReport {
    pub imported: usize,
    pub skipped: usize,
    pub duplicates: usize,
    pub reattached: usize,
    pub orphaned: usize,
}

pub fn import_json(path: &std::path::Path) -> Result<ImportReport> {
    let text = crate::input_error::read_file(path, "import file")?;
    import_json_text(&text)
}

/// Import exact-content-unique records; the same snapshot can be retried safely.
pub fn import_json_text(text: &str) -> Result<ImportReport> {
    ensure!(!off_mode(), "memory service is disabled; restore normal service before importing");
    ensure_writable()?;
    let items: Vec<ImportItem> = serde_json::from_str(text)
        .map_err(|e| anyhow!("failed to parse import JSON: {e}"))?;
    let mut report = ImportReport { imported: 0, skipped: 0, duplicates: 0, reattached: 0, orphaned: 0 };
    let mut entries: Vec<MemoryEntry> = Vec::new();
    let mut old_ids: HashMap<String, usize> = HashMap::new();
    for (index, item) in items.into_iter().enumerate() {
        if item.id.is_empty() && item.content.trim().is_empty() {
            report.skipped += 1;
            continue;
        }
        if !item.id.is_empty() {
            if let Some(previous) = old_ids.get(&item.id) {
                ensure!(entries[*previous].content == item.content, "duplicate id with different content at record {}", index + 1);
                let mut metadata = item.metadata.clone();
                metadata.verification = None;
                ensure!(entries[*previous].metadata == metadata && entries[*previous].unknown_fields == item.unknown_fields,
                    "duplicate id with different metadata at record {}", index + 1);
                report.skipped += 1;
                report.duplicates += 1;
                continue;
            }
            old_ids.insert(item.id, entries.len());
        }
        let kind = item.kind.unwrap_or_else(|| "context".to_owned()).to_lowercase();
        if !matches!(kind.as_str(), "context" | "decision" | "preference" | "task" | "emotion" | "time" | "skill" | "knowledge" | "hypothesis" | "reflection" | "self") {
            anyhow::bail!("invalid kind at record {}: {kind}", index + 1);
        }
        let importance = item.importance.unwrap_or_else(|| "trivial".to_owned());
        if !matches!(importance.as_str(), "normal" | "important" | "trivial") {
            anyhow::bail!("invalid importance at record {}", index + 1);
        }
        let emotion = item.emotion.unwrap_or(-1.0);
        if emotion != -1.0 && !(0.0..=1.0).contains(&emotion) {
            anyhow::bail!("invalid emotion at record {}", index + 1);
        }
        item.metadata.validate()?;
        crate::memory::model::metadata::validate_extra_fields(&item.unknown_fields)?;
        let created_at = item.created_at.filter(|s| !s.is_empty()).unwrap_or_else(now_stamp);
        let updated_at = item.updated_at.filter(|s| !s.is_empty()).unwrap_or_else(|| created_at.clone());
        entries.push(MemoryEntry {
            metadata: crate::memory::model::MemoryMeta { verification: None, ..item.metadata },
            unknown_fields: item.unknown_fields,
            supersedes: item.supersedes, superseded_by: item.superseded_by, see_also: item.see_also,
            id: uuid::Uuid::new_v4().to_string(), kind: Kind::try_from_str(&kind).ok_or_else(|| anyhow!("unsupported_metadata: unknown memory kind"))?,
            tags: match item.tags { ImportTags::List(tags) => tags, ImportTags::Text(tags) => split_tags(&tags) },
            title: item.title, content: item.content, user: item.user.unwrap_or_else(current_user),
            computer: item.computer, project: item.project, device: device_tag(), modified_by: device_tag(),
            created_at, updated_at, emotion, parent_id: item.parent_id.unwrap_or_default(), importance,
        });
    }
    let mut done = HashSet::new();
    for start in 0..entries.len() {
        let mut chain = HashSet::new();
        let mut cursor = Some(start);
        while let Some(index) = cursor {
            if done.contains(&index) { break; }
            if !chain.insert(index) { anyhow::bail!("import parent chain contains a cycle"); }
            cursor = old_ids.get(&entries[index].parent_id).copied();
        }
        done.extend(chain);
    }
    if entries.is_empty() { return Ok(report); }
    let app = App::open()?;
    app.store.write_transaction(|| {
        let mut contents = HashMap::new();
        let mut existing = app.store.all(false)?;
        existing.sort_by(|left, right| left.id.cmp(&right.id));
        let mut canonical = HashMap::new();
        for stored in existing {
            let entry = MemoryEngine::open(&app.keys, &stored)?;
            contents.entry(entry.content.clone()).or_insert(entry.id.clone());
            canonical.insert(entry.id.clone(), (stored, entry));
        }
        let incoming: HashMap<_, _> = entries.iter().map(|entry| (entry.id.clone(), entry.clone())).collect();
        let mut retained = Vec::new();
        for (index, entry) in entries.iter_mut().enumerate() {
            if let Some(id) = contents.get(&entry.content) {
                let existing = canonical.get(id).map(|(_, old)| old)
                    .or_else(|| incoming.get(id)).context("missing content duplicate")?;
                if entry.metadata.memory_contract != 0 || existing.metadata.memory_contract != 0 {
                    let mut previous = existing.metadata.clone();
                    previous.verification = None;
                    ensure!(entry.metadata == previous && entry.kind == existing.kind
                        && entry.title == existing.title && entry.unknown_fields == existing.unknown_fields,
                        "import duplicate content has different metadata; import was rolled back");
                }
                entry.id = id.clone();
                report.skipped += 1;
                report.duplicates += 1;
            } else {
                contents.insert(entry.content.clone(), entry.id.clone());
                retained.push(index);
            }
        }
        let ids: HashMap<String, String> = old_ids.iter()
            .map(|(id, index)| (id.clone(), entries[*index].id.clone())).collect();
        let mut fresh: Vec<MemoryEntry> = retained.into_iter().map(|index| entries[index].clone()).collect();
        remap_relations(&mut fresh, &ids)?;
        for entry in &mut fresh {
            entry.see_also.sort();
            entry.see_also.dedup();
            if entry.parent_id.is_empty() { continue; }
            match ids.get(&entry.parent_id) {
                Some(id) if id != &entry.id => {
                    entry.parent_id = id.clone();
                    report.reattached += 1;
                }
                Some(_) => anyhow::bail!("content deduplication would create a self parent; original library was preserved"),
                None => { entry.parent_id.clear(); report.orphaned += 1; }
            }
        }
        let parents: HashMap<_, _> = fresh.iter().map(|entry| (entry.id.as_str(), entry.parent_id.as_str())).collect();
        let mut complete = HashSet::new();
        for start in &fresh {
            let mut seen = HashSet::new();
            let mut cursor = start.id.as_str();
            while let Some(parent) = parents.get(cursor) {
                if complete.contains(cursor) { break; }
                if !seen.insert(cursor) { anyhow::bail!("content deduplication would create a parent cycle"); }
                cursor = parent;
            }
            complete.extend(seen);
        }
        let mut linked = HashSet::new();
        for entry in &fresh {
            if let Some((_, previous)) = canonical.get_mut(&entry.supersedes) {
                ensure!(previous.superseded_by.is_empty(), "deduplicated target already has a replacement; import was rolled back");
                previous.superseded_by = entry.id.clone();
                linked.insert(previous.id.clone());
            }
            for target in &entry.see_also {
                if let Some((_, other)) = canonical.get_mut(target) {
                    if !other.see_also.contains(&entry.id) {
                        other.see_also.push(entry.id.clone());
                        linked.insert(other.id.clone());
                    }
                }
            }
        }
        for id in &linked {
            let (_, entry) = canonical.get(id).context("missing deduplicated association target")?;
            crate::metadata_gate::ensure_write(&app.store, entry)?;
        }
        for entry in &fresh {
            crate::metadata_gate::ensure_write(&app.store, entry)?;
            crate::metadata_gate::ensure_sources(&app.keys, &app.store, entry)?;
        }
        for id in linked {
            let (row, entry) = canonical.get(&id).context("missing deduplicated association target")?;
            let stamp = app.store.edit_stamp(&id)?;
            let updated = MemoryEngine::reseal_edges(&app.keys, row, &stamp, |payload| {
                payload.superseded_by = entry.superseded_by.clone();
                payload.see_also = entry.see_also.clone();
                payload.modified_by = device_tag();
            })?;
            ensure!(app.store.put_inner(&updated, true)?, "deduplicated association target changed");
        }
        let stored = fresh.iter()
            .map(|entry| MemoryEngine::seal(&app.keys, &app.embedder, entry, &entry.user))
            .collect::<Result<Vec<_>>>()?;
        for memory in &stored {
            if !app.store.put_inner(memory, true)? { anyhow::bail!("import conflict: {}", memory.id); }
        }
        report.imported = stored.len();
        Ok(())
    })?;
    if report.imported > 0 { app.auto_sync(); }
    Ok(report)
}

