//! Sentence-aware, token-bounded local chunk indexing.

use crate::engine::validate_embedding;
#[cfg(test)]
use crate::search::Embedder;
use anyhow::Result;

/// Build every chunk before committing any index mutation.
pub fn embed_chunks(embedder: &impl crate::search::EmbeddingSource, content: &str) -> Result<Vec<Vec<f32>>> {
    let chunks = split_chunks(content);
    let mut pieces = Vec::new();
    if chunks.is_empty() {
        pieces = embedder.split_text(content)?;
        if pieces.len() < 2 && content.chars().count() <= 400 {
            return Ok(Vec::new());
        }
    } else {
        for chunk in chunks {
            pieces.extend(embedder.split_text(&chunk)?);
        }
    }
    pieces
        .iter()
        .map(|text| {
            let vector = embedder.try_embed(text)?;
            validate_embedding(&vector, embedder.dims())?;
            Ok(vector)
        })
        .collect()
}

/// Chunk size (characters) for the fine-grained index. Matches the evaluation harness
/// (`embed_ablation.py --chunk-chars 800`), where this setting lifted LoCoMo session hit by
/// +7.6pp on the Chinese model and +21.8pp on bge-m3.
pub const CHUNK_CHARS: usize = 800;

/// Hard ceiling for one chunk. A chunk closes at the first sentence end at or past
/// [`CHUNK_CHARS`]; only when no break arrives within this budget is the text cut mid-sentence.
const CHUNK_MAX_CHARS: usize = 1200;

/// Characters carried from the end of a chunk into the next one (2026-09-26).
///
/// A fixed 800-char cut splits whatever straddles the boundary into two halves, and neither half
/// embeds to what the whole fact does. Carrying a tail forward means at least one chunk always
/// holds the fact whole; retrieval takes the max over chunks, so the repeated text costs index
/// space, not ranking.
pub const CHUNK_OVERLAP_CHARS: usize = 120;

/// Split a body into chunks for the fine-grained index.
///
/// A body at or below [`CHUNK_CHARS`] needs no character splitting. `embed_chunks` still adds
/// token windows when the body exceeds the whole-entry prefix or the tokenizer budget.
/// Chunks are body slices; the whole-entry vector also carries the title and tags.
///
/// (2026-09-26) The cut is **break-aware**: sentences and lines are atomic, a chunk is filled to
/// [`CHUNK_CHARS`] and closed at a sentence end (never past [`CHUNK_MAX_CHARS`]), and every chunk
/// after the first repeats the tail of its predecessor ([`CHUNK_OVERLAP_CHARS`], snapped forward to
/// a break) so a fact sitting on a boundary survives in one vector.
pub fn split_chunks(content: &str) -> Vec<String> {
    let chars: Vec<char> = content.chars().collect();
    if chars.len() <= CHUNK_CHARS {
        return Vec::new();
    }
    with_overlap(&pack_units(&split_units(&chars)))
}

/// A sentence or line end — the preferred place to close a chunk.
fn is_sentence_end(ch: char) -> bool {
    crate::i18n::query_terms::SENTENCE_PUNCTUATION.contains(&ch) || matches!(ch, '…' | '!' | '?' | ';' | '\n')
}

/// A weaker break, used only when a chunk has to be cut inside a sentence.
fn is_soft_break(ch: char) -> bool {
    crate::i18n::query_terms::PHRASE_PUNCTUATION.contains(&ch) || matches!(ch, ',' | ':' | ' ' | '\t')
}

/// Atomic units: one sentence (or line) each. `.` counts only when followed by whitespace or the
/// end, so `v1.0.5-dev.6` stays one unit.
fn split_units(chars: &[char]) -> Vec<String> {
    let mut units: Vec<String> = Vec::new();
    let mut cur = String::new();
    for (i, &ch) in chars.iter().enumerate() {
        cur.push(ch);
        let closes = is_sentence_end(ch)
            || (ch == '.' && chars.get(i + 1).map_or(true, |next| next.is_whitespace()));
        if closes {
            units.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        units.push(cur);
    }
    units
}

/// Greedy packing: fill to [`CHUNK_CHARS`], never past [`CHUNK_MAX_CHARS`]. A single unit longer
/// than the ceiling is hard-sliced (still preferring a soft break near the cut).
fn pack_units(units: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for unit in units {
        let unit_len = unit.chars().count();
        if unit_len > CHUNK_MAX_CHARS {
            if !cur.trim().is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            out.extend(hard_slice(unit));
            continue;
        }
        if !cur.is_empty() && cur.chars().count() + unit_len > CHUNK_MAX_CHARS {
            out.push(std::mem::take(&mut cur));
        }
        cur.push_str(unit);
        if cur.chars().count() >= CHUNK_CHARS {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// Cut an over-long unit into [`CHUNK_CHARS`] slices, looking back up to 80 chars for a soft break
/// so the cut lands between words rather than inside one.
fn hard_slice(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let end = (start + CHUNK_CHARS).min(chars.len());
        let cut = if end < chars.len() {
            let floor = start + CHUNK_CHARS.saturating_sub(80);
            (floor..end)
                .rev()
                .find(|&i| is_soft_break(chars[i]) || is_sentence_end(chars[i]))
                .map_or(end, |i| i + 1)
        } else {
            end
        };
        // Always advance: a cut at `start` would loop forever.
        let cut = cut.max(start + 1);
        out.push(chars[start..cut].iter().collect::<String>());
        start = cut;
    }
    out
}

/// Prepend the tail of the previous chunk to every later chunk so boundary facts stay whole.
fn with_overlap(pieces: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(pieces.len());
    for (i, piece) in pieces.iter().enumerate() {
        if i == 0 {
            out.push(piece.clone());
            continue;
        }
        let tail = tail_window(&pieces[i - 1], CHUNK_OVERLAP_CHARS);
        let mut merged = String::with_capacity(tail.len() + piece.len());
        merged.push_str(&tail);
        merged.push_str(piece);
        out.push(merged);
    }
    out
}

/// Last `budget` characters of `prev`, snapped forward past a break so the overlap starts at a
/// sentence or word boundary instead of mid-word.
fn tail_window(prev: &str, budget: usize) -> String {
    let chars: Vec<char> = prev.chars().collect();
    if chars.len() <= budget {
        return prev.to_string();
    }
    let floor = chars.len() - budget;
    let start = (floor..chars.len())
        .find(|&i| i > floor && (is_sentence_end(chars[i]) || is_soft_break(chars[i])))
        .map_or(floor, |i| i + 1);
    let start = if start >= chars.len() { floor } else { start };
    chars[start..].iter().collect()
}

#[cfg(test)]
mod inference_failure_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FailSecondChunk(AtomicUsize);
    impl Embedder for FailSecondChunk {
        fn dims(&self) -> usize { 2 }
        fn embed(&self, _text: &str) -> Vec<f32> {
            panic!("fallible inference must not use the empty-vector adapter")
        }
        fn try_embed(&self, _text: &str) -> Result<Vec<f32>> {
            if self.0.fetch_add(1, Ordering::SeqCst) == 1 {
                anyhow::bail!("native fixture failure: allocation unavailable");
            }
            Ok(vec![1.0, 0.0])
        }
    }

    #[test]
    fn failed_chunk_returns_cause_without_partial_artifacts_and_can_retry() -> Result<()> {
        let embedder: Box<dyn Embedder> = Box::new(FailSecondChunk(AtomicUsize::new(0)));
        let body = "Example inference sentence. ".repeat(160);
        let failure = embed_chunks(&embedder, &body)
            .err().ok_or_else(|| anyhow::anyhow!("second chunk should fail"))?;
        assert!(format!("{failure:#}").contains("allocation unavailable"));
        assert!(embed_chunks(&embedder, &body)?.len() > 1);
        Ok(())
    }
}
