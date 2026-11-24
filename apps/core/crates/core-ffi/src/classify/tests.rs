use crate::localized_test_data::{CAUSAL_FULLWIDTH_RESPONSE, TITLE, TAGS, MIXED_BODY, SKILLS, PROGRAMMING, TASKS, PITFALLS, HEALTH, LEARNING};
use super::*;
use anyhow::{anyhow, Result};

#[test]
fn english_and_historical_category_responses_keep_canonical_keys() -> Result<()> {
    for category in CATALOG {
        for label in [category.prompt_label, category.title] {
            let response = json!({"answers":{"category":{"choice":label,"confidence":0.75,"probabilities":{label:0.75}}}});
            let verdict = parse_response(&response)?;
            assert_eq!(verdict.choice, category.title);
            assert_eq!(verdict.probabilities.get(category.title), Some(&0.75));
            assert_eq!(verdict.confidence, 0.75);
        }
    }
    assert!(parse_response(&json!({"answers":{"category":{"choice":"unrecognized"}}})).is_err());
    let mixed = parse_response(&json!({"answers":{"category":{"choice":"Skills","probabilities":{"Skills":0.3,(SKILLS):0.6,"external":0.1}}}}))?;
    assert_eq!(mixed.probabilities.len(), 2);
    assert_eq!(mixed.probabilities.get(SKILLS), Some(&0.6));
    assert_eq!(mixed.probabilities.get("external"), Some(&0.1));
    Ok(())
}

#[test]
fn english_projection_does_not_translate_state_or_storage() -> Result<()> {
    let questions = build_questions();
    let criteria = questions["category"]["criteria"].as_object().ok_or_else(|| anyhow!("missing criteria"))?;
    assert_eq!(criteria.len(), CATALOG.len());
    for category in CATALOG {
        assert!(category.prompt_label.is_ascii() && category.prompt_gist.is_ascii());
        assert_eq!(criteria.get(category.prompt_label), Some(&json!(category.prompt_gist)));
        assert!(!criteria.contains_key(category.title));
    }
    let state = build_state(TITLE, "context", TAGS, MIXED_BODY, 100);
    assert_eq!(state["title"], TITLE);
    assert_eq!(state["content"], MIXED_BODY);
    assert_eq!(catalog_titles()[9], PROGRAMMING);
    Ok(())
}

#[test]
fn ds_request_shape() -> Result<()> {
    let r = build_ds_request("title", "task", "a,b", "bodybody", 2, "deepseek-chat");
    assert_eq!(r["model"], "deepseek-chat");
    assert_eq!(r["logprobs"], true);
    assert_eq!(r["top_logprobs"], DS_TOP_LOGPROBS);
    assert_eq!(r["thinking"]["type"], "disabled");
    assert_eq!(r["temperature"], 0);
    // body is truncated
    let user = r["messages"][1]["content"]
        .as_str()
        .ok_or_else(|| anyhow!("missing user content"))?;
    assert!(user.contains("Body: bo\n"), "{user}");
    // numbered list includes every catalog title
    for c in CATALOG {
        assert!(user.contains(c.prompt_label), "missing catalog label {}", c.prompt_label);
    }
    assert!(user.contains("1. "));
    assert!(user.contains(&format!("{}. ", CATALOG.len())));
    // aidom_shot prompt: prefer the three AI-domain roots + examples
    assert!(user.contains("prefer the three AI-domain roots"), "{user}");
    assert!(user.contains("-> Pitfalls"));
    assert!(user.contains("-> Skills"));
    assert!(user.contains("-> Experience"));
    Ok(())
}

#[test]
fn ds_parse_choice_and_probs() -> Result<()> {
    // The tenth catalog title is the programming category (CATALOG order).
    let v = json!({
        "choices": [{
            "message": {"content": "10", "role": "assistant"},
            "logprobs": {"content": [{
                "token": "10",
                "logprob": -0.1053,
                "top_logprobs": [
                    {"token": "10", "logprob": -0.1053},   // e^-0.1053 ≈ 0.90
                    {"token": "9",  "logprob": (0.1f64).ln()}, // ln(0.10); expression form avoids clippy::approx_constant
                    {"token": "23", "logprob": -9.210_340}, // ln(0.0001) ≈0
                ]
            }]}
        }],
        "usage": {"prompt_tokens": 191, "completion_tokens": 1}
    });
    let ver = parse_ds_response(&v)?;
    assert_eq!(ver.choice, PROGRAMMING);
    assert!(
        (ver.confidence - 0.90).abs() < 0.02,
        "conf={}",
        ver.confidence
    );
    assert!(ver.probabilities.contains_key(TASKS));
    assert_eq!(ver.input_tokens, 191);
    assert_eq!(ver.output_tokens, 1);
    Ok(())
}

#[test]
fn ds_parse_falls_back_to_content() -> Result<()> {
    // First token is not a number (e.g. space) -> fall back to full content
    let v = json!({
        "choices": [{
            "message": {"content": " 23 "},
            "logprobs": {"content": [{"token": " ", "logprob": -1.0, "top_logprobs": []}]}
        }]
    });
    let ver = parse_ds_response(&v)?;
    assert_eq!(ver.choice, PITFALLS);
    Ok(())
}

#[test]
fn ds_parse_merges_same_category_variants() -> Result<()> {
    // Same-category token variants ("9" and "9.") merge probabilities
    let v = json!({
        "choices": [{
            "message": {"content": "9"},
            "logprobs": {"content": [{
                "token": "9", "logprob": -0.7,
                "top_logprobs": [
                    {"token": "9",  "logprob": -0.7},
                    {"token": "9.", "logprob": -0.9}
                ]
            }]}
        }]
    });
    let ver = parse_ds_response(&v)?;
    assert_eq!(ver.choice, TASKS);
    // e^-0.7 + e^-0.9 ≈ 0.4966 + 0.4066 = 0.9032
    assert!(
        (ver.confidence - 0.903).abs() < 0.01,
        "conf={}",
        ver.confidence
    );
    Ok(())
}

#[test]
fn ds_parse_errors_when_unparseable() {
    let v = json!({
        "choices": [{"message": {"content": "I do not know"},
                     "logprobs": {"content": [{"token": "I", "logprob": -1.0}]}}]
    });
    assert!(parse_ds_response(&v).is_err());
    assert!(parse_ds_response(&json!({})).is_err());
}

#[test]
fn causal_ops_parse() {
    let text =
        CAUSAL_FULLWIDTH_RESPONSE;
    let ops = parse_causal_ops(text);
    assert_eq!(ops.len(), 3, "{ops:?}");
    assert_eq!(ops[0], ("db550ed0".to_owned(), "5784dc5e".to_owned()));
    assert_eq!(ops[2], ("7b9cc0f0".to_owned(), "592ddd09".to_owned()));
}

#[test]
fn causal_validate_rejects_self_and_out_of_scope() {
    let rows = vec![
        (
            "aaaaaaaa-1".to_owned(),
            String::new(),
            "root".to_owned(),
            true,
        ),
        (
            "bbbbbbbb-1".to_owned(),
            "aaaaaaaa-1".to_owned(),
            "alpha".to_owned(),
            true,
        ),
        (
            "cccccccc-1".to_owned(),
            "aaaaaaaa-1".to_owned(),
            "beta".to_owned(),
            true,
        ),
        (
            "dddddddd-1".to_owned(),
            "bbbbbbbb-1".to_owned(),
            "gamma".to_owned(),
            true,
        ),
    ];
    let scope: std::collections::HashSet<String> =
        ["bbbbbbbb-1".to_owned(), "cccccccc-1".to_owned()]
            .into_iter()
            .collect();
    let ops = vec![
        ("bbbbbbbb".to_owned(), "bbbbbbbb".to_owned()), // self-parent
        ("cccccccc".to_owned(), "bbbbbbbb".to_owned()), // valid
        ("dddddddd".to_owned(), "cccccccc".to_owned()), // out of segment (dd not in scope)
    ];
    let (ok, bad) = validate_causal_ops(&ops, &rows, Some(&scope));
    assert_eq!(ok.len(), 1, "ok={ok:?} bad={bad:?}");
    assert_eq!(ok[0].0, "cccccccc-1");
    assert_eq!(bad.len(), 2, "{bad:?}");
}

#[test]
fn causal_detect_global_cycle() {
    // A->B and B->A: per-op check misses it; needs a global pass
    let rows = vec![
        (
            "aaaaaaaa-1".to_owned(),
            String::new(),
            "root".to_owned(),
            true,
        ),
        (
            "bbbbbbbb-1".to_owned(),
            "aaaaaaaa-1".to_owned(),
            "alpha".to_owned(),
            true,
        ),
        (
            "cccccccc-1".to_owned(),
            "aaaaaaaa-1".to_owned(),
            "beta".to_owned(),
            true,
        ),
        (
            "dddddddd-1".to_owned(),
            "aaaaaaaa-1".to_owned(),
            "gamma".to_owned(),
            true,
        ),
    ];
    let ops = vec![
        ("bbbbbbbb-1".to_owned(), "cccccccc-1".to_owned()),
        ("cccccccc-1".to_owned(), "bbbbbbbb-1".to_owned()),
        ("dddddddd-1".to_owned(), "bbbbbbbb-1".to_owned()), // this one is not in a cycle
    ];
    let (kept, bad) = detect_global_cycles(&ops, &rows);
    assert!(!bad.is_empty(), "should detect a cycle");
    // both cycling ops are dropped as a batch; dd is kept
    assert!(kept.iter().all(|(c, _)| c == "dddddddd-1"), "{kept:?}");
}

#[test]
fn ds_endpoint_normalizes() {
    assert_eq!(
        ds_endpoint("https://api.deepseek.com/v1"),
        "https://api.deepseek.com/v1/chat/completions"
    );
    assert_eq!(
        ds_endpoint("https://api.deepseek.com/v1/"),
        "https://api.deepseek.com/v1/chat/completions"
    );
    assert_eq!(
        ds_endpoint("https://x/y/chat/completions"),
        "https://x/y/chat/completions"
    );
}

#[test]
fn sample_verdict_raises_temperature() {
    // Sampling must raise temperature - at 0 the answer is locked and N identical votes fake conf=1.00.
    // A local mock cannot prove the temperature is sent, so we check the constant and body rewrite.
    assert!(DS_SAMPLE_TEMPERATURE > 0.0, "sample temperature must be >0");
    let mut body = build_ds_request("title", "task", "", "body", 10, "m");
    assert_eq!(body["temperature"], 0); // single-shot classify still uses 0 (most likely answer)
    body["temperature"] = json!(DS_SAMPLE_TEMPERATURE);
    assert_eq!(body["temperature"], 1.0);
}

#[test]
fn ds_detects_degenerate_logprobs() -> Result<()> {
    // Official DS endpoint degenerate shape: 0.0 and -9999.0
    let degen = json!([{
        "token": "2", "logprob": 0.0,
        "top_logprobs": [
            {"token": "2", "logprob": 0.0},
            {"token": "3", "logprob": -9999.0},
            {"token": "1", "logprob": -9999.0}
        ]
    }]);
    let degen_arr = degen
        .as_array()
        .ok_or_else(|| anyhow!("degen fixture is not an array"))?;
    assert!(logprobs_degenerate(degen_arr));

    // Real values (b.ai relay): at least one candidate that is neither 0 nor -9999
    let real = json!([{
        "token": "10", "logprob": -0.1053,
        "top_logprobs": [
            {"token": "10", "logprob": -0.1053},
            {"token": "9",  "logprob": (0.1f64).ln()}
        ]
    }]);
    let real_arr = real
        .as_array()
        .ok_or_else(|| anyhow!("real fixture is not an array"))?;
    assert!(!logprobs_degenerate(real_arr));

    // No candidates -> degenerate
    let empty = json!([{"token": "1", "logprob": 0.0, "top_logprobs": []}]);
    let empty_arr = empty
        .as_array()
        .ok_or_else(|| anyhow!("empty fixture is not an array"))?;
    assert!(logprobs_degenerate(empty_arr));
    assert!(logprobs_degenerate(&[]));
    Ok(())
}

#[test]
fn ds_probs_none_when_degenerate() -> Result<()> {
    let v = json!({
        "choices": [{
            "message": {"content": "2"},
            "logprobs": {"content": [{
                "token": "2", "logprob": 0.0,
                "top_logprobs": [{"token": "2", "logprob": 0.0}, {"token": "3", "logprob": -9999.0}]
            }]}
        }]
    });
    assert!(
        ds_probs_from(&v).is_none(),
        "degenerate values must not be treated as probabilities"
    );
    // Choice should still parse; confidence is 0
    let ver = parse_ds_response(&v)?;
    assert_eq!(ver.choice, HEALTH); // catalog index 2
    assert_eq!(ver.confidence, 0.0);
    assert!(ver.probabilities.is_empty());
    Ok(())
}

#[test]
fn state_truncates() {
    let st = build_state("title", "task", "a,b", "0123456789", 4);
    assert_eq!(st["content"], "0123");
    assert_eq!(st["title"], "title");
    assert_eq!(st["kind"], "task");
}

#[test]
fn questions_has_all_catalog() -> Result<()> {
    let q = build_questions();
    let crit = q["category"]["criteria"]
        .as_object()
        .ok_or_else(|| anyhow!("criteria is not an object"))?;
    assert_eq!(crit.len(), CATALOG.len());
    assert_eq!(q["category"]["type"], "choice");
    Ok(())
}

#[test]
fn parse_ok() -> Result<()> {
    let v = json!({
        "answers": {"category": {
            "type": "choice", "choice": PROGRAMMING,
            "confidence": 0.83,
            "probabilities": {(PROGRAMMING): 0.9, (LEARNING): 0.1}
        }},
        "usage": {"input_tokens": 321, "output_tokens": 12}
    });
    let ver = parse_response(&v)?;
    assert_eq!(ver.choice, PROGRAMMING);
    assert!((ver.confidence - 0.83).abs() < 1e-6);
    assert_eq!(ver.probabilities.len(), 2);
    assert_eq!(ver.input_tokens, 321);
    Ok(())
}

#[test]
fn parse_missing_answers_errs() {
    assert!(parse_response(&json!({"x": 1})).is_err());
}

#[test]
fn top_roots_chain_and_cycle() {
    // c is root; a->b->c; d is a self-loop
    let rows = vec![
        ("a".to_owned(), "b".to_owned(), "alpha".to_owned()),
        ("b".to_owned(), "c".to_owned(), "beta".to_owned()),
        ("c".to_owned(), String::new(), PROGRAMMING.to_owned()),
        ("d".to_owned(), "d".to_owned(), "self-loop".to_owned()),
    ];
    let m = top_roots(&rows);
    assert_eq!(m["a"], PROGRAMMING);
    assert_eq!(m["b"], PROGRAMMING);
    assert_eq!(m["c"], PROGRAMMING);
    assert!(m.contains_key("d")); // cycle guard must not hang
}

#[test]
fn report_counts() {
    let mk = |id: &str, cur: &str, choice: &str, conf: f32| ItemResult {
        id: id.to_owned(),
        title: format!("title{id}"),
        current_root: cur.to_owned(),
        verdict: Some(Verdict {
            choice: choice.to_owned(),
            confidence: conf,
            probabilities: BTreeMap::new(),
            input_tokens: 1,
            output_tokens: 1,
        }),
        error: None,
        suggest_parent: None,
    };
    let items = vec![
        mk("1", PROGRAMMING, PROGRAMMING, 0.9), // match
        mk("2", PROGRAMMING, LEARNING, 0.8), // mismatch
        mk("3", PROGRAMMING, LEARNING, 0.3), // low confidence
        mk("5", "", PITFALLS, 0.7),           // unrooted
        ItemResult {
            id: "4".to_owned(),
            title: "fail".to_owned(),
            current_root: String::new(),
            verdict: None,
            error: Some("HTTP 500".to_owned()),
            suggest_parent: None,
        },
    ];
    let r = render_report(&items, 0.5);
    assert!(r.contains("COMMAND"), "{r}");
    assert!(r.contains("classify  FAIL"), "{r}");
    assert!(
        r.contains("5      1      1         1         1               1"),
        "{r}"
    );
    assert!(r.contains("MISMATCH"), "{r}");
    assert!(r.contains("UNROOTED"), "{r}");
    assert!(r.contains("LOW_CONFIDENCE"), "{r}");
    assert!(r.contains("FAILED"), "{r}");
    // Keep the report free of the icon code points used by the legacy CLI.
    const LEGACY_ICON_CODEPOINTS: &[u32] =
        &[0x1F50D, 0x2705, 0x274C, 0x26A0, 0x1F4CC, 0x1F194, 0x1F5D3];
    assert!(r
        .chars()
        .all(|c| !LEGACY_ICON_CODEPOINTS.contains(&(c as u32))));
    let structured = build_report(&items, 0.5);
    assert_eq!(structured.command, "classify");
    assert_eq!(structured.status, "fail");
    assert_eq!(structured.summary.total, 5);
    let json = match serde_json::to_value(structured) {
        Ok(value) => value,
        Err(error) => panic!("report serialization failed: {error}"),
    };
    assert_eq!(json["command"], "classify");
    assert_eq!(json["summary"]["failed"], 1);
    assert!(json["items"].is_array());
}
