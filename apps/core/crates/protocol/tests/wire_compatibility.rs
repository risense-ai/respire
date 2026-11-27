use respire_protocol::{Artifact, MemoryEntry, MemoryMeta, MemoryQuery, RenderBudget, Scope};
use serde_json::{json, Value};

#[test]
fn historical_entry_and_query_keep_their_unknown_field_policies() -> anyhow::Result<()> {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../contracts/core-abi/fixtures/wire-v1-compat.json"
    ))?;
    let entry: MemoryEntry = serde_json::from_value(fixture["entry"].clone())?;
    assert_eq!(entry.importance, "normal");
    assert!(entry.device.is_empty());
    assert_eq!(entry.see_also, ["older-a", "older-b"]);
    let encoded = serde_json::to_value(entry)?;
    assert_eq!(encoded["future_field"], json!({"revision": 2}));
    assert!(encoded.get("memory_contract").is_none());

    let query: MemoryQuery = serde_json::from_value(fixture["query"].clone())?;
    assert_eq!(query.limit, 3);
    assert!(query.kind.is_none());
    assert!(serde_json::to_value(query)?.get("future_hint").is_none());
    Ok(())
}

#[test]
fn metadata_extensions_round_trip_without_granting_trust() -> anyhow::Result<()> {
    let extension = json!({"future_semantics": {"revision": 2}});
    let scope: Scope = serde_json::from_value(extension.clone())?;
    let artifact: Artifact = serde_json::from_value(extension.clone())?;
    assert_eq!(
        serde_json::to_value(scope)?["future_semantics"],
        extension["future_semantics"]
    );
    assert_eq!(
        serde_json::to_value(artifact)?["future_semantics"],
        extension["future_semantics"]
    );

    let metadata: MemoryMeta = serde_json::from_value(json!({
        "memory_contract": 1, "artifact": extension, "future_top_level": true
    }))?;
    let encoded = serde_json::to_value(metadata)?;
    assert_eq!(
        encoded["artifact"]["future_semantics"],
        json!({"revision": 2})
    );
    assert!(encoded.get("future_top_level").is_none());
    Ok(())
}

#[test]
fn complete_render_inputs_reject_unknown_fields() -> anyhow::Result<()> {
    let mut input = json!({
        "budget_chars": 100, "prefix_by_count": [""], "separator": "",
        "suffix": "", "items": []
    });
    let _: RenderBudget = serde_json::from_value(input.clone())?;
    input["future_field"] = json!(true);
    assert!(serde_json::from_value::<RenderBudget>(input).is_err());
    Ok(())
}
