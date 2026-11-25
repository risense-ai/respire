use super::*;

#[test]
#[ignore = "requires a pinned real rsrs-runtime binary and local CPU inference fixture"]
fn native_owned_candidate_can_be_reaped_without_retiring_foreground() -> Result<()> {
    let binary = std::env::var_os("RSRS_NATIVE_CANDIDATE_TEST_BINARY")
        .map(PathBuf::from)
        .context("pinned native fixture binary required")?;
    let root = tempfile::tempdir()?;
    let index = root.path().join("index");
    std::fs::create_dir(&index)?;
    let weights = root.path().join("identity.onnx");
    let tokenizer = root.path().join("tokenizer.json");
    let literal = concat!(
        "08093a8a010a1c0a0566697865641209656d62656464696e6722084964656e74697479120973796e7468657469632a1f0801",
        "0804100122100000803f000000000000000000000000420566697865645a210a09696e7075745f69647312140a120807120e",
        "0a0208010a081206746f6b656e73621b0a09656d62656464696e67120e0a0c080112080a0208010a0208044202100d",
    );
    let model = (0..literal.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&literal[i..i + 2], 16))
        .collect::<Result<Vec<_>, _>>()?;
    let tokens = br#"{"version":"1.0","model":{"type":"WordLevel","vocab":{"[UNK]":0},"unk_token":"[UNK]"}}"#;
    std::fs::write(&weights, &model)?;
    std::fs::write(&tokenizer, tokens)?;
    let resource = |path, bytes: &[u8]| json!({"path":path,"sha256":crate::host::digest(bytes),"size":bytes.len()});
    let selection: Selection = serde_json::from_value(json!({
        "model":"fixture-candidate","backend":"onnx","engine":"cpu",
        "profile":{
            "id":"fixture-candidate","embedding":{"model_id":"fixture-candidate","revision":"r1","dimensions":4,
                "max_tokens":32,"pooling":"sentence_embedding","normalize":true,"query_prefix":"",
                "document_prefix":"","pad_token_id":0,"pad_to_max_tokens":false},
            "onnx":{"input_ids":"input_ids","output":"embedding"},
            "weights":resource(weights,&model),"tokenizer":resource(tokenizer,tokens),
        },
        "backend_profile":{"id":"onnx","backend":"onnx"},
    }))?;
    let start =
        || CandidateWorker::start_inner(root.path(), &index, selection.clone(), Some(&binary));
    let foreground_generation = generation();
    let mut candidate = start()?;
    let pid = candidate.process.pid;
    assert_ne!(pid, std::process::id());
    assert!(start().is_err());
    assert!(!candidate.probe(|| Ok(()))?.is_null());
    let entry: MemoryEntry = serde_json::from_value(json!({
        "id":"fixture","kind":"Context","tags":[],"title":"Candidate native marker",
        "content":"Candidate bounded native fixture","user":"local","computer":"","project":"",
        "created_at":"2026-11-25T00:00:00Z","updated_at":"2026-11-25T00:00:00Z",
        "emotion":-1,"parent_id":"","importance":"important",
    }))?;
    let artifact = candidate.prepare(&entry, || Ok(()))?.artifact;
    assert!(!artifact.is_empty() && !artifact.starts_with(b"rsi1:"));
    assert!(crate::index_ready(&[artifact])?);
    assert_eq!(std::fs::read_dir(&index)?.count(), 0);
    assert!(!root.path().join("rsrs.db").exists());
    assert!(!root.path().join("lock.db").exists());
    let mut checks = 0;
    let error = candidate
        .probe(|| {
            checks += 1;
            anyhow::ensure!(checks == 1, "fixture cancelled");
            Ok(())
        })
        .expect_err("candidate must reject a cancelled reply");
    assert!(error.to_string().contains("fixture cancelled"));
    assert!(candidate.process.reaped.load(Ordering::Acquire));
    let second = start()?;
    assert_ne!(second.process.pid, pid);
    second.close()?;
    candidate.close()?;
    assert_eq!(generation(), foreground_generation);
    assert_eq!(status()?["phase"], "idle");
    Ok(())
}
