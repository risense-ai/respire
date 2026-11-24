use anyhow::Result;
use crate::localized_notation::FULLWIDTH_COLON;
use respire_core::search::cosine_similarity;
#[cfg(test)]
use respire_core::search::Embedder;
/// Auto-attach similarity floor (content × outline anchor, BGE cosine).
pub const CLASSIFY_HIT: f32 = 0.50;
/// Ask-to-create-outline similarity floor (below this, do not take the job).
pub const CLASSIFY_SUGGEST: f32 = 0.40;

/// One built-in class.
pub struct Category {
    /// Outline name (the root entry title)
    pub title: &'static str,
    /// Domain: human / AI
    pub ai: bool,
    /// Classify keywords (hitting one is a strong candidate)
    pub keywords: &'static [&'static str],
    /// One-sentence gist (body when creating the outline; also the classify semantic anchor)
    pub gist: &'static str,
    /// English model-facing projection; never used as a storage key or embedding anchor.
    pub prompt_label: &'static str,
    pub prompt_gist: &'static str,
}

include!("i18n/legacy_taxonomy.rs");

pub fn category(idx: usize) -> &'static Category {
    &CATALOG[idx]
}

pub fn find_by_title(title: &str) -> Option<usize> {
    CATALOG.iter().position(|c| c.title == title)
}

pub fn anchor_text(c: &Category) -> String {
    format!("{}{FULLWIDTH_COLON}{}", c.title, c.gist)
}
pub fn keyword_hit(text_lower: &str, idx: usize) -> usize {
    category(idx).keywords.iter().filter(|k| text_lower.contains(&k.to_lowercase())).count()
}
pub fn classify<E: respire_core::search::EmbeddingSource>(
    embedder: &E,
    text: &str,
) -> Result<Option<(usize, f32)>> {
    let text_lower = text.to_lowercase();
    let hits: Vec<usize> = (0..CATALOG.len()).filter(|&i| keyword_hit(&text_lower, i) > 0).collect();
    if hits.is_empty() {
        return Ok(None);
    }
    // Prefer more lexical hits; on a tie, compare all and take the highest anchor sim
    let mut scored: Vec<(usize, usize)> = hits.iter().map(|&i| (i, keyword_hit(&text_lower, i))).collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let q = embedder.try_embed_query(text)?;
    let mut best: Option<(usize, f32)> = None;
    for (i, _) in &scored {
        let anchor = embedder.try_embed(&anchor_text(category(*i)))?;
        let sim = cosine_similarity(&anchor, &q);
        if best.map_or(true, |(_, s)| sim > s) {
            best = Some((*i, sim));
        }
    }
    Ok(best.filter(|(_, s)| *s >= CLASSIFY_SUGGEST))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::localized_test_data::{TASKS, PITFALLS, GAMES, NGINX_PITFALL, STEAM_GAME, PLAN_KEYWORDS, UNRELATED_WEATHER};
    #[test]
    fn keyword_hit_matches_terms_case_insensitive() -> anyhow::Result<()> {
        let pits = CATALOG.iter().position(|c| c.title == PITFALLS).ok_or_else(|| anyhow::anyhow!("missing Pitfalls category"))?;
        let games = CATALOG.iter().position(|c| c.title == GAMES).ok_or_else(|| anyhow::anyhow!("missing Video games category"))?;
        let tasks = CATALOG.iter().position(|c| c.title == TASKS).ok_or_else(|| anyhow::anyhow!("missing Tasks and plans category"))?;
        assert!(keyword_hit(NGINX_PITFALL, pits) > 0);
        assert!(keyword_hit(STEAM_GAME, games) > 0);
        assert!(keyword_hit(PLAN_KEYWORDS, tasks) > 0);
        assert_eq!(keyword_hit(UNRELATED_WEATHER, 0), 0, "unrelated text must not hit Family and friends");
        Ok(())
    }

    struct FixedEmb;
    impl Embedder for FixedEmb {
        // Anchor orthogonal to query → cosine 0, proves a lexical hit is not enough to classify alone
        fn embed(&self, _text: &str) -> Vec<f32> { vec![1.0, 0.0, 0.0] }
        fn embed_query(&self, _text: &str) -> Vec<f32> { vec![0.0, 1.0, 0.0] }
        fn dims(&self) -> usize { 3 }
    }

    #[test]
    fn classify_requires_both_keyword_and_anchor_sim() -> anyhow::Result<()> {
        // Keyword hit but anchor similarity 0 (FixedEmb orthogonal to query) → below 0.40 → no class
        let r = classify(&FixedEmb, NGINX_PITFALL)?;
        assert!(r.is_none(), "must not force a class when anchor similarity is 0");
        Ok(())
    }

}
