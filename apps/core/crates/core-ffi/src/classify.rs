//! classify - batch category suggestions: TypeSafe JEV and a DS-emulation backend (added 2026-09-20).
//!
//! Why: tidy "classify" used to be one AI-session round after another, slow. JEV (System One)
//! returns a typed choice + probabilities + confidence in one request, at $42 / billion input
//! tokens - cheap at batch scale. TypeSafe waitlist means we also keep a DS backend: OpenAI-
//! compatible `logprobs` + `top_logprobs` on the first token, emulating JEV Choice + confidence
//! (`--ds`; thinking must be off or reasoning_content pollutes the first token).
//!
//! This module is "memory -> judge request" pure logic: **read-only**.
//! (suggestions only). Apply reparents via resort/attach. Default reads local plaintext index
//! columns (title/kind/tags/content_head); does not unlock or read ciphertext - no keys leak.
//!
//! API contract (docs.typesafe.ai/api): POST {base}, Authorization: Bearer key;
//! body `{state, model, questions{id:{type,instructions,criteria}}}`;
//! resp `{answers{id:{choice,probabilities,confidence}}, usage}`.
//! Official note: CJK accuracy is lower (primary English) - rechecked in practice.
//!
//! DS contract (OpenAI-compatible): POST {base}/chat/completions, body includes
//! `logprobs:true, top_logprobs:20, thinking:{type:disabled}`; catalog titles numbered 1..N;
//! first-token top_logprobs map back to titles (not RLCD-calibrated; threshold is yours).

use std::collections::BTreeMap;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::taxonomy::CATALOG;

/// How many candidate-token probabilities the DS backend asks for (covers 23 catalog numbers + slack).
pub const DS_TOP_LOGPROBS: usize = 20;
/// Temperature when sampling to estimate probability.
/// On a degenerate endpoint, confidence can only be estimated by "ask again, see if the answer holds" -
/// temperature=0 locks the answer, 5 identical votes, confidence always 1.00 (hit in practice),
/// so sampling must heat up to expose uncertainty. 1.0 distinguishes well (borderline 4:2, clear 6:0).
pub const DS_SAMPLE_TEMPERATURE: f64 = 1.0;

/// One verdict (the category Choice answer).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Verdict {
    pub choice: String,
    pub confidence: f32,
    #[serde(skip_serializing)]
    pub probabilities: BTreeMap<String, f32>,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Build state: local plaintext index columns (title/kind/tags/body head); body may be truncated.
pub fn build_state(
    title: &str,
    kind: &str,
    tags: &str,
    content_head: &str,
    max_chars: usize,
) -> Value {
    let content: String = content_head.chars().take(max_chars).collect();
    json!({
        "title": title,
        "kind": kind,
        "tags": tags,
        "content": content,
    })
}

/// Build questions with English display labels; persisted category keys stay unchanged.
pub fn build_questions() -> Value {
    let mut criteria = serde_json::Map::new();
    for c in CATALOG {
        criteria.insert(c.prompt_label.to_owned(), json!(c.prompt_gist));
    }
    json!({
        "category": {
            "type": "choice",
            "instructions": "A personal memory entry is provided in the state. Choose the single top-level category it belongs to. Judge by the semantic meaning of the whole entry, not by surface words. Prefer the most specific fitting category.",
            "criteria": criteria
        }
    })
}

/// Assemble the full request body.
pub fn build_request(
    title: &str,
    kind: &str,
    tags: &str,
    content_head: &str,
    max_chars: usize,
    model: &str,
) -> Value {
    json!({
        "state": build_state(title, kind, tags, content_head, max_chars),
        "model": model,
        "questions": build_questions(),
    })
}

/// Resolve only exact model-facing labels or historical category keys.
/// Returned category keys remain stable for existing roots and host consumers.
fn canonical_category(label: &str) -> Option<&'static str> {
    CATALOG
        .iter()
        .find(|c| c.prompt_label == label || c.title == label)
        .map(|c| c.title)
}

/// Parse the response: take the Choice answer of answers.category.
pub fn parse_response(v: &Value) -> Result<Verdict> {
    let ans = v
        .get("answers")
        .and_then(|a| a.get("category"))
        .ok_or_else(|| anyhow!("model response missing answers.category"))?;
    let choice = ans
        .get("choice")
        .and_then(|c| c.as_str())
        .ok_or_else(|| anyhow!("model answer missing category choice"))?;
    let choice = canonical_category(choice)
        .ok_or_else(|| anyhow!("model returned an unknown category"))?;
    let confidence = ans
        .get("confidence")
        .and_then(|c| c.as_f64())
        .unwrap_or(0.0) as f32;
    let mut probabilities = BTreeMap::new();
    if let Some(map) = ans.get("probabilities").and_then(|p| p.as_object()) {
        for (k, val) in map {
            if let Some(f) = val.as_f64() {
                // Do not add the same category twice if a response includes both labels.
                let key = canonical_category(k).unwrap_or(k).to_owned();
                let value = f as f32;
                probabilities
                    .entry(key)
                    .and_modify(|old: &mut f32| *old = old.max(value))
                    .or_insert(value);
            }
        }
    }
    let usage = v.get("usage");
    let input_tokens = usage
        .and_then(|u| u.get("input_tokens"))
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    let output_tokens = usage
        .and_then(|u| u.get("output_tokens"))
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    Ok(Verdict {
        choice: choice.to_owned(),
        confidence,
        probabilities,
        input_tokens,
        output_tokens,
    })
}

// ------------------------- DS backend (OpenAI-compatible + logprobs emulating JEV) -------------------------

/// Catalog titles (catalog order is the 1-based numbering).
pub fn catalog_titles() -> Vec<&'static str> {
    CATALOG.iter().map(|c| c.title).collect()
}

/// DS request body: numbered question + logprobs + thinking off.
/// Thinking off is required - default reasoning emits reasoning_content first, so the first token is not the answer.
///
/// Historical prior-language `aidom_shot` benchmark (2026-09-20 tuning, not validation of the English projection below):
/// 91.3% on a 23-item tune set, a stable 95.7% on a 23-item holdout (3 reruns), beating plain (tune 82.6%/holdout 91.3%),
/// gist (73.9%), rules (78.3%), aidom with no examples (65.2%), shot2 long examples (holdout 91.3% and more expensive).
/// The English labels/examples have not been behaviorally re-evaluated; do not reuse those accuracy figures for them.
/// Point: **prefer the three AI-domain roots** - if an entry is both "programming/work" and "steps/lessons", pick the AI-domain root first;
/// otherwise plain sucks pit-log / experience / skill entries into a human-domain topic tree (the main error in practice).
pub fn build_ds_request(
    title: &str,
    kind: &str,
    tags: &str,
    content_head: &str,
    max_chars: usize,
    model: &str,
) -> Value {
    let content: String = content_head.chars().take(max_chars).collect();
    let opts: String = CATALOG
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{}. {}", i + 1, c.prompt_label))
        .collect::<Vec<_>>()
        .join("\n");
    let user = format!(
        "Classification: if this is knowledge an AI should reuse, prefer the three AI-domain roots; \
         otherwise pick a human-domain topic root.\n\
         Examples:\n  \
         entry \"compile error unexpected argument, this version has no such flag, use another approach\" -> Pitfalls\n  \
         entry \"step 1 outline, step 2 generate pages, step 3 export pptx\" -> Skills\n  \
         entry \"everything is a filesystem so it is readable; three layers load on demand to cut tokens\" -> Experience\n  \
         entry \"set compose project-name with a run id, CI 12 greens\" -> Programming and development\n\n\
         Memory entry:\nTitle: {title}\nKind: {kind}\nTags: {tags}\nBody: {content}\n\n\
         Output only the number:\n{opts}"
    );
    json!({
        "model": model,
        "messages": [
            {"role": "system", "content": "You classify memory entries. Follow the instructions and examples. Output only one number. No explanation, no thinking, no other characters."},
            {"role": "user", "content": user}
        ],
        "max_tokens": 4,
        "temperature": 0,
        "logprobs": true,
        "top_logprobs": DS_TOP_LOGPROBS,
        "thinking": {"type": "disabled"},
    })
}

/// Whether a position's probability distribution is **degenerate**.
/// Official DeepSeek top_logprobs returns `0.0` and `-9999.0` (not real probabilities);
/// b.ai-style relays return real values - on degenerate, fall back to sampling, or conf fakes 1.00.
pub fn logprobs_degenerate(entries: &[Value]) -> bool {
    let first = match entries.first() {
        Some(f) => f,
        None => return true,
    };
    let alts = match first.get("top_logprobs").and_then(|t| t.as_array()) {
        Some(a) if a.len() > 1 => a,
        _ => return true, // no candidates = no distribution
    };
    // Real values: at least one candidate whose logprob is neither 0 nor -9999
    !alts.iter().any(|a| {
        a.get("logprob")
            .and_then(|x| x.as_f64())
            .map(|lp| lp != 0.0 && lp > -9999.0)
            .unwrap_or(false)
    })
}

/// First-token probability distribution from one response (number -> probability). None when degenerate.
pub fn ds_probs_from(v: &Value) -> Option<BTreeMap<String, f32>> {
    let entries = v
        .get("choices")?
        .get(0)?
        .get("logprobs")?
        .get("content")?
        .as_array()?;
    if logprobs_degenerate(entries) {
        return None;
    }
    let first = entries.first()?;
    let titles = catalog_titles();
    // The first token itself and top_logprobs repeat the same token (observed API behavior),
    // so dedupe by token (keep max logprob) then merge by catalog title - else confidence doubles.
    let mut per_token: BTreeMap<String, f64> = BTreeMap::new();
    let mut record = |tok: &str, lp: f64| {
        let t = tok.trim().to_owned();
        if t.is_empty() {
            return;
        }
        per_token
            .entry(t)
            .and_modify(|old| {
                if lp > *old {
                    *old = lp;
                }
            })
            .or_insert(lp);
    };
    record(
        first.get("token").and_then(|t| t.as_str()).unwrap_or(""),
        first
            .get("logprob")
            .and_then(|x| x.as_f64())
            .unwrap_or(f64::NEG_INFINITY),
    );
    for a in first.get("top_logprobs")?.as_array()? {
        record(
            a.get("token").and_then(|t| t.as_str()).unwrap_or(""),
            a.get("logprob")
                .and_then(|x| x.as_f64())
                .unwrap_or(f64::NEG_INFINITY),
        );
    }
    let mut probs: BTreeMap<String, f32> = BTreeMap::new();
    for (tok, lp) in &per_token {
        if let Ok(n) = tok.trim_end_matches('.').parse::<usize>() {
            if let Some(name) = titles.get(n.wrapping_sub(1)) {
                *probs.entry((*name).to_owned()).or_insert(0.0) += lp.exp() as f32;
            }
        }
    }
    if probs.is_empty() {
        None
    } else {
        Some(probs)
    }
}

/// Choice from the response (first-token number first, else full content).
pub fn ds_choice_from(v: &Value) -> Option<String> {
    let titles = catalog_titles();
    let pick = |s: &str| -> Option<String> {
        let t = s.trim().trim_end_matches('.').trim();
        t.parse::<usize>()
            .ok()
            .and_then(|n| titles.get(n.wrapping_sub(1)).map(|s| (*s).to_owned()))
    };
    let choice0 = v.get("choices")?.get(0)?;
    let first_tok = choice0
        .get("logprobs")?
        .get("content")?
        .as_array()?
        .first()?
        .get("token")
        .and_then(|t| t.as_str())
        .unwrap_or("");
    let text = choice0
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    pick(first_tok).or_else(|| pick(text))
}

/// Parse a DS response (when the endpoint returns real logprobs; degenerate -> empty probabilities, confidence=0).
pub fn parse_ds_response(v: &Value) -> Result<Verdict> {
    let choice = ds_choice_from(v)
        .ok_or_else(|| anyhow!("model response did not contain a valid category choice"))?;
    let probs = ds_probs_from(v).unwrap_or_default();
    let confidence = probs.get(&choice).copied().unwrap_or(0.0);
    let usage = v.get("usage");
    let input_tokens = usage
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    let output_tokens = usage
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    Ok(Verdict {
        choice,
        confidence,
        probabilities: probs,
        input_tokens,
        output_tokens,
    })
}

/// Sample to estimate probability: when endpoint logprobs are degenerate, **heat up** and repeat N times, then vote.
/// Cost is N× requests, for a usable confidence - the only honest approach on a degenerate endpoint.
/// Temperature must rise: at 0 the answer is locked, N identical votes -> conf fakes 1.00.
pub fn sample_evaluate(responses: &[std::result::Result<Value, String>]) -> Result<Verdict> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let (mut tok_in, mut tok_out) = (0u64, 0u64);
    let mut last_err: Option<anyhow::Error> = None;
    let mut ok = 0usize;
    for response in responses {
        match response
            .clone()
            .map_err(|error| anyhow!(error))
            .and_then(|v| {
                let c =
                    ds_choice_from(&v).ok_or_else(|| anyhow!("sample did not parse a choice"))?;
                let u = v.get("usage");
                Ok((
                    c,
                    u.and_then(|u| u.get("prompt_tokens"))
                        .and_then(|t| t.as_u64())
                        .unwrap_or(0),
                    u.and_then(|u| u.get("completion_tokens"))
                        .and_then(|t| t.as_u64())
                        .unwrap_or(0),
                ))
            }) {
            Ok((c, i, o)) => {
                *counts.entry(c).or_insert(0) += 1;
                tok_in += i;
                tok_out += o;
                ok += 1;
            }
            Err(e) => last_err = Some(e),
        }
    }
    if ok == 0 {
        return Err(last_err.unwrap_or_else(|| anyhow!("all samples failed")));
    }
    let total = ok as f32;
    let probabilities: BTreeMap<String, f32> = counts
        .iter()
        .map(|(k, c)| (k.clone(), *c as f32 / total))
        .collect();
    let (choice, best) = counts
        .iter()
        .max_by_key(|(_, c)| **c)
        .map(|(k, c)| (k.clone(), *c))
        .ok_or_else(|| anyhow!("sample produced no result"))?;
    Ok(Verdict {
        choice,
        confidence: best as f32 / total,
        probabilities,
        input_tokens: tok_in,
        output_tokens: tok_out,
    })
}

/// DS endpoint: append /chat/completions to base (tolerate with/without /v1 and a trailing slash).
pub fn ds_endpoint(base: &str) -> String {
    let b = base.trim_end_matches('/');
    if b.ends_with("/chat/completions") {
        b.to_owned()
    } else {
        format!("{b}/chat/completions")
    }
}

mod causal;
mod report;

pub use causal::*;
pub use report::*;

#[cfg(test)]
mod tests;
