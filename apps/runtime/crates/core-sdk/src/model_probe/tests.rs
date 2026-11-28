use super::*;
use crate::profiles::{self, Resource};

#[test]
fn external_onnx_data_works_with_file_probe_and_core_serving() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let literal = concat!(
        "08093a97010a1c0a0566697865641209656d62656464696e6722084964656e74697479120973796e7468657469632a2c0801",
        "08041001420566697865646a1b0a086c6f636174696f6e120f6d6f64656c2e6f6e6e785f6461746170015a210a09696e7075",
        "745f69647312140a120807120e0a0208010a081206746f6b656e73621b0a09656d62656464696e67120e0a0c080112080a0208010a0208044202100d",
    );
    let graph = (0..literal.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&literal[i..i + 2], 16))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let tokenizer = br#"{"version":"1.0","model":{"type":"WordLevel","vocab":{"[UNK]":0},"unk_token":"[UNK]"}}"#;
    let companion = [1.0f32, 0.0, 0.0, 0.0]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect::<Vec<_>>();
    let write = |name: &str, bytes: &[u8]| -> Result<Resource> {
        let path = dir.path().join(name);
        std::fs::write(&path, bytes)?;
        Ok(Resource {
            path,
            sha256: crate::host::digest(bytes),
            size: bytes.len() as u64,
            download: None,
        })
    };
    let weights = write("model.onnx", &graph)?;
    let tokenizer = write("tokenizer.json", tokenizer)?;
    let companion = write("installed-weights.data", &companion)?;
    let selection: Selection = serde_json::from_value(json!({
        "model":"fixture-external","backend":"onnx","engine":"cpu","variant":"fp32",
        "profile":{
            "id":"fixture-external","embedding":{"model_id":"fixture-external","revision":"r1","dimensions":4,
                "max_tokens":32,"pooling":"sentence_embedding","normalize":true,"query_prefix":"","document_prefix":""},
            "onnx":{"input_ids":"input_ids","output":"embedding"},
            "weights":weights,"tokenizer":tokenizer,"companions":[companion],
        },
        "backend_profile":{"id":"onnx","backend":"onnx"},
    }))?;
    profiles::scoped(selection.clone(), || {
        let mut incumbent = crate::Core::new()?;
        profiles::initialize(&mut incumbent, &selection)?;
        let infer = |core: &mut crate::Core| {
            core.call(
                "model_probe",
                json!({"model":selection.model,"text":"still serving"}),
            )
        };
        assert_eq!(infer(&mut incumbent)?["dimensions"], 4);
        let success = run_isolated(&selection, 4, false);
        assert_eq!(success["ok"], true, "{success}");
        assert_eq!(success["execution_provider"], "CPUExecutionProvider");
        let failure = run_isolated(&selection, 5, false);
        assert_eq!(failure["error_code"], "invalid_embedding", "{failure}");
        assert_eq!(infer(&mut incumbent)?["dimensions"], 4);
        Ok(())
    })
}
