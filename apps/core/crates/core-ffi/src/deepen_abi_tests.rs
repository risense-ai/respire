use crate::localized_test_data::{UNICODE_ROOT, UNICODE_ROOT_MISSING};
use anyhow::{Context, Result};

fn abi_call(core: &mut Core, request: &Value) -> Result<(i32, Value)> {
    let request = serde_json::to_vec(request)?;
    let mut output = Buffer::empty();
    // Request storage and the exclusive handle are live throughout the call.
    // The ABI owns the returned buffer until it is released.
    let code = unsafe { rs_core_call(core, request.as_ptr(), request.len(), &mut output) };
    anyhow::ensure!(
        !output.data.is_null() && output.len > 0,
        "missing ABI output"
    );
    let decoded = unsafe { serde_json::from_slice(slice::from_raw_parts(output.data, output.len)) };
    unsafe { rs_core_buffer_free(&mut output) };
    Ok((code, decoded?))
}

fn deepen_request(snapshots: &[Value], root: &str) -> Value {
    json!({"schema_version":1,"request_id":"fixture","operation":"deepen_plan",
        "payload":{"snapshots":snapshots,"root":root,"min":0.9}})
}

fn snapshot(id: &str, parent: &str) -> Value {
    json!({"id":id,"deleted":false,"updated_at":"","kind":"context","tags":"",
        "title":id,"project":"","computer":"","parent_id":parent,"created_at":"",
        "content_head":"","recall_count":0,"importance":"normal"})
}

fn assert_error(
    core: &mut Core,
    request: &Value,
    code: i32,
    request_id: &str,
    message: &str,
) -> Result<()> {
    let (got, response) = abi_call(core, request)?;
    assert_eq!(got, code);
    assert_eq!(
        response,
        json!({"schema_version":1,"request_id":request_id,
        "error":{"code":code,"message":message}})
    );
    Ok(())
}

#[test]
fn deepen_plan_v1_golden_valid_threshold_and_cluster() -> Result<()> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let mut core = Core::default();
    let mut snapshots = vec![snapshot("root-0000000000", "")];
    let embedding = |first: f32, second: f32| {
        STANDARD.encode([first.to_le_bytes(), second.to_le_bytes()].concat())
    };
    let root = "root-0000000000";
    for i in 0..14 {
        let mut child = snapshot(&format!("child-{i:02}"), root);
        if i < 4 {
            child["features"] = json!({"legacy":embedding(1.0, 0.0)});
        } else if i == 4 {
            child["features"] = json!({"legacy":embedding(0.0, 1.0)});
        }
        snapshots.push(child);
    }
    assert_error(&mut core, &deepen_request(&snapshots, root), ENGINE_FAILURE, "fixture",
        "this root is not flat (14 direct children, 0 in the grandchild chain) — hierarchy already exists or too few entries to deepen")?;
    snapshots.push(snapshot("child-14", root));
    for i in 0..10 {
        let parent = if i < 5 { "child-00" } else { "grand-04" };
        snapshots.push(snapshot(&format!("grand-{i:02}"), parent));
    }
    let request = deepen_request(&snapshots, root);
    let plan = json!({
        "sub_roots":[{"title":"child-03", "member_ids":["child-00","child-01","child-02","child-03"],
            "member_titles":["child-00","child-01","child-02","child-03"]}],
        "leftovers": (4..15).map(|i| {
            let id = format!("child-{i:02}");
            json!({"id":id,"title":id})
        }).collect::<Vec<_>>()
    });
    assert_eq!(
        abi_call(&mut core, &request)?,
        (
            OK,
            json!({"schema_version":1,"request_id":"fixture","result":[root,plan]})
        )
    );
    snapshots.push(snapshot("grand-10", "child-01"));
    assert_error(&mut core, &deepen_request(&snapshots, root), ENGINE_FAILURE, "fixture",
        "this root is not flat (15 direct children, 11 in the grandchild chain) — hierarchy already exists or too few entries to deepen")
}

#[test]
fn deepen_plan_v1_strict_envelopes_and_errors() -> Result<()> {
    let mut core = Core::default();
    let rows = [snapshot("root-0000-one", ""), snapshot("root-0000-two", "")];
    assert_error(
        &mut core,
        &deepen_request(&rows, "missing"),
        ENGINE_FAILURE,
        "fixture",
        "no such root: missing",
    )?;
    assert_error(
        &mut core,
        &deepen_request(&rows, "root-0000-one"),
        ENGINE_FAILURE,
        "fixture",
        "prefix is not unique: root-0000-one",
    )?;
    // A lone exact Unicode match never evaluated the legacy prefix slice.
    let unicode = [snapshot(UNICODE_ROOT, "")];
    assert_error(&mut core, &deepen_request(&unicode, UNICODE_ROOT), ENGINE_FAILURE, "fixture",
        "this root is not flat (0 direct children, 0 in the grandchild chain) — hierarchy already exists or too few entries to deepen")?;
    assert_error(
        &mut core,
        &deepen_request(&[], UNICODE_ROOT),
        ENGINE_FAILURE,
        "fixture",
        UNICODE_ROOT_MISSING,
    )?;
    let mut envelope = deepen_request(&rows, "missing");
    envelope["future_top"] = json!(true);
    let (status, response) = abi_call(&mut core, &envelope)?;
    assert_eq!(status, INVALID_ARGUMENT);
    assert_eq!(response["error"]["code"], INVALID_ARGUMENT);
    assert_eq!(response["request_id"], "");
    assert!(response["error"]["message"]
        .as_str()
        .context("invalid envelope error has no message")?
        .contains("unknown field `future_top`"));
    let mut unknown_snapshot = deepen_request(&rows, "missing");
    unknown_snapshot["payload"]["snapshots"][0]["future_snapshot"] = json!(true);
    let (status, response) = abi_call(&mut core, &unknown_snapshot)?;
    assert_eq!(status, ENGINE_FAILURE); // Snapshot validation precedes prefix resolution.
    assert_eq!(response["error"]["code"], ENGINE_FAILURE);
    assert_eq!(response["request_id"], "fixture");
    assert!(response["error"]["message"]
        .as_str()
        .context("invalid snapshot error has no message")?
        .contains("unknown field `future_snapshot`"));
    let mut wrong_schema = deepen_request(&rows, "missing");
    wrong_schema["schema_version"] = json!(2);
    assert_error(
        &mut core,
        &wrong_schema,
        ABI_MISMATCH,
        "fixture",
        "unsupported schema version",
    )?;
    wrong_schema["schema_version"] = json!(1);
    wrong_schema["operation"] = json!("future_test_operation");
    assert_error(
        &mut core,
        &wrong_schema,
        UNSUPPORTED_OPERATION,
        "fixture",
        "unsupported operation",
    )
}

#[test]
fn deepen_plan_v1_malformed_safety_exceptions_do_not_poison_handle() -> Result<()> {
    let mut core = Core::default();
    let root = snapshot("root", "");
    assert_error(
        &mut core,
        &deepen_request(&[root.clone()], UNICODE_ROOT),
        ENGINE_FAILURE,
        "fixture",
        UNICODE_ROOT_MISSING,
    )?;
    let cycle = [snapshot("root", "child"), snapshot("child", "root")];
    assert_error(
        &mut core,
        &deepen_request(&cycle, "root"),
        ENGINE_FAILURE,
        "fixture",
        "invalid tree: repeated descendant ID",
    )?;
    assert_error(&mut core, &deepen_request(&[root], "root"), ENGINE_FAILURE, "fixture",
        "this root is not flat (0 direct children, 0 in the grandchild chain) — hierarchy already exists or too few entries to deepen")
}
