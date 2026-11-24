//! memory::rank — lexical ranker (added 2026-09-12)
//!
//! Three pieces: (1) Chinese char-bigram tokenize (reliable lexical signal without a segmenter) (2) BM25 (IDF + length norm,
//! replacing char-bigram overlap — overlap has no IDF and common-char noise is large) (3) RRF fusion and MMR dedup.
//!
//! Why not SQLite FTS5 for the lexical channel: recall is full-library cosine and already loads live rows into memory,
//! so an FTS5 inverted index does not save a scan; and the FTS5 table would have to dual-write on every write/sync/hydrate path, a miss becoming
//! a silent ledger (this repo already had payload.parent_id dual-source drift, hence a repack patch). So the lexical channel
//! is computed in memory: zero migrate, zero dual-write, unit-testable. Revisit FTS5 if the library hits 100k rows and vector scan moves to an index.

use super::model::StoredMemory;

// ── tokenize ──

/// Whether a CJK-family character (Han plus extension/compatibility blocks and kana).
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3400..=0x4DBF     // CJK Extension A
        | 0x4E00..=0x9FFF   // CJK Unified Ideographs
        | 0xF900..=0xFAFF   // CJK Compatibility
        | 0x3040..=0x30FF   // Hiragana/Katakana
        | 0x20000..=0x2FA1F // Extension B~F
    )
}

/// Tokenize: CJK runs become char bigrams (a single char stays), ASCII/digits become words (lowercased);
/// other chars (whitespace, punctuation, emoji) are separators. Without a Chinese segmenter, bigrams are the reliable lexical unit.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cjk_run: Vec<char> = Vec::new();
    let mut word = String::new();

    let flush_cjk = |run: &mut Vec<char>, out: &mut Vec<String>| {
        match run.len() {
            0 => {}
            1 => out.push(run[0].to_string()),
            _ => {
                for w in run.windows(2) {
                    out.push(w.iter().collect());
                }
            }
        }
        run.clear();
    };

    for c in text.chars() {
        if is_cjk(c) {
            if !word.is_empty() {
                out.push(word.to_lowercase());
                word.clear();
            }
            cjk_run.push(c);
        } else if c.is_alphanumeric() {
            flush_cjk(&mut cjk_run, &mut out);
            word.push(c);
        } else {
            flush_cjk(&mut cjk_run, &mut out);
            if !word.is_empty() {
                out.push(word.to_lowercase());
                word.clear();
            }
        }
    }
    flush_cjk(&mut cjk_run, &mut out);
    if !word.is_empty() {
        out.push(word.to_lowercase());
    }
    out
}

// ── BM25 ──

/// BM25 knobs: k1 saturates term frequency, b length-normalizes.
const BM25_K1: f32 = 1.2;
const BM25_B: f32 = 0.75;
/// Field weights (merged document): title is the main channel, tags next, content head last.
const FIELD_W_TITLE: f32 = 3.0;
const FIELD_W_TAGS: f32 = 2.0;
const FIELD_W_HEAD: f32 = 1.0;

use std::collections::HashMap;

#[derive(Debug)]
struct FieldIndex {
    docs: Vec<HashMap<String, f32>>, // per-doc term frequencies
    avg_len: f32,
}

impl FieldIndex {
    fn build(texts: impl Iterator<Item = String>) -> Self {
        let mut docs = Vec::new();
        let mut total = 0usize;
        for text in texts {
            let mut tf: HashMap<String, f32> = HashMap::new();
            let mut len = 0usize;
            for tok in tokenize(&text) {
                *tf.entry(tok).or_insert(0.0) += 1.0;
                len += 1;
            }
            total += len;
            docs.push(tf);
        }
        let n = docs.len().max(1);
        let avg_len = if total == 0 { 1.0 } else { total as f32 / n as f32 };
        Self { docs, avg_len }
    }

    fn score(&self, i: usize, terms: &[String], idf: &HashMap<String, f32>) -> f32 {
        let Some(tf) = self.docs.get(i) else {
            return 0.0;
        };
        let dl = tf.values().sum::<f32>().max(1.0);
        let mut s = 0.0f32;
        for term in terms {
            let f = tf.get(term).copied().unwrap_or(0.0);
            if f <= 0.0 {
                continue;
            }
            let w = idf.get(term).copied().unwrap_or(0.0);
            // Saturated norm: tf·(k1+1)/(tf+K) then divide by (k1+1), lands in [0,1) — absolute per-field scale
            let k = BM25_K1 * (1.0 - BM25_B + BM25_B * dl / self.avg_len);
            s += w * (f / (f + k));
        }
        s
    }

    /// IDF-weighted coverage: Σ_t idf_t·sat(tf_t) / Σ_t idf_t, naturally in [0,1].
    /// Why absolute scale: **do not normalize by this query's max** — relative norm makes any common-word hit
    /// top the axis (measured 2026-09-12: query "dishwasher install" pushed "already installed qiaomu" to 0.90,
    /// and a nonsense query also pulled a pile of 0.88). Coverage is different: a rare-term hit lifts a lot,
    /// a common-term hit lifts little, and covering more query terms scores higher — isomorphic to "how much does this entry answer the question".
    fn coverage(&self, i: usize, terms: &[String], idf: &HashMap<String, f32>) -> f32 {
        let total: f32 = terms.iter().map(|t| idf.get(t).copied().unwrap_or(0.0)).sum();
        if total <= 1e-9 {
            return 0.0;
        }
        (self.score(i, terms, idf) / total).clamp(0.0, 1.0)
    }
}

/// Lexical index: title/tags/content-head, merged into a weighted-tf document for IDF-weighted coverage.
#[derive(Debug)]
pub struct LexicalIndex {
    title: FieldIndex,
    tags: FieldIndex,
    head: FieldIndex,
    /// Merged document of three weighted fields (title 3 / tags 2 / head 1); coverage is computed on this
    weighted: Vec<HashMap<String, f32>>,
    weighted_avg_len: f32,
    n: usize,
}

impl LexicalIndex {
    pub fn build(entries: &[&StoredMemory]) -> Self {
        let title = FieldIndex::build(entries.iter().map(|e| e.local_title.clone()));
        let tags = FieldIndex::build(entries.iter().map(|e| e.local_tags.clone()));
        let head = FieldIndex::build(entries.iter().map(|e| e.local_content_head.clone()));
        // Merged document: three-field weighted tf (title 3 / tags 2 / head 1)
        let mut weighted: Vec<HashMap<String, f32>> = Vec::with_capacity(entries.len());
        let mut total = 0usize;
        for (tf_t, (tf_g, tf_h)) in title.docs.iter().zip(tags.docs.iter().zip(head.docs.iter())) {
            let mut m: HashMap<String, f32> = HashMap::new();
            for (k, v) in tf_t {
                *m.entry(k.clone()).or_insert(0.0) += v * FIELD_W_TITLE;
            }
            for (k, v) in tf_g {
                *m.entry(k.clone()).or_insert(0.0) += v * FIELD_W_TAGS;
            }
            for (k, v) in tf_h {
                *m.entry(k.clone()).or_insert(0.0) += v * FIELD_W_HEAD;
            }
            total += m.values().sum::<f32>() as usize;
            weighted.push(m);
        }
        let n = entries.len().max(1);
        Self {
            title,
            tags,
            head,
            weighted,
            weighted_avg_len: if total == 0 { 1.0 } else { total as f32 / n as f32 },
            n: entries.len(),
        }
    }

    fn approx_bytes(&self) -> usize {
        let maps = |docs: &Vec<HashMap<String, f32>>| {
            docs.capacity() * std::mem::size_of::<HashMap<String, f32>>()
                + docs.iter().map(|doc| doc.capacity() * (std::mem::size_of::<(String, f32)>() + 1)
                    + doc.keys().map(String::capacity).sum::<usize>()).sum::<usize>()
        };
        std::mem::size_of::<Self>() + maps(&self.title.docs) + maps(&self.tags.docs)
            + maps(&self.head.docs) + maps(&self.weighted)
    }

    /// Lexical score per row over the whole library. Returns [0,1] **IDF-weighted coverage**:
    /// 0 = no query term hit, 1 = every query term (IDF-weighted) hit.
    ///
    /// Why absolute: **do not normalize by this query's max** — relative norm makes any common-word hit
    /// top the axis (measured 2026-09-12: query "dishwasher install" pushed "already installed qiaomu" to 0.90,
    /// and a query whose terms are all missing still pulled a pile of 0.88). Coverage is isomorphic to "how much does this entry answer the question".
    pub fn scores(&self, query: &str) -> Vec<f32> {
        let terms: Vec<_> = tokenize(query).into_iter().filter(|term| !term.is_ascii() || term.len() >= 2).collect();
        if terms.is_empty() || self.n == 0 {
            return vec![0.0; self.n];
        }
        let idf = self.idf_of(&terms);
        let total_idf: f32 = terms.iter().map(|t| idf.get(t).copied().unwrap_or(0.0)).sum();
        if total_idf <= 1e-9 {
            return vec![0.0; self.n];
        }
        (0..self.n)
            .map(|i| {
                let Some(tf) = self.weighted.get(i) else {
                    return 0.0;
                };
                let dl = tf.values().sum::<f32>().max(1.0);
                let k = BM25_K1 * (1.0 - BM25_B + BM25_B * dl / self.weighted_avg_len);
                let mut hit = 0.0f32;
                for t in &terms {
                    let f = tf.get(t).copied().unwrap_or(0.0);
                    if f <= 0.0 {
                        continue;
                    }
                    let w = idf.get(t).copied().unwrap_or(0.0);
                    hit += w * (f / (f + k)); // saturated per-query-term hit strength, in [0,1)
                }
                (hit / total_idf).clamp(0.0, 1.0)
            })
            .collect()
    }

    /// Term IDF (three-field merged df: appearing in any field counts as one doc).
    fn idf_of(&self, terms: &[String]) -> HashMap<String, f32> {
        let n = self.n as f32;
        let mut idf = HashMap::new();
        for t in terms {
            if idf.contains_key(t) {
                continue;
            }
            let df = (0..self.n)
                .filter(|&i| {
                    self.title.docs.get(i).is_some_and(|m| m.contains_key(t))
                        || self.tags.docs.get(i).is_some_and(|m| m.contains_key(t))
                        || self.head.docs.get(i).is_some_and(|m| m.contains_key(t))
                })
                .count() as f32;
            // Probabilistic IDF (+1 keeps it non-negative)
            idf.insert(t.clone(), (1.0 + (n - df + 0.5) / (df + 0.5)).ln());
        }
        idf
    }
}

/// Private to one authorized resident lineage; every hit checks the exact ordered corpus.
/// Retain at most two corpora with a row-scaled allocation estimate, capped at 512 MiB.
#[derive(Debug, Default)]
pub struct LexicalIndexCache {
    entries: std::sync::Mutex<Vec<CachedLexicalIndex>>,
}

#[derive(Debug)]
struct CachedLexicalIndex {
    rows: Vec<[String; 4]>,
    index: std::sync::Arc<LexicalIndex>,
    bytes: usize,
}

impl LexicalIndexCache {
    pub fn scores(&self, entries: &[&StoredMemory], query: &str) -> anyhow::Result<Vec<f32>> {
        let index = {
            let mut cached = self.entries.lock()
                .map_err(|_| anyhow::anyhow!("resident lexical cache poisoned"))?;
            if let Some(hit) = cached.iter().find(|cached| cached.rows.len() == entries.len()
                && cached.rows.iter().zip(entries).all(|(row, entry)|
                    row[0] == entry.id && row[1] == entry.local_title
                        && row[2] == entry.local_tags && row[3] == entry.local_content_head)) {
                std::sync::Arc::clone(&hit.index)
            } else {
                let index = std::sync::Arc::new(LexicalIndex::build(entries));
                let rows: Vec<[String; 4]> = entries.iter().map(|entry| [
                    entry.id.clone(), entry.local_title.clone(), entry.local_tags.clone(),
                    entry.local_content_head.clone(),
                ]).collect();
                let bytes = index.approx_bytes() + rows.capacity() * std::mem::size_of::<[String; 4]>()
                    + rows.iter().flatten().map(String::capacity).sum::<usize>();
                // Keep the main pass's budget during smaller skill passes; measured CJK rows need up to 80 KiB each.
                let corpus_rows = entries.len().max(cached.iter().map(|entry| entry.rows.len()).max().unwrap_or(0));
                let max_bytes = (32usize * 1024 * 1024).saturating_add(corpus_rows.saturating_mul(80 * 1024))
                    .min(512 * 1024 * 1024);
                if bytes <= max_bytes {
                    while cached.len() >= 2 || cached.iter().map(|entry| entry.bytes).sum::<usize>() + bytes > max_bytes {
                        cached.remove(0);
                    }
                    cached.push(CachedLexicalIndex { rows, index: std::sync::Arc::clone(&index), bytes });
                }
                index
            }
        };
        Ok(index.scores(query))
    }
}

// ── fusion and dedup ──

/// Reciprocal rank fusion (RRF, Cormack 2009): merge several ranked lists; k damps the head.
/// Independent scales (cosine / BM25 are not commensurate) are dissolved by ranking — so RRF is the right mixer for heterogeneous sources.
pub fn rrf(rankings: &[&[usize]], k: f32) -> HashMap<usize, f32> {
    let mut fused: HashMap<usize, f32> = HashMap::new();
    for ranking in rankings {
        for (rank, &idx) in ranking.iter().enumerate() {
            *fused.entry(idx).or_insert(0.0) += 1.0 / (k + rank as f32 + 1.0);
        }
    }
    fused
}

/// Rank: score desc, ties by idx asc (deterministic). Keep only scores > threshold.
pub fn to_ranking(scored: &[(usize, f32)], min: f32) -> Vec<usize> {
    let mut v: Vec<(usize, f32)> = scored.iter().copied().filter(|(_, s)| *s > min).collect();
    v.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    v.into_iter().map(|(i, _)| i).collect()
}

/// Near-dup threshold (cosine): similar enough to an already-picked item is a same-topic copy and skipped in the MMR round.
/// Measured: a pure MMR soft penalty does not stop copies — a high-score copy's 2nd-place after norm is still high,
/// and the penalty cannot close the gap (at weight 0.7 a 0.95-similar true copy still beats an independent row). Hence this hard gate.
pub const DUP_SIM_THRESHOLD: f32 = 0.92;

/// MMR greedy pick (Carbonell & Goldstein 1998): high relevance and not a duplicate of already-picked items.
/// `sim(i, j)` is supplied by the caller (usually cosine); `lambda` 1.0 = pure relevance, 0.0 = pure diversity.
/// Near-dups (sim ≥ `DUP_SIM_THRESHOLD`) are skipped first; if a round skips everyone and slots remain,
/// relax and take the highest MMR — dedup is to free slots, not to cut recall.
/// Purpose: top-k slots are not filled by same-topic copies (defrag/merge treats the cause; this treats the symptom).
pub fn mmr_select(
    scored: &[(usize, f32)],
    k: usize,
    lambda: f32,
    sim: impl Fn(usize, usize) -> f32,
) -> Vec<usize> {
    if scored.is_empty() || k == 0 {
        return Vec::new();
    }
    // Normalize scores to [0,1] so scale cannot drown the similarity term
    let max = scored.iter().map(|(_, s)| *s).fold(f32::MIN, f32::max);
    let min = scored.iter().map(|(_, s)| *s).fold(f32::MAX, f32::min);
    let span = (max - min).max(1e-6);
    let rel: HashMap<usize, f32> =
        scored.iter().map(|(i, s)| (*i, (*s - min) / span)).collect();

    let mut picked: Vec<usize> = Vec::new();
    let mut rest: Vec<usize> = scored.iter().map(|(i, _)| *i).collect();
    while picked.len() < k && !rest.is_empty() {
        // Compute MMR per candidate first; same-topic copies (sim ≥ threshold vs already picked) are set aside
        let mut best: Option<(usize, f32)> = None; // (index in rest, MMR score)
        let mut best_dup: Option<(usize, f32)> = None;
        for (pos, &cand) in rest.iter().enumerate() {
            let redundancy = picked
                .iter()
                .map(|&p| sim(cand, p))
                .fold(0.0f32, f32::max);
            let mmr = lambda * rel.get(&cand).copied().unwrap_or(0.0)
                - (1.0 - lambda) * redundancy;
            if !picked.is_empty() && redundancy >= DUP_SIM_THRESHOLD {
                if best_dup.is_none_or(|(_, b)| mmr > b) {
                    best_dup = Some((pos, mmr));
                }
                continue;
            }
            if best.is_none_or(|(_, b)| mmr > b) {
                best = Some((pos, mmr));
            }
        }
        // A round of all copies with slots left: relax and take the highest-MMR copy (do not cut recall for dedup)
        let Some((pos, _)) = best.or(best_dup) else {
            break;
        };
        picked.push(rest.remove(pos));
    }
    picked
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::fixtures::rank as text;

    fn stored(id: &str, title: &str, tags: &str, head: &str) -> StoredMemory {
        let mut m = StoredMemory::new_pending(id.to_owned());
        m.local_title = title.to_owned();
        m.local_tags = tags.to_owned();
        m.local_content_head = head.to_owned();
        m
    }

    #[test]
    fn tokenize_cjk_bigram_and_ascii_word() {
        let t = tokenize(text::SANDBOX_QUERY);
        assert!(t.contains(&text::BIGRAM_FIRST.to_owned()));
        assert!(t.contains(&text::BIGRAM_MIDDLE.to_owned()));
        assert!(t.contains(&text::BIGRAM_LAST.to_owned()));
        assert!(t.contains(&"sandbox".to_owned()));
        assert!(t.contains(&"v2".to_owned()));
        // A single CJK char is kept
        assert_eq!(tokenize(text::SINGLE_HAN), vec![text::SINGLE_HAN.to_owned()]);
        // Punctuation separators must not yield empty tokens
        assert!(tokenize("a, b").iter().all(|s| !s.is_empty()));
    }

    #[test]
    fn bm25_rare_term_beats_common_term() {
        let a = stored("a", text::EXAM_TITLE, "gesp", text::EXAM_BODY);
        let b = stored("b", text::DAILY_TITLE, "gesp", text::DAILY_BODY);
        let c = stored("c", text::SECOND_DAILY_TITLE, "gesp", text::MISC_BODY);
        let list = vec![&a, &b, &c];
        let idx = LexicalIndex::build(&list);
        let s = idx.scores(text::EXAM_QUERY);
        assert!(s[0] > s[1] && s[0] > s[2], "The only exam match should rank highest: {s:?}");
    }

    /// Absolute coverage scale: hitting one common word must not top the score (the old relative-norm bug).
    #[test]
    fn coverage_does_not_saturate_on_common_term() {
        // All three contain "install" (a library-wide common word); only a hits the real topic term
        let a = stored("a", text::DISHWASHER_GUIDE, text::HOUSEHOLD_TAG, text::KITCHEN_APPLIANCE);
        let b = stored("b", text::GENERIC_INSTALL_TITLE, "tool", text::GENERIC_INSTALL_BODY);
        let c = stored("c", text::INSTALL_GUIDE, "tool", text::PROCEDURE_TEXT);
        let list = vec![&a, &b, &c];
        let idx = LexicalIndex::build(&list);
        let s = idx.scores(text::DISHWASHER_INSTALL_QUERY);
        assert!(s[0] > s[1] && s[0] > s[2], "The specific topic should outrank common word hits: {s:?}");
        assert!(s[1] < 0.9, "A single common word should not saturate coverage: {}", s[1]);
    }

    /// When no query term is in the library, coverage should be near zero (not inflated by "relative max").
    #[test]
    fn coverage_near_zero_for_unrelated_query() {
        let a = stored("a", text::DATED_EXAM_TITLE, "", text::EXAM_LEVEL);
        let b = stored("b", text::BITSET_TITLE, "", text::PREFIX_SUM_BODY);
        let list = vec![&a, &b];
        let idx = LexicalIndex::build(&list);
        let s = idx.scores("xyzabcnonexist");
        assert!(s.iter().all(|v| *v < 0.05), "Unrelated queries should score near zero: {s:?}");
    }

    /// Coverage monotonicity: more query-term hits → higher score (inside one field).
    #[test]
    fn coverage_monotone_in_hit_count() {
        let a = stored("a", text::COMPLETE_SANDBOX_TITLE, "", "");
        let b = stored("b", text::SHORT_SANDBOX_TITLE, "", "");
        let c = stored("c", text::BIGRAM_FIRST, "", "");
        let list = vec![&a, &b, &c];
        let idx = LexicalIndex::build(&list);
        let s = idx.scores(text::SANDBOX_TIMEOUT_QUERY);
        assert!(s[0] > s[1] && s[1] > s[2], "More query terms should rank higher: {s:?}");
    }

    /// IDF's real role in **normalized coverage**: on a one-term query, idf cancels (idf·sat/idf = sat),
    /// so idf is not an "absolute penalty" but **relative weight in a multi-term query** — a rare-term hit benefits because denominator mass
    /// concentrates. The old test (comparing one-term coverage) contradicted this; corrected 2026-09-12.
    #[test]
    fn idf_weights_rare_term_within_multi_term_query() {
        // All three contain "record" (library-wide common); only a also has "mic denoise" (a singleton)
        let a = stored("a", text::FIRST_LOG, "", text::MICROPHONE_ISSUE);
        let b = stored("b", text::SECOND_LOG, "", text::UNRELATED_BODY);
        let c = stored("c", text::THIRD_LOG, "", text::UNRELATED_BODY);
        let list = vec![&a, &b, &c];
        let idx = LexicalIndex::build(&list);
        // Multi-term query: a covers "record"+"mic denoise", b/c only "record"
        let both = idx.scores(text::MICROPHONE_QUERY);
        assert!(both[0] > both[1] && both[0] > both[2], "The rare term should break the tie: {both:?}");
    }

    /// On a one-term query, idf cancels (numerator and denominator both times idf), so coverage is just the sat term:
    /// the only remaining effect is length norm (longer dl, same tf → lower score). This test fixes length,
    /// checking cancel and length-norm together (the first 2026-09-12 assert did not control length and misjudged).
    #[test]
    fn single_term_query_depends_only_on_length_norm() {
        let a = stored("a", text::GENERIC_LOG, "", text::SAME_LENGTH_BODY);
        let b = stored("b", text::GENERIC_LOG, "", text::SAME_LENGTH_BODY);
        let list = vec![&a, &b];
        let idx = LexicalIndex::build(&list);
        let s = idx.scores(text::GENERIC_LOG);
        assert!((s[0] - s[1]).abs() < 1e-6, "Equal-frequency, equal-length documents should tie: {s:?}");
        assert!(s[0] > 0.0);
    }
    #[test]
    fn rrf_favors_agreement_across_lists() {
        // Both lists rank 1 first (true consensus, i.e. both like it)
        let semantic: Vec<usize> = vec![1, 0, 2];
        let lexical: Vec<usize> = vec![1, 2, 0];
        let fused = rrf(&[&semantic, &lexical], 60.0);
        let (mut s): Vec<(usize, f32)> = fused.into_iter().collect();
        s.sort_by(|x, y| y.1.partial_cmp(&x.1).unwrap_or(std::cmp::Ordering::Equal));
        assert_eq!(s[0].0, 1, "Consensus first place should lead: {s:?}");
    }

    /// A counter-intuitive RRF fact (math, not a defect):
    /// f(0)+f(2) − 2·f(1) = 2/[k(k+1)(k+2)] > 0 always, independent of k —
    /// so "first on one list + last on the other" always beats "middle on both". It rewards being strongly liked by one list,
    /// not mediocrity. When fusing lists, know this; do not assume "always mid-pack" is better.
    #[test]
    fn rrf_rewards_extremes_over_middling() {
        let a: Vec<usize> = vec![0, 1, 2];
        let b: Vec<usize> = vec![2, 1, 0];
        let fused = rrf(&[&a, &b], 60.0);
        let s0 = fused[&0];
        let s1 = fused[&1];
        assert!(s0 > s1 && fused[&2] > s1, "Extremes should outrank the middle");
        assert!((s0 - fused[&2]).abs() < 1e-9, "Symmetric candidates should tie");
    }

    #[test]
    fn rrf_single_list_is_identity_order() {
        let only: Vec<usize> = vec![3, 1, 2];
        let fused = rrf(&[&only], 60.0);
        let (mut s): Vec<(usize, f32)> = fused.into_iter().collect();
        s.sort_by(|x, y| y.1.partial_cmp(&x.1).unwrap_or(std::cmp::Ordering::Equal));
        let order: Vec<usize> = s.into_iter().map(|(i, _)| i).collect();
        assert_eq!(order, vec![3, 1, 2], "A single ranking should preserve order");
    }

    #[test]
    fn mmr_prefers_diverse_over_duplicate() {
        // 0 and 1 are near same-topic (sim 0.95 ≥ threshold); 2 is a bit lower but independent
        let scored = vec![(0usize, 1.0f32), (1, 0.99), (2, 0.8)];
        let sim = |x: usize, y: usize| -> f32 {
            match (x.min(y), x.max(y)) {
                (0, 1) => 0.95,
                _ => 0.05,
            }
        };
        let picked = mmr_select(&scored, 2, 0.7, sim);
        assert!(picked.contains(&0));
        assert!(picked.contains(&2), "A diverse candidate should fill the second slot: {picked:?}");
    }

    /// Copies must not occupy slots, but leftover slots must not lose recall to dedup ("dedup frees slots, it does not cut recall").
    #[test]
    fn mmr_relaxes_when_only_duplicates_left() {
        let scored = vec![(0usize, 1.0f32), (1, 0.99), (2, 0.98)];
        let sim = |_: usize, _: usize| 0.99; // All three candidates duplicate one another.
        let picked = mmr_select(&scored, 3, 0.7, sim);
        assert_eq!(picked.len(), 3, "Relaxing duplicate filtering should fill all slots: {picked:?}");
        assert_eq!(picked[0], 0);
    }

    /// Below the threshold, the hard-dedup gate does not apply (0.9 < 0.92 counts as related rows that can coexist).
    #[test]
    fn mmr_keeps_below_threshold_pairs() {
        let scored = vec![(0usize, 1.0f32), (1, 0.99)];
        let sim = |_: usize, _: usize| 0.90;
        let picked = mmr_select(&scored, 2, 0.7, sim);
        assert_eq!(picked, vec![0, 1]);
    }

    #[test]
    fn mmr_lambda_one_is_pure_relevance() {
        let scored = vec![(0usize, 1.0f32), (1, 0.99), (2, 0.8)];
        let sim = |_: usize, _: usize| 0.95;
        let picked = mmr_select(&scored, 2, 1.0, sim);
        assert_eq!(picked, vec![0, 1]);
    }

    #[test]
    fn to_ranking_filters_and_orders() {
        let scored = vec![(5usize, 0.1f32), (1, 0.9), (2, 0.9)];
        let r = to_ranking(&scored, 0.5);
        assert_eq!(r, vec![1, 2], "Ties should preserve index order and sub-threshold entries should be omitted");
    }
}
