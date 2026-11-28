use super::*;

#[test]
fn switch_tasks_render_item_counts_and_elapsed_seconds() {
    let indexing = model_task_text(&serde_json::json!({
        "phase": "switch-index", "item": "granite", "done": 12, "total": 48,
        "elapsed": 7, "elapsed_secs": 99
    }));
    assert!(indexing.contains(&task_phase("switch-index")));
    assert!(indexing.contains("25.0% (12/48) granite 7"));
    assert!(!indexing.contains("MB"));
    assert!(!indexing.contains("99"));

    let diaries = model_task_text(&serde_json::json!({
        "phase": "switch-diary", "item": "granite", "done": 3, "total": 0,
        "elapsed_secs": 9
    }));
    assert!(diaries.contains(&task_phase("switch-diary")));
    assert!(diaries.contains("3 granite 9"));
    assert!(!diaries.contains('%'));
}

#[test]
fn recall_reference_label_covers_empty_results_without_changing_payload() {
    for (status, count) in [(Status::Skip, 0), (Status::Ok, 1)] {
        let result =
            ResultEnvelope::new("recall", status, serde_json::json!({"count":count}), vec![]);
        assert_eq!(result.summary["count"], count);
        assert!(result.summary["reference"]
            .as_str()
            .is_some_and(|value| value.contains("does not override")));
        assert!(result.items.is_empty());
    }
    let result = ResultEnvelope::new("status", Status::Ok, serde_json::json!({}), vec![]);
    assert!(result.summary.get("reference").is_none());
}

#[test]
fn table_is_aligned_and_compact() {
    assert_eq!(
        render_table(&["NAME", "STATUS"], &[vec!["store".into(), "PASS".into()]]),
        "NAME   STATUS\nstore  PASS"
    );
}

#[test]
fn failure_diagnostics_cover_display_only_failures_without_changing_other_states() {
    let mut fail = ResultEnvelope::new(
        "show",
        Status::Fail,
        serde_json::json!({}),
        vec![Item::new("entry", Status::Fail, "not found")],
    );
    fail.ensure_failure_diagnostics();
    assert_eq!(fail.errors, ["entry: not found"]);
    fail.ensure_failure_diagnostics();
    assert_eq!(fail.errors.len(), 1);
    fail.errors = vec![" ".into()];
    fail.items.clear();
    fail.ensure_failure_diagnostics();
    assert_eq!(fail.errors, ["show failed"]);
    fail.errors = vec!["original actionable diagnostic".into()];
    fail.ensure_failure_diagnostics();
    assert_eq!(fail.errors, ["original actionable diagnostic"]);
    for status in [Status::Ok, Status::Warn, Status::Pending, Status::Skip] {
        let mut result = ResultEnvelope::new(
            "decision",
            status,
            serde_json::json!({}),
            vec![Item::new("display", Status::Fail, "optional component")],
        );
        result.ensure_failure_diagnostics();
        assert!(result.errors.is_empty());
    }

    let input = anyhow::Error::new(respire_client_host::input_error::InputError(
        "fixture invalid input".to_owned(),
    ))
    .context("fixture input context");
    let (failure, exit) = crate::output_emit::command_failure(&input);
    assert!(matches!(failure.status, Status::Fail));
    assert_eq!(exit, 2);
    assert_eq!(failure.summary, serde_json::json!({"reason":"invalid_input"}));
    assert_eq!(failure.details, serde_json::json!({"error_type":"user"}));
    assert_eq!(failure.errors, [format!("{input:#}")]);

    let runtime = anyhow::anyhow!("fixture runtime failure");
    let (failure, exit) = crate::output_emit::command_failure(&runtime);
    assert!(matches!(failure.status, Status::Fail));
    assert_eq!(exit, 1);
    assert_eq!(failure.summary, serde_json::json!({"reason":"runtime_error"}));
    assert_eq!(failure.details, serde_json::json!({"error_type":"runtime"}));
    assert_eq!(failure.errors, [format!("{runtime:#}")]);

    let operation = crate::runtime_error::OperationContext {
        scope: respire_protocol::RuntimeRequestScope {
            account_id: "fixture-account".to_owned(),
            instance_id: "fixture-instance".to_owned(),
            generation: 7,
        },
        operation_id: "fixture-operation".to_owned(),
        idempotency_key: "fixture-idempotency-key".to_owned(),
    };
    let unknown = anyhow::Error::new(crate::runtime_error::RuntimeError::OutcomeUnknown {
        request_id: "fixture-request".to_owned(),
        operation,
        detail: "fixture response timeout".to_owned(),
    })
    .context("fixture transport context");
    let (failure, exit) = crate::output_emit::command_failure(&unknown);
    assert!(matches!(failure.status, Status::Fail));
    assert_eq!(exit, 1);
    assert_eq!(failure.summary, serde_json::json!({"reason":"request_outcome_unknown"}));
    assert_eq!(failure.details, serde_json::json!({
        "error_type":"outcome_unknown",
        "request_id":"fixture-request",
        "operation_id":"fixture-operation",
        "idempotency_key":"fixture-idempotency-key",
        "scope":{"account_id":"fixture-account","instance_id":"fixture-instance","generation":7},
        "replayed":false,
        "result_method":"runtime.result"
    }));
    assert_eq!(failure.errors, [format!("{unknown:#}")]);

    for (status, expected) in [
        (Status::Ok, 0), (Status::Skip, 0), (Status::Warn, 2),
        (Status::Pending, 2), (Status::Fail, 1),
    ] {
        assert_eq!(crate::output_state::status_exit_code(status), expected);
    }
}

#[test]
fn status_prioritizes_fail_then_warn() {
    assert!(matches!(
        status_from_items(&[Item::new("a", Status::Warn, "")]),
        Status::Warn
    ));
    assert!(matches!(
        status_from_items(&[Item::new("a", Status::Fail, "")]),
        Status::Fail
    ));
}

#[test]
fn table_handles_cjk_and_control_characters() {
    let table = render_table(
        &["NAME", "VALUE"],
        &[
            vec!["\u{4e2d}\u{6587}".into(), "one\ntwo".into()],
            vec!["a".into(), "plain".into()],
        ],
    );
    assert!(table.contains("\u{4e2d}\u{6587}  one two"));
    assert!(table.contains("a     plain"));
    assert_eq!(table.lines().count(), 3);
}

fn pin_english() -> (std::sync::MutexGuard<'static, ()>, Option<String>) {
    let guard = crate::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let prev = crate::env::var("RSRS_LANG").ok();
    std::env::set_var("RSRS_LANG", "en");
    (guard, prev)
}

fn restore_lang(prev: Option<String>) {
    match prev {
        Some(value) => std::env::set_var("RSRS_LANG", value),
        None => std::env::remove_var("RSRS_LANG"),
    }
}

#[test]
fn explicit_pending_result_is_not_replaced_with_success() -> anyhow::Result<()> {
    let (_lock, prev) = pin_english();
    let result = ResultEnvelope::new(
        "sync-restore",
        Status::Pending,
        serde_json::json!({"pending_sync":true}),
        vec![Item::new("sync", Status::Pending, "queued")],
    );
    let value: Value = serde_json::from_str(&result.render(true)?)?;
    assert_eq!(value["status"], "pending");
    assert!(value["errors"].is_array());
    assert!(value["actions"].is_array());
    let text = result.render(false)?;
    restore_lang(prev);
    assert!(
        text.contains("PENDING") && !text.contains("RESULT:"),
        "{text}"
    );
    Ok(())
}

#[test]
fn text_render_omits_details() -> anyhow::Result<()> {
    let (_lock, prev) = pin_english();
    let mut result = ResultEnvelope::new(
        "doctor",
        Status::Warn,
        serde_json::json!({"pass": 1, "warn": 1}),
        vec![Item::new("session", Status::Warn, "missing")],
    );
    result.details = serde_json::json!({"source":"local"});
    let text = result.render(false)?;
    result.items = vec![
        Item::new("embedder", Status::Warn, "BGE-M3 preparation/indexing in progress through the runtime; model task: {\"active\":true}"),
        Item::new("model index", Status::Warn, "BGE-M3 index pending; runtime status: {\"state\":\"running\"}"),
    ];
    result.details = serde_json::json!({
        "model_operation": {"active":true, "phase":"download", "item":"model_quantized.onnx",
            "done":29007544, "total":569694530, "elapsed":45, "idle":0},
        "retrieval_index": {"state":"running", "error":null},
    });
    let progress_text = result.render(false)?;
    anyhow::ensure!(
        progress_text.contains("5.1%") && progress_text.contains("29.0/569.7 MB"),
        "{progress_text}"
    );
    anyhow::ensure!(
        progress_text.contains("model_quantized.onnx") && !progress_text.contains("Elapsed"),
        "{progress_text}"
    );
    anyhow::ensure!(
        !progress_text.contains('{') && !progress_text.contains("worker_active"),
        "{progress_text}"
    );
    let json: Value = serde_json::from_str(&result.render(true)?)?;
    anyhow::ensure!(json["details"]["model_operation"]["done"] == 29007544);
    anyhow::ensure!(json["items"][0]["value"] == result.items[0].value);
    result.details["model_operation"]["total"] = serde_json::json!(0);
    result.details["retrieval_index"]["error"] = serde_json::json!("download failed");
    let pending = result.render(false)?;
    anyhow::ensure!(
        pending.contains("waiting for progress") && pending.contains("download failed"),
        "{pending}"
    );
    std::env::set_var("RSRS_LANG", "zh");
    let chinese = result.render(false)?;
    anyhow::ensure!(
        chinese.contains("\u{4e0b}\u{8f7d}\u{4e2d}")
            && chinese.contains("\u{7b49}\u{5f85}\u{8fdb}\u{5ea6}")
            && chinese.contains("\u{540e}\u{53f0}\u{7d22}\u{5f15}"),
        "{chinese}"
    );
    restore_lang(prev);
    assert!(!text.contains("SUMMARY") && !text.contains("ACTION"));
    assert!(text.contains("session") && text.contains("missing"));
    assert!(!text.contains('{'), "{text}");
    assert!(!text.contains("DETAILS:"));
    assert!(text.contains("WARN") && !text.contains("RESULT:"), "{text}");
    Ok(())
}

#[test]
fn human_summary_is_a_table_not_json() -> anyhow::Result<()> {
    let (_lock, prev) = pin_english();
    let result = ResultEnvelope::new(
        "sync",
        Status::Warn,
        serde_json::json!({
            "conflicts": 10,
            "local_alive": 3190,
            "converged": true
        }),
        vec![],
    );
    let text = result.render(false)?;
    restore_lang(prev);
    assert!(text.contains("Conflicts"), "{text}");
    assert!(text.contains("3190"), "{text}");
    assert!(text.contains("yes"), "{text}");
    assert!(!text.contains('{'), "{text}");
    assert!(!text.contains('}'), "{text}");
    Ok(())
}

#[test]
fn humanize_covers_json_phrases_and_sync_lines() -> anyhow::Result<()> {
    let (_lock, prev) = pin_english();
    let phrases = [
        "converged",
        "not-converged",
        "active-count-diff",
        "cloud sync (auto)",
        "cloud sync (manual)",
        "local authority store (offline)",
        "reset",
        "kept",
        "true",
        "false",
    ];
    for phrase in phrases {
        anyhow::ensure!(!humanize_value(phrase).is_empty(), "{phrase}");
    }
    anyhow::ensure!(humanize_value("12 total / 3 active").contains("total"));
    anyhow::ensure!(humanize_value("pulled 1 / pushed 2").contains("pushed"));
    anyhow::ensure!(
        humanize_value("send 0 / conflicts 1 / decrypt 2 / index 3").contains("index")
    );
    anyhow::ensure!(humanize_value("plain text") == "plain text");
    anyhow::ensure!(humanize_value("{not json") == "{not json");
    let object = humanize_value(r#"{"conflicts":1,"note":null}"#);
    anyhow::ensure!(
        object.contains("Conflicts") && object.contains("note"),
        "{object}"
    );
    anyhow::ensure!(humanize_value("[1,true]").contains("yes"));
    let deep = humanize_value(
        r#"{"state":{"task":{"active":true,"files":["model.onnx",{"done":29}]}}}"#,
    );
    anyhow::ensure!(
        !deep.contains('{') && !deep.contains('[') && deep.contains("model.onnx"),
        "{deep}"
    );
    let nested = summary_rows(&serde_json::json!({"outer":{"inner":{"a":1}},"ids":[1,2]}));
    anyhow::ensure!(
        nested.iter().any(|row| row[0].contains("inner")),
        "{nested:?}"
    );
    anyhow::ensure!(nested.iter().any(|row| row[1] == "2"), "{nested:?}");
    std::env::set_var("RSRS_LANG", "zh");
    anyhow::ensure!(humanize_value("converged") == "\u{5df2}\u{5bf9}\u{9f50}");
    anyhow::ensure!(humanize_value("12 total / 3 active").contains("\u{603b}\u{6570}"));
    anyhow::ensure!(humanize_value("pulled 1 / pushed 2").contains("\u{4e0b}\u{8f7d}"));
    anyhow::ensure!(humanize_value("send 0 / conflicts 1 / decrypt 2 / index 3")
        .contains("\u{5f85}\u{63a8}\u{9001}"));
    anyhow::ensure!(humanize_value("not-converged") == "\u{672a}\u{5bf9}\u{9f50}");
    anyhow::ensure!(humanize_value("active-count-diff").contains("\u{4e0d}\u{4e00}\u{81f4}"));
    anyhow::ensure!(humanize_value("cloud sync (auto)").contains("\u{81ea}\u{52a8}"));
    anyhow::ensure!(humanize_value("cloud sync (manual)").contains("\u{624b}\u{52a8}"));
    anyhow::ensure!(
        humanize_value("local authority store (offline)").contains("\u{79bb}\u{7ebf}")
    );
    anyhow::ensure!(humanize_value("reset") == "\u{5df2}\u{91cd}\u{7f6e}");
    anyhow::ensure!(humanize_value("kept") == "\u{4fdd}\u{7559}");
    let mut result = ResultEnvelope::new("sync", Status::Ok, serde_json::json!({}), vec![]);
    result.actions.push("reembed".into());
    let text = result.render(false)?;
    anyhow::ensure!(text.contains("\u{52a8}\u{4f5c}"), "{text}");
    anyhow::ensure!(!text.contains('{'), "{text}");
    std::env::set_var("RSRS_LANG", "en");
    let text = result.render(false)?;
    anyhow::ensure!(text.contains("ACTION"), "{text}");
    restore_lang(prev);
    Ok(())
}

#[test]
fn sanitize_details_removes_sealed_storage_and_credentials() {
    let value = serde_json::json!({
        "entry": {
            "id": "id",
            "title": "title",
            "content": "plaintext body",
            "ciphertext": "deadbeef",
            "nonce": "cafebabe",
            "embedding_enc": "0011"
        },
        "token": "access-token",
        "safe": "ok"
    });
    let clean = sanitize_details("show", &value);
    assert_eq!(clean["entry"]["id"], "id");
    assert_eq!(clean["entry"]["content"], "plaintext body");
    assert!(clean["entry"].get("ciphertext").is_none());
    assert!(clean.get("token").is_none());
    assert_eq!(clean["safe"], "ok");

    let secret = sanitize_details(
        "secret",
        &serde_json::json!({
            "secret": "revealed",
            "secret_key": "key"
        }),
    );
    assert_eq!(secret["secret"], "revealed");
    assert_eq!(secret["secret_key"], "key");

    let share = sanitize_details(
        "share",
        &serde_json::json!({
            "payload": "1MEMSHARE1:encoded"
        }),
    );
    assert_eq!(share["payload"], "1MEMSHARE1:encoded");
    let review = sanitize_details(
        "review",
        &serde_json::json!({"code":"evidence_unavailable","token":"not-public"}),
    );
    assert_eq!(review["code"], "evidence_unavailable");
    assert!(review.get("token").is_none());
}
