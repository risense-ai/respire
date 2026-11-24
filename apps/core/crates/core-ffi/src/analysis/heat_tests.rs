use crate::localized_test_data::{CANONICAL_SEGMENT_INSTRUCTION, LEGACY_EFFECT_MARKER, USER_TITLE, USER_BODY};
use super::*;
#[test]
fn maintenance_generates_canonical_english_sections_without_rewriting_input() -> Result<()> {
    let mut memory = StoredMemory::new_pending("fixture-id".into());
    memory.local_title = USER_TITLE.into();
    memory.local_content_head = USER_BODY.into();
    let before = memory.local_content_head.clone();
    let plan = maintenance_plan(std::slice::from_ref(&memory), 300, 1, 1)?;
    let prompt = plan["prompt"].as_str().ok_or_else(|| anyhow::anyhow!("missing prompt"))?;
    assert!(prompt.contains(CANONICAL_SEGMENT_INSTRUCTION));
    assert!(!prompt.contains(LEGACY_EFFECT_MARKER));
    assert!(prompt.contains("Output only those four sections"));
    assert!(prompt.contains("neither merge nor reparent nor demote"));
    assert_eq!(memory.local_content_head, before);
    Ok(())
}

/// Float judgment: explicit --good threshold, strictly hotter than parent,
/// grandparent exists, trivia excluded. recall_count alone never floats (P0-2).
#[test]
fn float_up_candidates_rules() {
    let mk = |id: &str, parent: &str, imp: &str| {
        let mut m = StoredMemory::new_pending(id.into());
        m.local_parent_id = parent.into();
        m.local_importance = imp.into();
        m
    };
    let counts = |entries: &[(&str, i64)]| -> HashMap<String, i64> {
        entries.iter().map(|(id, n)| ((*id).to_owned(), *n)).collect()
    };
    // root(L1) ← mid ← hot: hot at threshold and hotter than mid → float to root
    let all = vec![
        mk("aaaaaaaa-root", "", "normal"),
        mk("bbbbbbbb-mid", "aaaaaaaa-root", "normal"),
        mk("cccccccc-hot", "bbbbbbbb-mid", "normal"),
    ];
    let warm = counts(&[("aaaaaaaa-root", 1), ("bbbbbbbb-mid", 2), ("cccccccc-hot", 5)]);
    let cands = float_up_candidates(&all, 3, &warm);
    assert_eq!(
        cands,
        vec![("cccccccc-hot".to_owned(), "aaaaaaaa-root".to_owned())]
    );
    // recall_count never decides: a hot store with no --good marks floats nothing
    let mut hot_recall = all.clone();
    hot_recall[1].local_recall_count = 500;
    hot_recall[2].local_recall_count = 1000;
    let empty = HashMap::new();
    assert!(float_up_candidates(&hot_recall, 3, &empty).is_empty());
    // Below threshold does not float
    assert!(float_up_candidates(&all, 6, &warm).is_empty());
    // Not hotter than parent (tie/lower) does not float — steady state
    let flat = vec![
        mk("aaaaaaaa-root", "", "normal"),
        mk("bbbbbbbb-mid", "aaaaaaaa-root", "normal"),
        mk("cccccccc-hot", "bbbbbbbb-mid", "normal"),
    ];
    let even = counts(&[("aaaaaaaa-root", 9), ("bbbbbbbb-mid", 9), ("cccccccc-hot", 9)]);
    assert!(float_up_candidates(&flat, 3, &even).is_empty());
    // No grandparent (parent is root) does not float — do not pass L1
    let shallow = vec![
        mk("aaaaaaaa-root", "", "normal"),
        mk("cccccccc-hot", "aaaaaaaa-root", "normal"),
    ];
    let shallow_good = counts(&[("aaaaaaaa-root", 0), ("cccccccc-hot", 9)]);
    assert!(float_up_candidates(&shallow, 3, &shallow_good).is_empty());
    // Trivia does not float; tombstone does not float; missing parent does not float
    let messy = vec![
        mk("aaaaaaaa-root", "", "normal"),
        mk("bbbbbbbb-mid", "aaaaaaaa-root", "normal"),
        mk("cccccccc-hot", "bbbbbbbb-mid", "trivial"),
        mk("dddddddd-dead", "bbbbbbbb-mid", "normal"),
        mk("eeeeeeee-orphan", "ffffffff-none", "normal"),
    ];
    let messy_good = counts(&[
        ("aaaaaaaa-root", 0),
        ("bbbbbbbb-mid", 0),
        ("cccccccc-hot", 9),
        ("dddddddd-dead", 9),
        ("eeeeeeee-orphan", 9),
    ]);
    let mut dead = messy.clone();
    dead[3].deleted = true;
    assert!(float_up_candidates(&dead, 3, &messy_good).is_empty());
}
