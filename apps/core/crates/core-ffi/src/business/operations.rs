//! Dispatches business operations through the existing Core state and request transport.

use super::*;

#[cfg(test)]
pub(crate) fn dispatch(state: &mut State, operation: &str, input: Value) -> Result<Value> {
    dispatch_with_transport(state, operation, input, None)
}

pub(crate) fn dispatch_with_transport(state: &mut State, operation: &str, mut input: Value, transport: Option<crate::RequestTransport<'_>>) -> Result<Value> {
    if operation != "index_generation" { state.bind_model(&mut input)?; }
    if operation == "query" { crate::metadata::align_query_time(&mut input)?; }
    let query_settings: respire_core::query::QuerySettings = match input.get("query_settings") {
        Some(value) => serde_json::from_value(value.clone())?,
        None => Default::default(),
    };
    query_settings.validate()?;
    match operation {
        "resident_publish" => crate::resident::publish(input),
        "resident_release" => crate::resident::release(&input),
        "index_reusable" => {
            let before: MemoryEntry = serde_json::from_value(input["before"].clone())?;
            let after: MemoryEntry = serde_json::from_value(input["after"].clone())?;
            ensure!(before.id == after.id, "index reuse requires the same memory");
            Ok(json!(before.title == after.title && before.tags == after.tags && before.content == after.content))
        }
        "related_business" => crate::related::collect(input),
        "tree_float" => {
            let (memories, _) = snapshots(take(&mut input, "snapshots"), None)?;
            let min_hits = input["min_hits"].as_i64().context("missing min_hits")?;
            // P0-2 contract: hosts pass explicit query-log --good counts; the
            // float criterion no longer reads recall_count. Missing or malformed
            // good_counts is a loud error, never a silent fallback to heat.
            let counts = input["good_counts"].as_object().context("missing good_counts")?;
            let mut good_counts = HashMap::with_capacity(counts.len());
            for (id, value) in counts {
                let count = value
                    .as_i64()
                    .with_context(|| format!("good_counts[{id}] must be an integer"))?;
                good_counts.insert(id.clone(), count);
            }
            Ok(serde_json::to_value(crate::analysis::float_up_candidates(&memories, min_hits, &good_counts))?)
        }
        "maintenance_plan" => {
            let (memories, _) = snapshots(take(&mut input, "snapshots"), None)?;
            crate::analysis::maintenance_plan(&memories, count(&input, "max_chars")?, count(&input, "min_kids")?, count(&input, "segments")?)
        }
        "recall_policy" => crate::selection::policy(text(&input, "mode")?, text(&input, "query")?, count(&input, "limit")?, input["empty"].as_bool().context("missing empty flag")?),
        "selection_plan" => {
            let candidates: Vec<respire_core::query::RecalledWithContext> = serde_json::from_value(input["candidates"].clone())?;
            crate::selection::request(text(&input, "query")?, &candidates, count(&input, "limit")?, text(&input, "model")?)
        }
        "selection_evaluate" => {
            let candidates: Vec<respire_core::query::RecalledWithContext> = serde_json::from_value(input["candidates"].clone())?;
            let count = count(&input, "limit")?.max(1).min(candidates.len());
            Ok(serde_json::to_value(crate::selection::evaluate(&input["response"], candidates, count)?)?)
        }
        "model_probe" => {
            let name = text(&input, "model")?;
            if state.config.as_ref().is_some_and(|config| config["backend"].as_str() == Some("host_transport")) {
                let (provider, _lease) = state.model_with_transport(name, transport)?;
                provider.try_embed_query(text(&input,"text")?)?;
                return Ok(json!({"selected":"host_transport","ready":true,"dimensions":provider.dims(),"model":name}));
            }
            let (_, provider) = state.provider.as_ref().filter(|(model, provider)| model == name || provider.index_identity() == name)
                .context("model bytes have not been supplied by the host")?;
            Ok(provider.probe(text(&input, "text")?)?)
        }
        "model_status" => {
            let (provider, _model_use) = state.model_with_transport(text(&input, "model")?, transport.clone())?;
            Ok(json!({"dimensions":provider.dims()}))
        }
        "taxonomy_classify" => {
            let (provider, _model_use) = state.model_with_transport(text(&input, "model")?, transport.clone())?;
            Ok(serde_json::to_value(crate::taxonomy::classify(&provider, text(&input, "text")?)?)?)
        }
        "prepare" => {
            let name = text(&input, "model")?;
            let (provider, _model_use) = if let Some((_, loaded)) = state.provider.as_ref().filter(|(model, provider)| model == name || provider.index_identity() == name) {
                (Provider { embedder: Box::new(loaded.for_background()?) }, Some(loaded.lease()?))
            } else { state.model_with_transport(name, transport.clone())? };
            let (document, content) = if input.get("entry").is_some() {
                let entry = MemoryEntry::deserialize(&input["entry"])?;
                let head: String = entry.content.chars().take(400).collect();
                (respire_core::search::document_text(&entry.title, &head), entry.content)
            } else {
                let content = text(&input, "content")?.to_owned();
                (content.clone(), content)
            };
            let vector = provider.try_embed(&document)?;
            ensure!(vector.len() == provider.dims() && vector.iter().all(|v| v.is_finite()) && vector.iter().any(|v| v.abs() > 1e-8), "inference produced an invalid embedding; retry the command and inspect `rsrs doctor` if it persists");
            let chunks = respire_core::chunks::embed_chunks(&provider, &content)?;
            Ok(serde_json::to_value(Prepared { artifact:artifact(text(&input, "model")?, &vector, &chunks)?, legacy:String::new(), chunks:Vec::new() })?)
        }
        "remember_candidates" => {
            let model = text(&input, "model")?.to_owned();
            let view = crate::resident::view(&mut input, &model)?;
            let request_memories = crate::resident::retrieval_memories(&view, input["lexical_only"].as_bool() == Some(true));
            let (provider, _model_use) = retrieval_provider(state, &input, &request_memories, transport.clone())?;
            let (memories, entries) = (request_memories.as_ref(), &view.entries);
            let query = MemoryQuery::deserialize(&input["query"])?;
            let ranked = respire_core::query::MemoryEngine::recall_local_scored(&entries, &provider, &memories, &query, &query_settings)?;
            let mut merge = Vec::new();
            let mut parent = Vec::new();
            for candidate in ranked {
                if candidate.0 >= 0.45 { merge.push(candidate); }
                else if candidate.0 >= 0.30 && candidate.0 < 0.45 { parent.push(candidate); }
            }
            Ok(json!({"merge": merge, "parent": parent}))
        }
        "query" => query_with_transport(state, &mut input, &query_settings, transport),
        "candidate_report" => {
            let model = text(&input, "model")?.to_owned();
            let view = crate::resident::view(&mut input, &model)?;
            let request_memories = crate::resident::retrieval_memories(&view, input["lexical_only"].as_bool() == Some(true));
            let (provider, _model_use) = retrieval_provider(state, &input, &request_memories, transport.clone())?;
            let (memories, entries) = (request_memories.as_ref(), &view.entries);
            Ok(serde_json::to_value(crate::analysis::candidate_report(&entries, &provider, &memories, text(&input, "content")?, &query_settings)?)?)
        }
        "analyze_duplicates" => {
            let (memories, _) = snapshots(take(&mut input, "snapshots"), None)?;
            let mut report = serde_json::to_value(respire_core::defrag::analyze(&memories, score(&input, "min")?)?)?;
            if let Some(fields) = report.as_object_mut() {
                for name in ["cos_p10", "cos_p50", "cos_p90"] { fields.remove(name); }
                if let Some(clusters) = fields.get_mut("clusters").and_then(Value::as_array_mut) {
                    for cluster in clusters { if let Some(fields) = cluster.as_object_mut() { fields.remove("max_sim"); } }
                }
            }
            Ok(report)
        }
        "tree_cure" => {
            let (mut memories, _) = snapshots(take(&mut input, "snapshots"), None)?;
            let top = count(&input, "top")?;
            let minimum = score(&input, "min")?;
            let mut report = crate::analysis::tree_cure(&memories, top, minimum)?;
            if input["auto"].as_bool() == Some(true) {
                let rounds = if input.get("rounds").is_some() { count(&input, "rounds")? } else { 20 };
                let mut final_actions = Vec::new();
                for _ in 0..rounds {
                    let current = crate::analysis::tree_cure(&memories, 60, minimum)?;
                    if current.suggests.is_empty() { break; }
                    for action in current.suggests {
                        let Some(memory) = memories.iter_mut().find(|memory| memory.id == action.orphan.id) else { bail!("invalid Core action ID"); };
                        memory.local_parent_id = action.target.id.clone();
                        final_actions.push(action);
                    }
                }
                report.suggests = final_actions;
            }
            Ok(serde_json::to_value(report)?)
        }
        "deepen_plan" => {
            let (memories, _) = snapshots(take(&mut input, "snapshots"), None)?;
            Ok(serde_json::to_value(crate::analysis::deepen_plan(&memories, text(&input, "root")?, score(&input, "min")?, 4)?)?)
        }
        "index_generation" => {
            let model = text(&input,"model")?;
            let identity = if let Some(config) = input.get("config") { ensure!(config["model"].as_str() == Some(model), "request model and embedding configuration differ"); respire_core::embedding::index_identity(model, config)? }
            else if let Some((_, provider)) = state.provider.as_ref().filter(|(name, _)| name == model) { provider.index_identity().to_owned() }
            else if let Some(config) = state.config.as_ref().filter(|config| config["backend"].as_str() == Some("host_transport") && config["model"].as_str() == Some(model)) { respire_core::embedding::index_identity(model, config)? }
            else { ensure!(model == "m3" || model.starts_with("test-hash:"), "custom index generation requires config"); model.to_owned() };
            Ok(json!(digest(format!("{identity}:{INDEX_REVISION}:locator-v1").as_bytes())))
        },
        "index_status" => {
            let artifacts: Vec<String> = serde_json::from_value(take(&mut input, "artifacts"))?;
            for artifact in artifacts {
                if let Err(error) = read_artifact(&artifact) {
                    return Err(error);
                }
            }
            Ok(json!(true))
        }
        "import_legacy_features" => {
            bail!("unsupported business operation")
        }
        "export_legacy_features" => {
            bail!("unsupported business operation")
        }
        "engine_control" => {
            use respire_core::onnx;
            let action = text(&input, "action")?;
            match action {
                #[cfg(feature = "native-fault-tests")]
                "test_native_fault" => { onnx::arm_native_fault(input["mode"].as_u64().context("fixture mode missing")?)?; return Ok(json!({"armed":true})); },
                "inference_status" => return onnx::inference_status(),
                "get" => return state.config.as_ref().and_then(|config| config.get("engine")).cloned()
                    .context("engine configuration has not been supplied by the host"),
                "set" | "reset_cpu" => bail!("engine configuration is host-managed; supply model bytes with the new configuration"),
                "reset" => { onnx::reset_sessions()?; state.provider = None; state.config = None; },
                "accelerator_catalog" | "install_accelerators" => bail!("accelerator discovery and installation are host-managed"),
                _ => bail!("unknown engine action"),
            }
            Ok(Value::Null)
        }
        _ => bail!("unknown business operation"),
    }
}
