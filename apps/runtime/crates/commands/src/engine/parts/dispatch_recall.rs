{
            anyhow::ensure!(render_budget_base64.is_none() || titles || brief, "render budget requires --titles or --brief");
            anyhow::ensure!(render_min_score.is_none() || render_budget_base64.is_some() || brief, "render minimum score requires a render budget");
            let mut render_budget = recall_select::decode_render_budget(render_budget_base64.as_deref(), limit, &intent)?;
            if brief && render_budget.is_none() { render_budget=Some(recall_select::brief_layout(limit)?); }
            if brief { recall_select::require_metadata(true,render_min_score)?; }
            if render_budget.is_some() { recall_select::require_render_budget(render_min_score)?; }
            let recall_generation = rpc::sync_generation();
            let (session,owner) = build_session_with_owner()?;
            let owner=owner.unwrap_or_else(||"local".to_owned());
            let store = build_local()?;
            let review=respire_app::review::ReviewService::new(&session,&store,&owner)?;
            let source=review.recall_source()?;
            let metadata_aware=brief || source.has_metadata();
            // Dynamic blocks from new records need complete qualified variants.
            // Legacy-only title budgets retain their original WP3a bytes.
            let brief_render=brief || (metadata_aware && render_budget.is_some());
            if metadata_aware {recall_select::require_metadata(brief_render,render_min_score)?;}
            let (candidates,index_pending,resident)=if metadata_aware {
                let rows:Vec<_>=source.memories().iter().filter(|row|!row.deleted).cloned().collect();
                let pending=rows.iter().filter(|row|row.local_artifact.is_empty()).count();
                let lease=respire_app::resident::recall_authorized_lease(&session,&store,&owner,&source)?;
                (Arc::new(rows),pending,Some(lease))
            } else {
                let (rows,pending,lease)=respire_app::resident::recall_catalog(&session,&store)?;
                (rows,pending,Some(lease))
            };
            let _resident_scope=resident.as_ref().map(|lease|lease.enter());
            if index_pending > 0 { rpc::kick_index(); }
            if trace {
                let preview = &candidates;
                let primary = candidates_count_primary(&preview);
                let normals = candidates_count_normal(&preview);
                eprintln!(
                    "[trace] {} active candidates | important {} / normal {} / trivia {} | retrieval policy is Core-owned",
                    preview.len(),
                    primary,
                    normals,
                    preview.len() - primary - normals,
                );
                std::env::set_var("RSRS_DEBUG_SCORE", "1");
            }
            let mut q = MemoryQuery::new(&query).limit(limit);
            if let Some(k) = &r#type {
                q = q.of_kind(Kind::try_from_str(&k.trim().to_lowercase()).ok_or_else(|| anyhow!("unsupported_metadata: unknown memory kind"))?);
            }
            if let Some(p) = &project {
                q = q.of_project(p);
            }
            let core_intent = if metadata_aware || render_budget.is_some() {Some(intent.as_str())}else{None};
            // 3. small-to-big: hit body + ancestor bodies (budget is built-in; RSRS_ANCESTOR_BUDGET=0 turns it off)
            let (mut ranked, recall_mode, selection_fallback, routing, retrieval, budget) = if metadata_aware {
                recall_select::recall_metadata(&review,&source,embedder!(),&q,&store,render_budget,render_min_score,&intent,mode.as_deref())?
            } else {
                let (ranked,recall_mode,selection_fallback,routing,retrieval,budget)=match mode {
                Some(mode) => {
                    recall_select::recall_with_mode(&session, embedder!(), &candidates, &q, &store, render_budget, render_min_score, core_intent, &mode)?
                }
                None => recall_select::recall(&session, embedder!(), &candidates, &q, &store, render_budget, render_min_score, core_intent)?,
                };
                (ranked,recall_mode,selection_fallback,routing,retrieval,budget.map(serde_json::to_value).transpose()?)
            };
            // Procedural-memory routing (P1/P2): a how-to question gets a second,
            // skill-only pass; the union keeps one procedure inside the top three.
            // Facts keep Core's order and score; run history only ranks procedures.
            let howto = if core_intent.is_some() {
                retrieval["intent"] == "howto"
            } else { match intent.as_str() {
                "howto" => true,
                "fact" => false,
                _ => r#type.is_none() && skill::howto_intent(&query),
            }};
            let mut procedure_pass = if core_intent.is_some() {
                serde_json::from_value::<usize>(retrieval["procedure_pass"].clone())?
            } else { 0 };
            let mut procedure_promoted = if core_intent.is_some() {
                serde_json::from_value::<bool>(retrieval["procedure_promoted"].clone())?
            } else { false };
            if core_intent.is_none() && howto && r#type.is_none() {
                let mut q_skill = q.clone().of_kind(Kind::Skill);
                q_skill.limit = limit;
                let (skills, _, _, _, _, _) = recall_select::recall_with_mode(
                    &session,
                    embedder!(),
                    &candidates,
                    &q_skill,
                    &store,
                    None,
                    None,
                    None,
                    &recall_mode,
                )?;
                procedure_pass = skills.len();
                if !skills.is_empty() {
                    let ids: Vec<String> = ranked
                        .iter()
                        .chain(skills.iter())
                        .map(|r| r.entry.id.clone())
                        .collect();
                    let runs = store
                        .skill_run_stats(&ids)?
                        .into_iter()
                        .map(|(id, st)| (id, skill::RunScore { ok: st.ok, fail: st.fail }))
                        .collect();
                    let (merged, promoted) = skill::merge_howto(ranked, skills, limit, &runs);
                    ranked = merged;
                    procedure_promoted = promoted;
                }
            }
            let intent_label = if core_intent.is_some() {
                retrieval["intent"].as_str().ok_or_else(|| anyhow::anyhow!("missing Core intent receipt"))?
            } else if r#type.is_some() {
                "typed"
            } else if howto {
                "howto"
            } else {
                "fact"
            };
            let hit_ids: Vec<String> = ranked.iter().map(|r| r.entry.id.clone()).collect();
            let associations: respire_app::memory::model::RelatedResult = if metadata_aware {
                Default::default()
            } else if budget.is_some() {
                respire_app::memory::model::RelatedResult {
                    related: Vec::new(), superseded: serde_json::from_value(retrieval["superseded"].clone())?,
                }
            } else if no_related {
                Default::default()
            } else { respire_app::core_sdk::execute("related_business",serde_json::json!({
                "model":embedder!().model_name(), "snapshots":respire_app::memory::engine::snapshots(&session,&candidates)?,
                "query":q,"ids":hit_ids,"pairs":store.recall_pairs(&hit_ids)?
            }))? };
            let query_log_id=if respire_app::service::workspace_mode()=="normal" {Some(uuid::Uuid::new_v4().to_string())}else{None};
            // Query log: each recall writes a candidate record (including empty - negatives are post-training material too).
            // Note: this is the candidate set, not a hit - a hit is the model query-log mark self-grade
            {
                let candidates: Vec<String> = ranked.iter().map(|r| r.entry.id.clone()).collect();
                let scores: Vec<f32> = ranked.iter().map(|r| r.score).collect();
                let stats = rpc::RecallStats {
                    request_id:query_log_id.clone().unwrap_or_else(||uuid::Uuid::new_v4().to_string()),
                    query: query.clone(),
                    project: project.clone().unwrap_or_default(),
                    candidates,
                    scores,
                };
                if stats.candidates.is_empty() && query_log_id.is_some() {
                    // A returned empty-query identity must already have its durable negative record.
                    commit_prepared_write(true, recall_generation, || stats.persist(&store))?;
                } else if rpc::worker_active() {
                    rpc::queue_recall_stats(stats, recall_generation);
                } else if let Err(error) = stats.persist(&store) {
                    eprintln!("recall statistics writeback failed: {error:#}");
                }
                // Hit-heat writeback: top hits recall_count+1 - source of the heat-axis hit_score (used to be read-only, always 0; this round fills it)
                // Plugin hook post-recall: query and hit summary (post-training / external telemetry)
                {
                    let hits: Vec<(String, f32)> = ranked
                        .iter()
                        .map(|r| (r.entry.id.clone(), r.score))
                        .collect();
                    crate::hooks::record(
                        crate::hooks::HookEvent::PostRecall,
                        crate::hooks::recall_payload(
                            &query,
                            project.as_deref().unwrap_or(""),
                            &hits,
                        ),
                    );
                }
            }
            if ranked.is_empty() {
                let mut result=ResultEnvelope::new(
                    "recall",
                    OutputStatus::Skip,
                    serde_json::json!({"query":query,"budget":budget,"query_log_id":query_log_id,"count":0,"recall_mode":recall_mode,
                        "selection_fallback":selection_fallback,"embedding_model":store.retrieval_model()?,"routing":routing,"best_cos":retrieval["best_cos"],"best_pre_floor":retrieval["best_pre_floor"],
                        "dropped_by_floor":retrieval["dropped_by_floor"],"cos_gate":retrieval["cos_gate"],"retrieval":retrieval,"filtered_by_score":retrieval["filtered_by_score"],
                        "intent": intent_label,"procedure_pass":procedure_pass,"procedure_promoted":procedure_promoted,
                        "index_pending":index_pending,"indexed_candidates":candidates.len().saturating_sub(index_pending),"lexical_candidates":index_pending}),
                    vec![OutputItem::new("results", OutputStatus::Skip, "0")],
                );
                if metadata_aware {
                    result.summary["metadata_aware"]=serde_json::json!(true);
                    result.summary["brief"]=serde_json::json!(brief_render);
                    result.summary["filtered_by_evidence"]=retrieval["filtered_by_evidence"].clone();
                    result.summary["evidence"]=retrieval["evidence"].clone();
                    result.details=serde_json::json!([]);
                }
                emit_result(result)?;
                return Ok(());
            }
            let mut items = ranked
                .iter()
                .map(|r| {
                    OutputItem::new(
                        respire_app::service::short_id(&r.entry.id),
                        OutputStatus::Ok,
                        format!(
                            "{:.3} {}",
                            r.score,
                            if r.entry.title.is_empty() {
                                "(untitled)"
                            } else {
                                &r.entry.title
                            }
                        ),
                    )
                })
                .collect::<Vec<_>>();
            for (hit,row) in ranked.iter().zip(items.iter_mut()) {
                if associations.superseded.contains_key(&hit.entry.id) { row.value.push_str(" [superseded]"); }
            }
            let mut result = ResultEnvelope::new(
                "recall",
                OutputStatus::Ok,
                serde_json::json!({"query":query,"budget":budget,"query_log_id":query_log_id,"count":ranked.len(),"limit":limit,"project":project,
                    "recall_mode":recall_mode,"selection_fallback":selection_fallback,"embedding_model":store.retrieval_model()?,"related":associations.related.len(),"routing":routing,"best_cos":retrieval["best_cos"],"best_pre_floor":retrieval["best_pre_floor"],
                        "dropped_by_floor":retrieval["dropped_by_floor"],"cos_gate":retrieval["cos_gate"],"retrieval":retrieval,"filtered_by_score":retrieval["filtered_by_score"],
                    "intent": intent_label,"procedure_pass":procedure_pass,"procedure_promoted":procedure_promoted,
                    "index_pending":index_pending,"indexed_candidates":candidates.len().saturating_sub(index_pending),"lexical_candidates":index_pending}),
                items,
            );
            result.related = associations.related;
            result.details = if titles {
                serde_json::json!(ranked
                    .iter()
                    .map(|r| serde_json::json!({
                        "id":r.entry.id,"title":r.entry.title,"score":r.score,
                        "superseded_by":associations.superseded.get(&r.entry.id)
                    }))
                    .collect::<Vec<_>>())
            } else {
                serde_json::json!(ranked
                    .iter()
                    .map(|r| serde_json::json!({
                        "score":r.score,"entry":r.entry,"ancestors":r.ancestors,
                        "superseded_by":associations.superseded.get(&r.entry.id)
                    }))
                    .collect::<Vec<_>>())
            };
            if metadata_aware {
                result.summary["metadata_aware"]=serde_json::json!(true);
                result.summary["brief"]=serde_json::json!(brief_render);
                result.summary["filtered_by_evidence"]=retrieval["filtered_by_evidence"].clone();
                if let Some(details)=result.details.as_array_mut() {
                    for (detail,hit) in details.iter_mut().zip(ranked.iter()) {
                        detail["evidence"]=retrieval["evidence"].as_array().and_then(|values|values.iter().find(|value|value["id"]==hit.entry.id))
                            .ok_or_else(||anyhow!("Core omitted recall evidence"))?.clone();
                        detail["source_refs"]=serde_json::json!(hit.entry.metadata.source_refs);
                        detail["scope"]=serde_json::json!(hit.entry.metadata.verification.as_ref().map(|value|&value.scope));
                        detail["valid_until"]=serde_json::json!(hit.entry.metadata.valid_until);
                        detail["representation"]=budget.as_ref().and_then(|receipt|receipt["items"].as_array())
                            .and_then(|values|values.iter().find(|value|value["id"]==hit.entry.id)).map(|item|item["representation"].clone())
                            .unwrap_or_else(||serde_json::json!(if titles{"title"}else{"full"}));
                    }
                }
            }
            emit_result(result)?;
}
