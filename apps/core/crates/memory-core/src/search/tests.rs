use super::*;
use crate::i18n::fixtures::search as text;

#[test]
fn attention_head_distribution_is_a_probability_over_candidates() {
    let p = head_distribution(&[0.1, 0.9, 0.5], 1.0);
    let total: f32 = p.iter().sum();
    assert!((total - 1.0).abs() < 1e-5);
    let peak = p.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    assert_eq!(peak, p[1]);
    assert!(p[1] > p[2] && p[2] > p[0]);
    assert!(head_distribution(&[], 1.0).is_empty());
    let uniform = head_distribution(&[0.5, 0.5], 0.7);
    assert!((uniform[0] - 0.5).abs() < 1e-6 && (uniform[1] - 0.5).abs() < 1e-6);
}

#[test]
fn auto_gate_switches_to_attention_only_for_small_pools() {
    assert!(auto_attention_gate(49, "what happened"));
    assert!(auto_attention_gate(300, text::KEYWORD));
    // Fitted logistic flips between 376 and 377 authorized entries.
    assert!(auto_attention_gate(375, "q"));
    assert!(!auto_attention_gate(377, "q"));
    assert!(!auto_attention_gate(4_766, text::KEYWORD));
    assert!(!auto_attention_gate(4_853, text::KEYWORD));
}

#[test]
fn query_settings_reject_unknown_fusion_modes_and_bad_temperature() {
    let mut settings = crate::query::QuerySettings::default();
    assert!(settings.validate().is_ok());
    settings.fusion = "softmax".into();
    assert!(settings.validate().is_err());
    settings.fusion = "auto".into();
    settings.fusion_temperature = 0.0;
    assert!(settings.validate().is_err());
}

#[test]
fn exact_phrase_requires_latin_boundaries_but_preserves_cjk() {
    assert!(exact_phrase_matches("learn rust today", "rust"));
    assert!(!exact_phrase_matches("trust", "rust"));
    assert!(!exact_phrase_matches("resident regression keywords", "resident regression keyword"));
    assert!(exact_phrase_matches("resident regression keyword.", "resident regression keyword"));
    assert!(exact_phrase_matches(text::INDEX_STATE, text::INDEX_PHRASE));
    assert!(!exact_phrase_matches("anything", ""));
    assert!(!exact_phrase_matches("drives", "i"));
    assert_eq!(keyword_score(&["i".into()], &Default::default(), "context", "", "Drives", ""), 0.0);
}

#[test]
fn hashing_embedder_produces_normalized() {
    let e = HashingEmbedder::default();
    let v = e.embed(text::CHINESE_SENTENCE);
    assert_eq!(v.len(), 256);
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-5);
    let boxed: Box<dyn Embedder> = Box::new(HashingEmbedder::new(8));
    assert_eq!(Embedder::dims(&boxed), 8);
    assert_eq!(boxed.embed("ab").len(), 8);
    assert_eq!(boxed.embed_query("ab").len(), 8);
}

#[test]
fn reused_centered_scorer_preserves_legacy_bits() {
    let mean = [1.0, 0.0];
    let left = [0.0, 1.0];
    // Golden bits from the original pairwise scorer. A vector equal to the
    // mean has no centered direction, so two such vectors use raw cosine.
    let expected = [0x3f35_04f4, 0, 0x3f80_0000, 0];
    let centered = centered_cosine_from(&left, &mean);
    let uncentered = centered_cosine_from(&mean, &mean);
    let actual = [centered(&[-1.0, 0.0]), centered(&mean), uncentered(&mean), uncentered(&left)];
    assert_eq!(actual.map(f32::to_bits), expected);
}

#[test]
fn mean_vector_and_centered_cosine() {
    assert!(mean_vector(&[]).is_none());
    let a = [1.0f32, 0.0];
    let b = [0.0f32, 1.0];
    match mean_vector(&[&a, &b]) {
        Some(mu) => {
            assert!((mu[0] - 0.5).abs() < 1e-6);
            assert!((mu[1] - 0.5).abs() < 1e-6);
            let c = centered_cosine(&a, &a, &mu);
            assert!(c > 0.9);
        }
        None => panic!("mean vector of two non-empty slices must exist"),
    }
}

#[test]
fn similar_texts_have_higher_cosine() {
    let e = HashingEmbedder::default();
    let a = e.embed(text::RUST_MEMORY_SYSTEMS);
    let b = e.embed(text::RUST_MEMORY_PARALLEL);
    let c = e.embed(text::PYTHON_ML_FULL);
    let sim_ab = cosine_similarity(&a, &b);
    let sim_ac = cosine_similarity(&a, &c);
    assert!(
        sim_ab > sim_ac,
        "similar texts should have higher cosine: {sim_ab} vs {sim_ac}"
    );
}

#[test]
fn empty_embedding_returns_zero_cosine() {
    assert!((cosine_similarity(&[], &[1.0])).abs() < 1e-6);
    assert_eq!(document_text("same text", "same text"), "same text");
    assert_eq!(document_text("title", "body"), "title\nbody");
    assert_eq!(document_text("", "body"), "body");
    let v = vec![0.0; 256];
    let u = vec![1.0; 256];
    assert!((cosine_similarity(&v, &u)).abs() < 1e-6);
}

fn feedback_rows()->Vec<StoredMemory> {
    ["one","two"].into_iter().map(|id|{
        let mut row=StoredMemory::new_pending(id.into());
        row.local_title="Rust".into();row.local_embedding=Some(vec![1.0,0.0]);row
    }).collect()
}
#[test]
fn feedback_disabled_preserves_legacy_scores_and_enabled_replaces_read_heat() {
    let mut rows=feedback_rows();rows[0].local_recall_count=15;
    let refs:Vec<_>=rows.iter().collect();let tree=Default::default();let baseline=crate::query::QuerySettings::default();
    let old=rerank_semantic_with_settings(&refs,&[1.0,0.0],"Rust",&tree,&baseline);
    let mut disabled=baseline.clone();
    let mut ignored=crate::query::FeedbackSettings::default();ignored.good.insert("two".into(),15);ignored.bad.insert("one".into(),5);disabled.feedback=Some(ignored);
    assert_eq!(old,rerank_semantic_with_settings(&refs,&[1.0,0.0],"Rust",&tree,&disabled));
    assert_eq!(old[0].0,0);
    let mut enabled=baseline;let mut feedback=crate::query::FeedbackSettings::default();feedback.enabled=true;feedback.good.insert("two".into(),1);enabled.feedback=Some(feedback);
    let actual=rerank_semantic_with_settings(&refs,&[1.0,0.0],"Rust",&tree,&enabled);
    assert_eq!(actual[0].0,1);
}
/// P0-2 acceptance: with feedback=None a recall_count of 1000 must not move
/// any score once read heat is off (the default); read_heat=true must still
/// move it, so the equality is a real gate rather than a vacuous one.
#[test]
fn feedback_none_recall_count_1000_does_not_change_scores() {
    let build = |recall: i64| {
        let mut rows = feedback_rows();
        rows[0].local_recall_count = recall;
        rows
    };
    let cold = build(0);
    let hot = build(1000);
    let cold_refs: Vec<_> = cold.iter().collect();
    let hot_refs: Vec<_> = hot.iter().collect();
    let tree = Default::default();
    let settings = crate::query::QuerySettings::default();
    assert!(!settings.read_heat, "read heat must default off (P0-2)");
    assert_eq!(
        rerank_semantic_with_settings(&cold_refs, &[1.0, 0.0], "Rust", &tree, &settings),
        rerank_semantic_with_settings(&hot_refs, &[1.0, 0.0], "Rust", &tree, &settings),
        "feedback=None + recall_count=1000 must not change scores"
    );
    let on = crate::query::QuerySettings { read_heat: true, ..crate::query::QuerySettings::default() };
    assert_ne!(
        rerank_semantic_with_settings(&cold_refs, &[1.0, 0.0], "Rust", &tree, &on),
        rerank_semantic_with_settings(&hot_refs, &[1.0, 0.0], "Rust", &tree, &on),
        "read_heat=true must still apply recall_count heat (gate stays real)"
    );
}
#[test]
fn feedback_uses_original_good_heat_and_bad_penalty_saturation() {
    assert_eq!(hit_score(0),0.0);assert_eq!(hit_score(1),0.25);assert_eq!(hit_score(15),1.0);assert_eq!(hit_score(100),1.0);
    let rows=feedback_rows();let refs:Vec<_>=rows.iter().collect();let tree=Default::default();
    let mut settings=crate::query::QuerySettings::default();
    let mut feedback=crate::query::FeedbackSettings::default();feedback.enabled=true;settings.feedback=Some(feedback.clone());
    let neutral=rerank_semantic_with_settings(&refs,&[1.0,0.0],"Rust",&tree,&settings);
    for count in [1,5,500] {
        feedback.bad.insert("one".into(),count);settings.feedback=Some(feedback.clone());
        let actual=rerank_semantic_with_settings(&refs,&[1.0,0.0],"Rust",&tree,&settings);
        let score=actual.iter().find(|(id,_)|*id==0).map(|(_,score)|*score);
        let base=neutral.iter().find(|(id,_)|*id==0).map(|(_,score)|*score);
        if let (Some(score),Some(base))=(score,base) {assert!((score-base*(1.0-0.4*count.min(5) as f32/5.0)).abs()<1e-6);}else{panic!("fixture unexpectedly crossed candidate floor");}
    }
    feedback.bad_penalty=false;settings.feedback=Some(feedback);
    assert_eq!(neutral,rerank_semantic_with_settings(&refs,&[1.0,0.0],"Rust",&tree,&settings));
}
#[test]
fn feedback_protocol_defaults_and_numeric_fields_are_strict() -> anyhow::Result<()> {
    let legacy:crate::query::QuerySettings=serde_json::from_value(serde_json::json!({"route_on":true}))?;
    assert!(legacy.feedback.is_none());
    let enabled:crate::query::FeedbackSettings=serde_json::from_value(serde_json::json!({"enabled":true}))?;
    assert!(enabled.bad_penalty);assert!(enabled.good.is_empty()&&enabled.bad.is_empty());
    for invalid in [serde_json::json!({"enabled":"false"}),serde_json::json!({"enabled":true,"good":{"id":-1}}),serde_json::json!({"enabled":true,"bad":{"id":"1"}}),serde_json::json!({"enabled":true,"unknown":1})] {
        assert!(serde_json::from_value::<crate::query::FeedbackSettings>(invalid).is_err());
    }
    Ok(())
}

#[test]
fn feedback_contexts_do_not_leak_between_concurrent_requests() -> anyhow::Result<()> {
    let rows=std::sync::Arc::new(feedback_rows());let barrier=std::sync::Arc::new(std::sync::Barrier::new(4));let mut threads=Vec::new();
    for target in 0..4 {let rows=rows.clone();let barrier=barrier.clone();threads.push(std::thread::spawn(move||{
        let id=target%2;let mut feedback=crate::query::FeedbackSettings::default();feedback.enabled=true;feedback.good.insert(if id==0{"one"}else{"two"}.into(),15);
        let mut settings=crate::query::QuerySettings::default();settings.feedback=Some(feedback);let refs:Vec<_>=rows.iter().collect();barrier.wait();
        for _ in 0..25 {assert_eq!(rerank_semantic_with_settings(&refs,&[1.0,0.0],"Rust",&Default::default(),&settings)[0].0,id);}
    }));}
    for thread in threads {thread.join().map_err(|_|anyhow::anyhow!("request isolation test panicked"))?;}
    Ok(())
}

#[test]
fn rerank_semantic_filters_by_threshold() {
    let mut entries = vec![
        StoredMemory::new_pending("1".into()),
        StoredMemory::new_pending("2".into()),
    ];
    let e = HashingEmbedder::default();
    entries[0].local_embedding = Some(e.embed(text::RUST_MEMORY_PROGRAMMING));
    entries[0].local_title = text::RUST_BEGINNER.into();
    entries[0].local_kind = "context".into();
    entries[0].local_tags = "rust".into();
    entries[1].local_embedding = Some(e.embed(text::PYTHON_ML));
    entries[1].local_title = text::PYTHON_TUTORIAL.into();
    entries[1].local_kind = "context".into();
    entries[1].local_tags = "python".into();
    let query_vec = e.embed(text::RUST_MEMORY_QUERY);
    let refs: Vec<&StoredMemory> = entries.iter().collect();
    let tree = std::collections::HashMap::new();
    let ranked = rerank_semantic(&refs, &query_vec, text::RUST_MEMORY_QUERY, &tree);
    assert!(!ranked.is_empty());
    assert_eq!(ranked[0].0, 0); // first after sort is index 0 (Rust)
    let mut semantic_hit = StoredMemory::new_pending("semantic".into());
    semantic_hit.local_embedding = Some(vec![0.45, (1.0f32 - 0.45f32.powi(2)).sqrt()]);
    let mut lexical_noise = StoredMemory::new_pending("noise".into());
    lexical_noise.local_embedding = Some(vec![0.10, (1.0f32 - 0.10f32.powi(2)).sqrt()]);
    lexical_noise.local_title = "unrelated phrase".into();
    lexical_noise.local_recall_count = 1000;
    let rows = [&semantic_hit, &lexical_noise];
    let settings = crate::query::QuerySettings::default();
    let found = rerank_semantic_with_settings(&rows, &[1.0, 0.0], "unrelated phrase", &Default::default(), &settings);
    assert_eq!(found.iter().map(|row| row.0).collect::<Vec<_>>(), vec![0]);
    assert_eq!(settings.diagnostics.borrow().dropped_by_cos, 1);
    let mut ablated = crate::query::QuerySettings::default();
    ablated.ablate = "kw,recency,hot,tree".into();
    assert_eq!(rerank_semantic_with_settings(&rows, &[1.0, 0.0], "unrelated phrase", &Default::default(), &ablated).len(), 1);
}

/// Tree-position axis: a root (depth 0) and an active subtree should beat a same-semantics deep leaf.
#[test]
fn rerank_semantic_tree_axis_lifts_roots() {
    let e = HashingEmbedder::default();
    let mut root = StoredMemory::new_pending("root".into());
    root.local_title = text::NOTEBOOK_TITLE.into();
    root.local_kind = "context".into();
    root.local_embedding = Some(e.embed(text::NOTEBOOK_QUERY));
    let mut leaf = StoredMemory::new_pending("leaf".into());
    leaf.local_title = text::OLD_NOTEBOOK_TITLE.into();
    leaf.local_kind = "context".into();
    leaf.local_parent_id = "root".into();
    leaf.local_embedding = Some(e.embed(text::NOTEBOOK_QUERY));
    // Leaf has no subtree activity, depth 2+; root subtree activity comes from the leaf updated_at — the leaf old stamp is already empty (0),
    // root depth 0 gets +0.04; at equal semantics the root should lead.
    let list = vec![leaf, root];
    let refs: Vec<&StoredMemory> = list.iter().collect();
    let tree = crate::engine::build_tree_meta(&refs);
    let qv = e.embed(text::NOTEBOOK_QUERY);
    let ranked = rerank_semantic(&refs, &qv, text::NOTEBOOK_QUERY, &tree);
    assert_eq!(ranked[0].0, 1, "root (depth 0) should rank before the leaf");
}

#[test]
fn rerank_locally_fallback() {
    let entries = vec![
        (
            "context".into(),
            "".into(),
            text::UNRELATED.into(),
            text::UNRELATED_BODY.into(),
        ),
        (
            "context".into(),
            "".into(),
            "Rust".into(),
            text::NO_QUERY_WORD.into(),
        ),
        (
            "context".into(),
            "".into(),
            text::UNRELATED.into(),
            text::RUST_ONE_MENTION.into(),
        ),
    ];
    let ranked = rerank_locally(&entries, "rust");
    assert_eq!(ranked[0], 1);
}
