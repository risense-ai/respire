use super::*;
use serde_json::json;

fn entry(id: &str, kind: &str, title: &str) -> respire_app::MemoryEntry {
    // Kind deserializes from its variant name (Context, Skill, ...), not the lowercase wire form.
    let variant = format!("{}{}", &kind[..1].to_uppercase(), &kind[1..]);
    serde_json::from_value(json!({
        "id": id, "kind": variant, "tags": [], "title": title, "content": "",
        "user": "tester", "computer": "test", "project": "",
        "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z",
        "emotion": -1.0, "parent_id": "", "importance": "important"
    }))
    .expect("fixture entry")
}

fn row(id: &str, kind: &str, title: &str, score: f32) -> RecalledWithContext {
    RecalledWithContext {
        score,
        entry: entry(id, kind, title),
        ancestors: Vec::new(),
    }
}

#[test]
fn english_legacy_and_mixed_headings_have_the_same_requirements() {
    let english = "\u{3010}Cause\u{3011}fixture\n\u{3010}Action\u{3011}\nTrigger: authorized task\nPrerequisites: ready\nSteps: run\nVerification: exit 0\nRollback: restore\nScope: Linux\n\u{3010}Result\u{3011}verified";
    let legacy = "\u{3010}\u{524d}\u{56e0}\u{3011}fixture\n\u{3010}\u{884c}\u{4e3a}\u{3011}\n\u{89e6}\u{53d1}\u{6761}\u{4ef6}\u{ff1a}authorized task\n\u{524d}\u{7f6e}\u{6761}\u{4ef6}\u{ff1a}ready\n\u{6b65}\u{9aa4}\u{ff1a}run\n\u{6210}\u{529f}\u{5224}\u{636e}\u{ff1a}exit 0\n\u{5931}\u{8d25}\u{56de}\u{9000}\u{ff1a}restore\n\u{9002}\u{7528}\u{8fb9}\u{754c}\u{ff1a}Linux\n\u{3010}\u{540e}\u{679c}\u{3011}verified";
    let mixed = english.replace("Verification:", "\u{6821}\u{9a8c}\u{ff1a}").replace("\u{3010}Cause\u{3011}", "\u{3010}\u{524d}\u{56e0}\u{3011}");
    for body in [english, legacy, &mixed] {
        assert!(missing_sections(body).is_empty(), "{body}");
    }
    for (label, old) in [("Verification: exit 0", "\u{6210}\u{529f}\u{5224}\u{636e}\u{ff1a}exit 0"), ("Rollback: restore", "\u{5931}\u{8d25}\u{56de}\u{9000}\u{ff1a}restore")] {
        assert_eq!(missing_sections(&english.replace(label, "")), missing_sections(&legacy.replace(old, "")));
    }
    assert_eq!(missing_sections("\u{672a}\u{9a8c}\u{8bc1}; verification pending; rollback unavailable"), SECTIONS);
}

#[test]
fn template_accepts_aliases_and_flags_gaps() {
    let full = "\u{3010}\u{524d}\u{56e0}\u{3011}x\n\u{3010}\u{884c}\u{4e3a}\u{3011}\n\u{89e6}\u{53d1}\u{6761}\u{4ef6}\u{ff1a}\u{4e0d}\u{80fd}\u{672c}\u{5730}\u{8dd1}\n\u{524d}\u{7f6e}\u{ff1a}\u{5df2}\u{767b}\u{5f55}\n\u{6b65}\u{9aa4}\u{ff1a}1) run\n\u{6821}\u{9a8c}\u{ff1a}\u{9000}\u{51fa}\u{7801} 0\n\u{56de}\u{9000}\u{ff1a}\u{8fd8}\u{539f}\u{5907}\u{4efd}\n\u{8fb9}\u{754c}\u{ff1a}\u{4ec5} Linux\n\u{3010}\u{540e}\u{679c}\u{3011}y";
    let missing = missing_sections(full);
    assert!(missing.is_empty(), "missing={missing:?}");

    let missing = missing_sections("\u{3010}\u{524d}\u{56e0}\u{3011}x\u{3010}\u{884c}\u{4e3a}\u{3011}\u{6b65}\u{9aa4}\u{ff1a}\u{53ea}\u{5199}\u{4e86}\u{4e00}\u{6b65}\u{3010}\u{540e}\u{679c}\u{3011}y");
    assert_eq!(missing, vec!["Trigger", "Prerequisites", "Verification", "Rollback", "Scope"]);

    // Prose that merely contains an alias word does not satisfy the template.
    let prose = missing_sections("\u{3010}\u{524d}\u{56e0}\u{3011}x\u{3010}\u{884c}\u{4e3a}\u{3011}\u{6b65}\u{9aa4}\u{ff1a}\u{7ed3}\u{679c}\u{672a}\u{9a8c}\u{8bc1}\u{ff0c}\u{8fb9}\u{754c}\u{60c5}\u{51b5}\u{5f88}\u{591a}\u{3010}\u{540e}\u{679c}\u{3011}y");
    assert!(prose.contains(&"Verification"), "\u{672a}\u{9a8c}\u{8bc1} must not stand in for \u{6821}\u{9a8c}");
}

#[test]
fn intent_matches_procedural_questions_only() {
    assert!(howto_intent("OJ \u{4e0a}\u{4f20}\u{9898}\u{76ee}\u{62a5}\u{9519}\u{600e}\u{4e48}\u{4fee}"));
    assert!(howto_intent("how to rebuild the SDK"));
    assert!(howto_intent("\u{90e8}\u{7f72}\u{5931}\u{8d25}\u{6392}\u{67e5}"));
    assert!(!howto_intent("respire \u{54c1}\u{724c}\u{4e2d}\u{6587}\u{540d}"));
    assert!(!howto_intent("\u{8bb0}\u{5fc6}\u{7cfb}\u{7edf}\u{662f}\u{4e8b}\u{5b9e}\u{6027}\u{8fd8}\u{662f}\u{7a0b}\u{5e8f}\u{6027}"));
}

#[test]
fn merge_keeps_fact_order_but_surfaces_one_procedure() {
    let primary = vec![
        row("a", "context", "fact a", 0.9),
        row("b", "context", "fact b", 0.8),
        row("c", "context", "fact c", 0.7),
        row("d", "context", "fact d", 0.6),
    ];
    let secondary = vec![row("s", "skill", "procedure", 0.55)];
    let (merged, promoted) = merge_howto(primary, secondary, 4, &HashMap::new());
    assert!(promoted);
    let ids: Vec<&str> = merged.iter().map(|r| r.entry.id.as_str()).collect();
    assert_eq!(ids, vec!["a", "b", "s", "c"]);
    assert_eq!(merged[2].score, 0.55, "core score must stay untouched");
}

#[test]
fn merge_does_not_displace_when_a_procedure_is_already_visible() {
    let primary = vec![
        row("s", "skill", "procedure", 0.9),
        row("a", "context", "fact a", 0.8),
        row("b", "context", "fact b", 0.7),
    ];
    let (merged, promoted) = merge_howto(primary, Vec::new(), 3, &HashMap::new());
    assert!(!promoted);
    assert_eq!(merged[0].entry.id, "s");
}

#[test]
fn failing_procedure_sinks_below_working_one() {
    let primary = Vec::new();
    let secondary = vec![
        row("bad", "skill", "broken procedure", 0.70),
        row("good", "skill", "working procedure", 0.69),
    ];
    let mut runs = HashMap::new();
    runs.insert("bad".into(), RunScore { ok: 0, fail: 4 });
    runs.insert("good".into(), RunScore { ok: 4, fail: 0 });
    let (merged, promoted) = merge_howto(primary, secondary, 5, &runs);
    assert!(!promoted, "a procedure is already in the top three");
    let ids: Vec<&str> = merged.iter().map(|r| r.entry.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["good", "bad"],
        "run history reorders procedures only"
    );
    assert_eq!(merged[1].score, 0.70, "core score must stay untouched");
}

#[test]
fn merge_dedupes_by_id() {
    let primary = vec![row("x", "skill", "same", 0.8)];
    let secondary = vec![row("x", "skill", "same", 0.9)];
    let (merged, _) = merge_howto(primary, secondary, 5, &HashMap::new());
    assert_eq!(merged.len(), 1);
}

#[test]
fn operational_smell_separates_manuals_from_facts() {
    assert!(looks_operational(
        "\u{6b65}\u{9aa4}\u{ff1a}1) cargo build -p respire\u{ff1b}\u{6821}\u{9a8c}\u{ff1a}\u{9000}\u{51fa}\u{7801} 0"
    ));
    assert!(looks_operational("\u{4fee}\u{590d}\u{65b9}\u{6cd5} `git rebase --abort`"));
    assert!(!looks_operational("respire \u{7684}\u{4e2d}\u{6587}\u{540d}\u{662f}\u{777f}\u{601d}\u{8bb0}\u{5fc6}"));
    assert!(!looks_operational("\u{8fd9}\u{6761}\u{8bb0}\u{5fc6}\u{8bb2}\u{7684}\u{662f}\u{54c1}\u{724c}\u{547d}\u{540d}\u{7ed3}\u{8bba}"));
    assert_eq!(HOT_RECALL_THRESHOLD, 5);
}

#[test]
fn empty_recall_stays_empty() {
    let (merged, promoted) = merge_howto(Vec::new(), Vec::new(), 10, &HashMap::new());
    assert!(merged.is_empty());
    assert!(!promoted);
}

#[test]
fn knowledge_entries_are_procedures_too() {
    let primary = vec![
        row("a", "context", "fact a", 0.9),
        row("b", "context", "fact b", 0.8),
        row("c", "context", "fact c", 0.7),
    ];
    let secondary = vec![row("k", "knowledge", "manual", 0.5)];
    let (merged, promoted) = merge_howto(primary, secondary, 4, &HashMap::new());
    assert!(promoted);
    assert_eq!(merged[2].entry.id, "k");
    assert!(is_procedure("knowledge"));
    assert!(!is_procedure("task"));
}
