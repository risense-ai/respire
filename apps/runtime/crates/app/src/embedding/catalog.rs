use super::*;
use crate::core_sdk::onnx::{self, ModelVariant};
use std::borrow::Cow;

pub const DEFAULT_MODEL: &str = "granite-311m-r2";
pub const DEFAULT_VARIANT: &str = "quint8_avx2";
pub(super) const LEGACY_DISTILL: &str = "bge-m3-distill-8l";
const IDS: [&str; 4] = [DEFAULT_MODEL, "granite-97m-r2", "m3", "bge-base-zh-v1.5"];

pub fn variants(id: &str) -> Cow<'static, [ModelVariant]> {
    match id {
        DEFAULT_MODEL => Cow::Borrowed(onnx::GRANITE_311M_VARIANTS),
        "granite-97m-r2" => Cow::Borrowed(onnx::GRANITE_97M_VARIANTS),
        "m3" => Cow::Owned([onnx::MODEL_VARIANTS, onnx::M3_PROFILE_VARIANTS].concat()),
        "bge-base-zh-v1.5" => Cow::Borrowed(onnx::BGE_BASE_ZH_VARIANTS),
        _ => Cow::Borrowed(&[]),
    }
}

pub fn variant_names() -> BTreeMap<&'static str, Vec<&'static str>> {
    IDS.into_iter()
        .map(|id| {
            (
                id,
                variants(id).iter().map(|variant| variant.name).collect(),
            )
        })
        .collect()
}

pub(super) fn model(id: &str, variant: Option<&str>) -> Result<ModelProfile> {
    let granite = matches!(id, DEFAULT_MODEL | "granite-97m-r2");
    let (repo, revision, dims, size, hash, pad) = match id {
        DEFAULT_MODEL => (
            "ibm-granite/granite-embedding-311m-multilingual-r2",
            "44399559930365213510b1ee2eb15ded83374f0e",
            768,
            33384821,
            "0087c868b33bad550a78a08d19798cfd7f713cde4f020803b8f51f405503e15f",
            0,
        ),
        "granite-97m-r2" => (
            "ibm-granite/granite-embedding-97m-multilingual-r2",
            "835ad14087e140460703cf0fae09f97d469d65c2",
            384,
            25301672,
            "4f2842d568e2724370aec203652a42ac783c7937f8347a1a2cc7506d71f1582f",
            179935,
        ),
        "m3" => (
            "Xenova/bge-m3",
            "4de13258303883538bd53b696b452bf8099f0858",
            1024,
            17082821,
            "6710678b12670bc442b99edc952c4d996ae309a7020c1fa0096dd245c2faf790",
            0,
        ),
        "bge-base-zh-v1.5" => (
            "Xenova/bge-base-zh-v1.5",
            "71e50dc531959f9e04ebf190ea25b00261a0a186",
            768,
            439124,
            "7dfbf1966ebf99d471c3796e9b457329d2b2182b817e144f1e904b957745c839",
            0,
        ),
        _ => anyhow::bail!("unknown curated model {id}"),
    };
    let directory = if id == "m3" {
        crate::core_sdk::bge::m3_model_dir()?
    } else {
        crate::service::main_data_dir().join("models").join(id)
    };
    let variant = match variant {
        Some("quantized") if granite => DEFAULT_VARIANT,
        Some(variant) => variant,
        None if granite => DEFAULT_VARIANT,
        None => "quantized",
    };
    let pin = variants(id)
        .iter()
        .find(|candidate| candidate.name == variant)
        .copied()
        .with_context(|| format!("unknown variant {variant} for {id}"))?;
    let prefix = if id == "bge-base-zh-v1.5" {
        "\u{4e3a}\u{8fd9}\u{4e2a}\u{53e5}\u{5b50}\u{751f}\u{6210}\u{8868}\u{793a}\u{4ee5}\u{7528}\u{4e8e}\u{68c0}\u{7d22}\u{76f8}\u{5173}\u{6587}\u{7ae0}\u{ff1a}"
    } else {
        ""
    };
    let mut onnx = json!({"input_ids":"input_ids","attention_mask":"attention_mask","output":"last_hidden_state"});
    if !granite {
        onnx["token_type_ids"] = json!("token_type_ids");
    }
    let companions = if id == "m3" && variant == "fp32" {
        let data = onnx::M3_EXTERNAL_DATA;
        vec![resource(
            &directory,
            "onnx/model.onnx_data",
            data.size,
            data.sha256,
            repo,
            revision,
        )]
    } else {
        Vec::new()
    };
    Ok(ModelProfile {
        id: id.to_owned(),
        embedding: json!({"model_id":repo,"revision":revision,"dimensions":dims,"max_tokens":if granite {32768} else {512},"pooling":"cls","normalize":true,"query_prefix":prefix,"document_prefix":"","pad_token_id":pad,"pad_to_max_tokens":false}),
        onnx,
        weights: Some(resource(
            &directory,
            &onnx::variant_file_name(variant),
            pin.size,
            pin.sha256,
            repo,
            revision,
        )),
        tokenizer: Some(resource(
            &directory,
            "tokenizer.json",
            size,
            hash,
            repo,
            revision,
        )),
        companions,
        isolate_engines: true,
    })
}

pub(super) fn models() -> Result<BTreeMap<String, ModelProfile>> {
    IDS.into_iter()
        .map(|id| Ok((id.to_owned(), model(id, None)?)))
        .collect()
}

pub fn default_selection() -> Result<Selection> {
    select_from_registry(
        Choice {
            model: DEFAULT_MODEL.to_owned(),
            backend: "onnx".to_owned(),
            engine: Engine::Cpu,
            variant: Some(DEFAULT_VARIANT.to_owned()),
        },
        &builtin_registry()?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_families_have_pinned_variants_and_granite_is_default() -> Result<()> {
        let registry = builtin_registry()?;
        assert_eq!(
            registry
                .models
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["bge-base-zh-v1.5", DEFAULT_MODEL, "granite-97m-r2", "m3"]
        );
        assert_eq!(variant_names()["m3"].len(), 8);
        assert_eq!(variant_names()["bge-base-zh-v1.5"].len(), 8);
        let default = default_selection()?;
        assert_eq!(default.model, DEFAULT_MODEL);
        assert_eq!(default.variant.as_deref(), Some(DEFAULT_VARIANT));
        assert_eq!(
            default.profile.weights.context("default weights")?.size,
            313421909
        );
        assert_eq!(default.profile.embedding["dimensions"], 768);
        assert_eq!(default.profile.embedding["max_tokens"], 32768);
        for (id, variants) in variant_names() {
            for variant in variants {
                let profile = model(id, Some(variant))?;
                let weights = profile.weights.context("catalog weights")?;
                assert_eq!(weights.sha256.len(), 64);
                assert!(weights.path.ends_with(onnx::variant_file_name(variant)));
                assert!(weights
                    .download
                    .as_ref()
                    .is_some_and(|url| url.contains("/resolve/")));
                assert_eq!(
                    profile.companions.len(),
                    usize::from(id == "m3" && variant == "fp32")
                );
            }
        }
        assert_eq!(
            model("granite-97m-r2", Some("standard"))?.embedding["pad_token_id"],
            179935
        );
        Ok(())
    }

    #[test]
    fn new_library_default_never_rewrites_an_existing_selection_or_legacy_library() -> Result<()> {
        use crate::transport::local::LocalStore;
        let root = tempfile::tempdir()?;
        let path = root.path().join("fresh.db");
        let mut selection = default_selection()?;
        selection.generation = crate::core_sdk::profiles::generation_key_for_profile(&selection)?;
        let fresh = LocalStore::open_with_model_default(&path, || Ok(Some(selection.clone())))?;
        assert_eq!(
            fresh.active_selection()?.context("fresh selection")?.model,
            DEFAULT_MODEL
        );
        drop(fresh);
        let existing = LocalStore::open_with_model_default(&path, || {
            anyhow::bail!("default called for an upgrade")
        })?;
        assert_eq!(existing.active_generation_key()?, selection.generation);
        let path = root.path().join("legacy.db");
        drop(LocalStore::open(&path)?);
        let legacy = LocalStore::open_with_model_default(&path, || {
            anyhow::bail!("default called for legacy library")
        })?;
        assert!(legacy.active_selection()?.is_none());
        assert_eq!(legacy.retrieval_model()?, "m3");
        Ok(())
    }

    #[test]
    fn switching_variants_keeps_their_exact_configured_paths() -> Result<()> {
        let mut registry = builtin_registry()?;
        for (id, names) in [
            (DEFAULT_MODEL, [DEFAULT_VARIANT, "standard"]),
            ("granite-97m-r2", [DEFAULT_VARIANT, "standard"]),
            ("m3", ["q4f16", "fp16"]),
        ] {
            for name in names {
                let mut profile = model(id, Some(name))?;
                let weights = profile.weights.as_mut().context("candidate weights")?;
                weights.path = PathBuf::from(id)
                    .join(name)
                    .join(weights.path.file_name().context("weights filename")?);
                registry
                    .configured_variants
                    .entry(id.to_owned())
                    .or_default()
                    .insert(name.to_owned(), profile.clone());
                registry.models.insert(id.to_owned(), profile);
            }
            let mut generations = std::collections::BTreeSet::new();
            for name in names {
                for engine in [Engine::Cpu, Engine::Gpu, Engine::Npu] {
                    let target = select_from_registry(
                        Choice {
                            model: id.to_owned(),
                            backend: "onnx".to_owned(),
                            engine,
                            variant: Some(name.to_owned()),
                        },
                        &registry,
                    )?;
                    assert_eq!(target.variant.as_deref(), Some(name));
                    assert_eq!(target.engine, engine);
                    let weights = target
                        .profile
                        .weights
                        .as_ref()
                        .context("candidate weights")?;
                    assert_eq!(
                        weights.path.parent(),
                        Some(std::path::absolute(PathBuf::from(id).join(name))?.as_path())
                    );
                    assert!(generations.insert(
                        crate::core_sdk::profiles::generation_key_for_profile(&target)?
                    ));
                }
            }
        }
        Ok(())
    }

    #[test]
    fn legacy_m3_registry_uses_its_model_root_for_other_variants() -> Result<()> {
        let mut registry = builtin_registry()?;
        let root = tempfile::tempdir()?;
        let m3 = registry.models.get_mut("m3").context("M3 profile")?;
        m3.weights.as_mut().context("M3 weights")?.path = root.path().join("custom/current.onnx");
        m3.tokenizer.as_mut().context("M3 tokenizer")?.path =
            root.path().join("custom-tokenizer.json");
        m3.embedding["query_prefix"] = json!("legacy query: ");
        let target = select_from_registry(
            Choice {
                model: "m3".to_owned(),
                backend: "onnx".to_owned(),
                engine: Engine::Cpu,
                variant: Some("fp16".to_owned()),
            },
            &registry,
        )?;
        assert_eq!(
            target.profile.weights.context("variant weights")?.path,
            root.path().join("onnx/model_fp16.onnx")
        );
        assert_eq!(
            target.profile.tokenizer.context("variant tokenizer")?.path,
            root.path().join("custom-tokenizer.json")
        );
        assert_eq!(target.profile.embedding["query_prefix"], "legacy query: ");
        Ok(())
    }
}
