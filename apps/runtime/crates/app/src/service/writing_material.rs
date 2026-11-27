// ── book and portrait (material aggregation; the AI drafts) ──

#[derive(Debug, Clone, Serialize)]
pub struct BookEntry {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub content: String,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct BookChapter {
    pub id: String,
    pub title: String,
    pub entries: Vec<BookEntry>,
}

#[derive(Debug, Serialize)]
pub struct BookMaterial {
    pub root: NodeRef,
    pub chapters: Vec<BookChapter>,
    pub total_entries: usize,
    pub total_chars: usize,
}

impl App {
    /// Book material: one causal tree → volume (root) - chapter (direct children) - section (descendants), tree-order full text.
    /// The AI reads this and drafts the book (structure, narrative, cuts are the AI's call).
    pub fn book_material(&self, root_prefix: &str) -> Result<BookMaterial> {
        let all = self.store.all(false)?;
        let root_id = resolve_prefix(&all, root_prefix)?;
        let root = all
            .iter()
            .find(|m| m.id == root_id)
            .ok_or_else(|| anyhow!("root does not exist"))?;
        let to_entry = |m: &crate::memory::model::StoredMemory| -> BookEntry {
            BookEntry {
                id: m.id.clone(),
                title: if m.local_title.is_empty() {
                    short_id(&m.id)
                } else {
                    m.local_title.clone()
                },
                kind: m.local_kind.clone(),
                content: m.local_content_head.clone(),
                created_at: m.local_created_at.clone(),
            }
        };
        let direct: Vec<&crate::memory::model::StoredMemory> = all
            .iter()
            .filter(|m| m.local_parent_id == root_id)
            .collect();
        let mut chapters = Vec::new();
        let mut total_entries = 0usize;
        let mut total_chars = 0usize;
        for d in &direct {
            let mut entries = vec![to_entry(d)];
            // DFS collect grandchildren (depth cap 3 so a deep tree cannot explode)
            let mut stack: Vec<String> = vec![d.id.clone()];
            let mut depth = 0;
            while !stack.is_empty() && depth < 3 {
                let mut next = Vec::new();
                for cur in &stack {
                    for m in all.iter().filter(|m| m.local_parent_id == *cur) {
                        entries.push(to_entry(m));
                        next.push(m.id.clone());
                    }
                }
                stack = next;
                depth += 1;
            }
            total_entries += entries.len();
            total_chars += entries
                .iter()
                .map(|e| e.content.chars().count())
                .sum::<usize>();
            chapters.push(BookChapter {
                id: d.id.clone(),
                title: if d.local_title.is_empty() {
                    short_id(&d.id)
                } else {
                    d.local_title.clone()
                },
                entries,
            });
        }
        chapters.sort_by(|a, b| b.entries.len().cmp(&a.entries.len()));
        Ok(BookMaterial {
            root: NodeRef {
                id: root.id.clone(),
                title: if root.local_title.is_empty() {
                    short_id(&root.id)
                } else {
                    root.local_title.clone()
                },
            },
            chapters,
            total_entries,
            total_chars,
        })
    }

    /// Portrait material: preference/decision/emotion entries + theme-tree ranking + monthly timeline — the AI drafts a portrait.
    pub fn portrait_material(&self, limit: usize) -> Result<serde_json::Value> {
        let all = self.store.all(false)?;
        let pick = |kind: Kind| -> Vec<BookEntry> {
            let mut items: Vec<BookEntry> = all
                .iter()
                .filter(|m| m.local_kind == kind.as_str())
                .map(|m| BookEntry {
                    id: m.id.clone(),
                    title: if m.local_title.is_empty() {
                        short_id(&m.id)
                    } else {
                        m.local_title.clone()
                    },
                    kind: m.local_kind.clone(),
                    content: m.local_content_head.clone(),
                    created_at: m.local_created_at.clone(),
                })
                .collect();
            items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
            items.truncate(limit);
            items
        };
        // Theme-tree ranking (subtree size top 10)
        let mut children_of: HashMap<String, Vec<String>> = HashMap::new();
        for m in &all {
            if !m.local_parent_id.is_empty() {
                children_of
                    .entry(m.local_parent_id.clone())
                    .or_default()
                    .push(m.id.clone());
            }
        }
        let subtree = |r: &str| -> usize {
            let mut n = 0;
            let mut st = vec![r.to_owned()];
            while let Some(c) = st.pop() {
                if let Some(kids) = children_of.get(&c) {
                    n += kids.len();
                    st.extend(kids.iter().cloned());
                }
            }
            n
        };
        let mut themes: Vec<(String, usize)> = all
            .iter()
            .filter(|m| m.local_parent_id.is_empty() && children_of.contains_key(m.id.as_str()))
            .map(|m| {
                (
                    if m.local_title.is_empty() {
                        short_id(&m.id)
                    } else {
                        m.local_title.clone()
                    },
                    subtree(&m.id),
                )
            })
            .collect();
        themes.sort_by(|a, b| b.1.cmp(&a.1));
        themes.truncate(10);
        // Monthly timeline
        let mut timeline: HashMap<String, usize> = HashMap::new();
        for m in &all {
            let key = &m.local_created_at[..m.local_created_at.len().min(7)];
            if key.len() == 7 {
                *timeline.entry(key.to_owned()).or_default() += 1;
            }
        }
        let mut timeline: Vec<(String, usize)> = timeline.into_iter().collect();
        timeline.sort();
        Ok(serde_json::json!({
            "preferences": pick(Kind::Preference),
            "decisions": pick(Kind::Decision),
            "emotions": pick(Kind::Emotion),
            "skills": pick(Kind::Skill),
            "top_themes": themes,
            "timeline": timeline,
            "total_entries": all.len(),
        }))
    }
}

