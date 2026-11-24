use super::*;

#[test]
fn owned_snapshots_preserve_validation_errors() -> Result<()> {
    let valid = json!({"id":"a","deleted":false,"updated_at":"2026-10-10T00:00:00Z",
        "kind":"context","tags":"","title":"needle","project":"","computer":"",
        "parent_id":"","created_at":"2026-10-10T00:00:00Z","content_head":"",
        "recall_count":0,"importance":"important","entry":null,"features":null});
    let mut unknown = valid.clone();
    unknown["unexpected"] = json!(true);
    let mut missing = valid.clone();
    missing.as_object_mut().context("fixture object")?.remove("title");
    for value in [Value::Null, json!({}), json!([unknown]), json!([missing])] {
        let borrowed = Vec::<Snapshot>::deserialize(&value).err().context("borrowed error")?.to_string();
        let owned = snapshots(value, None).err().context("owned error")?.to_string();
        assert_eq!(owned, borrowed);
    }
    let duplicate = snapshots(json!([valid.clone(), valid.clone()]), None)
        .err().context("duplicate error")?;
    assert_eq!(duplicate.to_string(), "duplicate snapshot ID");
    let mut mismatch = valid.clone();
    mismatch["entry"] = json!({"id":"other","kind":"Context","tags":[],"title":"needle",
        "content":"body","user":"test","computer":"test","project":"",
        "created_at":"2026-10-10T00:00:00Z","updated_at":"2026-10-10T00:00:00Z",
        "emotion":0,"parent_id":"","importance":"important"});
    assert_eq!(snapshots(json!([mismatch]), None).err().context("entry error")?.to_string(),
        "snapshot entry ID mismatch");
    let mut wrong_model = valid.clone();
    wrong_model["features"] = json!({"artifact":artifact("other", &[1.0], &[])?});
    assert_eq!(snapshots(json!([wrong_model]), Some("m3")).err().context("model error")?.to_string(),
        "Core artifact belongs to another model; rebuild the index");
    let mut invalid_revision = read_artifact(&artifact("m3", &[1.0], &[])?)?;
    invalid_revision.revision = "old-revision".to_owned();
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    serde_json::to_writer(&mut encoder, &invalid_revision)?;
    let mut invalid = valid;
    invalid["features"] = json!({"artifact":STANDARD.encode(encoder.finish()?)});
    assert_eq!(snapshots(json!([invalid]), Some("m3")).err().context("revision error")?.to_string(),
        "unsupported Core artifact revision; rebuild the index");
    assert_eq!(take(&mut Value::Null, "snapshots"), Value::Null);
    assert_eq!(take(&mut json!({}), "snapshots"), Value::Null);
    Ok(())
}

#[test]
fn borrowed_artifact_preserves_existing_wire_bytes() -> Result<()> {
    use std::io::Write;
    let document = vec![0.0, -1.25, 1.0e-8, f32::MAX];
    let chunks = vec![vec![0.5, -0.75, 0.0, 1.0], vec![0.0; 4]];
    let encoded = artifact("test-model", &document, &chunks)?;
    let decoded = read_artifact(&encoded)?;
    assert_eq!(decoded.document, document);
    assert_eq!(decoded.chunks, chunks);
    // The former owned serializer is the compatibility oracle, including
    // JSON float formatting and zlib output, rather than a second decoder.
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&serde_json::to_vec(&decoded)?)?;
    assert_eq!(encoded, STANDARD.encode(encoder.finish()?));
    assert!(read_artifact("invalid-base64").is_err());
    Ok(())
}
