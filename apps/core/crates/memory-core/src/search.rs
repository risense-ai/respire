//! memory::search — search layer: semantic embed + cosine rerank (local-first)
//!
//! Under local-first, search is all local: plaintext embedding columns live in the local store,
//! a query does full cosine + keyword fusion, no server.
//!
//! Old SearchKeys (blind index) is gone — the server no longer takes part in search.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::StoredMemory;

// ── semantic embed ──

/// Semantic-embed trait.
/// Demo uses HashingEmbedder (zero deps); production can swap fastembed/bge.
pub trait Embedder: Send + Sync {
    fn dims(&self) -> usize;
    fn embed(&self, text: &str) -> Vec<f32>;
    /// Preserve native inference errors before any storage mutation.
    fn try_embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        Ok(self.embed(text))
    }
    fn try_embed_query(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        Ok(self.embed_query(text))
    }
    fn split_text(&self, text: &str) -> anyhow::Result<Vec<String>> {
        Ok(vec![text.to_owned()])
    }

    /// Query-side embed (default same as embed; BGE-class models add an instruction prefix here).
    fn embed_query(&self, text: &str) -> Vec<f32> {
        self.embed(text)
    }
}

/// Request-scoped sources may borrow synchronous host transport without crossing threads.
pub trait EmbeddingSource {
    fn dims(&self) -> usize;
    fn try_embed(&self, text: &str) -> anyhow::Result<Vec<f32>>;
    fn try_embed_query(&self, text: &str) -> anyhow::Result<Vec<f32>>;
    fn split_text(&self, text: &str) -> anyhow::Result<Vec<String>>;
}
impl<T: Embedder + ?Sized> EmbeddingSource for T {
    fn dims(&self) -> usize { Embedder::dims(self) }
    fn try_embed(&self, text: &str) -> anyhow::Result<Vec<f32>> { Embedder::try_embed(self, text) }
    fn try_embed_query(&self, text: &str) -> anyhow::Result<Vec<f32>> { Embedder::try_embed_query(self, text) }
    fn split_text(&self, text: &str) -> anyhow::Result<Vec<String>> { Embedder::split_text(self, text) }
}

/// Char-bigram hashing embedder (**test double**).
///
/// Production always uses BgeEmbedder (frozen 2026-09-05: embedder is BGE everywhere, no fallback).
/// A real embedder needs an onnx model (hundreds of MB, seconds to load); tests cannot depend on it, so this double stays:
/// mismatched dims yield cosine 0, tests stay self-consistent (same embedder, same dims).
#[derive(Clone)]
pub struct HashingEmbedder {
    dims: usize,
}

impl HashingEmbedder {
    pub fn new(dims: usize) -> Self {
        Self { dims }
    }
}

impl Default for HashingEmbedder {
    fn default() -> Self {
        Self::new(256) // 256 dims, 1KB per f32 row
    }
}

impl<T: Embedder + ?Sized> Embedder for Box<T> {
    fn try_embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        (**self).try_embed(text)
    }
    fn try_embed_query(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        (**self).try_embed_query(text)
    }
    fn split_text(&self, text: &str) -> anyhow::Result<Vec<String>> {
        (**self).split_text(text)
    }
    fn dims(&self) -> usize {
        (**self).dims()
    }
    fn embed(&self, text: &str) -> Vec<f32> {
        (**self).embed(text)
    }
    fn embed_query(&self, text: &str) -> Vec<f32> {
        (**self).embed_query(text)
    }
}

impl Embedder for HashingEmbedder {
    fn dims(&self) -> usize {
        self.dims
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        let mut vec = vec![0f32; self.dims];
        let chars: Vec<char> = text.to_lowercase().chars().collect();

        // unigram (single char, weight 1)
        for &c in &chars {
            let idx = hash_char(c) % self.dims;
            vec[idx] += 1.0;
        }

        // bigram (char pair, weight 2)
        for pair in chars.windows(2) {
            let idx = hash_pair(pair[0], pair[1]) % self.dims;
            vec[idx] += 2.0;
        }

        // L2 normalize (a zero vector stays zero)
        let norm: f32 = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > 1e-8 {
            for v in &mut vec {
                *v /= norm;
            }
        }
        vec
    }
}

fn hash_char(c: char) -> usize {
    let mut h = DefaultHasher::new();
    c.hash(&mut h);
    h.finish() as usize
}

fn hash_pair(a: char, b: char) -> usize {
    let mut h = DefaultHasher::new();
    a.hash(&mut h);
    b.hash(&mut h);
    h.finish() as usize
}

// ── cosine ──

/// Cosine similarity. A zero vector vs anything returns 0.0.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|v| v * v).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|v| v * v).sum::<f32>().sqrt();
    if na < 1e-8 || nb < 1e-8 {
        return 0.0;
    }
    (dot / (na * nb)).clamp(-1.0, 1.0)
}

// ── centering (kill anisotropy: same-domain Chinese used to pile up at 0.6–0.9) ──

pub fn mean_vector(vecs: &[&[f32]]) -> Option<Vec<f32>> {
    let dim = vecs.iter().find(|vector| !vector.is_empty())?.len();
    let mut mean = vec![0.0; dim];
    let mut count = 0usize;
    for vector in vecs.iter().filter(|vector| vector.len() == dim) {
        let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
        if !norm.is_finite() || norm < 1e-8 { continue; }
        // Keep already normalized M3 arithmetic unchanged.
        let scale = if (norm - 1.0).abs() < 1e-6 { 1.0 } else { norm };
        for (sum, value) in mean.iter_mut().zip(vector.iter()) { *sum += value / scale; }
        count += 1;
    }
    if count == 0 { return None; }
    for value in &mut mean { *value /= count as f32; }
    Some(mean)
}

/// Center and re-normalize: v' = normalize(v − μ). After dropping the common offset, unrelated pairs land near zero,
/// true similar pairs stay high — the right scale for doc-doc clustering/dedup.
pub fn center_normalize(v: &[f32], mu: &[f32]) -> Option<Vec<f32>> {
    if v.len() != mu.len() || v.is_empty() {
        return None;
    }
    let source_norm = v.iter().map(|value| value * value).sum::<f32>().sqrt();
    if !source_norm.is_finite() || source_norm < 1e-8 { return None; }
    let scale = if (source_norm - 1.0).abs() < 1e-6 { 1.0 } else { source_norm };
    let mut c: Vec<f32> = v.iter().zip(mu.iter()).map(|(x, m)| x / scale - m).collect();
    let norm: f32 = c.iter().map(|x| x * x).sum::<f32>().sqrt();
    if !norm.is_finite() || norm < 1e-8 {
        return None;
    }
    for x in &mut c { *x /= norm; }
    Some(c)
}

/// Centered-domain cosine: cosine after both vectors are centered on the same μ.
pub fn centered_cosine(a: &[f32], b: &[f32], mu: &[f32]) -> f32 {
    centered_cosine_from(a, mu)(b)
}

/// Reuse the centered left vector when comparing one candidate with many others.
pub fn centered_cosine_from<'a>(a: &'a [f32], mu: &'a [f32]) -> impl Fn(&[f32]) -> f32 + 'a {
    let left = center_normalize(a, mu);
    move |b| match (left.as_deref(), center_normalize(b, mu)) {
        (Some(ca), Some(cb)) => cosine_similarity(ca, &cb),
        (None, None) => cosine_similarity(a, b),
        _ => 0.0,
    }
}

// ── rerank ──

/// Char-bigram set (no Chinese segmenter; char-bigram overlap is a reliable lexical signal).
fn char_bigrams(s: &str) -> std::collections::HashSet<String> {
    let chars: Vec<char> = s.to_lowercase().chars().collect();
    chars
        .windows(2)
        .map(|w| w.iter().collect::<String>())
        .collect()
}

/// CJK ideograph test — used to pick the lexical-hit shape per query language.
/// Hiragana/Katakana and Hangul are included: they are unspaced scripts with the same
/// "whole term copied in" property as Chinese.
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF      // Hiragana + Katakana
        | 0x3400..=0x4DBF    // CJK Extension A
        | 0x4E00..=0x9FFF    // CJK Unified Ideographs
        | 0xF900..=0xFAFF    // CJK Compatibility Ideographs
        | 0xAC00..=0xD7AF    // Hangul syllables
        | 0x20000..=0x2FA1F  // CJK Extension B+
    )
}

/// Lexical score (BM25-equivalent, local plaintext closed loop):
/// Chinese char-bigram overlap — weighted overlap of query vs title/tags/content_head bigram sets.
/// title is the main channel (0.5), content next (0.35, lexical body), tags (0.15);
/// a whole-string lexical hit also adds. Without a segmenter, contains() of a whole Chinese string is always false, so bigrams are the reliable signal.
/// Squaring is gone — BM25 does not square; KW_CAP caps fake highs.
fn keyword_score(
    q_words: &[String],
    q_bigrams: &std::collections::HashSet<String>,
    kind: &str,
    tags: &str,
    title: &str,
    content_head: &str,
) -> f32 {
    let mut score = 0.0f32;
    if !q_bigrams.is_empty() {
        let t_bigrams = char_bigrams(title);
        let c_bigrams = char_bigrams(content_head);
        let g_bigrams = char_bigrams(tags);
        let n = q_bigrams.len() as f32;
        let t = q_bigrams.iter().filter(|b| t_bigrams.contains(*b)).count() as f32 / n;
        let c = q_bigrams.iter().filter(|b| c_bigrams.contains(*b)).count() as f32 / n;
        let g = q_bigrams.iter().filter(|b| g_bigrams.contains(*b)).count() as f32 / n;
        score += t * 0.5 + c * 0.35 + g * 0.15;
    }
    // Whole-string lexical hit (English/digits/short words) bonus.
    //
    // Title and tag terms accumulate per hit: those fields are short, so a match is discriminative.
    // The **content head** term is language-dependent (decided 2026-09-25 from measurement):
    //
    // * Whitespace-tokenized queries (English) normalize by the query word count. The content head
    //   is a long text where nearly every common word (when / did / to / the) hits, so the per-hit
    //   `+= 0.2` saturated - measured on LoCoMo, 52.3% of entries pinned at the cap. Since
    //   `semantic` is `max(stretch, kw)`, a saturated kw bypassed the semantic axis and cost 39.1pp
    //   of session hit; normalizing recovered +30.5pp.
    // * Un-spaced queries (Chinese) keep accumulating. Chinese queries carry no whitespace tokens,
    //   so this term barely fires there anyway - but when it does (an error string or a proper
    //   noun copied verbatim), the absolute strength is the signal, and dividing by a tiny word
    //   count made it swing. Measured: normalizing cost Chinese hit@5 7.9pp.
    // Shape is decided by **the query's script**, and it applies to all three whole-string terms.
    //
    // History of this decision (2026-09-25, three iterations, each measured):
    //  * Per-hit accumulation everywhere saturated: on LoCoMo 52.3% of entries pinned at the cap,
    //    and on a 604-fact English library **54%**, which collapsed recall - every row scored
    //    0.980 because a handful of common words (Caroline / Melanie / said) hit every entry.
    //    Since `semantic` is `max(stretch, kw)`, a saturated kw bypasses the semantic axis.
    //  * Dividing by the query word count for *all* libraries cost Chinese hit@5 7.9pp: Chinese
    //    queries are short and specific (pitfalls, client startup, validation), and a hit there means a whole
    //    term was copied in, so its absolute strength is the signal.
    //  * Deciding by the *document's* whitespace count was wrong too (Chinese bodies contain
    //    plenty of spaces from code and English words): Chinese fell to 0.675.
    //
    // So: a query **without** spaced words keeps accumulating (its own word count is meaningless
    // anyway); a query **with** them normalizes each field by its word count.
    let has_cjk = q_words.iter().any(|w| w.chars().any(is_cjk)) || q_words.len() <= 1;
    let title_lower = title.to_lowercase();
    let head_lower = content_head.to_lowercase();
    let tags_lower = tags.to_lowercase();
    let (mut ht, mut hh, mut hg, mut q_word_count) = (0usize, 0usize, 0usize, 0usize);
    for word in q_words {
        if word.is_empty() || (word.is_ascii() && word.len() < 2) {
            continue;
        }
        q_word_count += 1;
        if exact_phrase_matches(&title_lower, word) {
            ht += 1;
        }
        if exact_phrase_matches(&head_lower, word) {
            hh += 1;
        }
        if exact_phrase_matches(&tags_lower, word) {
            hg += 1;
        }
    }
    score += ht as f32 * 0.3 + hg as f32 * 0.1;
    if has_cjk {
        score += hh as f32 * 0.2;
    } else if q_word_count > 0 {
        score += (hh as f32 / q_word_count as f32) * 0.2;
    }
    // Kind micro-bias (decision/skill are often the retrieval target)
    if kind == "decision" || kind == "skill" {
        score += 0.05;
    }
    score.min(1.5)
}

/// Pedigree recall rerank (frozen 2026-09-03):
///
/// Combined = 0.60·semantic + 0.20·time decay + 0.10·hit heat + 0.10·emotion
/// Relevance admission uses raw cosine; composite scores rank admitted records.
/// Host settings own thresholds, lexical fusion, ablation and auxiliary caps.
/// Lexical cap (BM25_SEM_CAP): a keyword hit ≠ semantic related; stops fake tops.
#[allow(dead_code)]
const KW_CAP: f32 = 0.7;
/// Recency half-life (days, RECALL_DECAY_DAYS).
const RECALL_DECAY_DAYS: f64 = 90.0;

/// Parse RFC3339 → days from today (parse fail = 0 days = treat as recent, do not punish).
fn exact_phrase_matches(field: &str, query: &str) -> bool {
    if query.is_empty() { return false; }
    let word = |character: char| character.is_ascii_alphanumeric() || character == '_';
    let left_boundary = query.chars().next().is_some_and(word);
    let right_boundary = query.chars().next_back().is_some_and(word);
    field.match_indices(query).any(|(start, _)| {
        (!left_boundary || !field[..start].chars().next_back().is_some_and(word))
            && (!right_boundary || !field[start + query.len()..].chars().next().is_some_and(word))
    })
}

fn days_since(ts: &str, now: &impl Fn() -> chrono::DateTime<chrono::Utc>) -> f64 {
    use chrono::{DateTime, Utc};
    match DateTime::parse_from_rfc3339(ts) {
        Ok(dt) => {
            let now = now();
            let d = now
                .signed_duration_since(dt.with_timezone(&Utc))
                .num_seconds() as f64;
            (d / 86400.0).max(0.0)
        }
        Err(_) => 0.0,
    }
}

/// Time axis: exponential half-life decay, nearer scores higher.
fn recency_score(created_at: &str, now: &impl Fn() -> chrono::DateTime<chrono::Utc>) -> f32 {
    let days = days_since(created_at, now);
    (-(std::f64::consts::LN_2) * days / RECALL_DECAY_DAYS).exp() as f32
}

/// Emotion axis: unlabeled (-1) as neutral 0.5.
fn emotion_score(emotion: f32) -> f32 {
    if emotion < 0.0 {
        0.5
    } else {
        emotion.clamp(0.0, 1.0)
    }
}

/// Hit heat: recall_count log-norm `min(log2(n+1)/4, 1)` — 0 hits → 0, 15 hits → 1 cap.
fn hit_score(recall_count: i64) -> f32 {
    if recall_count <= 0 {
        return 0.0;
    }
    ((recall_count as f32 + 1.0).log2() / 4.0).min(1.0)
}

/// Per-head attention distribution over the admitted candidate pool.
///
/// Raw axis scores use incompatible units (cosine, BM25, decay), so each head is
/// standardized before the softmax; the result is a distribution over candidates,
/// not over axes. This makes axis scales comparable without a global share.
fn head_distribution(values: &[f32], temperature: f32) -> Vec<f32> {
    if values.is_empty() { return Vec::new(); }
    let count = values.len() as f32;
    let mean = values.iter().sum::<f32>() / count;
    let variance = values.iter().map(|value| (value - mean) * (value - mean)).sum::<f32>() / count;
    let deviation = variance.sqrt().max(1e-6);
    let standardized: Vec<f32> = values.iter().map(|value| (value - mean) / deviation).collect();
    let peak = standardized.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let weights: Vec<f32> = standardized.iter()
        .map(|value| ((value - peak) / temperature.max(1e-6)).exp())
        .collect();
    let mass: f32 = weights.iter().sum();
    weights.iter().map(|weight| weight / mass.max(f32::EPSILON)).collect()
}

fn is_question_query(query: &str) -> bool {
    if query.contains('?') || query.contains(crate::i18n::query_terms::FULLWIDTH_QUESTION_MARK) { return true; }
    crate::i18n::query_terms::QUESTION_TERMS.iter().any(|word| query.contains(word))
}

/// Auto-fusion gate: a small logistic over the authorized pool size. Query-shape
/// features were tested but did not improve the held-out decisions; pool size is
/// the only stable separator (boundary ~376 entries). Coefficients are fitted on
/// paired legacy/attention results from 99 natural-language and 99 keyword
/// queries on a 4,766-entry library plus 150 LoCoMo questions on 49 chunks.
const AUTO_GATE_BIAS: f32 = 6.4523;
const AUTO_GATE_POOL: f32 = -1.0878;
const AUTO_GATE_QUESTION: f32 = 0.0;
const AUTO_GATE_LEN: f32 = 0.0;

fn auto_attention_gate(pool: usize, query: &str) -> bool {
    let pool = (1.0 + pool as f32).ln();
    let question = if is_question_query(query) { 1.0 } else { 0.0 };
    let length = query.chars().count() as f32 / 100.0;
    let z = AUTO_GATE_BIAS + AUTO_GATE_POOL * pool + AUTO_GATE_QUESTION * question + AUTO_GATE_LEN * length;
    z >= 0.0
}

/// Five-axis fusion rerank: semantic + time + keyword + emotion + tree position.
///
/// Semantic leads; time axis adds recency (90d half-life); keyword axis (Chinese bigram + lexical) is an orthogonal boost;
/// emotion is a tweak; tree-position (outline boost + subtree activity) puts ancestor outlines first and lets active themes benefit from new leaves —
/// "hit the ancestor first, then walk down to the most recent day". Combined scores that pass the floor return desc.
pub fn is_temporal_query(query: &str) -> bool {
    crate::i18n::query_terms::TEMPORAL_TERMS.iter().any(|word| query.contains(word))
}

/// Final routing decision shared by scoring and the recall receipt.
pub fn routing(query: &str, settings: &crate::query::QuerySettings) -> respire_protocol::RecallRouting {
    let axes: std::collections::BTreeSet<String> = settings.ablate.split(',')
        .map(|axis| axis.trim().to_ascii_lowercase())
        .filter(|axis| matches!(axis.as_str(), "semantic" | "kw" | "recency" | "hot" | "emo" | "tree"))
        .collect();
    let (heat_source, heat_disabled_reason) = if axes.contains("hot") {
        ("none", Some("hot_ablated"))
    } else if settings.feedback.as_ref().is_some_and(|feedback| feedback.enabled) {
        ("feedback_good", None)
    } else if settings.read_heat {
        ("recall_count", None)
    } else {
        ("none", Some("read_heat_disabled"))
    };
    respire_protocol::RecallRouting {
        temporal: is_temporal_query(query),
        route_on: settings.route_on,
        ablate: axes.into_iter().collect::<Vec<_>>().join(","),
        read_heat: settings.read_heat,
        heat_source: heat_source.into(),
        heat_disabled_reason: heat_disabled_reason.map(str::to_owned),
    }
}

mod rerank;
pub use rerank::{
    document_text, rerank_locally, rerank_semantic, rerank_semantic_with_settings,
    rerank_semantic_with_settings_at, resolve_cos_gate,
};

#[cfg(test)]
#[path = "search/tests.rs"]
mod tests;
