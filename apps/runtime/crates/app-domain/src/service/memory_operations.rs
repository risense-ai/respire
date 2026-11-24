impl App {
    /// Unlock the local session → open the store → load BGE. Missing model is an error (no fallback).
    pub fn open() -> Result<Self> {
        let keys = auth::load_local_session()
            .map_err(|e| anyhow!("session not unlocked ({e}) \u{2014} register/login or keygen first"))?;
        let store = open_store()?;
        let embedder = BgeEmbedder::load_model(&store.retrieval_model()?)?;
        notify_index();
        Ok(Self {
            keys,
            store,
            embedder: Box::new(embedder),
        })
    }

    // ── read ──

    pub fn status(&self) -> Result<StatusInfo> {
        let (local_total, local_alive) = self.store.catalog_counts()?;
        Ok(StatusInfo {
            local_total,
            local_alive,
            remote_configured: remote_configured(),
            max_updated_at: self.store.max_updated_at().ok().flatten(),
            data_dir: data_dir().to_string_lossy().into_owned(),
        })
    }

    /// List using Core's combined-score order; CLI list uses metadata ordering.
    pub fn list(&self, limit: usize) -> Result<Vec<MemoryEntry>> {
        let candidates = self.store.all(false)?;
        require_candidates_ready(&self.store, &candidates)?;
        let q = MemoryQuery::default().limit(limit);
        MemoryEngine::recall_local(&self.keys, &self.embedder, &candidates, &q)
    }

    /// Semantic search (with score; a hit bumps heat, same as CLI recall).
    pub fn search(
        &self,
        query: &str,
        limit: usize,
        kind: Option<Kind>,
    ) -> Result<Vec<ScoredEntry>> {
        let mut q = MemoryQuery::new(query).limit(limit);
        if let Some(k) = kind {
            q = q.of_kind(k);
        }
        let (candidates, _) = recall_candidates(&self.store)?;
        let ranked:Vec<(f32,MemoryEntry)>=crate::core_sdk::execute("query",serde_json::json!({
            "model":self.embedder.model_name(),"snapshots":crate::memory::engine::snapshots(&self.keys,&candidates)?,
            "query":q,"mode":"scored","query_settings":crate::feedback::query_settings(&self.store,&q,&candidates)?
        }))?;
        for (score, e) in &ranked {
            if !readonly_mode() && !off_mode() { let _ = self.store.bump_recall_count(&e.id); }
            let _ = score;
        }
        Ok(ranked
            .into_iter()
            .map(|(score, entry)| ScoredEntry { score, entry })
            .collect())
    }

    /// One entry full text + ancestor chain + descendant probe (8-char prefix ok).
    pub fn show(&self, id_prefix: &str) -> Result<EntryDetail> {
        let entry = self.resolve_entry(id_prefix)?;
        let ancestors: Vec<NodeRef> = self
            .store
            .ancestor_chain(&entry.id)?
            .into_iter()
            .filter_map(|pid| {
                self.stored_title(&pid)
                    .map(|title| NodeRef { id: pid, title })
            })
            .collect();
        let children: Vec<ChildRef> = self
            .store
            .children(&entry.id)?
            .iter()
            .map(|c| ChildRef {
                id: c.id.clone(),
                title: if c.local_title.is_empty() {
                    short_id(&c.id)
                } else {
                    c.local_title.clone()
                },
                summary: c
                    .local_content_head
                    .chars()
                    .take(80)
                    .collect::<String>()
                    .replace('\n', " "),
            })
            .collect();
        Ok(EntryDetail {
            entry,
            ancestors,
            children,
        })
    }

    /// Causal tree (structured for UI). Empty `from` → root forest; depth 0 = roots only.
    ///
    /// **Trivial stays out of the tree** (fixed 2026-09-20): inject §3.4 says trivia goes to the diary only,
    /// not the tree. This fn used to leave trivial in — 115 diary rows (including daily "activity trail"
    /// entries whose parent is empty) became stray roots. Now dropped the same way as `tree_cure_with_min`/`defrag`.
    /// Orphans whose parent was dropped become roots; important subtrees are unchanged.
    /// Explicit `--from <trivial-id>` still renders that one entry (a direct lookup, not blocked).
    pub fn tree(&self, from: &str, depth: usize) -> Result<Vec<TreeNode>> {
        let raw = self.store.all(true)?;
        // Tree-view scope: drop trivia, promote orphans (see tree_scope_list)
        let all = tree_scope_list(&raw);
        let children_of: HashMap<String, Vec<String>> = {
            let mut m: HashMap<String, Vec<String>> = HashMap::new();
            for s in &all {
                if !s.local_parent_id.is_empty() {
                    m.entry(s.local_parent_id.clone())
                        .or_default()
                        .push(s.id.clone());
                }
            }
            m
        };
        let title_of = |id: &str| -> String {
            all.iter()
                .find(|s| s.id == id)
                .map(|s| {
                    if s.local_title.is_empty() {
                        short_id(id)
                    } else {
                        s.local_title.clone()
                    }
                })
                .unwrap_or_else(|| short_id(id))
        };
        let kind_of = |id: &str| -> String {
            all.iter()
                .find(|s| s.id == id)
                .map(|s| s.local_kind.clone())
                .unwrap_or_default()
        };
        let build = |id: &str, depth: usize| -> TreeNode {
            Self::build_node(id, depth, &children_of, &title_of, &kind_of)
        };
        if from.is_empty() {
            let mut roots: Vec<String> = all
                .iter()
                .filter(|m| m.local_parent_id.is_empty())
                .map(|m| m.id.clone())
                .collect();
            // Roots by updated_at desc (newer outlines first, UI reading order)
            roots.sort_by(|a, b| {
                let ta = all
                    .iter()
                    .find(|s| s.id == *a)
                    .map(|s| s.updated_at.clone())
                    .unwrap_or_default();
                let tb = all
                    .iter()
                    .find(|s| s.id == *b)
                    .map(|s| s.updated_at.clone())
                    .unwrap_or_default();
                tb.cmp(&ta)
            });
            Ok(roots
                .iter()
                .map(|r| build(r, depth.saturating_sub(1)))
                .collect())
        } else {
            let full = self.resolve_id(from)?;
            Ok(vec![build(&full, depth)])
        }
    }

    fn build_node(
        id: &str,
        depth: usize,
        children_of: &HashMap<String, Vec<String>>,
        title_of: &dyn Fn(&str) -> String,
        kind_of: &dyn Fn(&str) -> String,
    ) -> TreeNode {
        let kids = children_of.get(id).cloned().unwrap_or_default();
        let children = if depth == 0 {
            Vec::new()
        } else {
            kids.iter()
                .map(|k| Self::build_node(k, depth - 1, children_of, title_of, kind_of))
                .collect()
        };
        let mut descendants = kids.len();
        let mut frontier: Vec<String> = kids.clone();
        while let Some(cur) = frontier.pop() {
            if let Some(grand) = children_of.get(&cur) {
                descendants += grand.len();
                frontier.extend(grand.iter().cloned());
            }
        }
        TreeNode {
            id: id.to_owned(),
            title: title_of(id),
            kind: kind_of(id),
            is_leaf: kids.is_empty(),
            descendants,
            children,
        }
    }

    // ── store (judge first, then pick one of three) ──

    /// Store candidates: recall-similar bill before write (does not write).
    /// Ask the private Core for candidate actions; persistence stays in the application.
    pub fn candidates(&self, content: &str) -> Result<CandidateReport> {
        let all = self.store.all(false)?;
        require_candidates_ready(&self.store, &all)?;
        candidate_report(&self.keys, &self.embedder, &all, content)
    }

    /// Store a memory (after picking one of three): force write / parent as cause / merge_ids merge (delete old, store new, rehang children, inherit first item's parent).
    pub fn create(&self, req: &CreateReq) -> Result<MemoryEntry> {
        ensure!(req.metadata.verification.is_none(), "verification is only accepted by review commit");
        anyhow::ensure!(req.merge_ids.is_none() || (req.supersedes.is_none() && req.see_also.is_empty()), "merge and associations are mutually exclusive");
        let stamp = now_stamp();
        let title = if req.title.trim().is_empty() {
            derive_title(&req.content)
        } else {
            req.title.clone()
        };
        let id = uuid::Uuid::new_v4().to_string();
        let mut final_parent = req.parent.clone().unwrap_or_default();
        if req.merge_ids.is_none() && !final_parent.is_empty() {
            // Prefix resolve (full id first); if it fails, really empty = own root (same as CLI)
            let all = self.store.all(true)?;
            final_parent = resolve_prefix(&all, &final_parent).unwrap_or_default();
        }
        let mut entry = MemoryEntry {
            metadata: req.metadata.clone(),
            unknown_fields: Default::default(),
            supersedes: String::new(),
            superseded_by: String::new(),
            see_also: Vec::new(),
            id,
            kind: if req.kind.trim().is_empty() { Kind::Context } else {
                Kind::try_from_str(&req.kind.trim().to_lowercase()).ok_or_else(|| anyhow!("unsupported_metadata: unknown memory kind"))?
            },
            tags: split_tags(&req.tags),
            title,
            content: req.content.clone(),
            user: current_user(),
            computer: req.computer.clone(),
            device: device_tag(),
            modified_by: device_tag(),
            project: req.project.clone(),
            created_at: stamp.clone(),
            updated_at: stamp,
            emotion: req.emotion.unwrap_or(-1.0),
            parent_id: {
                // Empty parent means own root (after 2026-09-21 there is no default attach point)
                final_parent
            },
            importance: req
                .importance
                .clone()
                .unwrap_or_else(|| "trivial".to_owned()),
        };
        if req.merge_ids.is_none() {
            crate::metadata_gate::ensure_write(&self.store, &entry)?;
            crate::metadata_gate::ensure_sources(&self.keys, &self.store, &entry)?;
        }
        if let Some(ids) = &req.merge_ids {
            merge_entries_with_metadata(
                &self.keys,
                &self.store,
                &self.embedder,
                &mut entry,
                ids,
                req.parent.as_deref().unwrap_or_default(),
                false,
                req.merge_metadata.as_ref(),
            )?;
        } else if req.supersedes.is_some() || !req.see_also.is_empty() {
            store_related(&self.keys,&self.store,&self.embedder,&mut entry,req.supersedes.as_deref(),&req.see_also)?;
        } else {
            let stored = MemoryEngine::seal(&self.keys, &self.embedder, &entry, &entry.user)?;
            self.store.put(&stored)?;
        }
        self.auto_sync();
        Ok(entry)
    }

    /// Update an entry (any of title/body/tags/kind → decrypt, reseal, re-embed, LWW new stamp).
    pub fn update(
        &self,
        id: &str,
        title: Option<String>,
        content: Option<String>,
        tags: Option<String>,
        kind: Option<String>,
    ) -> Result<MemoryEntry> {
        self.update_with_metadata(id, title, content, tags, kind, None)
    }

    pub fn update_with_metadata(
        &self, id: &str, title: Option<String>, content: Option<String>,
        tags: Option<String>, kind: Option<String>, metadata: Option<&MetadataPatch>,
    ) -> Result<MemoryEntry> {
        let all = self.store.all(true)?;
        let full = resolve_prefix(&all, id)?;
        let stored = all
            .iter()
            .find(|m| m.id == full && !m.deleted)
            .ok_or_else(|| anyhow!("not found #{id}"))?;
        let mut entry = MemoryEngine::open(&self.keys, stored)?;
        if let Some(t) = title {
            entry.title = t;
        }
        if let Some(c) = content {
            entry.content = c;
        }
        if let Some(tg) = tags {
            entry.tags = split_tags(&tg);
        }
        if let Some(k) = kind {
            entry.kind = Kind::try_from_str(&k.trim().to_lowercase()).ok_or_else(|| anyhow!("unsupported_metadata: unknown memory kind"))?;
        }
        if let Some(patch) = metadata { patch.apply(&mut entry.metadata); }
        crate::metadata_gate::ensure_write(&self.store, &entry)?;
        crate::metadata_gate::ensure_sources(&self.keys, &self.store, &entry)?;
        entry.updated_at = self.store.edit_stamp(&stored.id)?;
        let user = entry.user.clone();
        let new_stored = MemoryEngine::seal(&self.keys, &self.embedder, &entry, &user)?;
        self.store.put(&new_stored)?;
        self.auto_sync();
        Ok(entry)
    }

    /// Delete (tombstone, propagates with sync). Full id or 8-char prefix.
    pub fn delete(&self, id: &str) -> Result<bool> {
        let all = self.store.all(true)?;
        let full = resolve_prefix(&all, id)?;
        let deleted = self.store.forget(&full)?;
        if deleted {
            self.auto_sync();
        }
        Ok(deleted)
    }

    /// Restore a tombstone: keep identity and content, republish with a new LWW stamp.
    pub fn restore(&self, id: &str) -> Result<MemoryEntry> {
        let all = self.store.all(true)?;
        let stored = all
            .iter()
            .find(|m| m.id == id)
            .ok_or_else(|| crate::input_error::InputError(format!("not found #{id}")))?;
        let mut entry = MemoryEngine::open(&self.keys, stored)?;
        if !stored.deleted {
            return Ok(entry);
        }
        entry.updated_at = self.store.edit_stamp(&stored.id)?;
        crate::metadata_gate::ensure_write(&self.store, &entry)?;
        crate::metadata_gate::ensure_sources(&self.keys, &self.store, &entry)?;
        let restored = MemoryEngine::seal(&self.keys, &self.embedder, &entry, &entry.user)?;
        self.store.put(&restored)?;
        self.auto_sync();
        Ok(entry)
    }

    // ── causal tree ops ──

    /// Attach (shared by attach/demote): prefix resolve + cycle guard + no self-parent.
    pub fn attach(&self, id: &str, parent: &str) -> Result<(String, String)> {
        let all = self.store.all(true)?;
        let child = resolve_prefix(&all, id)?;
        let parent_full = resolve_prefix(&all, parent)?;
        if child == parent_full {
            return Err(crate::input_error::InputError("cannot attach to self".into()).into());
        }
        for anc in self.store.ancestor_chain(&parent_full)? {
            if anc == child {
                return Err(crate::input_error::InputError("cycle: new parent is a descendant of this entry".into()).into());
            }
        }
        reparent(&self.keys, &self.store, &child, &parent_full)?;
        self.auto_sync();
        Ok((child, parent_full))
    }

    /// Promote: this entry becomes its grandparent/root (children move with it).
    pub fn promote(&self, id: &str) -> Result<(String, String)> {
        let all = self.store.all(true)?;
        let full = resolve_prefix(&all, id)?;
        let me = all
            .iter()
            .find(|m| m.id == full)
            .ok_or_else(|| anyhow!("not found #{id}"))?;
        if me.deleted || me.local_parent_id.is_empty() {
            anyhow::bail!("already a root (no parent) or deleted \u{2014} nothing to promote");
        }
        let parent_id = me.local_parent_id.clone();
        let grandparent = all
            .iter()
            .find(|m| m.id == parent_id)
            .and_then(|p| {
                if p.local_parent_id.is_empty() {
                    None
                } else {
                    Some(p.local_parent_id.clone())
                }
            })
            .unwrap_or_default();
        reparent(&self.keys, &self.store, &full, &grandparent)?;
        self.auto_sync();
        Ok((full, grandparent))
    }

    // ── sync ──

    /// Manual two-way sync.
    pub fn sync(&self) -> Result<SyncOutcome> {
        let remote = crate::sync::build_remote_from_env()?;
        let stats = sync_all(&self.keys, &self.store, &remote)?;
        let blobs = self.store.all(true)?;
        let local_total = blobs.len();
        let local_alive = blobs.iter().filter(|m| !m.deleted).count();
        Ok(SyncOutcome {
            pulled: stats.pulled,
            pushed: stats.pushed,
            remote_total: stats.remote_total,
            remote_alive: stats.remote_alive,
            local_total,
            local_alive,
            converged: local_alive == stats.remote_alive && local_total == stats.remote_total,
        })
    }

    /// Notify the owning runtime after a local commit; never wait for networking.
    pub fn auto_sync(&self) {
        notify_autosync();
    }

    // ── internals ──

    fn stored_title(&self, id: &str) -> Option<String> {
        self.store
            .all(true)
            .ok()?
            .iter()
            .find(|m| m.id == id)
            .map(|m| {
                if m.local_title.is_empty() {
                    short_id(&m.id)
                } else {
                    m.local_title.clone()
                }
            })
    }

    fn resolve_id(&self, prefix: &str) -> Result<String> {
        let all = self.store.all(true)?;
        resolve_prefix(&all, prefix)
    }

    fn resolve_entry(&self, prefix: &str) -> Result<MemoryEntry> {
        let all = self.store.all(false)?;
        let full = resolve_prefix(&all, prefix)?;
        let stored = all
            .iter()
            .find(|m| m.id == full)
            .ok_or_else(|| anyhow!("not found #{prefix}"))?;
        MemoryEngine::open(&self.keys, stored)
    }
}

