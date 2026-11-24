use super::*;
use anyhow::anyhow;

#[test]
fn verified_variant_survives_base_cleanup_and_publishes_without_network() -> Result<()> {
    let _lock = crate::TEST_ENV_LOCK
        .lock()
        .map_err(|error| anyhow!("{error}"))?;
    let dir = tempfile::tempdir()?;
    let name = "fp16";
    let file = "onnx/model_fp16.onnx";
    let size = 3;
    let sha256 = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    let staging = variant_staging_dir(dir.path());
    std::fs::create_dir_all(staging.join("onnx"))?;
    std::fs::write(staging.join("onnx/model_fp16.onnx"), b"abc")?;
    let base_staging = dir.path().join(format!(".download-{MODEL_REVISION}"));
    std::fs::create_dir_all(&base_staging)?;
    std::fs::remove_dir_all(base_staging)?;
    install_variant_into(name, file, sha256, size, None, dir.path(), &[])?;
    install_variant_into(name, file, sha256, size, None, dir.path(), &[])?;
    let destination = dir.path().join(file);
    assert!(replace_file(&dir.path().join("missing-staged-model"), &destination).is_err());
    assert_eq!(std::fs::read(&destination)?, b"abc");
    let replacement = dir.path().join("verified-staged-model");
    std::fs::write(&replacement, b"abc")?;
    replace_file(&replacement, &destination)?;
    assert!(!replacement.exists());
    assert_eq!(
        std::fs::read(dir.path().join("onnx/model_fp16.onnx"))?,
        b"abc"
    );
    assert!(!staging.join("onnx/model_fp16.onnx").exists());

    use respire_protocol::{AssetRequirement, AssetState, AssetsStatus};
    // Canonicalize the fixture root, not a plan-supplied path: macOS temp roots may be links.
    let root = dir.path().canonicalize()?;
    let asset = AssetRequirement {
        name: "bge-m3-model".to_owned(),
        version: Some(MODEL_REVISION.to_owned()),
        sha256: Some(sha256.to_owned()),
        suggested_source: file_url("https://huggingface.co", file),
        state: AssetState::Missing,
        path: Some(
            root.join(file)
                .to_str()
                .context("fixture path is not Unicode")?
                .to_owned(),
        ),
        expected_size: Some(size),
        actual_size: None,
        actual_sha256: None,
        reason: None,
    };
    let plan = AssetsStatus::installation_plan(vec![asset.clone()]);
    assert!(!plan.complete);
    assert!(plan.backend_load.is_none());
    assert_eq!(plan.missing, plan.assets);
    for sources in [vec![], vec![vec![]], vec![vec![String::new()]]] {
        assert!(prepare_model_assets_with_sources(&plan, std::slice::from_ref(&root), &sources)
            .is_err());
    }
    // The real official artifact cannot match this three-byte pin: downloading would fail.
    prepare_model_assets(&plan, std::slice::from_ref(&root))?;
    assert!(prepare_model_assets_with_sources(&plan, std::slice::from_ref(&root),
        &[vec!["https://local-mirror.invalid/unused-verified-file".to_owned()]])?);
    assert_eq!(std::fs::read(root.join(file))?, b"abc");

    for (case, expected_error) in [
        ("outside", "outside trusted local roots"),
        ("duplicate", "overlapping paths"),
        ("http", "official HTTPS origin"),
        ("sha", "fixed SHA-256"),
        ("sha-nonhex", "fixed SHA-256"),
        ("missing-size", "size missing"),
        ("zero-size", "size missing"),
    ] {
        let destination = root.join(format!("rejected-{case}"));
        let mut rejected = asset.clone();
        rejected.path = Some(
            destination
                .join(file)
                .to_str()
                .context("fixture path is not Unicode")?
                .to_owned(),
        );
        let allowed = if case == "outside" {
            root.join("allowed")
        } else {
            destination.clone()
        };
        match case {
            "http" => {
                rejected.suggested_source =
                    rejected.suggested_source.replacen("https://", "http://", 1)
            }
            "sha" => rejected.sha256 = Some("not-a-sha256".to_owned()),
            "sha-nonhex" => rejected.sha256 = Some("z".repeat(64)),
            "missing-size" => rejected.expected_size = None,
            "zero-size" => rejected.expected_size = Some(0),
            _ => {}
        }
        let assets = if case == "duplicate" {
            vec![rejected.clone(), rejected]
        } else {
            vec![rejected]
        };
        let error = prepare_model_assets(&AssetsStatus::installation_plan(assets), &[allowed])
            .err()
            .ok_or_else(|| anyhow!("invalid {case} plan was accepted"))?;
        assert!(
            error.to_string().contains(expected_error),
            "{case}: {error:#}"
        );
        assert!(
            !destination.exists(),
            "{case} rejection must precede directory creation"
        );
    }
    // Keep all built-in profile source layouts executable; no network is used here.
    for (repo, revision, weights) in [
        ("Xenova/bge-m3", MODEL_REVISION, "onnx/model_quantized.onnx"),
        (
            "Xenova/bge-small-zh-v1.5",
            "75c43b069aac4d136ba6bc1122f995fedcfd2781",
            "onnx/model_quantized.onnx",
        ),
        (
            "mlx-community/bge-m3-mlx-fp16",
            "a37eddded9a6a1273a87fb8b0da0d1cdbd98aeec",
            "model.safetensors",
        ),
    ] {
        for resource in ["tokenizer.json", weights] {
            let mut builtin = asset.clone();
            builtin.name = if resource == "tokenizer.json" {
                "profile-tokenizer"
            } else {
                "profile-weights"
            }
            .to_owned();
            builtin.version = Some(revision.to_owned());
            builtin.suggested_source =
                format!("https://huggingface.co/{repo}/resolve/{revision}/{resource}");
            let filename = resource
                .rsplit('/')
                .next()
                .context("fixture file name missing")?;
            check_plan_source(&builtin, filename)?;
        }
    }
    assert_eq!(std::fs::read(root.join(file))?, b"abc");
    Ok(())
}
