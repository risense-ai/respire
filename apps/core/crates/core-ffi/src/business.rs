//! Core owns derived indexes and model execution; account encryption keys stay outside.
use anyhow::{bail, ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use respire_core::{bge::BgeEmbedder, search::EmbeddingSource};
use respire_protocol::{MemoryEntry, MemoryQuery};
use respire_core::StoredMemory;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use sha2::{Digest, Sha256};

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepared {
    #[serde(default)]
    artifact: String,
    #[serde(default, skip_serializing)]
    legacy: String,
    #[serde(default, skip_serializing)]
    chunks: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    version: u32,
    revision: String,
    model: String,
    document: Vec<f32>,
    chunks: Vec<Vec<f32>>,
}

fn artifact(model: &str, document: &[f32], chunks: &[Vec<f32>]) -> Result<String> {
    #[derive(Serialize)]
    struct ArtifactRef<'a> {
        version: u32,
        revision: &'a str,
        model: &'a str,
        document: &'a [f32],
        chunks: &'a [Vec<f32>],
    }
    let value = ArtifactRef {version:1, revision:INDEX_REVISION, model, document, chunks};
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    serde_json::to_writer(&mut encoder, &value)?;
    let bytes = encoder.finish()?;
    Ok(STANDARD.encode(bytes))
}

fn read_artifact(encoded: &str) -> Result<Artifact> {
    let bytes = STANDARD.decode(encoded).context("invalid artifact encoding")?;
    ensure!(!bytes.starts_with(b"rsi1:"), "host must supply resolved opaque artifact bytes");
    let decoder = flate2::read::ZlibDecoder::new(bytes.as_slice());
    let artifact: Artifact = serde_json::from_reader(decoder).context("invalid Core artifact")?;
    ensure!(artifact.version == 1, "unsupported Core artifact version; rebuild the index");
    ensure!(artifact.revision == INDEX_REVISION, "unsupported Core artifact revision; rebuild the index");
    ensure!(artifact.document.iter().chain(artifact.chunks.iter().flatten()).all(|v| v.is_finite()), "non-finite Core artifact");
    ensure!(!artifact.document.is_empty() && artifact.chunks.iter().all(|chunk| chunk.len() == artifact.document.len()), "invalid Core artifact dimensions");
    Ok(artifact)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    id: String, deleted: bool, updated_at: String,
    kind: String, tags: String, title: String, project: String, computer: String,
    parent_id: String, created_at: String, content_head: String, recall_count: i64,
    importance: String, entry: Option<MemoryEntry>, features: Option<Prepared>,
}

fn decode(bytes: &str) -> Result<Vec<f32>> {
    let bytes = STANDARD.decode(bytes).context("invalid feature encoding")?;
    ensure!(bytes.len() % 4 == 0, "corrupt legacy feature length");
    let values: Vec<f32> = bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
    ensure!(values.iter().all(|value| value.is_finite()), "non-finite feature");
    Ok(values)
}

struct Provider<'call> {
    embedder: Box<dyn respire_core::search::EmbeddingSource + 'call>,
}
impl respire_core::search::EmbeddingSource for Provider<'_> {
    fn dims(&self) -> usize { self.embedder.dims() }
    fn try_embed(&self, text: &str) -> Result<Vec<f32>> { self.embedder.try_embed(text) }
    fn try_embed_query(&self, text: &str) -> Result<Vec<f32>> { self.embedder.try_embed_query(text) }
    fn split_text(&self, text: &str) -> Result<Vec<String>> { self.embedder.split_text(text) }
}

struct External<'call> {
    profile: respire_core::embedding::EmbeddingProfile,
    provider: String,
    timeout: u64,
    transport: crate::RequestTransport<'call>,
}
impl respire_core::search::EmbeddingSource for External<'_> {
    fn dims(&self) -> usize { self.profile.dimensions }
    fn try_embed(&self, text: &str) -> Result<Vec<f32>> { self.run(text, false) }
    fn try_embed_query(&self, text: &str) -> Result<Vec<f32>> { self.run(text, true) }
    fn split_text(&self, text: &str) -> Result<Vec<String>> { Ok(vec![text.to_owned()]) }
}
impl External<'_> {
    fn run(&self, text: &str, query: bool) -> Result<Vec<f32>> {
        use crate::Transport;
        let prefix = if query { &self.profile.query_prefix } else { &self.profile.document_prefix };
        let response = self.transport.clone().request(&json!({
            "operation":"embedding", "protocol":"openai_embeddings", "provider":self.provider,
            "model":self.profile.model_id,"input":[format!("{prefix}{text}")],
            "timeout":self.timeout,"retries":0
        }))?;
        let rows = response.get("data").and_then(Value::as_array).context("embedding transport response missing data")?;
        ensure!(rows.len() == 1 && rows[0]["index"].as_u64() == Some(0), "embedding transport response must contain exactly index 0");
        let embedding = rows[0].get("embedding").context("embedding transport response missing data[0].embedding")?;
        let mut vector: Vec<f32> = serde_json::from_value(embedding.clone()).context("invalid embedding transport vector")?;
        ensure!(vector.len() == self.profile.dimensions && vector.iter().all(|value| value.is_finite()), "embedding transport dimensions or values are invalid");
        let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
        ensure!(norm.is_finite() && norm > 1e-8, "invalid embedding transport norm");
        if self.profile.normalize { for value in &mut vector { *value /= norm; } }
        Ok(vector)
    }
}

// Each handle owns a reference to the injected model. No provider configuration
// is discovered from the host environment.
#[derive(Default)]
pub(super) struct State {
    provider: Option<(String, Arc<BgeEmbedder>)>,
    config: Option<Value>,
    test_mode: bool,
}

impl State {
    pub(super) fn new(test_mode: bool) -> Self { Self { provider: None, config: None, test_mode } }

    pub(super) fn load(&mut self, config: &Value, model_bytes: &[u8], tokenizer_bytes: &[u8]) -> Result<Value> {
        self.load_internal(config, model_bytes, tokenizer_bytes, None)
    }

    pub(super) fn load_with_options(&mut self, config: &Value, model_bytes: &[u8], tokenizer_bytes: &[u8], configure: &dyn Fn(*mut std::ffi::c_void, *mut std::ffi::c_void, *const std::ffi::c_void) -> Result<()>) -> Result<Value> {
        self.load_internal(config, model_bytes, tokenizer_bytes, Some(configure))
    }

    fn load_internal(&mut self, config: &Value, model_bytes: &[u8], tokenizer_bytes: &[u8], configure: Option<&dyn Fn(*mut std::ffi::c_void, *mut std::ffi::c_void, *const std::ffi::c_void) -> Result<()>>) -> Result<Value> {
        let name = text(config, "model")?;
        respire_core::embedding::validate_config(name, config)?;
        if config["backend"].as_str() == Some("host_transport") {
            ensure!(configure.is_none() && model_bytes.is_empty() && tokenizer_bytes.is_empty(), "host_transport must not include native options or model buffers");
            ensure!(config["protocol"].as_str().unwrap_or("openai_embeddings") == "openai_embeddings", "unsupported embedding transport protocol");
            ensure!(config["execution_timeout_secs"].as_u64().is_some_and(|value| value > 0), "host embedding requires a positive execution_timeout_secs");
            let profile = respire_core::embedding::EmbeddingProfile::from_config(name, config)?;
            let identity = respire_core::embedding::index_identity(name, config)?;
            BgeEmbedder::drop_shared()?;
            self.provider = None;
            self.config = Some(config.clone());
            return Ok(json!({"dimensions":profile.dimensions,"index_identity":identity}));
        }
        let loaded = if config.get("reuse_current").and_then(Value::as_bool) == Some(true) {
            ensure!(model_bytes.is_empty() && tokenizer_bytes.is_empty(), "cached reuse must not include model buffers");
            BgeEmbedder::load_cached(name, config)
        } else if let Some(configure) = configure {
            BgeEmbedder::load_bytes_with_native_options(name, model_bytes, tokenizer_bytes, config, configure)
        } else {
            BgeEmbedder::load_bytes(name, model_bytes, tokenizer_bytes, config)
        };
        let provider = match loaded {
            Ok(provider) => provider,
            Err(error) => {
                // A busy or invalid replacement can leave the previous group
                // healthy. Clear this handle only if that group was retired.
                if self.provider.as_ref().is_some_and(|(_, provider)| provider.lease().is_err()) {
                    self.provider = None;
                    self.config = None;
                }
                return Err(error);
            }
        };
        let provider = Arc::new(provider);
        let dimensions = provider.dims();
        self.provider = Some((name.to_owned(), provider));
        self.config = Some(config.clone());
        Ok(json!({"dimensions": dimensions,"index_identity":self.provider.as_ref().map(|(_, provider)| provider.index_identity())}))
    }

    #[cfg(feature = "cpu-owned-ort-experiment")]
    pub(super) fn load_owned(&mut self, config: &Value, model: respire_core::bge::ModelUpload, tokenizer_bytes: &[u8]) -> Result<Value> {
        let name = text(config, "model")?;
        respire_core::embedding::validate_config(name, config)?;
        let loaded = BgeEmbedder::load_streamed_bytes(name, model, tokenizer_bytes, config);
        let provider = match loaded {
            Ok(provider) => provider,
            Err(error) => {
                if self.provider.as_ref().is_some_and(|(_, provider)| provider.lease().is_err()) {
                    self.provider = None;
                    self.config = None;
                }
                return Err(error);
            }
        };
        let dimensions = provider.dims();
        self.provider = Some((name.to_owned(), Arc::new(provider)));
        self.config = Some(config.clone());
        Ok(json!({"dimensions": dimensions,"index_identity":self.provider.as_ref().map(|(_, provider)| provider.index_identity())}))
    }

    pub(super) fn request_model_lease(&self, input: &Value) -> Result<Option<respire_core::bge::ModelUse<'_>>> {
        if input["lexical_only"].as_bool() == Some(true) { return Ok(None); }
        let name = text(input, "model")?;
        if name.starts_with("test-hash:") {
            ensure!(self.test_mode, "test provider is disabled");
            return Ok(None);
        }
        if self.config.as_ref().is_some_and(|config| config["backend"].as_str() == Some("host_transport")) { return Ok(None); }
        let (_, provider) = self.provider.as_ref().filter(|(model, provider)| model == name || provider.index_identity() == name)
            .context("model bytes have not been supplied by the host")?;
        Ok(Some(provider.lease()?))
    }

    #[cfg(test)]
    fn model(&self, name: &str) -> Result<(Provider<'static>, Option<respire_core::bge::ModelUse<'_>>)> {
        self.model_with_transport(name, None)
    }

    fn model_with_transport<'call>(&self, name: &str, transport: Option<crate::RequestTransport<'call>>) -> Result<(Provider<'call>, Option<respire_core::bge::ModelUse<'_>>)> {
        if let Some(dimensions) = name.strip_prefix("test-hash:") {
            ensure!(self.test_mode, "test provider is disabled");
            let dimensions: usize = dimensions.parse()?;
            ensure!(dimensions > 0 && dimensions <= 1024, "invalid test provider dimensions");
            return Ok((Provider { embedder: Box::new(respire_core::search::HashingEmbedder::new(dimensions)) }, None));
        }
        if let Some(config) = self.config.as_ref().filter(|config| config["backend"].as_str() == Some("host_transport")) {
            let identity = respire_core::embedding::index_identity(text(config,"model")?, config)?;
            ensure!(config["model"].as_str() == Some(name) || identity == name, "embedding profile does not match the loaded configuration");
            return Ok((Provider { embedder:Box::new(External { profile:respire_core::embedding::EmbeddingProfile::from_config(text(config,"model")?, config)?, provider:text(config,"provider")?.to_owned(), timeout:config["execution_timeout_secs"].as_u64().filter(|value| *value > 0).context("host embedding requires a positive execution_timeout_secs")?, transport:transport.context("host embedding requires a transport callback on this call")? }) }, None));
        }
        let (_, provider) = self.provider.as_ref().filter(|(model, provider)| model == name || provider.index_identity() == name)
            .context("model bytes have not been supplied by the host")?;
        Ok((Provider { embedder: Box::new(provider.as_ref().clone()) }, Some(provider.lease()?)))
    }

    pub(super) fn bind_model(&self, input: &mut Value) -> Result<()> {
        let Some(name) = input.get("model").and_then(Value::as_str) else { return Ok(()); };
        let identity = if let Some(config) = input.get("config") {
            ensure!(config["model"].as_str() == Some(name), "request model and embedding configuration differ");
            Some(respire_core::embedding::index_identity(name, config)?)
        } else if let Some((_, provider)) = self.provider.as_ref().filter(|(model, _)| model == name) {
            Some(provider.index_identity().to_owned())
        } else if let Some(config) = self.config.as_ref().filter(|config| config["backend"].as_str() == Some("host_transport") && config["model"].as_str() == Some(name)) {
            Some(respire_core::embedding::index_identity(name, config)?)
        } else { None };
        if let Some(identity) = identity { input["model"] = json!(identity); }
        Ok(())
    }
}

const INDEX_REVISION: &str = "quantized-0826f8c1ab9edf1801db86c61919d4d108e8bfc0b809ec823ad366882ff0b77d-cls-token480-overlap48-body400-plain-v2";

pub(super) fn snapshots(value: Value, expected_model: Option<&str>) -> Result<(Vec<StoredMemory>, HashMap<String, MemoryEntry>)> {
    let snapshots = Vec::<Snapshot>::deserialize(value)?;
    let mut memories = Vec::with_capacity(snapshots.len());
    let mut entries = HashMap::new();
    let mut ids = std::collections::HashSet::new();
    for snapshot in snapshots {
        ensure!(ids.insert(snapshot.id.clone()), "duplicate snapshot ID");
        if let Some(entry) = snapshot.entry {
            ensure!(entry.id == snapshot.id, "snapshot entry ID mismatch");
            entries.insert(entry.id.clone(), entry);
        }
        let mut stored = StoredMemory::new_pending(snapshot.id);
        stored.deleted = snapshot.deleted;
        stored.updated_at = snapshot.updated_at;
        stored.local_kind = snapshot.kind;
        stored.local_tags = snapshot.tags;
        stored.local_title = snapshot.title;
        stored.local_project = snapshot.project;
        stored.local_computer = snapshot.computer;
        stored.local_parent_id = snapshot.parent_id;
        stored.local_created_at = snapshot.created_at;
        stored.local_content_head = snapshot.content_head;
        stored.local_recall_count = snapshot.recall_count;
        stored.local_importance = snapshot.importance;
        if let Some(entry) = entries.get(&stored.id) {
            stored.local_meta = entry.metadata.clone();
        }
        if let Some(features) = snapshot.features {
            if features.artifact.is_empty() {
                ensure!(expected_model.is_none_or(|model| model == "m3" || model.starts_with("test-hash:")), "legacy features do not identify the selected embedding profile; rebuild the index");
                stored.local_embedding = Some(decode(&features.legacy)?);
                stored.local_chunks = features.chunks.iter().map(|chunk| decode(chunk)).collect::<Result<_>>()?;
            } else {
                let artifact = read_artifact(&features.artifact)?;
                if let Some(model) = expected_model {
                    ensure!(artifact.model == model, "Core artifact belongs to another model; rebuild the index");
                }
                stored.local_embedding = Some(artifact.document);
                stored.local_chunks = artifact.chunks;
            }
        }
        if stored.local_embedding.is_none() && stored.local_chunks.is_empty() {
            if let Some(entry) = entries.get(&stored.id) {
                // Pending data remains searchable throughout its body before embedding completes.
                stored.local_content_head = entry.content.clone();
            }
        }
        memories.push(stored);
    }
    Ok((memories, entries))
}

/// Move large request fields into their decoder without cloning their strings.
/// A missing field stays null, matching read-only Value indexing on malformed requests.
pub(super) fn take(input: &mut Value, name: &str) -> Value {
    input.get_mut(name).map(Value::take).unwrap_or(Value::Null)
}

fn text<'a>(input: &'a Value, name: &str) -> Result<&'a str> {
    input[name].as_str().with_context(|| format!("missing string field {name}"))
}
fn count(input: &Value, name: &str) -> Result<usize> {
    usize::try_from(input[name].as_u64().with_context(|| format!("missing count {name}"))?).context("count overflow")
}
fn score(input: &Value, name: &str) -> Result<f32> {
    Ok(input[name].as_f64().with_context(|| format!("missing score {name}"))? as f32)
}

fn retrieval_provider<'a, 'call>(state: &'a State, input: &Value, memories: &[Arc<StoredMemory>], transport: Option<crate::RequestTransport<'call>>) -> Result<(Provider<'call>, Option<respire_core::bge::ModelUse<'a>>)> {
    if input["lexical_only"].as_bool() == Some(true) {
        ensure!(memories.iter().all(|memory| memory.local_embedding.is_none() && memory.local_chunks.is_empty()),
            "lexical-only retrieval requires a vector-free request view");
        Ok((Provider { embedder: Box::new(respire_core::search::HashingEmbedder::new(1024)) }, None))
    } else {
        let (provider, lease) = state.model_with_transport(text(input, "model")?, transport)?;
        ensure!(memories.iter().all(|memory| memory.local_embedding.as_ref().is_none_or(|vector| vector.len() == provider.dims()) && memory.local_chunks.iter().all(|vector| vector.len() == provider.dims())), "index embedding dimensions differ from the selected profile; rebuild the index");
        Ok((provider, lease))
    }
}

fn query_with_transport(state: &State, input: &mut Value, settings: &respire_core::query::QuerySettings, transport: Option<crate::RequestTransport<'_>>) -> Result<Value> {
    let mut render_budget = parse_render_budget(input)?;
    let model = text(input, "model")?.to_owned();
    let view = crate::resident::view(input, &model)?;
    let evidence = crate::metadata::query_context(input, &view)?;
    prepare_render_budget(render_budget.as_mut(), evidence.as_ref())?;
    let effective_view = evidence.as_ref().map_or(view.as_ref(), |context| &context.view);
    let result = query_view_with_transport(state, input, settings, effective_view, transport)?;
    let mode = text(input, "mode")?;
    let result = apply_render_budget(render_budget.as_ref(), result, mode, evidence.as_ref())?;
    match evidence { Some(context) => context.attach(result, mode), None => Ok(result) }
}

#[cfg(test)]
pub(super) fn query_view(state: &State, input: &Value, settings: &respire_core::query::QuerySettings, view: &crate::resident::View) -> Result<Value> {
    query_view_with_transport(state, input, settings, view, None)
}

pub(super) fn query_view_with_transport(state: &State, input: &Value, settings: &respire_core::query::QuerySettings, view: &crate::resident::View, transport: Option<crate::RequestTransport<'_>>) -> Result<Value> {
    let request_memories = crate::resident::retrieval_memories(view, input["lexical_only"].as_bool() == Some(true));
    let (provider, _model_use) = retrieval_provider(state, input, &request_memories, transport)?;
    let (memories, entries) = (request_memories.as_ref(), &view.entries);
    let query = MemoryQuery::deserialize(&input["query"])?;
    settings.lexical_indexes.replace(Some(std::sync::Arc::clone(&view.lexical_indexes)));
    match text(input, "mode")? {
        "plain" => Ok(serde_json::to_value(respire_core::query::MemoryEngine::recall_local(entries, &provider, memories, &query, settings)?)?),
        "scored" => Ok(serde_json::to_value(respire_core::query::MemoryEngine::recall_local_scored(entries, &provider, memories, &query, settings)?)?),
        "contextual" => Ok(serde_json::to_value(respire_core::query::MemoryEngine::recall_local_contextual(entries, &provider, memories, &query, settings)?)?),
        _ => bail!("unknown query mode"),
    }
}

pub(super) enum RenderBudgetRequest {
    Title(respire_protocol::RenderBudget),
    Brief(respire_core::representation::BriefRenderBudget),
}

pub(super) fn parse_render_budget(input: &Value) -> Result<Option<RenderBudgetRequest>> {
    let Some(value) = input.get("render_budget") else { return Ok(None); };
    if value.get("mode").is_some() {
        let request: respire_core::representation::BriefRenderBudget = serde_json::from_value(value.clone())?;
        request.validate()?;
        ensure!(input.get("evidence_context").is_some_and(|value| !value.is_null()),
            "brief rendering requires evidence_context");
        Ok(Some(RenderBudgetRequest::Brief(request)))
    } else {
        let request: respire_protocol::RenderBudget = serde_json::from_value(value.clone())?;
        ensure!(!request.prefix_by_count.is_empty(), "prefix_by_count must cover count zero");
        ensure!(request.items.iter().all(|item| !item.id.is_empty()), "render item ID must not be empty");
        Ok(Some(RenderBudgetRequest::Title(request)))
    }
}

pub(super) fn prepare_render_budget(
    request: Option<&mut RenderBudgetRequest>,
    evidence: Option<&crate::metadata::QueryContext>,
) -> Result<()> {
    match request {
        Some(RenderBudgetRequest::Title(request)) => {
            if let Some(context) = evidence { context.mark_budget(Some(request)); }
        }
        Some(RenderBudgetRequest::Brief(request)) => {
            let context = evidence.context("brief rendering requires evidence_context")?;
            respire_core::representation::validate_brief_bindings(request, &context.render_contexts())?;
        }
        None => {}
    }
    Ok(())
}

/// Budget only the final ranked result; internal quality-selection pools stay intact.
pub(super) fn apply_render_budget(
    request: Option<&RenderBudgetRequest>,
    mut result: Value,
    mode: &str,
    evidence: Option<&crate::metadata::QueryContext>,
) -> Result<Value> {
    let Some(request) = request else { return Ok(result); };
    let wrapped = result.is_object();
    let items = if wrapped { &mut result["items"] } else { &mut result };
    let items = items.as_array_mut().context("query result must contain an item array")?;
    let item_id = |item: &Value| -> Result<String> {
        let id = match mode {
            "plain" => &item["id"],
            "scored" => &item[1]["id"],
            "contextual" => &item["entry"]["id"],
            _ => bail!("unknown query mode"),
        };
        Ok(id.as_str().context("query result item is missing its ID")?.to_owned())
    };
    let ids: Vec<String> = items.iter().map(item_id).collect::<Result<_>>()?;
    let ranked_ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    let (mut selected, receipt): (std::collections::HashSet<String>, Value) = match request {
        RenderBudgetRequest::Title(request) => {
            let receipt = respire_core::query::select_render_budget(&ranked_ids, request)?;
            (receipt.items.iter().map(|item| item.id.clone()).collect(), serde_json::to_value(receipt)?)
        }
        RenderBudgetRequest::Brief(request) => {
            let context = evidence.context("brief rendering requires evidence_context")?;
            let receipt = respire_core::representation::select_brief_budget(&ranked_ids, request, &context.render_contexts())?;
            (receipt.items.iter().map(|item| item.id.clone()).collect(), serde_json::to_value(receipt)?)
        }
    };
    let mut index = 0;
    items.retain(|_| {
        let keep = selected.remove(ids[index].as_str());
        index += 1;
        keep
    });
    if wrapped {
        result["budget"] = receipt;
        Ok(result)
    } else {
        Ok(json!({"items":result,"budget":receipt}))
    }
}

mod operations;
#[cfg(test)]
pub(super) use operations::dispatch;
pub(super) use operations::dispatch_with_transport;

#[cfg(test)]
mod artifact_wire_tests;
#[cfg(test)]
mod model_lifetime_tests;
#[cfg(test)]
mod wire_contract_tests;
