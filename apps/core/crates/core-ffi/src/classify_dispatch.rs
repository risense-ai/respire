//! Whole model-backed business execution; no intermediate plan leaves Core.
use crate::classify::{self, ItemResult};
use anyhow::{bail, ensure, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Provider {
    pub name: String,
    pub model: String,
}

impl Provider {
    pub(super) fn validate(&self) -> Result<()> {
        ensure!(!self.model.trim().is_empty(), "model name is required");
        Ok(())
    }
    fn ds(&self) -> bool {
        self.name == "ds"
    }
}

pub(super) fn call_api(
    transport: &mut dyn crate::Transport,
    provider: &Provider,
    body: &Value,
    retries: u32,
    timeout: u64,
) -> Result<Value> {
    provider.validate()?;
    transport.request(&json!({"provider":provider.name,"body":body,"retries":retries,"timeout":timeout}))
}

#[derive(Deserialize, Clone)]
struct Snapshot {
    id: String,
    #[serde(default)]
    parent_id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    tags: String,
    #[serde(default)]
    content_head: String,
    #[serde(default)]
    importance: String,
    #[serde(default)]
    updated_at: String,
    #[serde(default)]
    deleted: bool,
}
impl Snapshot {
    fn row(&self) -> (String, String, String, bool) {
        (
            self.id.clone(),
            self.parent_id.clone(),
            self.title.clone(),
            self.importance == "important",
        )
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Options {
    mode: String,
    limit: usize,
    all: bool,
    root: Option<String>,
    max_chars: usize,
    min_confidence: f32,
    dry_run: bool,
    samples: usize,
    batch: usize,
    tree_depth: usize,
    min_kids: usize,
    segments: usize,
    rounds: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    snapshots: Vec<Snapshot>,
    backend: Option<Provider>,
    options: Options,
    // The binding supplies this profile-local locator root to all operations.
    // Metadata-only classification never reads or writes the index directory.
    #[serde(default, rename = "index_root")]
    _index_root: Option<String>,
}
#[derive(Default)]
struct Usage {
    input: u64,
    output: u64,
}
impl Usage {
    fn add(&mut self, response: &Value) {
        self.input += response["usage"]["prompt_tokens"]
            .as_u64()
            .or_else(|| response["usage"]["input_tokens"].as_u64())
            .unwrap_or(0);
        self.output += response["usage"]["completion_tokens"]
            .as_u64()
            .or_else(|| response["usage"]["output_tokens"].as_u64())
            .unwrap_or(0);
    }
    fn value(&self) -> Value {
        json!({"input":self.input,"output":self.output})
    }
}
fn response_text(response: &Value) -> Result<&str> {
    response["choices"][0]["message"]["content"]
        .as_str()
        .filter(|text| !text.trim().is_empty())
        .context("model returned no content")
}

fn selected<'a>(
    snapshots: &'a [Snapshot],
    options: &Options,
) -> Result<(Vec<&'a Snapshot>, usize, usize)> {
    let mut scope: HashSet<&str> = snapshots.iter().map(|s| s.id.as_str()).collect();
    if let Some(root) = options.root.as_deref().filter(|r| !r.is_empty()) {
        let matches: Vec<_> = snapshots
            .iter()
            .filter(|s| s.id == root || s.id.starts_with(root) || s.title == root)
            .collect();
        ensure!(
            matches.len() == 1,
            "classification root is missing or ambiguous"
        );
        scope = HashSet::from([matches[0].id.as_str()]);
        loop {
            let previous = scope.len();
            for snapshot in snapshots {
                if scope.contains(snapshot.parent_id.as_str()) {
                    scope.insert(snapshot.id.as_str());
                }
            }
            if scope.len() == previous {
                break;
            }
        }
    }
    let trivial = snapshots
        .iter()
        .filter(|s| scope.contains(s.id.as_str()) && s.importance != "important")
        .count();
    let mut list: Vec<_> = snapshots
        .iter()
        .filter(|s| scope.contains(s.id.as_str()) && s.importance == "important")
        .collect();
    list.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    let total = list.len();
    if !options.all {
        list.truncate(options.limit);
    }
    Ok((list, trivial, total))
}

fn category(
    transport: &mut dyn crate::Transport,
    provider: &Provider,
    list: &[&Snapshot],
    roots: &HashMap<String, String>,
    options: &Options,
    usage: &mut Usage,
    warnings: &mut Vec<String>,
) -> Result<Vec<ItemResult>> {
    let mut items = Vec::new();
    for snapshot in list {
        let body = if provider.ds() {
            classify::build_ds_request(
                &snapshot.title,
                &snapshot.kind,
                &snapshot.tags,
                &snapshot.content_head,
                options.max_chars,
                &provider.model,
            )
        } else {
            classify::build_request(
                &snapshot.title,
                &snapshot.kind,
                &snapshot.tags,
                &snapshot.content_head,
                options.max_chars,
                &provider.model,
            )
        };
        let timeout = if provider.ds() { 180 } else { 60 };
        let result = call_api(transport, provider, &body, 3, timeout).and_then(|response| {
            usage.add(&response);
            if !provider.ds() {
                return classify::parse_response(&response);
            }
            if classify::ds_probs_from(&response).is_some() {
                return classify::parse_ds_response(&response);
            }
            if options.samples <= 1 {
                let mut verdict = classify::parse_ds_response(&response)?;
                verdict.confidence = 0.0;
                verdict.probabilities.clear();
                warnings.push(format!(
                    "{} has no confidence; configure multiple samples",
                    snapshot.id
                ));
                return Ok(verdict);
            }
            let mut sampled = body.clone();
            sampled["temperature"] = json!(classify::DS_SAMPLE_TEMPERATURE);
            let responses: Vec<_> = (0..options.samples)
                .map(|_| {
                    call_api(transport, provider, &sampled, 3, timeout)
                        .map(|r| {
                            usage.add(&r);
                            r
                        })
                        .map_err(|e| e.to_string())
                })
                .collect();
            let failures = responses.iter().filter(|r| r.is_err()).count();
            if failures > 0 {
                warnings.push(format!("{}: {failures} model samples failed", snapshot.id));
            }
            classify::sample_evaluate(&responses)
        });
        let (verdict, error) = match result {
            Ok(v) => (Some(v), None),
            Err(e) => (None, Some(e.to_string())),
        };
        items.push(ItemResult {
            id: snapshot.id.clone(),
            title: snapshot.title.clone(),
            current_root: roots.get(&snapshot.id).cloned().unwrap_or_default(),
            verdict,
            error,
            suggest_parent: None,
        });
    }
    Ok(items)
}

fn tree(
    transport: &mut dyn crate::Transport,
    provider: &Provider,
    list: &[&Snapshot],
    rows: &[(String, String, String, bool)],
    roots: &HashMap<String, String>,
    options: &Options,
    usage: &mut Usage,
) -> Result<Vec<ItemResult>> {
    let nodes = classify::build_tree_nodes(rows, options.tree_depth, 1);
    let (outline, index) = classify::render_tree_outline(&nodes);
    let titles: HashMap<_, _> = nodes
        .iter()
        .map(|n| (n.id.as_str(), n.title.as_str()))
        .collect();
    let mut items = Vec::new();
    for group in list.chunks(options.batch.max(1)) {
        let cases: Vec<_> = group
            .iter()
            .map(|s| (s.title.clone(), s.content_head.clone()))
            .collect();
        let body = classify::build_tree_batch_request(
            &outline,
            &cases,
            &provider.model,
            options.max_chars,
        );
        let parsed = call_api(transport, provider, &body, 3, if provider.ds() { 300 } else { 120 }).and_then(
            |response| {
                usage.add(&response);
                Ok(classify::parse_tree_batch(response_text(&response)?))
            },
        );
        for (i, snapshot) in group.iter().enumerate() {
            let result = parsed.as_ref().map_err(|e| e.to_string()).and_then(|map| {
                let id = map
                    .get(&i)
                    .and_then(|n| index.get(n))
                    .context("model did not return a valid parent")
                    .map_err(|e| e.to_string())?;
                Ok((
                    id.clone(),
                    titles.get(id.as_str()).copied().unwrap_or("").to_owned(),
                    roots.get(id).cloned().unwrap_or_default(),
                ))
            });
            let (suggest_parent, error) = match result {
                Ok(p) => (Some(p), None),
                Err(e) => (None, Some(e)),
            };
            items.push(ItemResult {
                id: snapshot.id.clone(),
                title: snapshot.title.clone(),
                current_root: roots.get(&snapshot.id).cloned().unwrap_or_default(),
                verdict: None,
                error,
                suggest_parent,
            });
        }
    }
    Ok(items)
}

fn causal_pass(
    transport: &mut dyn crate::Transport,
    provider: &Provider,
    rows: &[(String, String, String, bool)],
    options: &Options,
    usage: &mut Usage,
) -> Result<(Vec<(String, String)>, Vec<String>, usize)> {
    let by_id: HashMap<_, _> = rows.iter().map(|r| (r.0.as_str(), r)).collect();
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    for row in rows {
        if !row.1.is_empty() && by_id.contains_key(row.1.as_str()) {
            children.entry(&row.1).or_default().push(&row.0);
        }
    }
    let mut parents: Vec<_> = children
        .iter()
        .filter(|(_, kids)| kids.len() >= options.min_kids)
        .map(|(p, kids)| (*p, kids.len()))
        .collect();
    parents.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    if options.segments > 0 {
        parents.truncate(options.segments);
    }
    let mut operations = Vec::new();
    let mut issues = Vec::new();
    for (parent, _) in &parents {
        let title = by_id.get(parent).map(|r| r.2.as_str()).unwrap_or("");
        let kids = children.get(parent).context("missing causal segment")?;
        let cases: Vec<_> = kids
            .iter()
            .filter_map(|id| {
                by_id
                    .get(id)
                    .map(|r| (id.chars().take(8).collect(), r.2.clone()))
            })
            .collect();
        let scope: HashSet<_> = kids.iter().map(|id| (*id).to_owned()).collect();
        let body = classify::build_causal_siblings_request(
            &parent.chars().take(8).collect::<String>(),
            title,
            &cases,
            &provider.model,
        );
        match call_api(transport, provider, &body, 3, if provider.ds() { 300 } else { 120 }).and_then(
            |response| {
                usage.add(&response);
                Ok(classify::parse_causal_ops(response_text(&response)?))
            },
        ) {
            Ok(ops) => {
                let (valid, bad) = classify::validate_causal_ops(&ops, rows, Some(&scope));
                operations.extend(valid);
                issues.extend(bad);
            }
            Err(e) => issues.push(format!("segment {parent}: {e}")),
        }
    }
    let (valid, bad) = classify::detect_global_cycles(&operations, rows);
    issues.extend(bad);
    Ok((valid, issues, parents.len()))
}

fn structural_preview(rows: &[(String, String, String, bool)], list: &[&Snapshot]) -> Vec<Value> {
    let parent_of: HashMap<_, _> = rows
        .iter()
        .map(|row| (row.0.as_str(), row.1.as_str()))
        .collect();
    let selected: HashSet<_> = list.iter().map(|snapshot| snapshot.id.as_str()).collect();
    let mut detach = HashSet::new();
    for snapshot in list {
        if !snapshot.parent_id.is_empty() && !parent_of.contains_key(snapshot.parent_id.as_str()) {
            detach.insert(snapshot.id.as_str());
        }
        let mut path = Vec::new();
        let mut positions = HashMap::new();
        let mut current = snapshot.id.as_str();
        while let Some(parent) = parent_of.get(current) {
            if let Some(position) = positions.get(current).copied() {
                if let Some(id) = path[position..]
                    .iter()
                    .copied()
                    .filter(|id| selected.contains(id))
                    .min()
                {
                    detach.insert(id);
                }
                break;
            }
            positions.insert(current, path.len());
            path.push(current);
            current = parent;
        }
    }
    let mut ids: Vec<_> = detach.into_iter().collect();
    ids.sort_unstable();
    ids.into_iter()
        .map(|id| json!({"id":id,"parent_id":""}))
        .collect()
}

pub fn dispatch(value: Value, transport: &mut dyn crate::Transport) -> Result<Value> {
    let input: Input = serde_json::from_value(value)?;
    let options = &input.options;
    ensure!(
        matches!(
            options.mode.as_str(),
            "standard" | "tree" | "causal" | "auto" | "preview"
        ),
        "unknown classification mode"
    );
    ensure!(
        options.min_confidence.is_finite() && (0.0..=1.0).contains(&options.min_confidence),
        "invalid minimum confidence"
    );
    let snapshots: Vec<_> = input.snapshots.into_iter().filter(|s| !s.deleted).collect();
    let ids: HashSet<_> = snapshots.iter().map(|s| s.id.as_str()).collect();
    ensure!(ids.len() == snapshots.len(), "duplicate snapshot ID");
    let (list, trivial, pool_total) = selected(&snapshots, options)?;
    let mut rows: Vec<_> = snapshots.iter().map(Snapshot::row).collect();
    let roots = classify::top_roots(
        &rows
            .iter()
            .map(|r| (r.0.clone(), r.1.clone(), r.2.clone()))
            .collect::<Vec<_>>(),
    );
    if options.mode == "preview" || options.dry_run {
        let actions = structural_preview(&rows, &list);
        return Ok(
            json!({"report":null,"actions":actions,"warnings":["Local structural repair preview only; no semantic classification, model request or changes were applied"],"summary":{"total":list.len(),"pool_total":pool_total,"trivial_skipped":trivial,"proposed":actions.len(),"dry_run":true},"items":[]}),
        );
    }
    ensure!(
        !list.is_empty(),
        "nothing to classify: important tier is empty or scope excluded everything"
    );
    let provider = input
        .backend
        .as_ref()
        .context("classification requires an explicitly configured model provider")?;
    ensure!(
        matches!(provider.name.as_str(), "ds" | "typesafe"),
        "unknown model backend"
    );
    provider.validate()?;
    let mut usage = Usage::default();
    let mut warnings = Vec::new();
    let mut operations = Vec::new();
    let items = match options.mode.as_str() {
        "standard" => category(transport, provider, &list, &roots, options, &mut usage, &mut warnings)?,
        "tree" => tree(transport, provider, &list, &rows, &roots, options, &mut usage)?,
        "causal" | "auto" => {
            let original: HashMap<_, _> = rows.iter().map(|r| (r.0.clone(), r.1.clone())).collect();
            let mut previous = None;
            for _ in 0..if options.mode == "auto" {
                options.rounds.max(1)
            } else {
                1
            } {
                let (ops, issues, segments) = causal_pass(transport, provider, &rows, options, &mut usage)?;
                warnings.extend(issues);
                if segments == 0 || ops.is_empty() {
                    break;
                }
                let count = ops.len();
                for (id, parent) in ops {
                    if let Some(row) = rows.iter_mut().find(|r| r.0 == id) {
                        row.1 = parent;
                    }
                }
                if let Some(prev) = previous {
                    if count * 10 >= prev * 9 {
                        break;
                    }
                }
                previous = Some(count);
            }
            operations = rows
                .iter()
                .filter(|r| original.get(&r.0) != Some(&r.1))
                .map(|r| (r.0.clone(), r.1.clone()))
                .collect();
            let depth = |id: &str| {
                let mut at = id;
                let mut seen = HashSet::new();
                while let Some(row) = rows.iter().find(|r| r.0 == at) {
                    if !seen.insert(at) {
                        break;
                    }
                    at = &row.1;
                }
                seen.len()
            };
            operations.sort_by_key(|(id, _)| depth(id));
            Vec::new()
        }
        _ => bail!("unknown classification mode"),
    };
    let report = if options.mode == "standard" {
        Some(classify::build_report(&items, options.min_confidence))
    } else {
        None
    };
    let actions: Vec<_> = operations
        .iter()
        .map(|(id, parent)| json!({"id":id,"parent_id":parent}))
        .collect();
    Ok(
        json!({"report":report,"actions":actions,"warnings":warnings,"summary":{"total":list.len(),"pool_total":pool_total,"trivial_skipped":trivial,"truncated":list.len()<pool_total,"tokens":usage.value(),"proposed":actions.len(),"dry_run":false},"items":items}),
    )
}
