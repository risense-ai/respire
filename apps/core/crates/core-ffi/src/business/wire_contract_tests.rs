use super::*;

#[test]
fn v1_wire_compatibility_is_explicit_at_each_boundary() -> Result<()> {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../contracts/core-abi/fixtures/wire-v1-compat.json"
    ))?;
    let entry: MemoryEntry = serde_json::from_value(fixture["entry"].clone())?;
    assert_eq!(entry.importance, "normal");
    assert!(entry.device.is_empty());
    assert_eq!(entry.see_also, ["older-a", "older-b"]);
    assert_eq!(serde_json::to_value(&entry)?["future_field"], json!({"revision": 2}));

    let query: MemoryQuery = serde_json::from_value(fixture["query"].clone())?;
    assert_eq!(query.limit, 3);
    assert!(query.kind.is_none());
    assert!(serde_json::to_value(query)?.get("future_hint").is_none());

    let mut snapshot = fixture["snapshot"].clone();
    let (rows, decoded_entries) = snapshots(json!([snapshot.clone()]), None)?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, entry.id);
    assert!(decoded_entries.is_empty());
    snapshot["future_field"] = json!(42);
    assert!(snapshots(json!([snapshot]), None).is_err());
    Ok(())
}
