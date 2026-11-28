use super::*;
use respire_app::core_sdk::profiles::{BackendProfile, ModelProfile, Resource};
use serde_json::json;

fn profile(id: &str) -> ModelProfile {
    let asset = Resource {
        path: "synthetic-local-secret-path".into(),
        size: 1,
        sha256: "ab".repeat(32),
        download: None,
    };
    ModelProfile {
        id: id.to_owned(),
        embedding: json!({
            "model_id":"fixture", "revision":"immutable", "dimensions":3,
            "max_tokens":64, "pooling":"cls", "normalize":true,
            "query_prefix":"", "document_prefix":""
        }),
        onnx: json!({"input_ids":"input_ids"}),
        weights: Some(asset.clone()),
        tokenizer: Some(asset),
        companions: Vec::new(),
        isolate_engines: false,
    }
}

#[test]
fn catalog_projects_only_verified_engine_and_public_fields() -> Result<()> {
    let mut registry = Registry::default();
    for id in ["m3", "m3-mlx"] {
        registry.models.insert(id.to_owned(), profile(id));
    }
    registry.backends.insert(
        "onnx".into(),
        BackendProfile {
            id: "onnx".into(),
            backend: "onnx".into(),
            native: None,
            provider: None,
            protocol: None,
            endpoint: None,
            keyring_service: Some("synthetic-keyring-private".into()),
            keyring_account: None,
        },
    );
    let value = serde_json::to_value(project_catalog("m3".to_owned(), &registry))?;
    assert_eq!(value["active_model_id"], "m3");
    assert_eq!(value["models"][0]["engines"][0]["available"], true);
    assert_eq!(value["models"][0]["engines"][2]["available"], false);
    assert_eq!(value["models"][1]["engines"][0]["available"], false);
    assert!(!value.to_string().contains("synthetic-"));

    let temp = tempfile::tempdir()?;
    let store = LocalStore::open(&temp.path().join("fixture.db"))?;
    let mut selected = profile("m3");
    let weights = temp.path().join("weights.onnx");
    let tokenizer = temp.path().join("tokenizer.json");
    std::fs::write(&weights, b"x")?;
    std::fs::write(&tokenizer, b"y")?;
    selected
        .weights
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("fixture weights"))?
        .path = weights.clone();
    selected
        .tokenizer
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("fixture tokenizer"))?
        .path = tokenizer.clone();
    let selection = Selection {
        model: "m3".to_owned(),
        backend: "onnx".to_owned(),
        engine: Engine::Cpu,
        variant: None,
        profile: selected,
        backend_profile: registry.backends["onnx"].clone(),
        generation: "fixture".to_owned(),
    };
    store.meta_set(
        respire_app::core_sdk::profiles::ACTIVE_SELECTION_KEY,
        &serde_json::to_string(&selection)?,
    )?;
    let assets = active_assets(&store).map_err(|problem| anyhow::anyhow!("{problem:?}"))?;
    assert_eq!(assets["active_model_id"], "m3");
    assert_eq!(assets["locked"], true);
    assert_eq!(
        assets["active_paths"],
        json!([weights.canonicalize()?, tokenizer.canonicalize()?,])
    );
    Ok(())
}
