//! Immutable authorized retrieval views. Host owns scope and persistence.
use anyhow::{ensure, Context, Result};
use respire_protocol::{MemoryEntry};
use respire_core::StoredMemory;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};

// A generation owns its ordering and membership. Unchanged records (including
// their vector buffers and entry bodies) are shared; deltas get fresh immutable
// records, so a retained View always keeps its original heat and content.
pub(super) struct View {
    pub memories: Vec<Arc<StoredMemory>>,
    pub entries: HashMap<String, Arc<MemoryEntry>>,
    pub lexical_indexes: Arc<respire_core::rank::LexicalIndexCache>,
    model: String,
}

/// A lexical request borrows an already vector-free full-body view, or projects its own copy.
/// Resident vectors remain available to concurrent semantic requests.
pub(super) fn retrieval_memories(view: &View, lexical_only: bool) -> std::borrow::Cow<'_, [Arc<StoredMemory>]> {
    // Pending resident rows already carry their full lexical body. Borrow that
    // immutable view rather than copying every body again for each request.
    // Mixed/semantic rows still require the vector-free projection below.
    if !lexical_only || view.memories.iter().all(|memory|
        memory.local_embedding.is_none() && memory.local_chunks.is_empty()
            && view.entries.get(&memory.id).is_none_or(|entry|
                memory.local_content_head == entry.content)) {
        return std::borrow::Cow::Borrowed(&view.memories);
    }
    std::borrow::Cow::Owned(view.memories.iter().map(|memory| Arc::new(StoredMemory {
        id: memory.id.clone(),
        updated_at: memory.updated_at.clone(),
        deleted: memory.deleted,
        local_meta: memory.local_meta.clone(),
        local_kind: memory.local_kind.clone(),
        local_tags: memory.local_tags.clone(),
        local_title: memory.local_title.clone(),
        local_project: memory.local_project.clone(),
        local_computer: memory.local_computer.clone(),
        local_embedding: None,
        local_chunks: Vec::new(),
        local_parent_id: memory.local_parent_id.clone(),
        local_created_at: memory.local_created_at.clone(),
        local_content_head: view.entries.get(&memory.id)
            .map_or_else(|| memory.local_content_head.clone(), |entry| entry.content.clone()),
        local_recall_count: memory.local_recall_count,
        local_importance: memory.local_importance.clone(),
    })).collect())
}

static VIEWS: LazyLock<Mutex<HashMap<String, Arc<View>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) fn view(input: &mut Value, model: &str) -> Result<Arc<View>> {
    if let Some(key) = input.get("resident_key").and_then(Value::as_str) {
        let view = VIEWS.lock().map_err(|_| anyhow::anyhow!("resident index registry poisoned"))?
            .get(key).cloned().context("authorized resident view is no longer available")?;
        ensure!(view.model == model, "resident index belongs to another model");
        return Ok(view);
    }
    let (memories, entries) = crate::business::snapshots(crate::business::take(input, "snapshots"), Some(model))?;
    Ok(Arc::new(View {
        memories: memories.into_iter().map(Arc::new).collect(),
        entries: entries.into_iter().map(|(id, entry)| (id, Arc::new(entry))).collect(),
        lexical_indexes: Default::default(),
        model: model.to_owned(),
    }))
}

/// Request-local membership; the published generation and its leases stay unchanged.
pub(super) fn filtered(view: &View, allowed: &HashSet<&str>) -> View {
    View {
        memories: view.memories.iter().filter(|memory| allowed.contains(memory.id.as_str())).map(Arc::clone).collect(),
        entries: view.entries.iter().filter(|(id, _)| allowed.contains(id.as_str()))
            .map(|(id, entry)| (id.clone(), Arc::clone(entry))).collect(),
        // Evidence membership is checked against the exact ordered lexical corpus on every hit.
        lexical_indexes: Arc::clone(&view.lexical_indexes),
        model: view.model.clone(),
    }
}

pub(super) fn publish(mut input: Value) -> Result<Value> {
    let key = input["key"].as_str().context("missing resident key")?.to_owned();
    ensure!(!key.is_empty() && key.len() <= 128, "invalid resident key");
    let model = input["model"].as_str().context("missing resident model")?.to_owned();
    let base = match input.get("base_key").and_then(Value::as_str) {
        Some(base_key) => Some(view(&mut json!({"resident_key":base_key}), &model)?),
        None => None,
    };
    let (delta, delta_entries) = crate::business::snapshots(crate::business::take(&mut input, "snapshots"), Some(&model))?;
    let removed = HashSet::<&str>::deserialize(&input["removed"])?;
    let changed: HashSet<&str> = delta.iter().map(|memory| memory.id.as_str()).collect();
    let mut memories = Vec::new();
    let mut entries = HashMap::new();
    // Heat/artifact updates may leave the lexical corpus unchanged; exact corpus checks
    // fence changed rows while independent authorized contexts keep separate caches.
    let lexical_indexes = base.as_ref().map_or_else(Default::default, |base| Arc::clone(&base.lexical_indexes));
    if let Some(base) = base {
        memories.extend(base.memories.iter().filter(|memory|
            !removed.contains(memory.id.as_str()) && !changed.contains(memory.id.as_str())).map(Arc::clone));
        entries.extend(base.entries.iter().filter(|(id, _)|
            !removed.contains(id.as_str()) && !changed.contains(id.as_str())).map(|(id, entry)|
            (id.clone(), Arc::clone(entry))));
    }
    memories.extend(delta.into_iter().map(Arc::new));
    entries.extend(delta_entries.into_iter().map(|(id, entry)| (id, Arc::new(entry))));
    let count = memories.len();
    let indexed = memories.iter().filter(|memory| memory.local_embedding.is_some()).count();
    let mut views = VIEWS.lock().map_err(|_| anyhow::anyhow!("resident index registry poisoned"))?;
    ensure!(!views.contains_key(&key), "resident key already exists");
    views.insert(key, Arc::new(View { memories, entries, model, lexical_indexes }));
    Ok(json!({"count":count,"indexed":indexed}))
}

pub(super) fn release(input: &Value) -> Result<Value> {
    let key = input["key"].as_str().context("missing resident key")?;
    VIEWS.lock().map_err(|_| anyhow::anyhow!("resident index registry poisoned"))?.remove(key);
    Ok(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(id: &str, title: &str) -> Value {
        json!({"id":id,"title":title,"deleted":false,"updated_at":"2026-10-10T00:00:00Z",
            "kind":"context","tags":"","project":"","computer":"","parent_id":"",
            "created_at":"2026-10-10T00:00:00Z","content_head":"","recall_count":0,
            "importance":"important","entry":null,"features":null})
    }

    #[test]
    fn pending_body_is_recalled_without_loading_a_model() -> Result<()> {
        let key = "resident-pending-body";
        let mut item = snapshot("pending", "unrelated title");
        let content = format!("{} resident regression keyword", "prefix ".repeat(100));
        item["entry"] = json!({"id":"pending","kind":"Context","tags":[],
            "title":"unrelated title","content":content,"user":"test","computer":"test",
            "project":"","created_at":"2026-10-10T00:00:00Z","updated_at":"2026-10-10T00:00:00Z",
            "emotion":0,"parent_id":"","importance":"important"});
        publish(json!({"key":key,"model":"m3","snapshots":[item],"removed":[]}))?;
        let retained = view(&mut json!({"resident_key":key}), "m3")?;
        assert!(matches!(retrieval_memories(&retained, true), std::borrow::Cow::Borrowed(_)));
        let input = json!({"model":"m3","resident_key":key,"lexical_only":true,
            "query":{"text":"resident regression keyword","limit":5},"mode":"plain"});
        let mut state = crate::business::State::new(false);
        let result = crate::business::dispatch(&mut state, "query", input.clone());
        let candidates = crate::business::dispatch(&mut state, "remember_candidates", input.clone());
        let mut unrelated = input.clone();
        unrelated["query"]["text"] = json!("absent_unrelated_query");
        let negative = crate::business::dispatch(&mut state, "query", unrelated);
        let mut report_input = input;
        report_input["content"] = json!("resident regression keyword");
        let report = crate::business::dispatch(&mut state, "candidate_report", report_input);
        release(&json!({"key":key}))?;
        candidates?;
        report?;
        assert!(negative?.as_array().is_some_and(Vec::is_empty));
        let items = result?;
        assert!(items.as_array().is_some_and(|items| items.iter().any(|item| item["id"] == "pending")));
        Ok(())
    }

    #[test]
    fn mixed_lexical_requests_preserve_resident_vectors() -> Result<()> {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let key = "resident-mixed-lexical";
        let mut indexed = snapshot("indexed", "unrelated indexed title");
        let mut pending = snapshot("pending", "unrelated pending title");
        for item in [&mut indexed, &mut pending] {
            let id = item["id"].as_str().context("fixture ID")?.to_owned();
            item["entry"] = json!({"id":id,"kind":"Context","tags":[],
                "title":item["title"],"content":format!("{} mixed lexical needle", "prefix ".repeat(100)),
                "user":"test","computer":"test","project":"","created_at":"2026-10-10T00:00:00Z",
                "updated_at":"2026-10-10T00:00:00Z","emotion":0,"parent_id":"","importance":"important"});
            item["content_head"] = json!("truncated prefix");
        }
        let vector: Vec<f32> = vec![1.0; 1024];
        let encoded = STANDARD.encode(vector.iter().flat_map(|value| value.to_le_bytes()).collect::<Vec<_>>());
        indexed["features"] = json!({"artifact":"","legacy":encoded,"chunks":[encoded]});
        publish(json!({"key":key,"model":"m3","snapshots":[indexed,pending],"removed":[]}))?;
        let retained = view(&mut json!({"resident_key":key}), "m3")?;
        assert!(matches!(retrieval_memories(&retained, false), std::borrow::Cow::Borrowed(_)));
        let lexical = retrieval_memories(&retained, true);
        assert!(lexical.iter().all(|memory| memory.local_embedding.is_none() && memory.local_chunks.is_empty()));
        assert!(lexical.iter().all(|memory| memory.local_content_head.contains("mixed lexical needle")));
        let input = json!({"model":"m3","resident_key":key,"lexical_only":true,
            "query":{"text":"mixed lexical needle","limit":5},"mode":"plain",
            "content":"mixed lexical needle"});
        // No model is loaded: all three operations must remain entirely lexical.
        let mut state = crate::business::State::new(false);
        let query = crate::business::dispatch(&mut state, "query", input.clone());
        let candidates = crate::business::dispatch(&mut state, "remember_candidates", input.clone());
        let report = crate::business::dispatch(&mut state, "candidate_report", input.clone());
        let mut negative = input;
        negative["query"]["text"] = json!("absent_unrelated_query");
        negative["content"] = json!("absent_unrelated_query");
        let absent = crate::business::dispatch(&mut state, "query", negative.clone());
        let absent_candidates = crate::business::dispatch(&mut state, "remember_candidates", negative.clone());
        let absent_report = crate::business::dispatch(&mut state, "candidate_report", negative);
        let current = view(&mut json!({"resident_key":key}), "m3")?;
        release(&json!({"key":key}))?;
        assert!(Arc::ptr_eq(&retained, &current));
        assert_eq!(retained.memories[0].local_embedding.as_ref(), Some(&vector));
        assert_eq!(retained.memories[0].local_chunks, vec![vector]);
        assert_eq!(retained.memories[0].local_content_head, "truncated prefix");
        let query = query?;
        let candidates = candidates?;
        let report = report?;
        for id in ["indexed", "pending"] {
            assert!(query
                .as_array().is_some_and(|items| items.iter().any(|item| item["id"] == id)));
            assert!(candidates["merge"]
                .as_array().is_some_and(|items| items.iter().any(|item| item[1]["id"] == id)));
            assert!(report["merge"]
                .as_array().is_some_and(|items| items.iter().any(|item| item["entry"]["id"] == id)));
        }
        assert!(absent?.as_array().is_some_and(Vec::is_empty));
        for result in [absent_candidates?, absent_report?] {
            assert!(result["merge"].as_array().is_some_and(Vec::is_empty));
            assert!(result["parent"].as_array().is_some_and(Vec::is_empty));
        }
        Ok(())
    }

    #[test]
    fn publication_preserves_readers_and_rejects_an_invalid_delta() -> Result<()> {
        let first = "resident-unit-first";
        let second = "resident-unit-second";
        publish(json!({"key":first,"model":"m3","snapshots":[snapshot("a","old")],"removed":[]}))?;
        let retained = view(&mut json!({"resident_key":first}), "m3")?;
        publish(json!({"key":second,"base_key":first,"model":"m3",
            "snapshots":[snapshot("a","new"),snapshot("b","added")],"removed":[]}))?;
        release(&json!({"key":first}))?;
        assert_eq!(retained.memories[0].local_title, "old");
        let current = view(&mut json!({"resident_key":second}), "m3")?;
        assert_eq!(current.memories.len(), 2);
        assert_eq!(current.memories[0].local_title, "new");
        assert!(view(&mut json!({"resident_key":second}), "wrong-model").is_err());
        assert!(publish(json!({"key":"resident-unit-invalid","base_key":second,"model":"m3",
            "snapshots":[snapshot("a","duplicate"),snapshot("a","duplicate")],"removed":[]})).is_err());
        assert!(view(&mut json!({"resident_key":"resident-unit-invalid"}), "m3").is_err());
        assert_eq!(view(&mut json!({"resident_key":second}), "m3")?.memories.len(), 2);
        release(&json!({"key":second}))?;
        Ok(())
    }
    fn indexed_snapshot(id: &str, heat: i64) -> Value {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let encoded = STANDARD.encode([1.0_f32; 8].iter()
            .flat_map(|value| value.to_le_bytes()).collect::<Vec<_>>());
        let mut item = snapshot(id, "shared needle");
        item["recall_count"] = json!(heat);
        item["entry"] = json!({"id":id,"kind":"Context","tags":[],
            "title":"shared needle","content":"immutable body shared needle","user":"test",
            "computer":"test","project":"","created_at":"2026-10-10T00:00:00Z",
            "updated_at":"2026-10-10T00:00:00Z","emotion":0,"parent_id":"",
            "importance":"important"});
        item["features"] = json!({"artifact":"","legacy":encoded,"chunks":[encoded]});
        item
    }

    #[test]
    fn delta_shares_unchanged_payloads_and_preserves_heat_and_deletes() -> Result<()> {
        let first = "resident-shared-first";
        let second = "resident-shared-second";
        publish(json!({"key":first,"model":"test-hash:8","removed":[],
            "snapshots":[indexed_snapshot("a",0),indexed_snapshot("b",0),indexed_snapshot("c",0)]}))?;
        let old = view(&mut json!({"resident_key":first}), "test-hash:8")?;
        let mut changed = indexed_snapshot("b",32);
        changed["updated_at"] = json!("2026-10-11T00:00:00Z");
        changed["entry"]["content"] = json!("updated body shared needle");
        let result = publish(json!({"key":second,"base_key":first,"model":"test-hash:8",
            "snapshots":[changed],"removed":["c"]}))?;
        let new = view(&mut json!({"resident_key":second}), "test-hash:8")?;
        release(&json!({"key":first}))?;
        release(&json!({"key":second}))?;
        assert_eq!(result, json!({"count":2,"indexed":2}));
        assert!(Arc::ptr_eq(&old.memories[0], &new.memories[0]));
        assert!(Arc::ptr_eq(&old.entries["a"], &new.entries["a"]));
        assert_eq!(old.memories[0].local_embedding.as_ref().map(Vec::as_ptr),
            new.memories[0].local_embedding.as_ref().map(Vec::as_ptr));
        assert_eq!(old.memories[0].local_chunks[0].as_ptr(), new.memories[0].local_chunks[0].as_ptr());
        assert!(!Arc::ptr_eq(&old.memories[1], &new.memories[1]));
        assert!(!Arc::ptr_eq(&old.entries["b"], &new.entries["b"]));
        assert_eq!(old.memories[1].local_recall_count, 0);
        assert_eq!(new.memories[1].local_recall_count, 32);
        assert_eq!(old.memories[1].updated_at, "2026-10-10T00:00:00Z");
        assert_eq!(new.memories[1].updated_at, "2026-10-11T00:00:00Z");
        assert_eq!(old.entries["b"].content, "immutable body shared needle");
        assert_eq!(new.entries["b"].content, "updated body shared needle");
        assert!(old.entries.contains_key("c"));
        assert!(!new.entries.contains_key("c"));
        let rank = |v: &View| {
            let rows: Vec<_> = v.memories.iter().map(Arc::as_ref).collect();
            // This test proves the resident delta propagates local_recall_count;
            // heat must be opted into explicitly now that it defaults off (P0-2).
            let settings = respire_core::query::QuerySettings {
                read_heat: true,
                ..respire_core::query::QuerySettings::default()
            };
            respire_core::search::rerank_semantic_with_settings(&rows, &[1.0;8],
                "shared needle", &HashMap::new(), &settings)
        };
        let old_rank = rank(&old);
        let new_rank = rank(&new);
        assert_eq!(old_rank[0].0, 0);
        assert_eq!(new_rank[0].0, 1, "new heat must affect only the new generation's ranking");
        assert!(new_rank[0].1 > old_rank[0].1);
        // Wire output still contains owned entries, not Arc-specific serialization.
        let embedder = respire_core::search::HashingEmbedder::new(8);
        let query = respire_protocol::MemoryQuery::new("").limit(1);
        let selected = respire_core::query::MemoryEngine::recall_local(
            &new.entries, &embedder, &new.memories, &query, &Default::default())?;
        assert_eq!(selected.len(), 1);
        assert_eq!(serde_json::to_value(&selected[0])?, serde_json::to_value(new.entries[&selected[0].id].as_ref())?);
        Ok(())
    }

    #[test]
    fn publishing_moves_request_strings_into_the_resident_payload() -> Result<()> {
        let key = "resident-owned-request";
        let request = json!({"key":key,"model":"test-hash:8","removed":[],
            "snapshots":[indexed_snapshot("owned",0)]});
        let title = request["snapshots"][0]["title"].as_str().context("fixture title")?.as_ptr();
        let content = request["snapshots"][0]["entry"]["content"].as_str().context("fixture content")?.as_ptr();
        publish(request)?;
        let retained = view(&mut json!({"resident_key":key}), "test-hash:8")?;
        release(&json!({"key":key}))?;
        assert_eq!(title, retained.memories[0].local_title.as_ptr());
        assert_eq!(content, retained.entries["owned"].content.as_ptr());
        Ok(())
    }

    #[test]
    fn concurrent_reader_keeps_its_snapshot_after_republish_and_release() -> Result<()> {
        let first = "resident-concurrent-first";
        let second = "resident-concurrent-second";
        publish(json!({"key":first,"model":"test-hash:8","removed":[],
            "snapshots":[indexed_snapshot("a",0),indexed_snapshot("b",0)]}))?;
        let old = view(&mut json!({"resident_key":first}), "test-hash:8")?;
        let gate = Arc::new(std::sync::Barrier::new(2));
        let reader_gate = Arc::clone(&gate);
        let reader_old = Arc::clone(&old);
        let reader = std::thread::spawn(move || {
            reader_gate.wait();
            assert_eq!(reader_old.memories.len(), 2);
            assert_eq!(reader_old.memories[1].local_recall_count, 0);
            assert_eq!(reader_old.entries["b"].content, "immutable body shared needle");
            reader_old
        });
        let published = publish(json!({"key":second,"base_key":first,"model":"test-hash:8",
            "snapshots":[indexed_snapshot("b",7)],"removed":["a"]}));
        let released = release(&json!({"key":first}));
        gate.wait();
        let retained = reader.join().map_err(|_| anyhow::anyhow!("snapshot reader panicked"))?;
        published?;
        released?;
        let new = view(&mut json!({"resident_key":second}), "test-hash:8")?;
        release(&json!({"key":second}))?;
        assert!(Arc::ptr_eq(&old, &retained));
        assert_eq!(new.memories.len(), 1);
        assert_eq!(new.memories[0].local_recall_count, 7);
        assert!(view(&mut json!({"resident_key":first}), "test-hash:8").is_err());
        Ok(())
    }

    #[test]
    fn quality_inline_snapshots_survive_both_queries_and_fallback() -> Result<()> {
        struct Selector { fail: bool, calls: usize }
        impl crate::Transport for Selector {
            fn request(&mut self, request: &Value) -> Result<Value> {
                self.calls += 1;
                let body: Value = serde_json::from_str(request["body"]["messages"][1]["content"]
                    .as_str().context("selection candidates")?)?;
                assert_eq!(body["candidates"].as_array().context("candidate list")?.len(), 2);
                if self.fail { anyhow::bail!("fixture selector unavailable"); }
                Ok(json!({"choices":[{"message":{"content":"{\"ids\":[\"b\"]}"}}]}))
            }
        }
        let state = crate::business::State::new(false);
        let input = json!({"model":"m3","lexical_only":true,
            "snapshots":[indexed_snapshot("a",0),indexed_snapshot("b",0)],
            "query":{"text":"shared needle","limit":1},"mode":"quality",
            "provider":{"name":"fixture","model":"fixture-model"}});
        let mut selector = Selector { fail: false, calls: 0 };
        let selected = crate::selection::recall(&state, input.clone(), &mut selector)?;
        assert_eq!(selector.calls, 1);
        assert_eq!(selected["items"][0]["entry"]["id"], "b");
        assert_eq!(selected["warning"], Value::Null);
        selector.fail = true;
        let fallback = crate::selection::recall(&state, input, &mut selector)?;
        assert_eq!(selector.calls, 2);
        assert_eq!(fallback["items"][0]["entry"]["id"], "a");
        assert!(fallback["warning"].as_str().context("fallback warning")?
            .contains("fixture selector unavailable"));
        Ok(())
    }

    /// Lexical projection and semantic borrowing preserve a shared retained generation.
    #[test]
    fn simultaneous_lexical_and_semantic_readers_keep_old_mixed_view() -> Result<()> {
        let first = "resident-simultaneous-first";
        let second = "resident-simultaneous-second";
        // Keep the fixture beyond the time-dependent seven-day tree-score boundary.
        let stable_snapshot = |id, heat| {
            let mut item = indexed_snapshot(id, heat);
            item["entry"]["created_at"] = json!("2000-01-01T00:00:00Z");
            item["entry"]["updated_at"] = json!("2000-01-01T00:00:00Z");
            item
        };
        let indexed = stable_snapshot("indexed", 0);
        let mut pending = stable_snapshot("pending", 0);
        pending["features"] = Value::Null;
        publish(json!({"key":first,"model":"test-hash:8","removed":[],"snapshots":[indexed,pending]}))?;
        let old = view(&mut json!({"resident_key":first}), "test-hash:8")?;
        assert!(matches!(retrieval_memories(&old, true), std::borrow::Cow::Owned(_)));
        assert!(matches!(retrieval_memories(&old, false), std::borrow::Cow::Borrowed(_)));
        let query = |lexical| json!({"model":"test-hash:8","lexical_only":lexical,
            "query":{"text":"shared needle","limit":5},"mode":"scored"});
        let state = crate::business::State::new(true);
        let expected_lexical = crate::business::query_view(&state, &query(true), &Default::default(), &old)?;
        let expected_semantic = crate::business::query_view(&state, &query(false), &Default::default(), &old)?;
        assert!(!expected_lexical.as_array().context("lexical results")?.is_empty());
        assert!(!expected_semantic.as_array().context("semantic results")?.is_empty());
        let gate = Arc::new(std::sync::Barrier::new(5));
        std::thread::scope(|scope| -> Result<()> {
            let readers = (0..4).map(|index| {
                let retained = Arc::clone(&old);
                let gate = Arc::clone(&gate);
                let lexical = index % 2 == 0;
                let input = query(lexical);
                let expected = if lexical { &expected_lexical } else { &expected_semantic };
                scope.spawn(move || -> Result<()> {
                    let state = crate::business::State::new(true);
                    gate.wait();
                    for _ in 0..3 {
                        assert_eq!(&crate::business::query_view(&state, &input, &Default::default(), &retained)?, expected);
                    }
                    Ok(())
                })
            }).collect::<Vec<_>>();
            let changed = stable_snapshot("indexed", 50);
            let published = publish(json!({"key":second,"base_key":first,"model":"test-hash:8",
                "snapshots":[changed],"removed":["pending"]}));
            let released = release(&json!({"key":first}));
            gate.wait();
            for reader in readers { reader.join().map_err(|_| anyhow::anyhow!("simultaneous reader panicked"))??; }
            published?;released?;
            Ok(())
        })?;
        assert_eq!(old.memories.len(), 2);
        assert_eq!(old.memories[0].local_recall_count, 0);
        assert_eq!(old.memories[0].local_embedding.as_ref(), Some(&vec![1.0; 8]));
        let new = view(&mut json!({"resident_key":second}), "test-hash:8")?;
        assert_eq!(new.memories.len(), 1);
        assert_eq!(new.memories[0].local_recall_count, 50);
        release(&json!({"key":second}))?;
        Ok(())
    }

}
