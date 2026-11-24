{
            let session = build_session()?;
            let store = build_local()?;
            // Full id or first-8 prefix match
            let candidates = store.all(false)?;
            let mut hit: Option<&respire_app::StoredMemory> = None;
            for s in &candidates {
                if s.id == id
                    || (s.id.len() >= 8
                        && id.len() >= 8
                        && respire_app::service::short_id(&s.id) == respire_app::service::short_id(&id))
                {
                    hit = Some(s);
                    break;
                }
            }
            match hit {
                Some(s) => {
                    let e = MemoryEngine::open(&session, s)?;
                    let all_t = store.all(true).unwrap_or_default();
                    let title_of = |pid: &str| -> String {
                        all_t
                            .iter()
                            .find(|m| m.id == pid)
                            .map(|m| {
                                if m.local_title.is_empty() {
                                    short_id(&m.id)
                                } else {
                                    m.local_title.clone()
                                }
                            })
                            .unwrap_or_else(|| pid.chars().take(8).collect())
                    };
                    let ancestors: Vec<serde_json::Value> = store
                        .ancestor_chain(&e.id)
                        .unwrap_or_default()
                        .iter()
                        .map(|pid| serde_json::json!({"id":pid,"title":title_of(pid)}))
                        .collect();
                    let children: Vec<serde_json::Value> = store.children(&e.id).unwrap_or_default().iter().map(|c| serde_json::json!({"id":c.id,"title":if c.local_title.is_empty(){c.id.chars().take(8).collect::<String>()}else{c.local_title.clone()},"summary":c.local_content_head.chars().take(80).collect::<String>()})).collect();
                    let mut result = ResultEnvelope::new(
                        "show",
                        OutputStatus::Ok,
                        serde_json::json!({"id":e.id,"title":e.title}),
                        vec![OutputItem::new("entry", OutputStatus::Ok, short_id(&e.id))],
                    );
                    result.details = serde_json::json!({"entry":e,"ancestors":ancestors,"children":children,
                        "recall_count": s.local_recall_count});
                    emit_result(result)?;
                }
                None => emit_result(ResultEnvelope::new(
                    "show",
                    OutputStatus::Fail,
                    serde_json::json!({"id":id}),
                    vec![OutputItem::new("entry", OutputStatus::Fail, "not found")],
                ))?,
            }
}
